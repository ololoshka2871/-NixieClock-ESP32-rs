//! Приложение-шаблон для ESP32 (строится поверх esp32-wifi-bootloader на плате):
//! - WiFi (esp-radio + embassy-net + DHCP), логи POST'ом на адрес коллектора
//!   (NVS/WebGUI; не настроен — логи только в UART и /logs),
//!   HTTP-эндпоинты на устройстве: /status /logs /ota?url=... /revert;
//! - WiFi OTA по штатному ESP-IDF механизму: закачка в свободный слот,
//!   гейт по магик-байту 0xE9, активация, подтверждение Valid только после
//!   поднятия WiFi; неподтверждённый образ откатывается загрузчиком (guard 60 с);
//! - бизнес-логика живёт в pure-крейте clock-core (хостовые тесты: cargo test -p clock-core),
//!   здесь только инфраструктура и задачи (app.rs).

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_net::{
    ConfigV4, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    pubsub::{PubSubChannel, Publisher},
};
use embassy_time::Timer;
use esp_alloc as _;
use esp_bootloader_esp_idf::{
    ota::OtaImageState,
    partitions::{
        AppPartitionSubType, PartitionType, read_partition_table,
    },
};
use esp_hal::{
    analog::adc::{Adc, AdcCalLine, AdcConfig, Attenuation},
    clock::CpuClock,
    i2c::master::{Config as I2cConfig, I2c},
    peripherals::{ADC1, GPIO34, GPIO35, GPIO39},
    ram,
    rng::Rng,
    rtc_cntl::{Rtc, RwdtStage},
    system::software_reset,
    time::{Duration as HalDuration, Rate},
    timer::timg::TimerGroup,
    uart::{Config as UartConfig, Uart},
};
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::{
    sta::StationConfig, AuthenticationMethodConfig, Config as WifiConfig, ControllerConfig,
    PowerSaveMode, WifiController,
};
use esp_storage::FlashStorage;

use clock_core::softap::{self, WifiCredentials};
use netdev::{NetDevice, MODE_AP, MODE_STA, RADIO_MODE};

