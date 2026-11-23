//! Конфигурация SoftAP-эвакуации (issue #6, п.3).
//!
//! Когда STA не подключается (AP легла/пароль не подошёл), плата поднимает
//! свою точку доступа, чтобы оператор мог зайти на страницу настроек и
//! починить Wi-Fi. Параметры зафиксированы владельцем:
//!   SSID   `Nixie-Clock-IN14`
//!   адрес  `192.168.139.1/24`
//!   клиент `192.168.139.100` (один-единственный, `max_client = 1`)
//!   пароль — из секрета репозитория `SOFTAP_PASS`; если секрета нет —
//!   SoftAP **не включается** (никаких плейсхолдеров: образ с плейсхолдером
//!   вместо пароля уже выводил плату из сети).

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

/// IPv4 в текстовом виде (без allocation): `192.168.139.1`.
pub fn ipv4_str(octets: [u8; 4], buf: &mut heapless::String<15>) -> heapless::String<15> {
    use core::fmt::Write;
    buf.clear();
    let _ = write!(buf, "{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]);
    core::mem::take(buf)
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
        assert_eq!(ipv4_str([10, 0, 0, 7], &mut buf).as_str(), "10.0.0.7");
    }
}
