//! Фоновая OTA-задача: качает образ по HTTP, пишет в следующий OTA-слот
//! (стирая блок перед записью в него), проверяет CRC32 по заголовку X-Crc32,
//! активирует слот (activate_next_partition + состояние New) и сбрасывает чип.
//! Дальше работает связка загрузчика: New -> PendingVerify -> подтверждение
//! приложением; не подтвердился — откат на предыдущий слот.

use core::fmt::Write as _;
use core::sync::atomic::Ordering;

use embassy_net::{
    Stack,
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
};
use embassy_time::Timer;
use embedded_io_async::Read;
use heapless::String;
use reqwless::client::HttpClient;
use reqwless::request::{Method, RequestBuilder};

use crate::{OTA_BUSY, OTA_LOCK, OTA_REQ, OtaRequest};

use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_hal::system::software_reset;

const CHUNK: usize = 4096;
const ERASE_BLOCK: usize = 64 * 1024;
/// esp_image_header_t.magic — первый байт образа в OTA-слоте.
const ESP_IMAGE_MAGIC: u8 = 0xE9;

type OtaErr = String<160>;

fn errf(args: core::fmt::Arguments<'_>) -> OtaErr {
    let mut s: OtaErr = String::new();
    let _ = s.write_fmt(args);
    s
}

#[embassy_executor::task]
pub async fn ota_worker_task(stack: Stack<'static>) {
    let mut tcp = TcpClient::new(
        stack,
        mkstatic!(TcpClientState<1, CHUNK, CHUNK>, TcpClientState::<1, CHUNK, CHUNK>::new()),
    );
    tcp.set_timeout(Some(embassy_time::Duration::from_secs(15)));
    let dns = DnsSocket::new(stack);
    let Ok(mut sub) = OTA_REQ.subscriber() else {
        log::error!("no OTA subscriber slot");
        return;
    };
    let chunk = mkstatic!([u8; CHUNK], [0u8; CHUNK]);
    let hdrbuf = mkstatic!([u8; 1024], [0u8; 1024]);

    loop {
        let req = sub.next_message_pure().await;
        OTA_BUSY.store(true, Ordering::SeqCst);
        log::info!("OTA starting: {}", req.url.as_str());
        let res = do_ota(&tcp, &dns, &req, chunk, hdrbuf).await;
        OTA_BUSY.store(false, Ordering::SeqCst);
        match res {
            Ok(size) => {
                log::info!("OTA ok: {} bytes written; rebooting into the new image", size);
                // Пауза, чтобы лог успел уйти на коллектор.
                Timer::after_millis(2000).await;
                software_reset();
            }
            Err(e) => log::error!("OTA failed: {}", e.as_str()),
        }
    }
}

/// Стандартный CRC32 (отражённый, полином 0xEDB88320), без таблицы.
fn crc32(crc: u32, bytes: &[u8]) -> u32 {
    let mut c = crc;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            if c & 1 == 1 {
                c = (c >> 1) ^ 0xEDB88320;
            } else {
                c >>= 1;
            }
        }
    }
    c
}

fn next_slot_capacity() -> Result<usize, OtaErr> {
    OTA_LOCK.lock(|cell| match cell.borrow_mut().as_mut() {
        Some(u) => match u.next_partition() {
            Ok((region, _ty)) => Ok(region.capacity()),
            Err(e) => Err(errf(format_args!("no target slot: {:?}", e))),
        },
        None => Err(errf(format_args!("no OTA updater"))),
    })
}