// Статическая аллокация для embassy-задач (как в примерах esp-hal).
// macro_rules виден всем модулям crate'а, объявленным после него.
macro_rules! mkstatic {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

mod app;
mod clock;
mod crashlog;
#[cfg(feature = "display")]
mod display;
mod flashcell;
mod hangdetect;
mod icmp;
mod logger;
mod netdev;
mod power;
mod rtcmark;
mod sensors;
mod server;
mod settings;
mod tz;
mod ui;
mod updater;
mod uarttap;
mod ws2812;

esp_bootloader_esp_idf::esp_app_desc!();

pub const SSID: &str = env!("SSID");
pub const PASSWORD: &str = env!("PASSWORD");
pub const LOG_URL: &str = match option_env!("LOG_URL") {
    Some(v) => v,
    // Коллектор опционален (issue #6): без build-time адреса часы работают
    // сами — логи только в UART/ring, адрес ставится из WebGUI в NVS.
    None => "",
};

/// Build-time пароль SoftAP-эвакуации из секрета репозитория `SOFTAP_PASS`
/// (Gitea secrets). Если секрета нет — SoftAP-эвакуация не поднимается
/// (NULL-пароль esp-radio запрещает, softap::ap_config это ловит).
pub const SOFTAP_PASS: Option<&str> = option_env!("SOFTAP_PASS");

/// Сколько секунд даём новому (неподтверждённому) образу подтвердиться
/// (`confirm_app_alive` — циклы индикации; Wi-Fi условием не является).
pub const PENDING_TIMEOUT_SECS: u64 = 60;
/// Таймаут reset-watchdog.
pub const WDT_TIMEOUT_SECS: u64 = 30;

pub static PENDING_BOOT: AtomicBool = AtomicBool::new(false);
pub static BOOT_CONFIRMED: AtomicBool = AtomicBool::new(false);
pub static ROLLBACK_LATCHED: AtomicBool = AtomicBool::new(false);
/// Wi-Fi реально поднят (есть валидные креды и контроллер инициализирован).
/// False — устройство живёт без сети: индикация, датчики и локальные логи
/// работают как обычно, недоступен только HTTP/OTA по LAN.
pub static WIFI_READY: AtomicBool = AtomicBool::new(false);
pub static OTA_BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub struct OtaRequest {
    pub url: heapless::String<256>,
}

pub type OtaReqChannel = PubSubChannel<CriticalSectionRawMutex, OtaRequest, 2, 1, 2>;
pub static OTA_REQ: OtaReqChannel = PubSubChannel::new();
pub type OtaPublisher<'a> =
    Publisher<'a, CriticalSectionRawMutex, OtaRequest, 2, 1, 2>;

/// Полный размер DRAM2_UNINIT для ESP32.
const DRAM2_UNINIT_SIZE: usize = {
    let range: core::ops::Range<usize> =
        esp_metadata_generated::memory_range!("DRAM2_UNINIT");
    range.end - range.start
};

#[esp_hal::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: DRAM2_UNINIT_SIZE);
    esp_alloc::heap_allocator!(size: 48 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // Независимый детектор зависаний на TIMG0.timer1 (см. hangdetect.rs).
    // TIMG1 на классике — вотчина WiFi-блоба (wdev/BLE timers), он ломает
    // наш аларм при инициализации WiFi: 0.6.3 именно так и умерла.
    hangdetect::setup(timg0.timer1);

    logger::init();
    log::info!(
        "=== {} v{} starting (template app over esp32-wifi-bootloader) ===",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );
    // Что заставило чип перезапуститься: RWDT (зависание), brownout, panic,
    // software reset — критично для диагностики crash-loop'ов.
    log::info!("boot reason: {:?}", esp_hal::system::reset_reason());
    // Счётчики в RTC: сколько подъёмов и сколько из них после BOD.
    let brownout = matches!(
        esp_hal::system::reset_reason(),
        Some(esp_hal::rtc_cntl::SocResetReason::SysBrownOut)
    );
    let (boots, brownouts) = rtcmark::boot_tick(brownout);
    log::info!("boot #{} (brown-out сбросов: {})", boots, brownouts);
    log::info!("cpu clock: {}", esp_hal::clock::cpu_clock());
    // Brown-out детектор: включаем/печатаем до индикации, чтобы любая
    // просадка питания под нагрузкой стала видна на следующем буте.
    power::brownout_report();

    // Watchdog включаем сразу: завис/упал -> сброс -> откат неподтверждённого образа.
    let mut rtc = Rtc::new(peripherals.RTC_TIMER);
    rtc.rwdt
        .set_timeout(RwdtStage::Stage0, HalDuration::from_secs(WDT_TIMEOUT_SECS));
    rtc.rwdt.enable();
    spawner.spawn(wdt_task(rtc).unwrap());

    // Читаем состояние OTA: если загрузчик запустил новый (неподтверждённый) образ,
    // включаем guard подтверждения.
    // Единая flash-ячейка: NVS и OTA-апдейтер получают свои zero-sized ручки,
    // доступ к FlashStorage сериализуется мьютексом (см. flashcell.rs).
    flashcell::init(FlashStorage::new(peripherals.FLASH));
    // Причина смерти прошлого бута (если это была паника) — в логи.
    crashlog::dump_previous_panic();
    // Снимок прошлой зависимости от independent-ISR (если был).
    hangdetect::report_prev();
    // Крошки из RTC: последний HTTP-запрос и текст паники, если NVS не успела.
    rtcmark::report();

    // После UART-прошивки otadata стёрта: crate считает «текущий» слот Factory,
    // и первый OTA может перетереть уже загруженный слот. Запоминаем
    // реально загруженный слот (по MMU entry 0) — ниже инициализируем otadata им.
    let booted_app = flashcell::with_flash(|flash| {
        let ptbuf = unsafe { flashcell::ptbuf() };
        match read_partition_table(flash, ptbuf) {
            Ok(pt) => match pt.booted_partition() {
                Ok(Some(p)) => match p.partition_type() {
                    PartitionType::App(sub) => Some(sub),
                    _ => None,
                },
                _ => None,
            },
            Err(e) => {
                log::warn!("cannot read partition table for booted slot: {:?}", e);
                None
            }
        }
    });

    // otadata стёрта (selected == Factory) -> фиксируем выбор на
    // реально загруженном слоте, иначе первый OTA пойдёт в него же.
    let otadata_fix = updater::with_updater(|u| match u.selected_partition() {
        Ok(AppPartitionSubType::Factory) => {
            let target = booted_app.unwrap_or(AppPartitionSubType::Ota0);
            let name = match target {
                AppPartitionSubType::Ota0 => "ota_0",
                AppPartitionSubType::Ota1 => "ota_1",
                _ => "slot?",
            };
            log::warn!("otadata erased: initialising selection to {}", name);
            u.ota_data()
                .and_then(|mut o| o.set_current_app_partition(target))
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    });
    match otadata_fix {
        Ok(()) => {}
        Err(e) => log::error!("no OTA partitions / otadata init failed: {}", e),
    }

    // Если otadata选的 слот != реально загруженный — загрузчик пытался
    // поднять новый слот, НЕ смог его загрузить (битый образ) и откатился.
    // Подтверждать/гардить можно только свой (загруженный) слот: иначе
    // хороший образ пометит VALID мусорный слот, в котором загрузчик
    // уже был.
    let selected = updater::with_updater(|u| u.selected_partition());
    let fell_back = match (&booted_app, &selected) {
        (Some(bapp), Ok(sapp)) if *sapp != AppPartitionSubType::Factory && *sapp != *bapp => {
            log::warn!(
                "otadata selects {:?} but this app boots from {:?}: bootloader rolled back",
                sapp,
                bapp
            );
            true
        }
        _ => false,
    };
    if fell_back {
        let r = updater::with_updater(
            |u| -> core::result::Result<(), esp_bootloader_esp_idf::partitions::Error> {
                let st = u.current_ota_state()?;
                if st == OtaImageState::New || st == OtaImageState::PendingVerify {
                    log::warn!(
                        "selected slot state {:?} -> marking INVALID (its image failed to load)",
                        st
                    );
                    u.set_current_ota_state(OtaImageState::Invalid)?;
                }
                Ok(())
            },
        );
        if let Err(e) = r {
            log::warn!("fallback state fix failed: {}", e);
        }
    } else {
        let state = updater::with_updater(|u| u.current_ota_state());
        match state {
            Ok(st) => {
                log::info!("OTA state of the booted image: {:?}", st);
                if st == OtaImageState::New || st == OtaImageState::PendingVerify {
                    PENDING_BOOT.store(true, Ordering::SeqCst);
                    log::warn!(
                        "image is not confirmed yet: {}s to show live display cycles, otherwise rollback",
                        PENDING_TIMEOUT_SECS
                    );
                }
            }
            Err(e) => log::warn!("cannot read OTA state: {}", e),
        }
    }

    // WiFi: сохранённые в NVS креды; если их нет — креды из времени сборки (env).
    // Ни того, ни другого нет (или креды невалидны) — НЕ паникуем: часы живут
    // без сети, Wi-Fi настраивается позже (POST /api/wifi) либо UART-прошивкой.
    let creds = wifi_credentials();
    let wifi_ssid: Option<heapless::String<32>> = creds.as_ref().map(|c| c.ssid.clone());
    // Копия кредов станции: нужны connection-таску для возврата из SoftAP-эвакуации
    // в STA (build-time SSID/PASSWORD могли остаться плейсхолдерами, если креды
    // взяты из NVS).
    let station_creds = creds.clone();
    settings::set_active(&creds.clone().unwrap_or_default(), settings::load_wifi().is_some());
    // Пояс из NVS: до старта индикации и часовых задач.
    tz::load();
    // Подсветка из NVS (цвет часов, эффект) — до старта задач индикации.
    settings::load_led();
    // Адрес лог-коллектора (issue #6): NVS > сборочный default LOG_URL > выключен.
    match settings::load_log_url() {
        Some(u) => settings::apply_log_url_runtime(u.as_str()),
        None => settings::apply_log_url_runtime(LOG_URL),
    }
    // Wi-Fi поднимается только если есть валидные креды. Без них сеть просто
    // отсутствует — индикация, датчики, SNTP-задачи и локальные логи работают.
    let mut controller = creds.and_then(|c| {
        let station = StationConfig::default()
            .with_ssid(c.ssid.as_str().try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                c.password.as_str().try_into().unwrap(),
            ));
        match WifiController::new(
            peripherals.WIFI,
            ControllerConfig::default().with_initial_config(WifiConfig::Station(station)),
        ) {
            Ok(ctrl) => Some(ctrl),
            Err(e) => {
                log::error!("wifi init failed ({:?}) — running without wifi", e);
                None
            }
        }
    });
    if let Some(ctrl) = controller.as_mut() {
        // Без light sleep: логи должны уходить без задержек.
        let _ = ctrl.set_power_saving(PowerSaveMode::None);
        WIFI_READY.store(true, Ordering::SeqCst);
    } else {
        WIFI_READY.store(false, Ordering::SeqCst);
    }
    // M4: DS1307 на I2C0 (SDA=GPIO21, SCL=GPIO22, 400 kHz) — пины/адрес с
    // оригинальной IDF-платы (RTC.cpp). Единственный владелец шины —
    // clock_task; при отказе инициализации модель часов живёт на SNTP/HTTP.
    let i2c_res = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .map(|i2c| {
        i2c.with_sda(peripherals.GPIO21)
            .with_scl(peripherals.GPIO22)
            .into_async()
    });

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, runner) = embassy_net::new(
        // ICMP echo (ping) отвечают на уровне драйвера: smoltcp не принимает
        // Echo Request с произвольным identifier. См. firmware/src/icmp.rs.
        // NetDevice — диспетчер STA/SoftAP поверх одного стека (проектная
        // точка «один embassy_net на смену режимов», см. firmware/src/netdev.rs).
        icmp::IcmpDevice::new(NetDevice::new()),
        embassy_net::Config::dhcpv4(Default::default()),
        // smoltcp SocketSet фиксированный: DHCP занимает 1 слот, каждый
        // DnsSocket (logger + updater) и каждый TcpSocket сервера — ещё слоты,
        // каждое connect() на LOG_URL/OTA — ещё. StackResources<3> давал panic
        // "adding a socket to a full SocketSet" на первом же POST логов.
        // + UDP-слот SNTP и запас на параллельные DnsSocket/TcpSocket.
        mkstatic!(StackResources<12>, StackResources::<12>::new()),
        seed,
    );

    spawner.spawn(net_task(runner).unwrap());
    match i2c_res {
        Ok(i2c) => spawner.spawn(clock::clock_task(i2c).unwrap()),
        Err(e) => log::warn!("I2C0 init failed ({:?}): DS1307 unavailable, time via SNTP/HTTP only", e),
    }
    // M5: датчики. DS18x20 — GPIO4; MH-Z19 — UART2 (RX=GPIO16, TX=GPIO17;
    // зонд подтвердил пригодность пинов, доке-предупреждение про UART2 на
    // старых ревизиях нас не касается: MIN_CHIP_REVISION=100); шины питания —
    // ADC1 GPIO39/35/34 (input-only, никем не заняты), аттенюация 0 dB
    // (~1.1 В полный шкал; с делителей приходит 0.48–0.58 В), калибровка по eFuse.
    spawner.spawn(sensors::onewire_task(peripherals.GPIO4).unwrap());
    match Uart::new(peripherals.UART2, UartConfig::default().with_baudrate(9600)) {
        Ok(uart) => spawner.spawn(
            sensors::mhz19_task(
                uart.with_rx(peripherals.GPIO16)
                    .with_tx(peripherals.GPIO17)
                    .into_async(),
            )
            .unwrap(),
        ),
        Err(e) => log::warn!("UART2 init failed ({:?}): MH-Z19 unavailable", e),
    }
    let mut adc_cfg = AdcConfig::new();
    let p33 = adc_cfg.enable_pin_with_cal::<GPIO39<'static>, AdcCalLine<ADC1<'static>>>(
        peripherals.GPIO39,
        Attenuation::_0dB,
    );
    let p5 = adc_cfg.enable_pin_with_cal::<GPIO35<'static>, AdcCalLine<ADC1<'static>>>(
        peripherals.GPIO35,
        Attenuation::_0dB,
    );
    let p12 = adc_cfg.enable_pin_with_cal::<GPIO34<'static>, AdcCalLine<ADC1<'static>>>(
        peripherals.GPIO34,
        Attenuation::_0dB,
    );
    spawner.spawn(
        sensors::adc_task(Adc::new(peripherals.ADC1, adc_cfg), p33, p5, p12).unwrap(),
    );
    // M5: индикация HHMMSS. I2S1 8-bit + DMA: шина A GPIO27/26/25/33,
    // шина B GPIO32/12/18/19 (D2B=GPIO12 — jumper на плате, подтверждён
    // осциллографом 2026-11-16; оригинал так же вёл D2B на 12), стробы
    // TG0/TG1/TG2 = GPIO13/2/14 (активны
    // HIGH), write-strobe GPIO3. Слоты переключаются в полностью
    // погашенных окнах — призрачные цифры исключены (display.rs).
    #[cfg(feature = "display")]
    display::spawn(
        &spawner,
        peripherals.I2S1,
        peripherals.DMA_I2S1,
        peripherals.GPIO27,
        peripherals.GPIO26,
        peripherals.GPIO25,
        peripherals.GPIO33,
        peripherals.GPIO32,
        peripherals.GPIO12,
        peripherals.GPIO18,
        peripherals.GPIO19,
        peripherals.GPIO3,
        peripherals.GPIO13,
        peripherals.GPIO2,
        peripherals.GPIO14,
    );

    // Подсветка WS2812 (GPIO15) и кнопка BOOT (GPIO0) — как в оригинале.
    spawner.spawn(ws2812::led_task(peripherals.RMT, peripherals.GPIO15).unwrap());
    spawner.spawn(ui::button_task(peripherals.GPIO0).unwrap());

    spawner.spawn(clock::sntp_task(stack).unwrap());
    // SoftAP-эвакуация (issue #6, п.3): пароль — из build-time секрета
    // репозитория SOFTAP_PASS; без валидного секрета эвакуация выключена.
    let softap_creds: Option<WifiCredentials> =
        SOFTAP_PASS.and_then(|p| softap::ap_credentials(p));
    if softap_creds.is_none() {
        log::warn!("softap evacuation disabled: no valid SOFTAP_PASS build-time secret");
    }
    match controller.take() {
        Some(ctrl) => spawner.spawn(
            connection(
                ctrl,
                wifi_ssid.unwrap_or_else(heapless::String::new),
                station_creds,
                softap_creds,
                stack,
            )
            .unwrap(),
        ),
        None => log::warn!("wifi task skipped: no valid credentials (clock runs offline)"),
    };
    spawner.spawn(status_task(stack).unwrap());
    // DHCP-сервер SoftAP (порт :67): слушает только пока поднят AP — см.
    // dhcpd_task. Сокет smoltcp занимает слот только в активном режиме.
    spawner.spawn(dhcpd_task(stack).unwrap());
    spawner.spawn(server::server_task(stack).unwrap());
    logger::spawn_logger_task(&spawner, stack);
    spawner.spawn(updater::ota_worker_task(stack).unwrap());
    spawner.spawn(clock::clock_ticker_task().unwrap());
    spawner.spawn(app::app_task().unwrap());
    spawner.spawn(app::watchdog_monitor_task().unwrap());
    if PENDING_BOOT.load(Ordering::SeqCst) {
        spawner.spawn(pending_guard().unwrap());
    }

    // Подтверждение образа: сеть не является условием. Доказательство
    // работоспособности — живые циклы индикации (offline-сценарий: коллектор
    // недоступен, Wi-Fi настроен криво — часы всё равно должны жить).
    confirm_app_alive().await;
    if let Some(cfg) = stack.config_v4() {
        icmp::set_our_ip(cfg.address.address().octets());
        log::info!("got IP {}", cfg.address);
    }
    if PENDING_BOOT.load(Ordering::SeqCst) && !ROLLBACK_LATCHED.load(Ordering::SeqCst) {
        match updater::with_updater(|u| u.set_current_ota_state(OtaImageState::Valid)) {
            Ok(()) => {
                BOOT_CONFIRMED.store(true, Ordering::SeqCst);
                log::info!(
                    "OTA image confirmed VALID (display alive; wifi={})",
                    WIFI_READY.load(Ordering::SeqCst)
                );
            }
            Err(e) => log::warn!("confirm failed: {}", e),
        }
    }

    log::info!("system is running: /status /logs /ota?url=... /revert on port 80");
    loop {
        Timer::after_secs(3600).await;
    }
}

