//! Модель Wi-Fi настроек: лимиты, валидация, разбор POST-тела SPA.
//! Чистая логика без железа и NVS (NVS-слой живёт в firmware); тестируется
//! на хосте (`cargo test -p clock-core`).

use alloc::format;
use alloc::string::String;
use heapless::String as HString;

/// Версия схемы настроек в NVS (ключ `schema_ver`). Поднимать при несовместимых
/// изменениях формата — firmware при несовпадении игнорирует старые ключи.
pub const SCHEMA_VER: u32 = 1;

pub const SSID_MAX: usize = 31; // лимит 802.11
pub const PASS_MIN: usize = 8; // WPA2-PSK
pub const PASS_MAX: usize = 63;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiCredentials {
    pub ssid: HString<32>,
    pub password: HString<64>,
}

impl WifiCredentials {
    /// Валидация по WPA2/802.11: печатный ASCII (0x20..=0x7e). SSID additionally
    /// без кавычек/обратной косой — он печатается в статусном JSON без экранирования.
    pub fn new(ssid: &str, password: &str) -> Result<Self, &'static str> {
        if ssid.is_empty() || ssid.len() > SSID_MAX {
            return Err("ssid: длина 1..31");
        }
        if ssid.bytes().any(|c| !(0x20..=0x7e).contains(&c) || c == b'"' || c == b'\\') {
            return Err("ssid: только печатный ASCII, без кавычек и слэшей");
        }
        if password.len() < PASS_MIN || password.len() > PASS_MAX {
            return Err("password: длина 8..63");
        }
        if password.bytes().any(|c| c < 0x20 || c > 0x7e) {
            return Err("password: только печатный ASCII");
        }
        let mut ssid_h: HString<32> = HString::new();
        ssid_h.push_str(ssid).map_err(|_| "ssid: слишком длинный")?;
        let mut pass_h: HString<64> = HString::new();
        pass_h.push_str(password).map_err(|_| "password: слишком длинный")?;
        Ok(Self {
            ssid: ssid_h,
            password: pass_h,
        })
    }
}

/// Извлечь строковое значение из плоского JSON {"key":"value",...};
/// поддержаны backslash-эскейпы (\" \\ и любые другие — как есть).
fn json_str_value(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let at = json.find(pat.as_str())?;
    let mut rest = &json[at + pat.len()..];
    rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let quoted = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = quoted.chars();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    Some(out)
}

/// Разобрать тело `{"ssid":"...","password":"..."}` (порядок ключей произвольный).
/// Не разбирает значения с невалидированным содержимым — дальше валидация.
pub fn parse_wifi_json(body: &str) -> Result<WifiCredentials, &'static str> {
    let ssid = json_str_value(body, "ssid").ok_or("нет поля ssid")?;
    let password = json_str_value(body, "password").ok_or("нет поля password")?;
    WifiCredentials::new(&ssid, &password)
}

/// NTP-сервер пользователя: имя хоста или IPv4. Пусто/отсутствие ключа —
/// встроенные пулы (pool.ntp.org, time.google.com).
pub const NTP_MAX: usize = 63;

pub fn validate_ntp(s: &str) -> Result<HString<64>, &'static str> {
    if s.is_empty() || s.len() > NTP_MAX {
        return Err("ntp: длина 1..63");
    }
    if s
        .bytes()
        .any(|c| !(c.is_ascii_alphanumeric() || c == b'.' || c == b'-'))
    {
        return Err("ntp: только латиница, цифры, точки и дефис");
    }
    if s.starts_with(['.', '-']) || s.ends_with(['.', '-']) {
        return Err("ntp: не начинается/кончается точкой или дефисом");
    }
    if s.contains("..") {
        return Err("ntp: пустая метка в имени");
    }
    let mut h: HString<64> = HString::new();
    h.push_str(s).map_err(|_| "ntp: слишком длинный")?;
    Ok(h)
}

