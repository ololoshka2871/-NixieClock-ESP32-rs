//! M4: часы. DS1307 на I2C0 (SDA=GPIO21, SCL=GPIO22, 400 kHz, адрес 0x68 —
//! пины/адрес сняты с оригинального IDF-проекта) + SNTP-коррекция поверх.
//!
//! Владелец шины I2C — одна задача clock_task (единственный держатель
//! I2c-драйвера, разделения нет). Модель времени живёт в clock_core::time::Clock
//! под мьютексом: монотонные часы embassy-time дают ход, DS1307 — источник
//! при рестарте/holdover, SNTP/HTTP — коррекция.
//!
//! SNTP выбран вместо оригинального «время из HTTP-заголовка Virtuino», т.к.
//! NTP-сервер не требует учётки и даёт коррекцию без внешнего приложения;
//! ручная HTTP-установка (/api/time) оставлена как запасной путь без сети.

use core::sync::atomic::{AtomicBool, Ordering};

use alloc::string::String;
use clock_core::time::{self, TimeSource};
use embassy_net::dns::{DnsQueryType, DnsSocket};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::Async;
use esp_hal::i2c::master::I2c;

/// Адрес DS1307 на шине (DS1307_ADDR в оригинале).
pub const DS1307_ADDR: u8 = 0x68;
/// Период коррекции модели по чипу (SQW 1 Гц на GPIO23 в этой сборке не
/// используется — опрос шины; SQW можно будет подключить позже к RMT-тикам).
pub const RTC_POLL_SECS: u64 = 60;
/// Расхождение модели и чипа, при котором модель подтягивается к чипу.
pub const DRIFT_LIMIT_SECS: i64 = 2;
/// Период сетевой синхронизации (корректировка дрейфа DS1307).
pub const SNTP_PERIOD_SECS: u64 = 24 * 3600;
/// Пауза после ошибки синхронизации (нет сети / битый ответ).
pub const SNTP_RETRY_SECS: u64 = 60;
/// Таймаут ожидания ответа одного NTP-запроса.
pub const SNTP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

pub static CLOCK: Mutex<CriticalSectionRawMutex, time::Clock> =
    Mutex::new(time::Clock::new(time::BUILD_EPOCH, 0));

/// Запрошенная (пере)запись времени в DS1307: выставляется при SNTP/HTTP
/// синхронизации и снимается clock_task после записи в чип.
pub static RTC_SAVE_PENDING: AtomicBool = AtomicBool::new(false);

/// Монотонные секунды от старта: embassy-time-тик (esp-hal time-driver)
/// запускается вместе с приложением, Instant::now() == время с бутa.
fn mono_secs() -> u64 {
    Instant::now().as_secs()
}

/// Текущий epoch модели (секунды Unix, UTC).
pub async fn now_secs() -> u64 {
    CLOCK.lock().await.now_secs(mono_secs())
}

/// Текущий epoch + гражданское время + источник (для /api/status).
pub async fn snapshot() -> (u64, String, TimeSource, bool) {
    let c = CLOCK.lock().await;
    let epoch = c.now_secs(mono_secs());
    (
        epoch,
        alloc::format!("{}", time::epoch_to_civil(epoch)),
        c.source(),
        c.synced(),
    )
}

/// Применить сетевое/ручное время: переустановка модели + запрос записи в чип.
pub async fn sync(epoch: u64, source: TimeSource) {
    {
        let mut c = CLOCK.lock().await;
        c.set(epoch, mono_secs(), source);
    }
    RTC_SAVE_PENDING.store(true, Ordering::SeqCst);
    log::info!(
        "time set from {}: {}",
        source.as_str(),
        time::epoch_to_civil(epoch)
    );
}

// ---------------------------------------------------------------- DS1307 ----

