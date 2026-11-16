//! Независимый детектор зависаний.
//!
//! Периодический ISR на TIMG1.timer1 (priority 6; таймер embassy-исполнителя
//! у esp-rtos жёстко Priority1) раз в секунду снимает счётчики живучести задач.
//! Если app_task не циклует >=15 с — исполнитель заморожен: снимок счётчиков
//! пишется в RTC slow memory (переживает software_reset и RWDT-сброс на
//! классике) и делается software_reset() ДО 30-секундного RWDT.
//!
//! На следующем буте report_prev() выкладывает снимок в collector. Если снимок
//! пустой, а рестарт был SysRtcWdt — значит умер и наш ISR (deadlock с
//! выключенными прерываниями / исключение уровня CPU): два разных signatures.

use core::sync::atomic::{AtomicU32, Ordering};

use esp_hal::interrupt::{InterruptHandler, Priority};
use esp_hal::timer::PeriodicTimer;
use esp_hal::time::Rate;
use static_cell::ConstStaticCell;

// Счётчики живучести: задачи дергают beat(), ISR следит изменение/застой.
pub static CYCLE: AtomicU32 = AtomicU32::new(0); // app_task: каждые 2 с
pub static FRAME: AtomicU32 = AtomicU32::new(0); // display: посланные DMA-чанки (~1 к/с)
pub static MHZ: AtomicU32 = AtomicU32::new(0); // mhz19: цикл ~2 с
pub static DS18: AtomicU32 = AtomicU32::new(0); // ds18x20: цикл 10 с

#[inline]
pub fn beat(c: &AtomicU32) {
    c.fetch_add(1, Ordering::Relaxed);
}

// Классика ESP32: RTC slow memory, 512 B на 0x5000_0000. В домене RTC —
// переживает sw_cpu_reset и RWDT-сброс; теряется только при полной потере питания.
const RTC_SLOW_BASE: usize = 0x5000_0000;
const MAGIC: u32 = 0xBAD_71E7;

static ISR_TICK: AtomicU32 = AtomicU32::new(0);
static PREV: [AtomicU32; 4] = [
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
];
static STALL: [AtomicU32; 4] = [
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
    const { AtomicU32::new(0) },
];

static PT_CELL: ConstStaticCell<Option<PeriodicTimer<'static, esp_hal::Blocking>>> =
    ConstStaticCell::new(None);

extern "C" fn diag_isr() {
    unsafe {
    let tick = ISR_TICK.fetch_add(1, Ordering::Relaxed) + 1;
    let counters = [&CYCLE, &FRAME, &MHZ, &DS18];
    for (i, c) in counters.iter().enumerate() {
        let v = c.load(Ordering::Relaxed);
        if v == PREV[i].load(Ordering::Relaxed) {
            STALL[i].fetch_add(1, Ordering::Relaxed);
        } else {
            PREV[i].store(v, Ordering::Relaxed);
            STALL[i].store(0, Ordering::Relaxed);
        }
    }
    // 15 тиков без цикла app_task (норма 2 с) => исполнитель заморожен.
    // Реагируем раньше 30-с RWDT: снимок -> software_reset.
    if STALL[0].load(Ordering::Relaxed) >= 15 {
        let m = RTC_SLOW_BASE as *mut u32;
        core::ptr::write_volatile(m, MAGIC);
        core::ptr::write_volatile(m.add(1), tick);
        core::ptr::write_volatile(m.add(2), CYCLE.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(3), FRAME.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(4), MHZ.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(5), DS18.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(6), STALL[0].load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(7), STALL[1].load(Ordering::Relaxed));
        esp_hal::system::software_reset();
    }
    if let Some(pt) = PT_CELL.take() {
        pt.clear_interrupt(); // хардварный auto_reload уже перезарядил счётчик
    }
    }
}

pub fn setup(timer: impl esp_hal::timer::Timer + Into<esp_hal::timer::AnyTimer<'static>>) {
    let mut pt = PeriodicTimer::new(timer);
    let Ok(()) = pt.start(Rate::from_hz(1).as_duration()) else {
        log::error!("hang-detect: TIMG1 старт не удался");
        return;
    };
    // Тип PeriodicTimer живёт без set_interrupt_handler (unstable API).
    pt.set_interrupt_handler(InterruptHandler::new(diag_isr, Priority::max()));
    pt.listen();
    let slot: &'static mut Option<PeriodicTimer<'static, esp_hal::Blocking>> = PT_CELL.take();
    *slot = Some(pt);
    log::info!("hang-detect armed (TIMG1, 1 s tick, prio max, reset при stall 15 с)");
}

/// Вызывать в начале бута после logger::init(): снимок прошлой смерти, если ISR
/// успел его оставить. Отсутствие снимка при SysRtcWdt = ISR тоже замёрз.
pub fn report_prev() {
    unsafe {
        let m = RTC_SLOW_BASE as *const u32;
        if core::ptr::read_volatile(m) == MAGIC {
            log::error!(
                "ЗАВИСАНИЕ ЗАФИКСИВАНО ISR: tick={} cycle={} frame={} mhz={} ds18={} stall_cycle={}s stall_frame={}s",
                core::ptr::read_volatile(m.add(1)),
                core::ptr::read_volatile(m.add(2)),
                core::ptr::read_volatile(m.add(3)),
                core::ptr::read_volatile(m.add(4)),
                core::ptr::read_volatile(m.add(5)),
                core::ptr::read_volatile(m.add(6)),
                core::ptr::read_volatile(m.add(7)),
            );
            let w = m as *mut u32;
            for i in 0..8 {
                core::ptr::write_volatile(w.add(i), 0);
            }
        }
    }
}