#[embassy_executor::task]
async fn wdt_task(mut rtc: Rtc<'static>) {
    loop {
        Timer::after_secs(5).await;
        rtc.rwdt.feed();
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, icmp::IcmpDevice<NetDevice>>) {
    runner.run().await
}

/// Раз в 30 с публикуем состояние сети: без IP виден предупреждающий hb,
/// с IP — полученный адрес. Логи идут в UART и, если коллектор настроен,
/// через logfwd на его адрес (issue #6: без адреса отправка выключена).
/// Заодно обновляем OUR_IP для ICMP-резолвера (ping отвечает только когда
/// адрес известен).
#[embassy_executor::task]
async fn status_task(stack: Stack<'static>) {
    loop {
        match stack.config_v4() {
            Some(cfg) => {
                icmp::set_our_ip(cfg.address.address().octets());
                // post/uart — телеметрия канала логов: если `post` не растёт,
                // а `hb` печатается, значит исполнитель жив, а сеть/коллектор нет.
                log::info!(
                    "hb: station up, ip={}, post={}ok/{}to, sending={}, uart_lost={}B",
                    cfg.address,
                    logger::post_ok(),
                    logger::post_timeouts(),
                    logger::sending(),
                    uarttap::DROPPED_BYTES.load(Ordering::Relaxed),
                );
            }
            None => log::warn!("hb: no IP yet (wifi station not connected?)"),
        }
        Timer::after_secs(30).await;
    }
}

#[embassy_executor::task]
async fn connection(
    mut controller: WifiController<'static>,
    want: heapless::String<32>,
    station_creds: Option<WifiCredentials>,
    softap_creds: Option<WifiCredentials>,
    stack: Stack<'static>,
) {
    loop {
        // Лестница ретраев (core::diag): 5 с → 15 с → 60 с → 300 с. Креды при
        // неудаче НЕ удаляем — AP могла лечь/перестроиться, восстановим.
        let pause = clock_core::diag::note_attempt();
        match controller.connect_async().await {
            Ok(info) => {
                clock_core::diag::note_connect_ok();
                log::info!(
                    "wifi connected: ch={} auth={:?} aid={} bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    info.channel,
                    info.authmode,
                    info.aid,
                    info.bssid[0],
                    info.bssid[1],
                    info.bssid[2],
                    info.bssid[3],
                    info.bssid[4],
                    info.bssid[5]
                );
                let d = controller.wait_for_disconnect_async().await;
                log::warn!("wifi disconnected: {:?}", d);
            }
            Err(e) => {
                clock_core::diag::note_connect_fail();
                let (attempts, fails, _, _, _) = clock_core::diag::snapshot();
                log::warn!(
                    "wifi connect failed: {:?} (attempt {}, fail streak {}, next pause {}s; creds kept)",
                    e,
                    attempts,
                    fails,
                    pause
                );
                let (seen_any, target_seen) =
                    log_wifi_scan_diag(&mut controller, &want, "after-failure").await;
                clock_core::diag::note_scan(seen_any, target_seen);
                // SoftAP-эвакуация (issue #6, п.3): серия неудач -> на время
                // поднимаем собственную точку, чтобы можно было зайти на часы
                // напрямую (192.168.139.100 -> 192.168.139.1).
                if softap_creds.is_some()
                    && clock_core::diag::ap_fallback_due()
                    && rescue_softap_ap(
                        &mut controller,
                        &stack,
                        softap_creds.as_ref(),
                        station_creds.as_ref(),
                    )
                    .await
                {
                    clock_core::diag::note_connect_ok();
                }
            }
        }
        Timer::after_secs(pause).await;
    }
}

/// Поднимает SoftAP поверх того же embassy-net стека (NetDevice переключает
/// трафик по `RADIO_MODE`), ждёт клиента (окно 30 мин), затем штатно гасит
/// AP и возвращает стек в STA/DHCP. Возвращает true, если эвакуация
/// отработала (клиент хотя бы раз подключался).
async fn rescue_softap_ap(
    controller: &mut WifiController<'static>,
    stack: &Stack<'static>,
    creds: Option<&WifiCredentials>,
    station_creds: Option<&WifiCredentials>,
) -> bool {
    let Some(creds) = creds else { return false };
    // Собираем AP-конфиг из уже валидированных кредов (валидация пароля —
    // та же WifiCredentials-проверка, что и у STA; channel 1 — фиксированный).
    let cfg = AccessPointConfig::default()
        .with_ssid(creds.ssid.as_str().try_into().unwrap())
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
            creds.password.as_str().try_into().unwrap(),
        ))
        .with_max_connections(1u16);
    let mut ap_ip = heapless::String::<15>::new();
    let mut client_ip = heapless::String::<15>::new();
    softap::ipv4_str(softap::AP_IP, &mut ap_ip);
    softap::ipv4_str(softap::AP_CLIENT_IP, &mut client_ip);
    log::warn!(
        "softap evacuation: AP '{}' up, client fixed {} -> AP {}; wait up to 1800s",
        // cfg.ssid приватный (BuilderLite в esp-radio) — логируем креды, из которых
        // cfg собран.
        creds.ssid.as_str(),
        client_ip,
        ap_ip,
    );
    RADIO_MODE.store(MODE_AP, Ordering::SeqCst);
    // set_config в esp-radio beta.1 принимает &Config (синхронный) — конфиг
    // держим локально и передаём заимствованным.
    let apcfg = WifiConfig::AccessPoint(cfg);
    if let Err(e) = controller.set_config(&apcfg) {
        log::error!("softap set_config failed: {:?} — back to station", e);
        RADIO_MODE.store(MODE_STA, Ordering::SeqCst);
        return false;
    }
    stack.set_config_v4(ConfigV4::Static(StaticConfigV4 {
        // embassy_net::Ipv4Address == smoltcp 0.13.1 == core::net::Ipv4Addr:
        // у него нет from_bytes ([u8;4] -> new по октетам).
        address: Ipv4Cidr::new(
            Ipv4Address::new(
                softap::AP_IP[0],
                softap::AP_IP[1],
                softap::AP_IP[2],
                softap::AP_IP[3],
            ),
            softap::AP_PREFIX,
        ),
        gateway: None,
        dns_servers: heapless::Vec::new(),
    }));
    icmp::set_our_ip(softap::AP_IP);
    netdev::AP_ACTIVE.store(1, Ordering::SeqCst);

    let mut served = false;
    // Окно эвакуации: ждём клиента (или уже пришедший disconnect) 30 мин.
    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(1800),
        controller.wait_for_access_point_connected_event_async(),
    )
    .await
    {
        Ok(Ok(esp_radio::wifi::ap::EventInfo::Connected(info))) => {
            served = true;
            log::info!(
                "softap client joined: mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} aid={} — serving via dhcpd",
                info.mac[0],
                info.mac[1],
                info.mac[2],
                info.mac[3],
                info.mac[4],
                info.mac[5],
                info.aid,
            );
            // Держим AP, пока клиент не уйдёт (или 30 мин истекли).
            let _ = embassy_time::with_timeout(
                embassy_time::Duration::from_secs(1800),
                controller.wait_for_access_point_connected_event_async(),
            )
            .await;
        }
        Ok(Ok(esp_radio::wifi::ap::EventInfo::Disconnected(info))) => {
            log::warn!("softap client left before we waited: {:?}", info.reason);
        }
        Ok(Err(e)) => {
            log::error!("softap event wait failed: {:?} — abort evacuation", e);
        }
        Err(_) => {
            log::warn!("softap evacuation window expired (1800s), no client");
        }
    }

    // Возврат в STA: режим -> радио -> конфиг стека. Порядок фиксированный
    // (сначала флаг, потом set_config — Runner не увидит AP-кадры в STA-фазе).
    netdev::AP_ACTIVE.store(0, Ordering::SeqCst);
    RADIO_MODE.store(MODE_STA, Ordering::SeqCst);
    match station_creds {
        Some(st) => {
            // Станционные креды (NVS > build-time) — те же, из которых поднимался
            // STA на старте; build-time константы SSID/PASSWORD могли быть
            // плейсхолдерами. Сигнатуры Ssid/Password — как в initial_config.
            let station = StationConfig::default()
                .with_ssid(st.ssid.as_str().try_into().unwrap())
                .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                    st.password.as_str().try_into().unwrap(),
                ));
            if let Err(e) = controller.set_config(&WifiConfig::Station(station)) {
                log::error!("station reconfig after softap failed: {:?}", e);
            }
        }
        None => {
            // Недостижимо: connection-таск спавнится только с валидными креды.
            log::warn!("no station creds: station config left as-is after softap");
        }
    }
    // DHCP-вариант ConfigV4 называется Dhcp(DhcpConfig) (embassy-net 0.9.1);
    // dhcpv4() — конструктор верхнего Config (см. embassy_net::new выше).
    stack.set_config_v4(ConfigV4::Dhcp(Default::default()));
    log::info!("softap evacuation done (served={}) — station mode restored", served);
    served
}