/// Разобрать тело `{"server":"..."}` (пустая строка = сброс на пулы по умолчанию).
pub fn parse_ntp_json(body: &str) -> Result<String, &'static str> {
    json_str_value(body, "server").ok_or("нет поля server")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ntp_servers() {
        assert_eq!(validate_ntp("192.168.0.28").unwrap().as_str(), "192.168.0.28");
        assert_eq!(validate_ntp("pool.ntp.org").unwrap().as_str(), "pool.ntp.org");
        assert_eq!(validate_ntp("time.google.com").unwrap().as_str(), "time.google.com");
        assert_eq!(validate_ntp("NTP-1.lan").unwrap().as_str(), "NTP-1.lan");
        assert_eq!(parse_ntp_json(r#"{"server":"192.168.0.28"}"#).unwrap(), "192.168.0.28");
    }

    #[test]
    fn rejects_bad_ntp_servers() {
        assert!(validate_ntp("").is_err());
        assert!(validate_ntp(&"a".repeat(64)).is_err());
        assert!(validate_ntp(&("a".repeat(63) + ".ru")).is_err()); // 66 > 63
        assert!(validate_ntp("ntp server").is_err()); // пробел
        assert!(validate_ntp("http://time").is_err()); // схема/двоеточие/слэш
        assert!(validate_ntp(".ntp.org").is_err());
        assert!(validate_ntp("ntp..org").is_err());
        assert!(validate_ntp("время.рф").is_err()); // не ASCII
        assert!(parse_ntp_json(r#"{"ssid":"x"}"#).is_err());
    }

    #[test]
    fn valid_credentials_ok() {
        let c = WifiCredentials::new("Mokrot-5G-oblu4atel", "somepw12").unwrap();
        assert_eq!(c.ssid.as_str(), "Mokrot-5G-oblu4atel");
        assert_eq!(c.password.as_str(), "somepw12");
    }

    #[test]
    fn rejects_bad_lengths() {
        assert!(WifiCredentials::new("", "12345678").is_err());
        assert!(WifiCredentials::new(&"a".repeat(32), "12345678").is_err());
        assert!(WifiCredentials::new("Net", "1234567").is_err()); // 7
        assert!(WifiCredentials::new("Net", &"p".repeat(64)).is_err());
        assert!(WifiCredentials::new("Net", &"p".repeat(63)).is_ok());
    }

    #[test]
    fn rejects_bad_chars() {
        assert!(WifiCredentials::new("a\"b", "12345678").is_err());
        assert!(WifiCredentials::new("a\\b", "12345678").is_err());
        assert!(WifiCredentials::new("a\tb", "12345678").is_err());
        assert!(WifiCredentials::new("ВайФай", "12345678").is_err());
        assert!(WifiCredentials::new("Net", "pass\nword").is_err());
        assert!(WifiCredentials::new("Net", "p1\"q2\\s3!").is_ok()); // пароль с кавычкой разрешён
    }

    #[test]
    fn parses_spa_body() {
        let c = parse_wifi_json(r#"{"ssid":"Net","password":"12345678"}"#).unwrap();
        assert_eq!(c.ssid.as_str(), "Net");
        assert_eq!(c.password.as_str(), "12345678");
        // порядок и пробелы могут быть любые
        let c = parse_wifi_json(r#"{ "password" : "12345678", "ssid" : "Net" }"#).unwrap();
        assert_eq!(c.ssid.as_str(), "Net");
    }

    #[test]
    fn parses_escaped_password() {
        let c = parse_wifi_json(r#"{"ssid":"Net","password":"p1\"q2\\s3!!"}"#).unwrap();
        assert_eq!(c.password.as_str(), r#"p1"q2\s3!!"#);
    }

    #[test]
    fn rejects_missing_or_invalid_fields() {
        assert!(parse_wifi_json(r#"{"ssid":"Net"}"#).is_err());
        assert!(parse_wifi_json(r#"{"password":"12345678"}"#).is_err());
        assert!(parse_wifi_json(r#"{"ssid":"Net","password":"short"}"#).is_err());
        assert!(parse_wifi_json("").is_err());
    }
}

/// Достать целое число `{"tz_min": 180}` (знак разрешён, пробелы — да).
fn json_int_value(json: &str, key: &str) -> Option<i32> {
    let pat = format!("\"{key}\"");
    let at = json.find(pat.as_str())?;
    let rest = json[at + pat.len()..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(rest.len());
    rest[..end].parse::<i32>().ok()
}

/// Разобрать тело `{"tz_min":180}` (смещение от UTC в минутах).
/// Пустое тело/отсутствие ключа — ошибка: пояс задаётся явно.
pub fn parse_tz_json(body: &str) -> Result<i16, &'static str> {
    let minutes = json_int_value(body, "tz_min").ok_or("нет поля tz_min (целое, минуты)")?;
    crate::time::validate_tz_min(minutes)
}

#[cfg(test)]
mod tz_json_tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse_tz_json("{\"tz_min\":180}"), Ok(180));
        assert_eq!(parse_tz_json("{ \"tz_min\" : -300 }"), Ok(-300));
        assert_eq!(parse_tz_json("{\"tz_min\":330}"), Ok(330));
        assert_eq!(parse_tz_json("{\"tz_min\":0}"), Ok(0));
    }

    #[test]
    fn rejects_missing_and_out_of_range() {
        assert!(parse_tz_json("{}").is_err());
        assert!(parse_tz_json("{\"tz\":180}").is_err());
        assert!(parse_tz_json("{\"tz_min\":900}").is_err());
        assert!(parse_tz_json("{\"tz_min\":-800}").is_err());
    }
}
