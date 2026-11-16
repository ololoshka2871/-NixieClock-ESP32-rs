//! Мини-HTTP-сервер на устройстве (порт 80):
//!   GET /              — статус (OTA-состояние, флаги, uptime)
//!   GET /logs          — дамп ring-буфера логов
//!   GET /ota?url=http://host/app.bin — запрос OTA (обрабатывает фоновая ota-задача)
//!   GET /revert        — пометить текущий образ Invalid и сброситься (откат)

use core::fmt::Write as _;
use core::sync::atomic::Ordering;

use embassy_net::{Stack, tcp::TcpSocket};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use heapless::String;

use crate::{
    BOOT_CONFIRMED, LOG_URL, OTA_BUSY, OTA_REQ, OtaPublisher, OtaRequest,
    PENDING_BOOT, ROLLBACK_LATCHED, logger,
};
use esp_hal::system::software_reset;


#[embassy_executor::task]
pub async fn server_task(stack: Stack<'static>) {
    let publisher = OTA_REQ.publisher().ok();

    // Все буферы аллоцируются ровно один раз при старте задачи: StaticCell
    // нельзя записать повторно, а mkstatic! внутри loop/функции вызывался бы
    // на каждое соединение -> panic "`StaticCell` is already full" на 2-м
    // визите страницы. Каждому сокету — временный split_at_mut заём, handle
    // живёт ровно время соединения (lifetime-generic, без 'static).
    let rx = mkstatic!([u8; 2048], [0u8; 2048]); // 1024: rx сокета | 1024: запрос
    let tx = mkstatic!([u8; 2048], [0u8; 2048]); // tx сокета
    let body = mkstatic!(String<4096>, String::new());

    loop {
        let (rxa, rxb) = rx.split_at_mut(1024);
        let mut socket = TcpSocket::new(stack, rxa, tx);
        socket.set_timeout(Some(Duration::from_secs(15)));

        let accepted = match socket.accept(80).await {
            Ok(()) => true,
            Err(e) => {
                log::debug!("accept failed: {:?}", e);
                false
            }
        };
        if accepted {
            handle(&mut socket, stack, publisher.as_ref(), rxb, body).await;
            // Критично: дождаться реальной отправки ответа — socket.abort() или
            // ранний drop топят TX-буфер (клиент видит Connection reset вместо
            // ответа). НО flush().await без лимита ждёт не только ACK данных, а
            // весь TCP-тейл (FinWait1/Closing/LastAck/rst_pending — см. embassy-net
            // tcp.rs Io::flush), и вешает сервер-таск на таймаут smoltcp (~40-60с),
            // в окне которого слушателя нет и новые SYN отбиваются refused'ом.
            // Поэтому: flush c жёстким лимитом. send_queue()==0 означает "все
            // байты переданы в сеть хотя бы раз" — потеря ретранслируется
            // сколько живёт сокет, а за 2с на LAN этого хватает.
            // ВАЖНО: перед flush — явный close(): без него мы не отправляем
            // FIN, smoltcp никогда не доводит состояние до Closed, flush
            // выкручивает весь 2с таймаут, и всё это время слушателя нет —
            // SYN в этом окне получают RST (клиент видит "Connection refused"
            // до 2с после каждого ответа). close() переводит конечный автомат
            // и flush завершается за миллисекунды.
            socket.close();
            match with_timeout(Duration::from_millis(2000), socket.flush()).await {
                Ok(Ok(())) => {}
                _ => {
                    if socket.send_queue() > 0 {
                        log::warn!("response not fully transmitted, {} bytes lost", socket.send_queue());
                    }
                }
            }
        } else {
            socket.abort();
        }
        // мягкий троттл против шторма переподключений
        Timer::after_millis(100).await;
    }
}

