//! Логирование по WiFi: каждая log-запись кладётся в ring-buffer с номером seq;
//! фоновая задача собирает неотправленные записи и пакетами POST'ит на адрес
//! коллектора (issue #6: коллектор опционален).
//!
//! Адрес живёт в NVS (`log_url`) и меняется из WebGUI без пересборки. Пустой
//! адрес = коллектора нет: задача не поднимает TCP/DNS, не делает POST и не
//! влияет на работу устройства (нет warn'ов, таймаутов и retry-штормов). Логи
//! всегда остаются в UART и в ring-буфере, который отдаёт /logs.
//!
//! Раньше на время POST стоял флаг SENDING, который глушил ВСЕ логи. Это была
//! ловушка: если POST зависал (например, reqwless ждал ответ, а соединение
//! умерло), SENDING оставался true навсегда, и лог замолкал ровно тогда, когда
//! он нужнее всего — при разборе поломки. Теперь:
//!   * логи не подавляются никогда (рекурсии нет: хук только пишет в ring);
//!   * POST ограничен `POST_TIMEOUT` — зависший сокет не блокирует задачу;
//!   * SENDING остался как индикатор «идёт отправка» для телеметрии.

use core::cell::RefCell;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_net::{
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
    Stack,
};
use embassy_sync::blocking_mutex::{
    raw::CriticalSectionRawMutex,
    Mutex,
};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use heapless::{Deque, String};
use reqwless::client::HttpClient;
use reqwless::request::{Method, RequestBuilder};

use crate::settings::log_url_runtime;


/// Максимальная длина строки лога. Раньше было 180, и длинная диагностика
/// (int_raw/int_ena/state) молча обрезалась в `write!` — в коллекторе строки
/// кончались на `on=true`. 256 хватает самой длинной строке с запасом; ring
/// 64x256 = 16 KB .bss (стек main-задачи уменьшается на столько же).
const MAX_LINE: usize = 256;
const RING_MAX: usize = 64;
const BATCH_MAX: usize = 3500;

static SEQ: AtomicU32 = AtomicU32::new(0);
static RING: Mutex<
    CriticalSectionRawMutex,
    RefCell<Deque<(u32, String<MAX_LINE>), RING_MAX>>,
> = Mutex::new(RefCell::new(Deque::new()));
/// Идёт POST (только индикатор: логи не подавляются).
static SENDING: AtomicBool = AtomicBool::new(false);
/// Успешные POST'ы и жёсткие таймауты — видны в строке `display: alive`,
/// чтобы по коллектору было ясно, живёт ли канал логов.
pub static POST_OK: AtomicU32 = AtomicU32::new(0);
pub static POST_TIMEOUTS: AtomicU32 = AtomicU32::new(0);
/// Сколько записей потеряно переполнением ring, пока коллектор недоступен.
static DROPPED: AtomicU32 = AtomicU32::new(0);

/// Сколько секунд даём одному POST. `TcpClient::set_timeout` покрывает только
/// connect; чтение ответа в reqwless может ждать вечно, если соединение
/// «повисло» (пир исчез без RST/FIN).
const POST_TIMEOUT: Duration = Duration::from_secs(20);

pub fn post_ok() -> u32 {
    POST_OK.load(Ordering::Relaxed)
}

pub fn post_timeouts() -> u32 {
    POST_TIMEOUTS.load(Ordering::Relaxed)
}

pub fn sending() -> bool {
    SENDING.load(Ordering::Relaxed)
}

struct WifiLogger;

impl log::Log for WifiLogger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::LevelFilter::Debug
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
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
        // Неблокирующий дубль в UART0 (см. uarttap.rs): esp_println на ESP32
        // крутится на ROM-функции, которая ждёт места в TX-FIFO, — это
        // миллисекунды в вызывающем коде на каждую строку.
        crate::uarttap::line(line.as_str());
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

/// Дамп ring-буфера для HTTP-эндпоинта /logs — **последние** строки, которые
/// влезают в 4096 байт, в хронологическом порядке.
///
/// Раньше отдавались первые строки кольца (64 x ~250 B = 16 KB), то есть
/// самые старые: при разборе «что случилось только что» страница показывала
/// состояние на минуту назад. Теперь считаем, сколько хвостовых строк влезает,
/// и отдаём именно хвост.
pub fn dump_into(out: &mut String<4096>) {
    out.clear();
    RING.lock(|cell| {
        let ring = cell.borrow();
        let mut start = ring.len();
        let mut need = 0usize;
        while start > 0 {
            let len = ring.get(start - 1).map(|(_, l)| l.len() + 1).unwrap_or(0);
            if need + len > out.capacity() {
                break;
            }
            need += len;
            start -= 1;
        }
        for (_seq, line) in ring.iter().skip(start) {
            let _ = out.push_str(line);
            let _ = out.push('\n');
        }
    });
}

