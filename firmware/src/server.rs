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
    BOOT_CONFIRMED, LOG_URL, OTA_BUSY, OTA_LOCK, OTA_REQ, OtaPublisher, OtaRequest,
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

    if method != "GET" {
        respond(socket, "400 Bad Request", "text/plain", "only GET is supported\n").await;
        return;
    }

    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path, ""),
    };

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
            if let Some(cfg) = stack.config_v4() {
                // StaticConfigV4.address — StackedIpAddr; Display печатает "a.b.c.d/prefix"
                let _ = write!(ipbuf, "{}", cfg.address);
                if let Some((ip, _)) = ipbuf.split_once('/') {
                    ipbuf.truncate(ip.len());
                }
            }
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
            };
            let _ = write!(body, "{}", snap);
            respond(socket, "200 OK", "application/json", body.as_str()).await;
        }
        "/status" => {
            status_body(body);
            let _ = write!(body, "log_url={}\n", LOG_URL);
            respond(socket, "200 OK", "text/plain; charset=utf-8", body.as_str()).await;
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
            OTA_LOCK.lock(|cell| {
                if let Some(u) = cell.borrow_mut().as_mut() {
                    use esp_bootloader_esp_idf::ota::OtaImageState;
                    let _ = u.set_current_ota_state(OtaImageState::Invalid);
                }
            });
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
                "endpoints: / /api/status /status /logs /api/logs /ota?url=... /revert\n",
            )
            .await;
        }
    }
}

/// Состояние OTA-слота одной строкой для JSON (ище строки короче 32 символов).
fn current_ota_state_str() -> heapless::String<32> {
    let mut s: heapless::String<32> = String::new();
    match OTA_LOCK.lock(|cell| {
        cell.borrow_mut()
            .as_mut()
            .map(|u| u.current_ota_state())
    }) {
        Some(Ok(st)) => {
            let _ = write!(s, "{:?}", st);
        }
        Some(Err(_)) => {
            s.push_str("Error").ok();
        }
        None => {
            s.push_str("no_updater").ok();
        }
    }
    s
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
    match OTA_LOCK.lock(|cell| {
        cell.borrow_mut()
            .as_mut()
            .map(|u| u.current_ota_state())
    }) {
        Some(Ok(st)) => {
            let _ = write!(body, "ota_state={:?}\n", st);
        }
        Some(Err(e)) => {
            let _ = write!(body, "ota_state_error={:?}\n", e);
        }
        None => {
            let _ = body.push_str("ota_state=no_updater\n");
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
