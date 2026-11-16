//! Логирование по WiFi: каждая log-запись кладётся в ring-buffer с номером seq;
//! фоновая задача собирает неотправленные записи и пакетами POST'ит на LOG_URL.
//! Пока идёт POST, логирование подавлено (защита от рекурсии лога внутри reqwless).

use core::cell::RefCell;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering}; // +DROPPED ниже

use embassy_net::{
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
    Stack,
};
use embassy_sync::blocking_mutex::{
    raw::CriticalSectionRawMutex,
    Mutex,
};
use embassy_time::{Duration, Instant, Timer};
use heapless::{Deque, String};
use reqwless::client::HttpClient;
use reqwless::request::{Method, RequestBuilder};

use crate::LOG_URL;


const MAX_LINE: usize = 180;
const RING_MAX: usize = 64;
const BATCH_MAX: usize = 3500;

static SEQ: AtomicU32 = AtomicU32::new(0);
static RING: Mutex<
    CriticalSectionRawMutex,
    RefCell<Deque<(u32, String<MAX_LINE>), RING_MAX>>,
> = Mutex::new(RefCell::new(Deque::new()));
static SENDING: AtomicBool = AtomicBool::new(false);
/// Сколько записей потеряно переполнением ring, пока коллектор недоступен.
static DROPPED: AtomicU32 = AtomicU32::new(0);

struct WifiLogger;

impl log::Log for WifiLogger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::LevelFilter::Debug
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) || SENDING.load(Ordering::SeqCst) {
            return;
        }
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut line: String<MAX_LINE> = String::new();
        let _ = write!(
            line,
            "{:<5} t={:>9}ms {}",
            record.level(),
            Instant::now().as_millis(),
            record.args()
        );
        // Дублируем в UART: esp_println пишет напрямую в UART0, минуя log::,
        // рекурсии нет. Без этого UART молчал: лог-хук уводил всё в ring/WiFi.
        esp_println::println!("{}", line);
        RING.lock(|cell| {
            let ring = &mut *cell.borrow_mut();
            if ring.is_full() {
                ring.pop_front();
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
            let _ = ring.push_back((seq, line));
        });
    }

    fn flush(&self) {}
}

static LOGGER: WifiLogger = WifiLogger;

pub fn init() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// Дамп ring-буфера для HTTP-эндпоинта /logs (от самых старых к новым).
pub fn dump_into(out: &mut String<4096>) {
    out.clear();
    RING.lock(|cell| {
        for (_seq, line) in cell.borrow().iter() {
            if out.len() + line.len() + 1 > out.capacity() {
                break;
            }
            let _ = out.push_str(line);
            let _ = out.push('\n');
        }
    });
}

#[embassy_executor::task]
pub async fn logfwd_task(stack: Stack<'static>) {
    let mut tcp = TcpClient::new(
        stack,
        mkstatic!(TcpClientState<1, 1024, 1500>, TcpClientState::<1, 1024, 1500>::new()),
    );
    tcp.set_timeout(Some(Duration::from_secs(10)));
    let dns = DnsSocket::new(stack);
    let hdrbuf = mkstatic!([u8; 512], [0u8; 512]);
    let body = mkstatic!(String<4096>, String::new());

    // Host:port из LOG_URL для заголовка Host
    let mut host: String<96> = String::new();
    if let Some(rest) = LOG_URL.strip_prefix("http://") {
        let _ = host.push_str(rest.split('/').next().unwrap_or(""));
    }

    let mut last_sent: u32 = 0;
    let mut batch_last: u32 = 0;
    let mut fails: u32 = 0;
    let mut backoff_ms:u64 = 1000;
    let mut lost_mark: u32 = 0;

    loop {
        if body.is_empty() {
            // Собираем пакет из неотправленных записей (пауза ~1 сек между пакетами).
            batch_last = RING.lock(|cell| {
                let mut upto = last_sent;
                for (seq, line) in cell.borrow().iter() {
                    if *seq > upto {
                        if body.len() + line.len() + 1 > BATCH_MAX {
                            break;
                        }
                        let _ = body.push_str(line);
                        let _ = body.push('\n');
                        upto = *seq;
                    }
                }
                upto
            });

            if batch_last == last_sent {
                Timer::after_millis(1000).await;
                continue; // нечего отправлять
            }
        }

        SENDING.store(true, Ordering::SeqCst);
        let result = do_post(&tcp, &dns, host.as_str(), body.as_str(), hdrbuf).await;
        SENDING.store(false, Ordering::SeqCst);

        match result {
            Ok(()) => {
                last_sent = batch_last;
                body.clear();
                if fails > 0 {
                    log::info!(
                        "log link restored: attempts={}, lost={}",
                        fails,
                        DROPPED.load(Ordering::Relaxed) - lost_mark
                    );
                    fails = 0;
                    backoff_ms = 1000;
                }
            }
            Err(e) => {
                // Тело не сброшено: следующий цикл повторит POST того же пакета.
                // Экспоненциальный backoff (1..30 с) и подавление спама: мёртвый
                // коллектор не должен кормить сам себя warn'ами и не грузить исполнитель.
                if fails == 0 {
                    lost_mark = DROPPED.load(Ordering::Relaxed);
                }
                fails += 1;
                if fails == 1 || fails % 16 == 0 {
                    log::warn!("log flush failed (attempt {}): {}", fails, e.as_str());
                }
                Timer::after_millis(backoff_ms).await;
                backoff_ms = (backoff_ms * 2).min(30_000);
            }
        }
    }
}

async fn do_post(
    tcp: &TcpClient<'static, 1, 1024, 1500>,
    dns: &DnsSocket<'static>,
    host: &str,
    body: &str,
    hdrbuf: &mut [u8; 512],
) -> Result<(), String<128>> {
    let mut client = HttpClient::new(tcp, dns);
    let handle = match client.request(Method::POST, LOG_URL).await {
        Ok(h) => h,
        Err(e) => {
            let mut s: String<128> = String::new();
            let _ = write!(s, "request: {:?}", e);
            return Err(s);
        }
    };
    let headers = [
        ("Host", &host[..]),
        ("Content-Type", "text/plain"),
        ("Connection", "close"),
    ];
    let mut handle = handle.headers(&headers[..]).body(body.as_bytes());
    match handle.send(hdrbuf).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let mut s: String<128> = String::new();
            let _ = write!(s, "send: {:?}", e);
            Err(s)
        }
    }
}
