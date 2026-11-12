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

#[cfg(test)]
mod tests {
    use super::*;

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
