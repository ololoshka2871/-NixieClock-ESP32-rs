//! Wi-Fi настройки в NVS-разделе (ESP-IDF-совместимом).
//! Namespace `settings`, ключи: `schema_ver` (u32), `ssid`, `pass` (u32 = строки),
//! `ntp_server`, `tz_min`, `clock_color`, `led_effect`, `led_freq_pct`,
//! `led_amp_pct`, `log_url` (адрес лог-коллектора; пусто = коллектора нет).
//! Раздел ищется в таблице разделов; при первой прошивке/битой NVS — fallback.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use clock_core::settings::{SCHEMA_VER, WifiCredentials};
use embassy_sync::blocking_mutex::{
    raw::CriticalSectionRawMutex,
    Mutex,
};
use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PartitionType, read_partition_table,
};
use esp_nvs::Key;
use esp_nvs::Nvs;
use heapless::String;

use crate::flashcell::SharedFlash;

const NS: Key = const { Key::from_str("settings") };
const K_SCHEMA: Key = const { Key::from_str("schema_ver") };
const K_SSID: Key = const { Key::from_str("ssid") };
const K_PASS: Key = const { Key::from_str("pass") };
const K_NTP: Key = const { Key::from_str("ntp_server") };
const K_TZ: Key = const { Key::from_str("tz_min") };
const K_COLOR: Key = const { Key::from_str("clock_color") };
const K_EFFECT: Key = const { Key::from_str("led_effect") };
const K_EFFECT_FREQ: Key = const { Key::from_str("led_freq_pct") };
const K_EFFECT_AMP: Key = const { Key::from_str("led_amp_pct") };
const K_LOG_URL: Key = const { Key::from_str("log_url") };

/// Кэш подсветки: flash читать нельзя в горячем пути индикации.
/// `u32::MAX` = пользовательский цвет не сохранён -> действует цвет месяца.
static CLOCK_COLOR: AtomicU32 = AtomicU32::new(u32::MAX);
static LED_EFFECT: AtomicU8 = AtomicU8::new(0);
static LED_EFFECT_FREQ: AtomicU8 = AtomicU8::new(clock_core::led::FREQ_PCT_DEFAULT);
static LED_EFFECT_AMP: AtomicU8 = AtomicU8::new(clock_core::led::AMP_PCT_DEFAULT);

/// Открыть NVS поверх общей flash: найти раздел nvs в таблице разделов,
/// открыть, выполнить замыкание, закрыть (flash при этом освобождается).
/// pub(crate) + try_with_flash: вызывается и из panic-хэндлера (crashlog),
/// где flash может быть ещё не инициализирована — без expect/panic.
pub(crate) fn with_nvs<R>(f: impl FnOnce(&mut Nvs<SharedFlash>) -> R) -> Option<R> {
    let loc = match crate::flashcell::try_with_flash(|flash| {
        let ptbuf = unsafe { crate::flashcell::ptbuf() };
        match read_partition_table(flash, ptbuf) {
            Ok(table) => table
                .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
                .map(|entry| entry.map(|e| (e.offset(), e.len()))),
            Err(e) => Err(e),
        }
    }) {
        Some(r) => r,
        None => return None, // flash ещё не инициализирована
    };
    let (offset, size) = match loc {
        Ok(Some((o, l))) => (o as usize, l as usize),
        Ok(None) => {
            log::warn!("NVS: раздел nvs не найден в таблице разделов");
            return None;
        }
        Err(e) => {
            log::warn!("NVS: таблица разделов повреждена: {:?}", e);
            return None;
        }
    };
    match Nvs::new(offset, size, SharedFlash) {
        Ok(mut nvs) => {
            let r = f(&mut nvs);
            drop(nvs.into_inner()); // SharedFlash — zero-sized, flash остаётся общей
            Some(r)
        }
        Err(e) => {
            log::warn!("NVS: не удалось открыть раздел: {:?}", e);
            None
        }
    }
}