/// DHCP-сервер SoftAP (issue #6, п.3): DISCOVER->OFFER->REQUEST->ACK для
/// единственного клиента с фиксированным адресом 192.168.139.100 (кодек —
/// clock_core::dhcp, побайтово закреплен хост-тестами). Сокет :67 открывается
/// только в режиме AP (слот smoltcp в простое занимает один слот из 12);
/// ответы уходят unicast на yiaddr клиента (broadcast-флаг запроса
/// игнорируется — smoltcp не резолвит /24 broadcast через ARP; риск помечен
/// на стендовом тесте). Вне AP таск просто ждёт флага.
#[embassy_executor::task]
async fn dhcpd_task(stack: Stack<'static>) {
    // embassy-net 0.9.1: UdpSocket живёт в pub mod udp (на уровне crate нет
    // реэкспорта); PacketMetadata — мета-описатели датаграмм буферов сокета.
    use embassy_net::udp::{PacketMetadata, UdpSocket};
    use embassy_net::IpListenEndpoint;
    use clock_core::dhcp as d;

    let mut lease = d::Lease::idle();
    // Буферы сокета живут в таске: пул embassy-net одалживает их слоту сокета
    // на всё время жизни UdpSocket (DROP освобождает слот из StackResources<12>).
    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut sock_rx = [0u8; 600]; // DHCP-запросы клиента (RFC минималка 576 B)
    let mut sock_tx = [0u8; 300]; // OFFER/ACK ~272 B, NAK ~254 B
    loop {
        if !netdev::ap_is_active() {
            Timer::after_millis(500).await;
            continue;
        }
        let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut sock_rx, &mut tx_meta, &mut sock_tx);
        if let Err(e) = sock.bind(IpListenEndpoint { addr: None, port: 67 }) {
            log::warn!("dhcpd bind :67 failed: {:?} — retry in 5s", e);
            Timer::after_secs(5).await;
            continue;
        }
        log::info!("dhcpd serving :67 (fixed client -> 192.168.139.100)");
        let mut rx = [0u8; 576];
        while netdev::ap_is_active() {
            // recv_from -> Result<(n, UdpMetadata), RecvError>; мета (отправитель)
            // в ответах не используется: dst фиксирован — broadcast :68.
            let (n, _meta) = match embassy_time::with_timeout(
                embassy_time::Duration::from_secs(2),
                sock.recv_from(&mut rx),
            )
            .await
            {
                Ok(Ok(v)) => v,
                // Truncated: DHCPoption-шквал длиннее буфера — режем и игнорируем.
                Ok(Err(e)) => {
                    log::warn!("dhcpd recv error: {:?}", e);
                    continue;
                }
                Err(_) => continue,
            };
            let pkt = &rx[..n];
            let req = match d::parse(pkt) {
                Ok(r) => r,
                Err(why) => {
                    log::debug!("dhcpd: drop bad packet ({} B): {}", n, why);
                    continue;
                }
            };
            let now = embassy_time::Instant::now().as_secs();
            let action = lease.on(&req, now, d::LEASE_SECS);
            // const-generic argument: константа из модуля оборачивается в {} (E0747)
            let reply: Option<heapless::Vec<u8, { d::MAX_REPLY }>> = match action {
                d::LeaseAction::Offer => {
                    d::build_offer(&req, d::CLIENT_IP, d::AP_IP, d::LEASE_SECS).ok()
                }
                d::LeaseAction::Ack => d::build_ack(&req, d::CLIENT_IP, d::AP_IP, d::LEASE_SECS).ok(),
                d::LeaseAction::Nak => d::build_nak(&req, d::AP_IP).ok(),
                d::LeaseAction::Ignore => None,
            };
            let Some(reply) = reply else { continue };
            // Ответы — на subnet broadcast 192.168.139.255:68: smoltcp
            // (iface/interface/mod.rs:1050 lookup_hardware_addr) шлёт broadcast
            //_frames_ напрямую на ff:ff:.. через WiFi AP, без ARP — единственный
            // способ, который примет клиент на DISCOVER (у него ещё нет IP).
            // smoltcp 0.13.1 не имеет From<(Ipv4Address,u16)> для IpEndpoint —
            // есть From<core::net::SocketAddrV4>, по нему UdpMetadata ловится
            // blanket-impl'ом. Broadcast 192.168.139.255:68.
            let dst = core::net::SocketAddrV4::new(Ipv4Address::new(192, 168, 139, 255), 68);
            match sock.send_to(&reply, dst).await {
                Ok(()) => log::info!(
                    "dhcpd: {:?} -> 192.168.139.255:68 (mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
                    action,
                    req.chaddr[0],
                    req.chaddr[1],
                    req.chaddr[2],
                    req.chaddr[3],
                    req.chaddr[4],
                    req.chaddr[5],
                ),
                Err(e) => log::warn!("dhcpd send {:?} failed: {:?}", action, e),
            }
        }
        sock.close();
        lease = d::Lease::idle();
        log::info!("dhcpd closed (:67 released)");
    }
}

