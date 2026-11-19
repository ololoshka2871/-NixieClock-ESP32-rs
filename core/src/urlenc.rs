//! Percent-decoding query-значений (RFC 3986).
//!
//! Зачем: страница отправляет OTA-URL через `encodeURIComponent`, то есть
//! `http://host:8000/app_ota.bin` приходит как
//! `http%3A%2F%2Fhost%3A8000%2Fapp_ota.bin`. Без декодирования сервер видел
//! строку, не начинающуюся с `http://`, и отвечал `usage: GET /ota?url=...`.
//!
//! `+` намеренно НЕ превращается в пробел: в URL плюс — значимый символ, а
//! `encodeURIComponent` кодирует его как `%2B`, поэтому литеральный `+`
//! пришёл из адреса и трогать его нельзя.

/// Декодирует `%XX` в `dst`. Возвращает длину результата.
/// `None` — не влезло в `dst` или битая escape-последовательность.
pub fn percent_decode_into(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut n = 0usize;
    let mut i = 0usize;
    while i < src.len() {
        let b = if src[i] == b'%' {
            let hi = hex(*src.get(i + 1)?)?;
            let lo = hex(*src.get(i + 2)?)?;
            i += 2;
            (hi << 4) | lo
        } else {
            src[i]
        };
        if n >= dst.len() {
            return None;
        }
        dst[n] = b;
        n += 1;
        i += 1;
    }
    Some(n)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> String {
        let mut buf = [0u8; 256];
        let n = percent_decode_into(s.as_bytes(), &mut buf).expect("decode");
        String::from_utf8(buf[..n].to_vec()).unwrap()
    }

    #[test]
    fn decoded_ota_url_is_accepted_by_server_check() {
        // Ровно то, что присылает браузер после encodeURIComponent.
        let raw = "http%3A%2F%2F192.168.0.168%3A8001%2Fapp_ota.bin";
        let url = dec(raw);
        assert_eq!(url, "http://192.168.0.168:8001/app_ota.bin");
        assert!(url.starts_with("http://"), "серверная проверка схемы");
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(dec("http://host/app.bin"), "http://host/app.bin");
    }

    #[test]
    fn plus_is_kept_and_utf8_survives() {
        assert_eq!(dec("a+b"), "a+b");
        // %D0%AF = «Я»
        assert_eq!(dec("%D0%AF"), "Я");
    }

    #[test]
    fn broken_escape_is_rejected() {
        let mut buf = [0u8; 16];
        assert_eq!(percent_decode_into(b"abc%", &mut buf), None);
        assert_eq!(percent_decode_into(b"abc%2", &mut buf), None);
        assert_eq!(percent_decode_into(b"abc%ZZ", &mut buf), None);
    }

    #[test]
    fn overflow_is_reported() {
        let mut small = [0u8; 4];
        assert_eq!(percent_decode_into(b"http%3A", &mut small), None);
    }
}