/// Прочитать сохранённые креды; None — если NVS нет/битая/первая прошивка.
/// Прочитанное повторно валидируется (битые ключи не должны уронить boot).
pub fn load_wifi() -> Option<WifiCredentials> {
    with_nvs(|nvs| {
        match nvs.get::<u32>(&NS, &K_SCHEMA) {
            Ok(SCHEMA_VER) => {}
            Ok(other) => {
                log::warn!(
                    "NVS: схема настроек {} не совпадает с приложением {} — игнорируем",
                    other,
                    SCHEMA_VER
                );
                return None;
            }
            Err(_) => {
                log::info!("NVS: ключей настроек ещё нет (первая прошивка?)");
                return None;
            }
        }
        let ssid = nvs
            .get::<alloc::string::String>(&NS, &K_SSID)
            .ok()?;
        let pass = nvs
            .get::<alloc::string::String>(&NS, &K_PASS)
            .ok()?;
        let creds = WifiCredentials::new(ssid.as_str(), pass.as_str()).ok();
        if creds.is_none() {
            log::warn!("NVS: сохранённые креды не прошли валидацию — игнорируем");
        }
        creds
    })
    .flatten()
}

/// Сохранить креды в NVS (после валидации spa-моделью).
pub fn save_wifi(creds: &WifiCredentials) -> Result<(), &'static str> {
    // let r + r? (образец save_color): без этого ошибка записи из замыкания
    // выбрасывалась и страница считала креды сохранёнными.
    let r = with_nvs(|nvs| {
        nvs.set(&NS, &K_SSID, creds.ssid.as_str())
            .map_err(|_| "nvs: запись ssid")?;
        nvs.set(&NS, &K_PASS, creds.password.as_str())
            .map_err(|_| "nvs: запись pass")?;
        nvs.set(&NS, &K_SCHEMA, SCHEMA_VER)
            .map_err(|_| "nvs: запись schema_ver")?;
        log::info!(
            "NVS: сохранены Wi-Fi креды (ssid={}, schema={})",
            creds.ssid,
            SCHEMA_VER
        );
        Ok(())
    })
    .ok_or("nvs: раздел недоступен")?;
    r?;
    Ok(())
}

/// Прочитать NTP-сервер; None — не сохранён/пустой (значит встроенные пулы).
pub fn load_ntp() -> Option<String<64>> {
    with_nvs(|nvs| {
        let raw = nvs.get::<alloc::string::String>(&NS, &K_NTP).ok()?;
        if raw.is_empty() {
            return None;
        }
        match clock_core::settings::validate_ntp(raw.as_str()) {
            Ok(v) => Some(v),
            Err(e) => {
                log::warn!("NVS: ключ ntp_server невалиден ({}) — игнорируем", e);
                None
            }
        }
    })
    .flatten()
}