/// Диагностика Wi-Fi: скан эфира с каналом и RSSI каждой точки — **только при
/// неудачном подключении**. Показывает, видит ли радио нужный SSID и на каком
/// канале он вещает (главный кандидат при отказе ассоциации).
///
/// Сканирование сразу после успешной ассоциации намеренно **не** делаем:
/// esp-radio/esp-wifi в состоянии STA-associated отдаёт неполный результат
/// (на плате фиксировалось `1 AP(s)` без целевой сети) — строка ввела бы в
/// заблуждение. Канал/BSSID успешного коннекта даёт `connect_async` (log
/// `wifi connected: ch=... bssid=...`), скан при connected+без DHCP — удел
/// отдельной диагностики.
async fn log_wifi_scan_diag(
    controller: &mut WifiController<'static>,
    want: &heapless::String<32>,
    tag: &str,
) -> (bool, bool) {
    match controller.scan_async(&esp_radio::wifi::scan::ScanConfig::default()).await {
        Ok(aps) => {
            log::info!("wifi scan {tag}: {} AP(s)", aps.len());
            for ap in aps.iter().take(10) {
                log::info!(
                    "  ap '{}' ch={} rssi={}dBm auth={:?}",
                    ap.ssid.as_str(),
                    ap.channel,
                    ap.signal_strength,
                    ap.auth_method
                );
            }
            match aps.iter().find(|ap| ap.ssid.as_str() == want.as_str()) {
                Some(ap) => {
                    log::info!(
                        "  target '{}' visible: ch={} rssi={}dBm",
                        want,
                        ap.channel,
                        ap.signal_strength
                    );
                    (true, true)
                }
                None => {
                    log::warn!("  target '{}' NOT seen in scan", want);
                    (true, false)
                }
            }
        }
        Err(e) => {
            log::warn!("wifi scan {tag} failed: {e:?}");
            (false, false)
        }
    }
}