async fn ds1307_read(i2c: &mut I2c<'static, Async>) -> Result<u64, time::TimeError> {
    let mut regs = [0u8; 7];
    i2c.write_read_async(DS1307_ADDR, &[0x00], &mut regs)
        .await
        .map_err(|_| time::TimeError::MalformedPacket)?;
    time::regs_to_epoch(&time::Ds1307Regs(regs))
}

async fn ds1307_write(i2c: &mut I2c<'static, Async>, epoch: u64) -> Result<(), time::TimeError> {
    let regs = time::epoch_to_regs(epoch)?;
    let mut payload = [0u8; 8];
    payload[0] = 0x00; // старт с регистра 0x00, чип сам инкрементирует указатель
    payload[1..].copy_from_slice(&regs.0);
    i2c.write_async(DS1307_ADDR, &payload)
        .await
        .map_err(|_| time::TimeError::MalformedPacket)
}

#[embassy_executor::task]
pub async fn clock_task(mut i2c: I2c<'static, Async>) {
    let build = time::BUILD_EPOCH;

    // Семантика оригинала (RTC.cpp): чип, показывающий время раньше времени
    // сборки прошивки, считается «мертвым» (севшая батарейка/новый чип) —
    // такое время не принимается, вместо него в чип пишется время сборки.
    match ds1307_read(&mut i2c).await {
        Ok(chip) => {
            if chip >= time::RTC_MIN_EPOCH && chip + 2 >= build {
                CLOCK.lock().await.set(chip, mono_secs(), TimeSource::Rtc);
                log::info!(
                    "DS1307 holds time: {} (source: rtc)",
                    time::epoch_to_civil(chip)
                );
            } else {
                log::warn!(
                    "DS1307 stale ({}): writing build-time {}",
                    time::epoch_to_civil(chip),
                    time::epoch_to_civil(build)
                );
                match ds1307_write(&mut i2c, build).await {
                    Ok(()) => CLOCK.lock().await.set(build, mono_secs(), TimeSource::Build),
                    Err(e) => log::error!("DS1307 write failed: {:?}", e),
                }
            }
        }
        Err(e) => {
            log::warn!("DS1307 unreadable at boot ({:?}): using build time", e);
            match ds1307_write(&mut i2c, build).await {
                Ok(()) => CLOCK.lock().await.set(build, mono_secs(), TimeSource::Build),
                Err(e2) => log::error!("DS1307 write failed: {:?}", e2),
            }
        }
    }

    loop {
        Timer::after_secs(RTC_POLL_SECS).await;

        if RTC_SAVE_PENDING.load(Ordering::SeqCst) {
            let epoch = CLOCK.lock().await.now_secs(mono_secs());
            match ds1307_write(&mut i2c, epoch).await {
                Ok(()) => {
                    RTC_SAVE_PENDING.store(false, Ordering::SeqCst);
                    log::info!("time written to DS1307: {}", time::epoch_to_civil(epoch));
                }
                Err(e) => log::warn!("DS1307 save failed ({:?}), retry next cycle", e),
            }
            continue;
        }

        match ds1307_read(&mut i2c).await {
            Ok(chip) => {
                let mut c = CLOCK.lock().await;
                let model = c.now_secs(mono_secs());
                let diff = chip as i64 - model as i64;
                if diff.abs() > DRIFT_LIMIT_SECS {
                    log::warn!(
                        "DS1307/model drift {}s: model <- chip ({})",
                        diff,
                        time::epoch_to_civil(chip)
                    );
                    c.set(chip, mono_secs(), TimeSource::Rtc);
                }
            }
            Err(e) => log::warn!("DS1307 poll failed: {:?}", e),
        }
    }
}

// ------------------------------------------------------------------ SNTP ----