async fn handle<'a>(
    socket: &mut TcpSocket<'a>,
    stack: Stack<'static>,
    publisher: Option<&OtaPublisher<'_>>,
    reqbuf: &mut [u8],
    body: &mut String<4096>,
) {
    // Read the request headers (up to the empty line).
    let mut len = 0usize;
    let mut have_head = false;
    while len < reqbuf.len() {
        match socket.read(&mut reqbuf[len..]).await {
            Ok(0) => break,
            Ok(n) => {
                len += n;
                if reqbuf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                    have_head = true;
                    break;
                }
            }
            Err(e) => {
                log::debug!("request read failed: {:?}", e);
                return;
            }
        }
    }
    if !have_head {
        return;
    }

    let Ok(text) = core::str::from_utf8(&reqbuf[..len]) else {
        return;
    };
    let Some(line_end) = text.find("\r\n") else {
        return;
    };
    let mut parts = text[..line_end].split(' ');
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return;
    };

    if method != "GET" && method != "POST" {
        respond(socket, "400 Bad Request", "text/plain", "only GET/POST are supported\n").await;
        return;
    }

    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path, ""),
    };

    // POST принимается на два маршрута: сохранение Wi-Fi и ручная установка
    // времени (HTTP-путь, унаследованный от оригинала как запас без сети).
    if method == "POST" {
        match path {
            "/api/wifi/connect" => {
                let mut raw: heapless::Vec<u8, 512> = heapless::Vec::new();
                read_post_body(socket, reqbuf, &mut raw, len).await;
                handle_wifi_connect(socket, raw.as_slice()).await;
            }
            "/api/time" => {
                // capacity обязана совпадать с сигнатурой read_post_body (512).
                let mut raw: heapless::Vec<u8, 512> = heapless::Vec::new();
                read_post_body(socket, reqbuf, &mut raw, len).await;
                handle_time_set(socket, raw.as_slice()).await;
            }
            "/api/ntp" => {
                let mut raw: heapless::Vec<u8, 512> = heapless::Vec::new();
                read_post_body(socket, reqbuf, &mut raw, len).await;
                handle_ntp_set(socket, raw.as_slice()).await;
            }
            _ => {
                respond(
                    socket,
                    "405 Method Not Allowed",
                    "text/plain",
                    "POST is accepted on /api/wifi/connect, /api/time and /api/ntp\n",
                )
                .await;
            }
        }
        return;
    }

    match path {
        "/" => {
            respond_gz(socket, "text/html; charset=utf-8", clock_core::page::PAGE_GZ).await;
        }
        "/style.css" => {
            respond_gz(socket, "text/css; charset=utf-8", clock_core::page::CSS_GZ).await;
        }
        "/app.js" => {
            respond_gz(socket, "application/javascript; charset=utf-8", clock_core::page::JS_GZ).await;
        }
        "/api/status" => {
            body.clear();
            let mut ipbuf: heapless::String<45> = heapless::String::new();
            let mut gwbuf: heapless::String<16> = heapless::String::new();
            let mut dnsbuf: heapless::String<45> = heapless::String::new();
            let cfg_opt = stack.config_v4();
            let dhcp_done = cfg_opt.is_some();
            if let Some(cfg) = cfg_opt {
                // StaticConfigV4.address — StackedIpAddr; Display печатает "a.b.c.d/prefix"
                let _ = write!(ipbuf, "{}", cfg.address);
                if let Some((ip, _)) = ipbuf.split_once('/') {
                    ipbuf.truncate(ip.len());
                }
                if let Some(gw) = cfg.gateway {
                    let _ = write!(gwbuf, "{}", gw);
                }
                if let Some(d) = cfg.dns_servers.first() {
                    let _ = write!(dnsbuf, "{}", d);
                }
            }
            let ntp_saved = crate::settings::load_ntp();
            let aw = crate::settings::active_wifi();
            let (epoch, civil, tsource, synced) = crate::clock::snapshot().await;
            // M5: показания датчиков (None, если таск ещё не отчитался).
            // temp_mc из 1-Wire кодека — микроградусы (разрешение DS18B20 1/16 °C,
            // целые миллиградусы не выразимы); приводим к общему знаменателю JSON —
            // все температуры в milli-°C (как co2_temp_mc), усечение < 0.1 °C.
            let (temp_mc, co2_ppm, co2_temp_mc, v33_mv, v5_mv, v12_mv) = {
                let s = crate::sensors::SENSORS.lock().await;
                (
                    s.temp_mc.map(|mc| mc / 1000),
                    s.co2_ppm,
                    s.co2_temp_mc,
                    s.v33_mv,
                    s.v5_mv,
                    s.v12_mv,
                )
            };
            let snap = clock_core::StatusSnapshot {
                uptime_secs: Instant::now().as_secs(),
                fw: env!("CARGO_PKG_NAME"),
                version: env!("CARGO_PKG_VERSION"),
                ip: if ipbuf.is_empty() { None } else { Some(ipbuf.as_str()) },
                ota_state: &current_ota_state_str(),
                pending: PENDING_BOOT.load(Ordering::SeqCst),
                confirmed: BOOT_CONFIRMED.load(Ordering::SeqCst),
                latched: ROLLBACK_LATCHED.load(Ordering::SeqCst),
                busy: OTA_BUSY.load(Ordering::SeqCst),
                wifi_ssid: if aw.ssid.is_empty() { None } else { Some(aw.ssid.as_str()) },
                wifi_saved: aw.from_nvs,
                time_epoch: epoch,
                time: Some(civil.as_str()),
                time_source: tsource.as_str(),
                time_synced: synced,
                ntp_server: ntp_saved.as_ref().map(|s| s.as_str()),
                gateway: if gwbuf.is_empty() { None } else { Some(gwbuf.as_str()) },
                dhcp_done,
                dns: if dnsbuf.is_empty() { None } else { Some(dnsbuf.as_str()) },
                temp_mc,
                co2_ppm,
                co2_temp_mc,
                v33_mv,
                v5_mv,
                v12_mv,
            };
            let _ = write!(body, "{}", snap);
            respond(socket, "200 OK", "application/json", body.as_str()).await;
        }
        "/status" => {
            status_body(body);
            let _ = write!(body, "log_url={}\n", LOG_URL);
            respond(socket, "200 OK", "text/plain; charset=utf-8", body.as_str()).await;
        }
        "/display/test" => {
            // Диагностика M5.1: /display/test?digit=5 | ?cycle=1 | ?code=13
            // | ?rawcycle=1 | ?off=1. digit=1..9 фиксированная цифра на всех
            // лампах; cycle = цифры 1..9 по 1 с; code=0..15 сырой код на оба
            // дешифратора (без перемарки); rawcycle = перебор 0..15 по 1 с.
            let mut mode: u8 = 0;
            for kv in query.split('&') {
                match kv.split_once('=') {
                    Some(("digit", v)) => {
                        if let Ok(d) = v.parse::<u8>() {
                            match d {
                                0 => mode = clock_core::display::TEST_ZERO,
                                1..=9 => mode = d,
                                _ => {}
                            }
                        }
                    }
                    Some(("cycle", _)) => mode = clock_core::display::TEST_CYCLE,
                    Some(("code", v)) => {
                        if let Ok(c) = v.parse::<u8>() {
                            if c <= 15 {
                                mode = clock_core::display::TEST_RAW_BASE + c;
                            }
                        }
                    }
                    Some(("rawcycle", _)) => mode = clock_core::display::TEST_RAW_CYCLE,
                    _ => {}
                }
            }
            crate::display::TEST_MODE.store(mode, core::sync::atomic::Ordering::Relaxed);
            let mut msg: heapless::String<64> = String::new();
            let _ = write!(msg, "display test mode = {}\n", mode);
            respond(socket, "200 OK", "text/plain", msg.as_str()).await;
        }
        "/logs" | "/api/logs" => {
            logger::dump_into(body);
            if body.is_empty() {
                let _ = body.push_str("(log buffer empty)\n");
            }
            respond(socket, "200 OK", "text/plain; charset=utf-8", body.as_str()).await;
        }
        "/revert" => {
            respond(socket, "200 OK", "text/plain", "marking current image INVALID, resetting\n").await;
            log::warn!("/revert: marking the current image invalid");
            ROLLBACK_LATCHED.store(true, Ordering::SeqCst);
            use esp_bootloader_esp_idf::ota::OtaImageState;
            let _ = crate::updater::with_updater(|u| u.set_current_ota_state(OtaImageState::Invalid));
            Timer::after_millis(500).await;
            software_reset();
        }
        "/ota" => {
            let mut url: String<256> = String::new();
            for pair in query.split('&') {
                if let Some(v) = pair.strip_prefix("url=") {
                    url.clear();
                    let _ = url.push_str(v);
                }
            }
            if url.is_empty() || !url.starts_with("http://") {
                respond(
                    socket,
                    "400 Bad Request",
                    "text/plain",
                    "usage: GET /ota?url=http://host:port/firmware.bin\n",
                )
                .await;
                return;
            }
            if OTA_BUSY.load(Ordering::SeqCst) {
                respond(socket, "409 Conflict", "text/plain", "another OTA in progress\n").await;
                return;
            }
            match publisher {
                Some(p) => match p.try_publish(OtaRequest { url }) {
                    Ok(()) => respond(socket, "200 OK", "text/plain", "OTA scheduled, watch device logs\n").await,
                    Err(_) => respond(socket, "409 Conflict", "text/plain", "OTA queue full\n").await,
                },
                None => respond(socket, "500 Internal Server Error", "text/plain", "OTA channel unavailable\n")
                    .await,
            }
        }
        _ => {
            respond(
                socket,
                "404 Not Found",
                "text/plain",
                "endpoints: / /api/status /status /logs /api/logs /ota?url=... /revert (POST /api/wifi/connect, /api/time)\n",
            )
            .await;
        }
    }
}