/// Завести фоновую задачу отправки логов. Коллектор опционален (issue #6),
/// поэтому задача поднимается ВСЕГДА: пустой адрес она обрабатывает сама —
/// не создаёт TCP/DNS-клиент, не делает POST'ов и не влияет на индикацию.
/// Именно так адрес, выставленный из WebGUI, начинает работать без перезагрузки.
/// В embassy-executor 0.10 конструктор задачи возвращает `Result<SpawnToken,
/// SpawnError>` (пул слотов фиксирован), а сам `spawn` принимает токен и
/// возвращает `()` — поэтому `?` тут не нужен и неуместен: падать из-за
/// логгера нельзя, индикация важнее.
pub fn spawn_logger_task(spawner: &embassy_executor::Spawner, stack: Stack<'static>) {
    match logfwd_task(stack) {
        Ok(token) => spawner.spawn(token),
        // SpawnError в 0.10 имеет единственный вариант Busy и реализует Display.
        Err(e) => log::warn!("log forwarder not spawned: {}", e),
    }
}

#[embassy_executor::task]
pub async fn logfwd_task(stack: Stack<'static>) {
    // Коллектор опционален (issue #6): пустой адрес = отправка выключена.
    // Тогда задача просто ждёт — без TCP/DNS-клиента, без POST'ов, без warn'ов
    // и retry-штормов, то есть отсутствие коллектора никак не влияет на работу
    // устройства. Логи при этом остаются в UART и в ring-буфере (/logs).
    let mut tcp: Option<TcpClient<'static, 1, 1024, 1500>> = None;
    let mut dns: Option<DnsSocket<'static>> = None;
    let hdrbuf = mkstatic!([u8; 512], [0u8; 512]);
    let body = mkstatic!(String<4096>, String::new());

    let mut last_sent: u32 = 0;
    let mut batch_last: u32 = 0;
    let mut fails: u32 = 0;
    let mut backoff_ms: u64 = 1000;
    let mut lost_mark: u32 = 0;

    loop {
        let url = log_url_runtime();
        if url.is_empty() {
            if tcp.is_some() {
                log::info!("log forwarder disabled (address cleared)");
                tcp = None;
                dns = None;
                body.clear();
                fails = 0;
                backoff_ms = 1000;
            }
            Timer::after_millis(2000).await;
            continue;
        }
        if tcp.is_none() {
            tcp = Some(TcpClient::new(
                stack,
                mkstatic!(TcpClientState<1, 1024, 1500>, TcpClientState::<1, 1024, 1500>::new()),
            ));
            tcp.as_mut().unwrap().set_timeout(Some(Duration::from_secs(10)));
            dns = Some(DnsSocket::new(stack));
            log::info!("log forwarder -> {}", url);
        }
        let client = tcp.as_mut().unwrap();
        let dnsref = dns.as_ref().unwrap();

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

        // Host:port из адреса коллектора для заголовка Host
        let mut host: String<96> = String::new();
        if let Some(rest) = url.strip_prefix("http://") {
            let _ = host.push_str(rest.split('/').next().unwrap_or(""));
        }

        SENDING.store(true, Ordering::SeqCst);
        // Крошка в RTC: если зависнем именно здесь, на следующем буте будет
        // видно, что последним делом был POST логов.
        crate::rtcmark::mark(crate::rtcmark::T_LOG, crate::rtcmark::L_POST);
        let result = match with_timeout(
            POST_TIMEOUT,
            do_post(client, dnsref, url.as_str(), host.as_str(), body.as_str(), hdrbuf),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                POST_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                let mut s: String<128> = String::new();
                let _ = write!(s, "timeout {}s (ответ не пришёл)", POST_TIMEOUT.as_secs());
                Err(s)
            }
        };
        crate::rtcmark::clear();
        SENDING.store(false, Ordering::SeqCst);

        match result {
            Ok(()) => {
                POST_OK.fetch_add(1, Ordering::Relaxed);
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
    url: &str,
    host: &str,
    body: &str,
    hdrbuf: &mut [u8; 512],
) -> Result<(), String<128>> {
    let mut client = HttpClient::new(tcp, dns);
    let handle = match client.request(Method::POST, url).await {
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