/// Диагностика STA для `/api/status` и WebUI: счётчик попыток, серия неудач,
/// текущая пауза лестницы и результат последнего after-failure скана.
/// Пишет `connection`-таск, читает HTTP — атомиксы, блокировки нет.
pub fn wifi_diagnostics() -> (u32, u32, u64, bool, bool) {
    clock_core::diag::snapshot()
}

/// Креды Wi-Fi: NVS > build-time (env). Отсутствие или невалидность кредов —
/// **штатная ситуация**, а не фатальная: возвращаем None, контроллер Wi-Fi не
/// поднимается, всё остальное (индикация, датчики, локальные логи) работает.
fn wifi_credentials() -> Option<clock_core::settings::WifiCredentials> {
    if let Some(c) = settings::load_wifi() {
        match clock_core::settings::WifiCredentials::new(c.ssid.as_str(), c.password.as_str()) {
            Ok(v) => {
                log::info!("wifi creds loaded from NVS (ssid={})", v.ssid);
                return Some(v);
            }
            Err(e) => log::warn!("saved wifi creds invalid: {e}"),
        }
    }
    match clock_core::settings::WifiCredentials::new(SSID, PASSWORD) {
        Ok(v) => {
            log::info!("no usable creds in NVS: using build-time SSID={}", v.ssid);
            Some(v)
        }
        Err(e) => {
            log::warn!("no wifi creds (NVS empty, build-time invalid: {e}) — running without wifi");
            None
        }
    }
}