/// Сохранить/сбросить NTP-сервер (Some("") или None = сброс на пулы по умолчанию).
pub fn save_ntp(server: Option<&str>) -> Result<(), &'static str> {
    let value = server.unwrap_or("");
    let r = with_nvs(|nvs| {
        nvs.set(&NS, &K_NTP, value)
            .map_err(|_| "nvs: запись ntp_server")?;
        log::info!(
            "NVS: NTP-сервер {}",
            if value.is_empty() { "сброшен на пулы по умолчанию" } else { value }
        );
        Ok(())
    })
    .ok_or("nvs: раздел недоступен")?;
    r?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Лог-коллектор (issue #6): адрес опциональный и настраивается из WebGUI.
// ---------------------------------------------------------------------------

pub use clock_core::settings::LOG_URL_MAX;

/// Копия адреса коллектора в RAM: её читает logfwd-задача и статусная
/// страница, её меняет /api/logs/url — без чтения flash в рантайме.
static LOG_URL_SHARED: Mutex<CriticalSectionRawMutex, RefCell<String<128>>> =
    Mutex::new(RefCell::new(String::new()));

/// Прочитать адрес коллектора из NVS. None — ключа нет или он пустой, т.е.
/// коллектор не настроен (это нормальное состояние, а не ошибка).
pub fn load_log_url() -> Option<String<128>> {
    with_nvs(|nvs| {
        let raw = nvs.get::<alloc::string::String>(&NS, &K_LOG_URL).ok()?;
        if raw.is_empty() {
            return None;
        }
        match clock_core::settings::validate_log_url(raw.as_str()) {
            Ok(v) => Some(v),
            Err(e) => {
                log::warn!("NVS: ключ log_url невалиден ({}) — игнорируем", e);
                None
            }
        }
    })
    .flatten()
}

/// Применить адрес к рантайму (boot / после записи в NVS). Пустая строка
/// выключает отправку: logfwd-задача гасит TCP/DNS-клиент и ждёт.
pub fn apply_log_url_runtime(value: &str) {
    LOG_URL_SHARED.lock(|cell| {
        let mut s = cell.borrow_mut();
        s.clear();
        let _ = s.push_str(value);
    });
    log::info!(
        "log collector: {}",
        if value.is_empty() { "отключён (логи только в UART и /logs)" } else { value }
    );
}

/// Текущий адрес коллектора для отправки; "" = коллектора нет.
/// Возвращаем копию (128 Б): ссылку из-под мьютекса отдавать нельзя — её мог
/// бы инвалидировать другой исполнитель (logfwd и HTTP-сервер работают параллельно).
pub fn log_url_runtime() -> String<128> {
    LOG_URL_SHARED.lock(|cell| cell.borrow().clone())
}

/// Сохранить/сбросить адрес коллектора и тут же применить его к рантайму.
/// Some("") / None = отключить отправку логов.
pub fn save_log_url(url: Option<&str>) -> Result<(), &'static str> {
    let value = url.unwrap_or("").trim();
    if value.len() > LOG_URL_MAX {
        return Err("nvs: лог-URL длиннее 128 символов");
    }
    let stored: String<128> = match clock_core::settings::validate_log_url(value) {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    let r = with_nvs(|nvs| {
        nvs.set(&NS, &K_LOG_URL, stored.as_str())
            .map_err(|_| "nvs: запись log_url")?;
        log::info!(
            "NVS: лог-коллектор {}",
            if stored.is_empty() { "сброшен (логи только в UART и /logs)" } else { stored.as_str() }
        );
        Ok::<(), &'static str>(())
    })
    .ok_or("nvs: раздел недоступен")?;
    r?;
    apply_log_url_runtime(stored.as_str());
    Ok(())
}

/// Активный Wi-Fi после boot (для статусной страницы; пароль не хранится).
#[derive(Clone, Debug)]
pub struct ActiveWifi {
    pub ssid: String<32>,
    pub from_nvs: bool,
}

static WIFI_ACTIVE: Mutex<CriticalSectionRawMutex, RefCell<ActiveWifi>> = Mutex::new(
    RefCell::new(ActiveWifi {
        ssid: String::new(),
        from_nvs: false,
    }),
);

pub fn set_active(creds: &WifiCredentials, from_nvs: bool) {
    let mut ssid = String::new();
    let _ = ssid.push_str(creds.ssid.as_str());
    WIFI_ACTIVE.lock(|cell| *cell.borrow_mut() = ActiveWifi { ssid, from_nvs });
}

pub fn active_wifi() -> ActiveWifi {
    WIFI_ACTIVE.lock(|cell| cell.borrow().clone())
}

/// Прочитать смещение пояса (минуты от UTC); None — не сохранено или битое.
pub fn load_tz() -> Option<i16> {
    with_nvs(|nvs| nvs.get::<i32>(&NS, &K_TZ).ok())
        .flatten()
        .and_then(|v| clock_core::time::validate_tz_min(v).ok())
}

/// Сохранить смещение пояса. Часы остаются в UTC — это только отображение.
pub fn save_tz(minutes: i16) -> Result<(), &'static str> {
    let r = with_nvs(|nvs| {
        nvs.set(&NS, &K_TZ, minutes as i32)
            .map_err(|_| "nvs: запись tz_min")?;
        log::info!("NVS: часовой пояс {:+.2} ч от UTC", minutes as f32 / 60.0);
        Ok(())
    })
    .ok_or("nvs: раздел недоступен")?;
    r?;
    Ok(())
}

/// Прочитать сохранённый цвет подсветки часов; None — ключа нет (цвет месяца).
pub fn clock_color() -> Option<u32> {
    let v = CLOCK_COLOR.load(Ordering::Relaxed);
    if v == u32::MAX {
        None
    } else {
        Some(v)
    }
}

pub fn led_effect() -> u8 {
    LED_EFFECT.load(Ordering::Relaxed)
}

pub fn led_freq_pct() -> u8 {
    LED_EFFECT_FREQ.load(Ordering::Relaxed)
}

pub fn led_amp_pct() -> u8 {
    LED_EFFECT_AMP.load(Ordering::Relaxed)
}