/// Состояние OTA-слота одной строкой для JSON (ище строки короче 32 символов).
fn current_ota_state_str() -> heapless::String<32> {
    let mut s: heapless::String<32> = String::new();
    match crate::updater::with_updater(|u| u.current_ota_state()) {
        Ok(st) => {
            let _ = write!(s, "{:?}", st);
        }
        Err(_) => {
            s.push_str("Error").ok();
        }
    }
    s
}

/// Прочитать POST-тело (Content-Length, потолок 512 Б) в `body`: сначала
/// используем байты, уже приехавшие в буфер вместе с заголовками, потом
/// дочитываем с сокета. Невалидный UTF-8 молча отбрасывается — парсер
/// core сообщит ошибку разбора.
async fn read_post_body<'a>(
    socket: &mut TcpSocket<'a>,
    reqbuf: &mut [u8],
    out: &mut heapless::Vec<u8, 512>,
    head_len: usize,
) {
    out.clear();
    let Ok(text) = core::str::from_utf8(&reqbuf[..head_len]) else {
        return;
    };
    let head_end = match text.find("\r\n\r\n") {
        Some(i) => i + 4,
        None => head_len,
    };
    let mut cl: usize = 0;
    for line in text[..head_end].lines() {
        if line.get(..15).is_some_and(|p| p.eq_ignore_ascii_case("content-length:")) {
            cl = line[15..].trim().parse().unwrap_or(0);
        }
    }
    let need = cl.min(512);
    let have = (head_len - head_end).min(need);
    let _ = out.extend_from_slice(&reqbuf[head_end..head_end + have]);
    let mut got = have;
    while got < need {
        match socket.read(reqbuf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let take = n.min(need - got);
                let _ = out.extend_from_slice(&reqbuf[..take]);
                got += take;
            }
        }
    }
}