/// Подтверждение работоспособности образа НЕ требует сети: достаточного
/// живого цикла индикации (см. `hangdetect::CYCLE`/`FRAME`).
fn display_cycles_alive() -> bool {
    hangdetect::CYCLE.load(Ordering::SeqCst) >= 2
        || hangdetect::FRAME.load(Ordering::SeqCst) > 200
}

async fn confirm_app_alive() {
    for _ in 0..24 {
        if display_cycles_alive() {
            return;
        }
        Timer::after_millis(500).await;
    }
    log::warn!("display produced no cycles within 12s — confirming without display evidence");
}

/// Если неподтверждённый образ не подтвердился за PENDING_TIMEOUT_SECS —
/// помечаем его Invalid и сбрасываем чип: загрузчик с auto-rollback откатится
/// на последний исправный слот.
#[embassy_executor::task]
async fn pending_guard() {
    Timer::after_secs(PENDING_TIMEOUT_SECS).await;
    if BOOT_CONFIRMED.load(Ordering::SeqCst) {
        return;
    }
    ROLLBACK_LATCHED.store(true, Ordering::SeqCst);
    log::error!(
        "image did not confirm in {}s: marking INVALID, resetting (bootloader rolls back)",
        PENDING_TIMEOUT_SECS
    );
    let _ = updater::with_updater(|u| u.set_current_ota_state(OtaImageState::Invalid));
    Timer::after_millis(1000).await;
    software_reset();
}