async fn do_ota(
    tcp: &TcpClient<'static, 1, CHUNK, CHUNK>,
    dns: &DnsSocket<'static>,
    req: &OtaRequest,
    chunk: &mut [u8; CHUNK],
    hdrbuf: &mut [u8; 1024],
) -> Result<usize, OtaErr> {
    let mut host: String<96> = String::new();
    if let Some(rest) = req.url.strip_prefix("http://") {
        let _ = host.push_str(rest.split('/').next().unwrap_or(""));
    }

    let cap = next_slot_capacity()?;

    let mut client = HttpClient::new(tcp, dns);
    let handle = match client.request(Method::GET, req.url.as_str()).await {
        Ok(h) => h,
        Err(e) => return Err(errf(format_args!("request failed: {:?}", e))),
    };
    let headers = [("Host", &host[..]), ("Connection", "close")];
    let mut handle = handle.headers(&headers[..]);
    let response = match handle.send(hdrbuf).await {
        Ok(r) => r,
        Err(e) => return Err(errf(format_args!("send failed: {:?}", e))),
    };
    if response.status.0 != 200 {
        return Err(errf(format_args!("HTTP status {}", response.status.0)));
    }

    let size = match response.content_length {
        Some(n) => n,
        None => return Err(errf(format_args!("server sent no Content-Length"))),
    };
    if size == 0 || size > cap {
        return Err(errf(format_args!(
            "bad image size {} bytes (slot capacity {})",
            size,
            cap
        )));
    }

    // Ожидаемый CRC32 образа из заголовка X-Crc32 (его добавляет tools/serve_ota.py).
    let mut want_crc: Option<u32> = None;
    for (name, value) in response.headers() {
        if name.eq_ignore_ascii_case("x-crc32") {
            if let Ok(s) = core::str::from_utf8(value) {
                want_crc = u32::from_str_radix(s.trim(), 16).ok();
            }
        }
    }
    match want_crc {
        Some(c) => log::info!("image size {} bytes, expected CRC32 {:08x}", size, c),
        None => log::warn!("no X-Crc32 header; installing without CRC check (header check remains)"),
    }

    let mut reader = response.body().reader();
    let mut total = 0usize;
    let mut erased_to = 0usize;
    let mut crc = 0xFFFF_FFFFu32;

    loop {
        if total >= size {
            break;
        }

        // Стираем следующий блок перед записью в него; уступаем исполнителю,
        // чтобы watchdog продолжал кормиться.
        while total + CHUNK > erased_to {
            let from = erased_to as u32;
            let to = (erased_to + ERASE_BLOCK).min(cap) as u32;
            let res = OTA_LOCK.lock(|cell| {
                cell.borrow_mut().as_mut().map(|u| {
                    u.next_partition().map(|(mut r, _)| r.erase(from, to))
                })
            });
            match res {
                Some(Ok(Ok(()))) => {}
                Some(Ok(Err(e))) => return Err(errf(format_args!("erase: {:?}", e))),
                Some(Err(e)) => return Err(errf(format_args!("slot: {:?}", e))),
                None => return Err(errf(format_args!("no OTA updater"))),
            }
            erased_to = (erased_to + ERASE_BLOCK).min(cap);
            Timer::after_millis(1).await;
        }

        let n = match reader.read(&mut chunk[..]).await {
            Ok(n) => n,
            Err(e) => return Err(errf(format_args!("stream read: {:?}", e))),
        };
        if n == 0 {
            break;
        }
        let n = n.min(size - total);
        // Защита: образ в OTA-слоте обязан начинаться с esp_image magic 0xE9.
        // Битый/чужой файл отсекается до записи и активации слота.
        if total == 0 && chunk[0] != ESP_IMAGE_MAGIC {
            return Err(errf(format_args!(
                "image rejected: bad magic 0x{:02x}, expected 0xE9",
                chunk[0]
            )));
        }
        crc = crc32(crc, &chunk[..n]);

        let off = total as u32;
        let res = OTA_LOCK.lock(|cell| {
            cell.borrow_mut().as_mut().map(|u| {
                u.next_partition()
                    .map(|(mut r, _)| r.write(off, &chunk[..n]))
            })
        });
        match res {
            Some(Ok(Ok(()))) => {}
            Some(Ok(Err(e))) => return Err(errf(format_args!("flash write: {:?}", e))),
            Some(Err(e)) => return Err(errf(format_args!("slot: {:?}", e))),
            None => return Err(errf(format_args!("no OTA updater"))),
        }
        total += n;

        if total % 131072 < CHUNK {
            log::info!("OTA progress {}/{} bytes", total, size);
        }
    }

    if total != size {
        return Err(errf(format_args!(
            "download incomplete: {}/{} bytes",
            total,
            size
        )));
    }

    let got = !crc;
    if let Some(w) = want_crc {
        if got != w {
            return Err(errf(format_args!(
                "CRC32 mismatch: got {:08x}, expected {:08x} (slot NOT activated)",
                got,
                w
            )));
        }
        log::info!("CRC32 verified: {:08x}", got);
    }

    // Активация: otadata выбирает следующий слот; состояние New означает, что
    // загрузчик (с rollback) переведёт его в PendingVerify и не подтверждённый
    // образ будет откачен.
    let res = OTA_LOCK.lock(|cell| {
        cell.borrow_mut().as_mut().map(|u| {
            u.activate_next_partition()
                .and_then(|_| u.set_current_ota_state(OtaImageState::New))
        })
    });
    match res {
        Some(Ok(())) => Ok(total),
        Some(Err(e)) => Err(errf(format_args!("activate failed: {:?}", e))),
        None => Err(errf(format_args!("no OTA updater"))),
    }
}