/// Загрузить подсветку из NVS в атомики (вызывается один раз на бутe).
pub fn load_led() {
    let color = with_nvs(|nvs| nvs.get::<u32>(&NS, &K_COLOR).ok()).flatten();
    let eff = with_nvs(|nvs| nvs.get::<u8>(&NS, &K_EFFECT).ok()).flatten();
    let freq = with_nvs(|nvs| nvs.get::<u8>(&NS, &K_EFFECT_FREQ).ok()).flatten();
    let amp = with_nvs(|nvs| nvs.get::<u8>(&NS, &K_EFFECT_AMP).ok()).flatten();
    CLOCK_COLOR.store(color.unwrap_or(u32::MAX), Ordering::Relaxed);
    LED_EFFECT.store(eff.filter(|v| *v <= 3).unwrap_or(0), Ordering::Relaxed);
    LED_EFFECT_FREQ.store(
        freq.filter(|v| *v >= clock_core::led::FREQ_PCT_MIN && *v <= clock_core::led::FREQ_PCT_MAX)
            .unwrap_or(clock_core::led::FREQ_PCT_DEFAULT),
        Ordering::Relaxed,
    );
    LED_EFFECT_AMP.store(
        amp.filter(|v| *v >= clock_core::led::AMP_PCT_MIN && *v <= clock_core::led::AMP_PCT_MAX)
            .unwrap_or(clock_core::led::AMP_PCT_DEFAULT),
        Ordering::Relaxed,
    );
    match color {
        Some(v) => log::info!(
            "подсветка: цвет #{:06x}, эффект {}",
            v,
            clock_core::led::effect_name(led_effect())
        ),
        None => log::info!(
            "подсветка: цвет месяца, эффект {}",
            clock_core::led::effect_name(led_effect())
        ),
    }
}

/// Сохранить цвет (0xRRGGBB) или сбросить ключ (None = цвет месяца).
pub fn save_color(value: Option<u32>) -> Result<(), &'static str> {
    let r = with_nvs(|nvs| {
        match value {
            Some(v) => nvs.set(&NS, &K_COLOR, v).map_err(|_| "nvs: запись clock_color")?,
            None => nvs.delete(&NS, &K_COLOR).map_err(|_| "nvs: удаление clock_color")?,
        }
        Ok::<(), &'static str>(())
    })
    .ok_or("nvs: раздел недоступен")?;
    r?;
    CLOCK_COLOR.store(value.unwrap_or(u32::MAX), Ordering::Relaxed);
    log::info!("NVS: подсветка часов -> {:?}", value);
    Ok(())
}

/// Сохранить эффект (0..3) и, если переданы, частоту/амплитуду эффекта.
pub fn save_effect(update: clock_core::settings::EffectUpdate) -> Result<(), &'static str> {
    if update.effect > 3 {
        return Err("effect: 0..3");
    }
    let r = with_nvs(|nvs| {
        nvs.set(&NS, &K_EFFECT, update.effect)
            .map_err(|_| "nvs: запись led_effect")?;
        if let Some(v) = update.freq_pct {
            if v < clock_core::led::FREQ_PCT_MIN || v > clock_core::led::FREQ_PCT_MAX {
                return Err("freq_pct: 10..200");
            }
            nvs.set(&NS, &K_EFFECT_FREQ, v)
                .map_err(|_| "nvs: запись led_freq_pct")?;
        }
        if let Some(v) = update.amp_pct {
            if v > clock_core::led::AMP_PCT_MAX {
                return Err("amp_pct: 0..100");
            }
            nvs.set(&NS, &K_EFFECT_AMP, v)
                .map_err(|_| "nvs: запись led_amp_pct")?;
        }
        Ok::<(), &'static str>(())
    })
    .ok_or("nvs: раздел недоступен")?;
    r?;
    LED_EFFECT.store(update.effect, Ordering::Relaxed);
    if let Some(v) = update.freq_pct {
        LED_EFFECT_FREQ.store(v, Ordering::Relaxed);
    }
    if let Some(v) = update.amp_pct {
        LED_EFFECT_AMP.store(v, Ordering::Relaxed);
    }
    log::info!(
        "NVS: эффект подсветки -> {}, частота {}%, амплитуда {}%",
        clock_core::led::effect_name(update.effect),
        led_freq_pct(),
        led_amp_pct()
    );
    Ok(())
}
