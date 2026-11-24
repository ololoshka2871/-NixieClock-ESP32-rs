//! Конфигурация SoftAP (issue #6, п.3).
//!
//! Точка доступа поднимается в двух случаях:
//!   1. на старте, если нет валидных STA-кредов (пустой SSID после прошивки);
//!   2. как эвакуация, когда STA долго не может подключиться.
//!
//! В обоих случаях оператор заходит на страницу настроек и чинит/задаёт Wi-Fi.
//! Параметры зафиксированы владельцем:
//!   SSID   `Nixie-Clock-IN14`
//!   адрес  `192.168.139.1/24`
//!   клиент `192.168.139.100` (один-единственный, `max_client = 1`)
//!   пароль — из `SOFTAP_PASS` (CI-секрет либо `[env]` в config.toml).
//! Без валидного пароля AP не стартует: esp-radio запрещает пустой пароль.

// Реэкспорт: firmware импортирует креды как `clock_core::softap::WifiCredentials`
// (main.rs,SoftAP-эвакуация); сам тип живёт в settings.
pub use crate::settings::WifiCredentials;

/// SSID режимы точки доступа.
pub const AP_SSID: &str = "Nixie-Clock-IN14";
/// Адрес самой AP.
pub const AP_IP: [u8; 4] = [192, 168, 139, 1];
/// Маска AP-подсети (/24).
pub const AP_PREFIX: u8 = 24;
/// Единственный выдаваемый клиенту адрес.
pub const AP_CLIENT_IP: [u8; 4] = [192, 168, 139, 100];
/// Максимум одновременных клиентов AP.
pub const AP_MAX_CLIENT: u8 = 1;
/// Долгота выдаваемой аренды (секунды) — клиент один, аренды длинные.
pub const AP_LEASE_SECS: u32 = 3600;

/// Режим Wi-Fi на старте. Пустой SSID — не ошибка: поднимаем setup-AP,
/// чтобы оператор мог зайти на страницу и сохранить креды своей сети.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiMode {
    /// Есть валидные STA-креды (NVS или build-time): обычная станция.
    Station,
    /// STA-кредов нет, но есть креды AP: поднимаем SoftAP и ждём настройки.
    AccessPoint,
    /// Нет ни STA-, ни AP-кредов: радио не поднимается, часы работают offline.
    Offline,
}

/// Выбор стартового режима из двух фактов «креды есть». Чистая функция, чтобы
/// политику «пустой SSID -> SoftAP» можно было закрепить хост-тестом.
pub fn select_mode(has_station_creds: bool, has_ap_creds: bool) -> WifiMode {
    if has_station_creds {
        WifiMode::Station
    } else if has_ap_creds {
        WifiMode::AccessPoint
    } else {
        WifiMode::Offline
    }
}

/// IPv4 в текстовом виде (без allocation): `192.168.139.1`.
pub fn ipv4_str(octets: [u8; 4], buf: &mut heapless::String<15>) -> heapless::String<15> {
    use core::fmt::Write;
    buf.clear();
    let _ = write!(buf, "{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]);
    // Возвращаем копию, но НЕ опустошаем `buf`: вызывающий код использует
    // переданный буфер для логов (раньше здесь был mem::take => пустые адреса).
    buf.clone()
}

/// Креды AP из build-time значения: `None` если пароля нет или он не проходит
/// обычную валидацию крелов (тот же `WifiCredentials`, что и для STA).
/// `from_build` — признак «секрет репозитория получен», для диагностики.
pub fn ap_credentials(pass: &str) -> Option<WifiCredentials> {
    match WifiCredentials::new(AP_SSID, pass) {
        Ok(c) => Some(c),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ap_addressing_fixed() {
        assert_eq!(AP_SSID, "Nixie-Clock-IN14");
        assert_eq!(AP_IP, [192, 168, 139, 1]);
        assert_eq!(AP_PREFIX, 24);
        assert_eq!(AP_CLIENT_IP, [192, 168, 139, 100]);
        assert_eq!(AP_MAX_CLIENT, 1);
        // Клиент и AP в одной подсети /24.
        assert_eq!(AP_IP[..3], AP_CLIENT_IP[..3]);
    }

    #[test]
    fn empty_ssid_selects_softap() {
        // После прошивки без SSID плата должна быть доступна как AP.
        assert_eq!(select_mode(false, true), WifiMode::AccessPoint);
        // Сохранённые/build-time креды имеют приоритет над setup-AP.
        assert_eq!(select_mode(true, true), WifiMode::Station);
        assert_eq!(select_mode(true, false), WifiMode::Station);
        // Нет и того, ни другого — честно работаем без радио.
        assert_eq!(select_mode(false, false), WifiMode::Offline);
    }

    #[test]
    fn creds_validation_rejects_junk() {
        assert!(ap_credentials("normal-pass-123").is_some());
        assert!(ap_credentials("").is_none());
        assert!(ap_credentials("short").is_none()); // < PASS_MIN
        // Кавычки в пароле легальны (печатный ASCII; экранирует JSON-сервер),
        // а вот не-ASCII (например NUL) — нет.
        assert!(ap_credentials("has \"quote").is_some());
        assert!(ap_credentials("nul\0inside").is_none());
        assert!(ap_credentials(&"x".repeat(70)).is_none()); // > PASS_MAX
        let c = ap_credentials("secret-from-ci").unwrap();
        assert_eq!(c.ssid.as_str(), AP_SSID);
    }

    #[test]
    fn ipv4_render() {
        let mut buf = heapless::String::new();
        assert_eq!(ipv4_str(AP_IP, &mut buf).as_str(), "192.168.139.1");
        // Возвращаемое значение и переданный буфер должны совпадать:
        // именно buf уходит в log::warn!, а не результат вызова.
        assert_eq!(buf.as_str(), "192.168.139.1");
        assert_eq!(ipv4_str([10, 0, 0, 7], &mut buf).as_str(), "10.0.0.7");
        assert_eq!(buf.as_str(), "10.0.0.7");
    }
}
