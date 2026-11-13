//! Wi-Fi настройки в NVS-разделе (ESP-IDF-совместимом).
//! Namespace `settings`, ключи: `schema_ver` (u32), `ssid`, `pass` (u32 = строки).
//! Раздел ищется в таблице разделов; при первой прошивке/битой NVS — fallback.

use core::cell::RefCell;

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

/// Открыть NVS поверх общей flash: найти раздел nvs в таблице разделов,
/// открыть, выполнить замыкание, закрыть (flash при этом освобождается).
fn with_nvs<R>(f: impl FnOnce(&mut Nvs<SharedFlash>) -> R) -> Option<R> {
    let loc = crate::flashcell::with_flash(|flash| {
        let ptbuf = unsafe { crate::flashcell::ptbuf() };
        match read_partition_table(flash, ptbuf) {
            Ok(table) => table
                .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
                .map(|entry| entry.map(|e| (e.offset(), e.len()))),
            Err(e) => Err(e),
        }
    });
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
    with_nvs(|nvs| {
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
    .ok_or("nvs: раздел недоступен")?
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
    with_nvs(|nvs| {
        nvs.set(&NS, &K_NTP, value)
            .map_err(|_| "nvs: запись ntp_server")?;
        log::info!(
            "NVS: NTP-сервер {}",
            if value.is_empty() { "сброшен на пулы по умолчанию" } else { value }
        );
        Ok(())
    })
    .ok_or("nvs: раздел недоступен")?
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
