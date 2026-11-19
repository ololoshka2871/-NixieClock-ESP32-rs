//! Статические ассеты страницы настроек.
//! Исходники — в `core/assets/` (index.html, style.css, app.js); build.rs
//! сжимает их gzip'ом, сюда попадают только gz-константы. Сервер отдаёт их
//! с `Content-Encoding: gzip`.

/// gzip-константы (магия 1f 8b) — сырые байты для отдачи в сокёт.
pub const PAGE_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));
pub const CSS_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/style.css.gz"));
pub const JS_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app.js.gz"));

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use std::io::Read;

    use super::{CSS_GZ, JS_GZ, PAGE_GZ};

    fn gunzip(gz: &[u8]) -> String {
        assert_eq!(&gz[..2], &[0x1f, 0x8b], "not a gzip stream");
        let mut d = flate2::read::GzDecoder::new(gz);
        let mut s = String::new();
        d.read_to_string(&mut s).expect("gunzip");
        s
    }

    #[test]
    fn assets_are_gzip() {
        for gz in [PAGE_GZ, CSS_GZ, JS_GZ] {
            assert_eq!(&gz[..2], &[0x1f, 0x8b]);
            assert!(!gz.is_empty());
        }
    }

    #[test]
    fn page_is_coherent_html() {
        let page = gunzip(PAGE_GZ);
        assert!(page.starts_with("<!DOCTYPE html>"));
        assert!(page.trim_end().ends_with("</html>"));
        for id in ["uptime", "fw", "ip", "ota_state", "ota_url", "ota_btn", "revert_btn", "logs", "wifi_cur", "wifi_src", "wifi_ssid_in", "wifi_pass_in", "wifi_btn", "wifi_msg",
            "digits", "lamp_sel", "nostrobe", "fade", "d_clock", "d_off", "d_cycle", "d_raw",
            "st_all", "st_bad", "st_bus", "st_out", "st_msg",
            "p_boot", "p_bo", "p_rst", "p_bod", "p_min", "p_win", "p_post", "p_disp"] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing #{id}");
        }
        // стили и скрипты — отдельными файлами, ничего инлайн
        assert!(page.contains("href=\"/style.css\""), "css link missing");
        assert!(page.contains("src=\"/app.js\""), "js link missing");
        assert!(!page.contains("<style"), "no inline styles");
        assert!(!page.contains("<script>"), "no inline scripts");
        assert!(!page.contains("http://localhost"), "no dev leftovers");
    }

    #[test]
    fn app_js_uses_our_endpoints() {
        let js = gunzip(JS_GZ);
        for ep in ["/api/status", "/ota?url=", "/revert", "/logs", "/api/wifi/connect", "/display/test?", "/display/on", "/display/off", "/display/fade?", "/status"] {
            assert!(js.contains(ep), "missing endpoint {ep}");
        }
        // относительные fetch -> страница работает с любого адреса платы
        assert!(js.contains("j(\"/api/status\")"));
        assert!(js.contains("fetch(\"/logs\""));
        assert!(!js.contains("http://localhost"));
    }

    #[test]
    fn css_has_theme_and_cards() {
        let css = gunzip(CSS_GZ);
        for marker in ["--bg", "--acc", ".card", "#ota_state.ok"] {
            assert!(css.contains(marker), "missing css marker {marker}");
        }
    }

    #[test]
    fn assets_fit_budget() {
        // флеш-бюджет: суммарный gz ассетов держим компактным.
        // Было 4096; страница диагностики (цифры/лампы/самопроверка/питание)
        // добавила ~2.5 KB gz — 8 KB всё ещё ничтожно на фоне 836 KB образа,
        // но предел нужен, чтобы ассеты не начали расти бесконтрольно.
        let total = PAGE_GZ.len() + CSS_GZ.len() + JS_GZ.len();
        assert!(total < 8192, "gz assets too large: {total}");
    }
}
