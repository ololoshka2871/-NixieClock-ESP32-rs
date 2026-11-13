//! Приложение-шаблон для ESP32 (строится поверх esp32-wifi-bootloader на плате):
//! - WiFi (esp-radio + embassy-net + DHCP), логи POST'ом на LOG_URL (коллектор),
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
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    pubsub::{PubSubChannel, Publisher},
};
use embassy_time::Timer;
use esp_alloc as _;
use esp_backtrace as _;
use esp_bootloader_esp_idf::{
    ota::OtaImageState,
    partitions::{
        AppPartitionSubType, PartitionType, read_partition_table,
    },
};
use esp_hal::{
    clock::CpuClock,
    ram,
    rng::Rng,
    rtc_cntl::{Rtc, RwdtStage},
    system::software_reset,
    time::Duration as HalDuration,
    timer::timg::TimerGroup,
};
use esp_radio::wifi::{
    sta::StationConfig, AuthenticationMethodConfig, Config as WifiConfig, ControllerConfig,
    Interface, PowerSaveMode, WifiController,
};
use esp_storage::FlashStorage;

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
mod flashcell;
mod logger;
mod server;
mod settings;
mod updater;

esp_bootloader_esp_idf::esp_app_desc!();

pub const SSID: &str = env!("SSID");
pub const PASSWORD: &str = env!("PASSWORD");
pub const LOG_URL: &str = match option_env!("LOG_URL") {
    Some(v) => v,
    None => "http://192.168.1.205:8000/logs",
};

/// Сколько секунд даём новому (неподтверждённому) образу подняться.
pub const PENDING_TIMEOUT_SECS: u64 = 60;
/// Таймаут reset-watchdog.
pub const WDT_TIMEOUT_SECS: u64 = 30;

pub static PENDING_BOOT: AtomicBool = AtomicBool::new(false);
pub static BOOT_CONFIRMED: AtomicBool = AtomicBool::new(false);
pub static ROLLBACK_LATCHED: AtomicBool = AtomicBool::new(false);
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

    logger::init();
    log::info!(
        "=== {} v{} starting (template app over esp32-wifi-bootloader) ===",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );

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
    let fell_back = match (booted_app, selected) {
        (Some(bapp), Ok(sapp)) if sapp != AppPartitionSubType::Factory && sapp != bapp => {
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
                        "image is not confirmed yet: {}s to bring WiFi up, otherwise rollback",
                        PENDING_TIMEOUT_SECS
                    );
                }
            }
            Err(e) => log::warn!("cannot read OTA state: {}", e),
        }
    }

    // WiFi: сохранённые в NVS креды; если их нет — креды из времени сборки (env).
    let saved = settings::load_wifi();
    let creds = match &saved {
        Some(c) => {
            log::info!("wifi creds loaded from NVS (ssid={})", c.ssid);
            c.clone()
        }
        None => {
            log::info!("no creds in NVS: using build-time SSID={}", SSID);
            clock_core::settings::WifiCredentials::new(SSID, PASSWORD)
                .expect("build-time SSID/PASSWORD must be valid")
        }
    };
    settings::set_active(&creds, saved.is_some());
    let station_config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(creds.ssid.as_str().try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                creds.password.as_str().try_into().unwrap(),
            )),
    );
    let wifi_interface = Interface::station();
    let mut controller = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default().with_initial_config(station_config),
    )
    .expect("wifi init failed");
    // Без light sleep: логи должны уходить без задержек.
    let _ = controller.set_power_saving(PowerSaveMode::None);

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, runner) = embassy_net::new(
        wifi_interface,
        embassy_net::Config::dhcpv4(Default::default()),
        // smoltcp SocketSet фиксированный: DHCP занимает 1 слот, каждый
        // DnsSocket (logger + updater) и каждый TcpSocket сервера — ещё слоты,
        // каждое connect() на LOG_URL/OTA — ещё. StackResources<3> давал panic
        // "adding a socket to a full SocketSet" на первом же POST логов.
        mkstatic!(StackResources<8>, StackResources::<8>::new()),
        seed,
    );

    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(status_task(stack).unwrap());
    spawner.spawn(server::server_task(stack).unwrap());
    spawner.spawn(logger::logfwd_task(stack).unwrap());
    spawner.spawn(updater::ota_worker_task(stack).unwrap());
    spawner.spawn(app::app_task().unwrap());
    spawner.spawn(app::watchdog_monitor_task().unwrap());
    if PENDING_BOOT.load(Ordering::SeqCst) {
        spawner.spawn(pending_guard().unwrap());
    }

    // Подтверждение образа: только когда реально получили IP по DHCP.
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        log::info!("got IP {}", cfg.address);
    }
    if PENDING_BOOT.load(Ordering::SeqCst) && !ROLLBACK_LATCHED.load(Ordering::SeqCst) {
        match updater::with_updater(|u| u.set_current_ota_state(OtaImageState::Valid)) {
            Ok(()) => {
                BOOT_CONFIRMED.store(true, Ordering::SeqCst);
                log::info!("OTA image confirmed VALID");
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
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

/// Раз в 30 с публикуем состояние сети: без IP виден предупреждающий hb,
/// с IP — полученный адрес. Логи идут в UART и через logfwd на LOG_URL.
#[embassy_executor::task]
async fn status_task(stack: Stack<'static>) {
    loop {
        match stack.config_v4() {
            Some(cfg) => log::info!("hb: station up, ip={}", cfg.address),
            None => log::warn!("hb: no IP yet (wifi station not connected?)"),
        }
        Timer::after_secs(30).await;
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(info) => {
                log::info!("wifi connected: {:?}", info);
                let d = controller.wait_for_disconnect_async().await;
                log::warn!("wifi disconnected: {:?}", d);
            }
            Err(e) => log::warn!("wifi connect failed: {:?}", e),
        }
        Timer::after_secs(5).await;
    }
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