async fn sntp_exchange(
    sock: &mut UdpSocket<'static>,
    read_buf: &mut [u8],
    dns: &DnsSocket<'_>,
    ref_epoch: u64,
    names: &[&str],
) -> Result<u64, &'static str> {
    // Имена из настроек (кастомный первым). IP-литерал используем сразу, без DNS;
    // IP-запаска time.google.com — только если кастомного сервера нет.
    const FALLBACK_IP: IpAddress = IpAddress::Ipv4(embassy_net::Ipv4Address::new(216, 239, 35, 8)); // time.google.com
    let mut server: Option<IpAddress> = None;
    for name in names {
        if let Ok(v4) = name.parse::<embassy_net::Ipv4Address>() {
            server = Some(IpAddress::Ipv4(v4));
            break;
        }
        match dns.query(name, DnsQueryType::A).await {
            Ok(addrs) => {
                // DnsQueryType::A возвращает только A-записи; embassy-net/ipv4
                // IpAddress в этой сборке имеет единственный вариант Ipv4.
                if let Some(ip) = addrs.first() {
                    server = Some(*ip);
                    break;
                }
            }
            Err(e) => log::debug!("dns {name}: {e:?}"),
        }
    }
    let server = server.unwrap_or(FALLBACK_IP);

    sock.send_to(&time::sntp_request(), (server, 123u16))
        .await
        .map_err(|_| "sntp send failed")?;
    let (len, _from) = with_timeout(SNTP_RESPONSE_TIMEOUT, sock.recv_from(read_buf))
        .await
        .map_err(|_| "sntp timeout")?
        .map_err(|_| "sntp recv failed")?;
    time::sntp_parse(&read_buf[..len], ref_epoch).map_err(|e| match e {
        time::TimeError::MalformedPacket => "malformed sntp response",
        time::TimeError::KissOfDeath => "sntp kiss-of-death",
        time::TimeError::TooFarOff => "sntp response too far from local clock",
        _ => "sntp parse error",
    })
}

#[embassy_executor::task]
pub async fn sntp_task(stack: Stack<'static>) {
    // Буферы/сокет статически аллоцируются РОВНО ОДИН раз в начале задачи
    // (задача не перезапускается): mkstatic! внутри цикла = паника
    // StaticCell "already full" (тот же класс ошибки, что упала v0.3.0).
    let rx_meta = mkstatic!([PacketMetadata; 4], [PacketMetadata::EMPTY; 4]);
    let rx_buf = mkstatic!([u8; 128], [0u8; 128]);
    let tx_meta = mkstatic!([PacketMetadata; 4], [PacketMetadata::EMPTY; 4]);
    let tx_buf = mkstatic!([u8; 48], [0u8; 48]);
    // read_buf отдельный: recv_from копирует пакет из внутреннего буфера
    // сокета в предоставленный срез — давать ему же собственный буфера нельзя.
    let read_buf = mkstatic!([u8; 48], [0u8; 48]);
    let sock = mkstatic!(
        UdpSocket<'static>,
        UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf)
    );
    // bind с портом 0: smoltcp выбирает динамический порт.
    if sock.bind(0).is_err() {
        log::error!("SNTP: no free UDP socket slot");
        return;
    }
    let dns = DnsSocket::new(stack);
    loop {
        stack.wait_config_up().await;
        let reference = now_secs().await;
        // Кастомный сервер из NVS (если сохранён) — первым, затем пулы по умолчанию.
        let custom = crate::settings::load_ntp();
        let mut names: heapless::Vec<&str, 3> = heapless::Vec::new();
        if let Some(c) = &custom {
            let _ = names.push(c.as_str());
        }
        let _ = names.push("pool.ntp.org");
        let _ = names.push("time.google.com");
        match sntp_exchange(sock, read_buf, &dns, reference, &names).await {
            Ok(epoch) => {
                let drift = epoch as i64 - reference as i64;
                sync(epoch, TimeSource::Ntp).await;
                log::info!("SNTP sync ok, drift {}s; next sync in 24 h", drift);
                Timer::after_secs(SNTP_PERIOD_SECS).await;
            }
            Err(e) => {
                log::warn!("SNTP sync failed: {e}; retry in {}s", SNTP_RETRY_SECS);
                Timer::after_secs(SNTP_RETRY_SECS).await;
            }
        }
    }
}