/// POST /api/time: ручная установка epoch (секунды Unix) — HTTP-путь из
/// оригинала, работает когда нет NTP/DHCP. Валидация диапазона — в core.
async fn handle_time_set<'a>(socket: &mut TcpSocket<'a>, raw: &[u8]) {
    let text = core::str::from_utf8(raw).unwrap_or("");
    let Some(v) = text.split('&').find_map(|p| p.strip_prefix("epoch=")) else {
        respond(socket, "400 Bad Request", "text/plain", "usage: epoch=<unix-seconds>\n").await;
        return;
    };
    let Ok(epoch) = v.trim().parse::<u64>() else {
        respond(socket, "400 Bad Request", "text/plain", "epoch must be unix seconds\n").await;
        return;
    };
    if epoch < 1_700_000_000 || epoch > 4_102_444_800 {
        respond(socket, "400 Bad Request", "text/plain", "epoch out of range 2023..2100\n").await;
        return;
    }
    crate::clock::sync(epoch, clock_core::time::TimeSource::Http).await;
    let civil = clock_core::time::epoch_to_civil(epoch);
    let mut msg: String<96> = String::new();
    let _ = write!(msg, "{{\"status\":\"set\",\"time\":\"{}\"}}\n", civil);
    respond(socket, "200 OK", "application/json", msg.as_str()).await;
}

/// POST /api/wifi/connect: валидация (core::settings) -> запись в NVS ->
/// ответ -> мягкий reboot, чтобы креды реально применялись новым Wi-Fi стеком.
async fn handle_wifi_connect<'a>(socket: &mut TcpSocket<'a>, raw: &[u8]) {
    let text = core::str::from_utf8(raw).unwrap_or("");
    match clock_core::settings::parse_wifi_json(text) {
        Err(e) => {
            log::warn!("wifi/connect: rejected: {}", e);
            let mut resp: String<256> = String::new();
            let _ = write!(resp, "{{\"status\":\"error\",\"error\":\"{}\"}}", e);
            respond(socket, "400 Bad Request", "application/json", resp.as_str()).await;
        }
        Ok(creds) => match crate::settings::save_wifi(&creds) {
            Err(e) => {
                log::error!("wifi/connect: NVS save failed: {}", e);
                let mut resp: String<256> = String::new();
                let _ = write!(resp, "{{\"status\":\"error\",\"error\":\"{}\"}}", e);
                respond(socket, "500 Internal Server Error", "application/json", resp.as_str()).await;
            }
            Ok(()) => {
                crate::settings::set_active(&creds, true);
                log::info!(
                    "wifi/connect: creds saved to NVS (ssid={}), rebooting in 2s",
                    creds.ssid
                );
                respond(socket, "200 OK", "application/json", "{\"status\":\"saved\",\"reboot_in_ms\":2000}").await;
                Timer::after_millis(2000).await;
                software_reset();
            }
        },
    }
}

/// POST /api/ntp: {"server":"host"} — сохранить кастомный NTP-сервер
/// (пустая строка — сброс на встроенные пулы). Никакого ребута: sntp_task
/// перечитывает NVS на каждой итерации и сменит сервер на следующем
/// цикле (≤ SNTP_RETRY_SECS).
async fn handle_ntp_set<'a>(socket: &mut TcpSocket<'a>, raw: &[u8]) {
    let text = core::str::from_utf8(raw).unwrap_or("");
    let server = match clock_core::settings::parse_ntp_json(text) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("ntp: rejected: {}", e);
            let mut resp: String<256> = String::new();
            let _ = write!(resp, "{{\"status\":\"error\",\"error\":\"{}\"}}", e);
            respond(socket, "400 Bad Request", "application/json", resp.as_str()).await;
            return;
        }
    };
    let saved: String<80> = if server.is_empty() {
        match crate::settings::save_ntp(None) {
            Err(e) => {
                log::error!("ntp: NVS save failed: {}", e);
                let mut resp: String<256> = String::new();
                let _ = write!(resp, "{{\"status\":\"error\",\"error\":\"{}\"}}", e);
                respond(socket, "500 Internal Server Error", "application/json", resp.as_str()).await;
                return;
            }
            Ok(()) => {
                let mut s: String<80> = String::new();
                let _ = s.push_str("default pools");
                s
            }
        }
    } else {
        match clock_core::settings::validate_ntp(&server) {
            Err(e) => {
                log::warn!("ntp: rejected: {}", e);
                let mut resp: String<256> = String::new();
                let _ = write!(resp, "{{\"status\":\"error\",\"error\":\"{}\"}}", e);
                respond(socket, "400 Bad Request", "application/json", resp.as_str()).await;
                return;
            }
            Ok(v) => match crate::settings::save_ntp(Some(v.as_str())) {
                Err(e) => {
                    log::error!("ntp: NVS save failed: {}", e);
                    let mut resp: String<256> = String::new();
                    let _ = write!(resp, "{{\"status\":\"error\",\"error\":\"{}\"}}", e);
                    respond(socket, "500 Internal Server Error", "application/json", resp.as_str()).await;
                    return;
                }
                Ok(()) => {
                    let mut s: String<80> = String::new();
                    let _ = s.push_str(v.as_str());
                    s
                }
            },
        }
    };
    log::info!("ntp: server set to {}", saved);
    let mut resp: String<256> = String::new();
    let _ = write!(resp, "{{\"status\":\"saved\",\"server\":\"{}\"}}", saved);
    respond(socket, "200 OK", "application/json", resp.as_str()).await;
}

async fn respond_gz<'a>(socket: &mut TcpSocket<'a>, content_type: &str, gz: &[u8]) {
    let mut headers: String<256> = String::new();
    let _ = write!(
        headers,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        content_type,
        gz.len()
    );
    write_all(socket, headers.as_bytes()).await;
    write_all(socket, gz).await;
}

fn status_body(body: &mut String<4096>) {
    body.clear();
    let _ = write!(body, "uptime_secs={}\n", Instant::now().as_secs());
    match crate::updater::with_updater(|u| u.current_ota_state()) {
        Ok(st) => {
            let _ = write!(body, "ota_state={:?}\n", st);
        }
        Err(e) => {
            let _ = write!(body, "ota_state_error={}\n", e);
        }
    }
    let _ = write!(
        body,
        "pending_boot={}\nconfirmed={}\nrollback_latched={}\nota_busy={}\n",
        PENDING_BOOT.load(Ordering::SeqCst),
        BOOT_CONFIRMED.load(Ordering::SeqCst),
        ROLLBACK_LATCHED.load(Ordering::SeqCst),
        OTA_BUSY.load(Ordering::SeqCst),
    );
}

async fn respond<'a>(socket: &mut TcpSocket<'a>, status: &str, content_type: &str, body: &str) {
    // Короткий заголовок живёт на стеке задачи — статический буфер здесь
    // не нужен и не должен писаться в StaticCell повторно.
    let mut headers: String<256> = String::new();
    let _ = write!(
        headers,
        "HTTP/1.1 {}\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );
    write_all(socket, headers.as_bytes()).await;
    write_all(socket, body.as_bytes()).await;
}

async fn write_all<'a>(socket: &mut TcpSocket<'a>, mut data: &[u8]) {
    while !data.is_empty() {
        match socket.write(data).await {
            Ok(0) => break,
            Ok(n) => data = &data[n..],
            Err(_) => break,
        }
    }
}
