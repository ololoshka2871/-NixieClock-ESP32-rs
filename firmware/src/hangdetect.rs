//! Независимый детектор зависаний.
//!
//! Периодический ISR на TIMG0.timer1 (prio max; esp-rtos занимает только
//! TIMG0.timer0; TIMG1 на классике зарезервирован WiFi-блобом и непригоден
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

// Счётчики живучести: задачи дергают beat(), ISR следит изменение/застой.
pub static CYCLE: AtomicU32 = AtomicU32::new(0); // app_task: каждые 2 с
pub static FRAME: AtomicU32 = AtomicU32::new(0); // display: посланные DMA-чанки (~1 к/с)
pub static MHZ: AtomicU32 = AtomicU32::new(0); // mhz19: цикл ~2 с
pub static DS18: AtomicU32 = AtomicU32::new(0); // ds18x20: цикл 10 с

// Тонкие счётчики: последняя фаза, до которой дошла задача перед зависанием.
// В снимке видно, в какой именно код залип исполнитель (esp-hal 1.2.2 tx_start
// содержит безлимитные спины while tx_rempty / while tx_idle).
pub static PHASE_DISP: AtomicU32 = AtomicU32::new(0);
pub static PHASE_MHZ: AtomicU32 = AtomicU32::new(0);
pub static PHASE_DS18: AtomicU32 = AtomicU32::new(0);
pub static PHASE_APP: AtomicU32 = AtomicU32::new(0);
pub static DEFIBS: AtomicU32 = AtomicU32::new(0); // сколько раз ISR дефибриллировал I2S

#[inline]
pub fn set_phase(p: &AtomicU32, v: u32) {
    p.store(v, Ordering::Relaxed);
}

#[inline]
pub fn beat(c: &AtomicU32) {
    c.fetch_add(1, Ordering::Relaxed);
}

// Классика ESP32: RTC slow memory, 512 B на 0x5000_0000. В домене RTC —
// переживает sw_cpu_reset и RWDT-сброс; теряется только при полной потере питания.
const RTC_SLOW_BASE: usize = 0x5000_0000;
const MAGIC: u32 = 0xBAD_71E7;

/// true только когда прошивка собрана с фичей display (есть I2S-кадры).
/// Без индикации кадровый счётчик вечно стоит -> сверять его нельзя.
const DISPLAY_ACTIVE: bool = cfg!(feature = "display");

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

// НЕ ConstStaticCell: его take() одноразовый, а ISR дёргает его каждый
// тик -> второй take() = panic в контексте прерывания (так умерли 0.6.3/0.6.4:
// CoreSw уже на первой секунде).
static mut PT_OPT: Option<PeriodicTimer<'static, esp_hal::Blocking>> = None;

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
    // Индикация встала (счётчик фреймов), цикл приложения жив => скорее всего
    // исполнитель упал в безлимитный спин esp-hal 1.2.2 tx_start (while tx_rempty
    // / while tx_idle) после того, как помехи от ламп залили состояние I2S/DMA.
    // Дефибриллируем I2S/DMA из ISR, чтобы спин смог выйти.
    if DISPLAY_ACTIVE {
        let f_stall = STALL[1].load(Ordering::Relaxed);
        if f_stall >= 3 && f_stall < 15 && STALL[0].load(Ordering::Relaxed) < 15 {
            DEFIBS.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "display")]
            i2s_defib();
        }
    }
    // 15 тиков без цикла app_task ИЛИ без кадров индикации => исполнитель/задача
    // мертвы. Реагируем раньше 30-секундного RWDT: слепок -> software_reset.
    if STALL[0].load(Ordering::Relaxed) >= 15
        || (DISPLAY_ACTIVE && STALL[1].load(Ordering::Relaxed) >= 15)
    {
        let m = RTC_SLOW_BASE as *mut u32;
        core::ptr::write_volatile(m, MAGIC);
        core::ptr::write_volatile(m.add(1), tick);
        core::ptr::write_volatile(m.add(2), CYCLE.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(3), FRAME.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(4), MHZ.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(5), DS18.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(6), STALL[0].load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(7), STALL[1].load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(8), DEFIBS.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(9), PHASE_DISP.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(10), PHASE_MHZ.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(11), PHASE_DS18.load(Ordering::Relaxed));
        core::ptr::write_volatile(m.add(12), PHASE_APP.load(Ordering::Relaxed));
        esp_hal::system::software_reset();
    }
    unsafe {
        if let Some(pt) = &mut *core::ptr::addr_of_mut!(PT_OPT) {
            pt.clear_interrupt(); // хардварный auto_reload уже перезарядил счётчик
        }
    }
    }
}

/// Импульсная расцепилка I2S1/DMA «снаружи» (контекст ISR). esp-hal 1.2.2
/// tx_start() содержит безлимитные спины: `while !tx_rempty` и `while tx_idle`.
/// Если из-за наводок от ламп состояние DMA/FIFO залипает, исполнитель задачи
/// display падает в спин и вешает всю плату (снимки ISR: stall у всех задач
/// сразу, тики ISR живые). Сброс out_rst/FIFO + снятие прерываний выводит
/// периферию из залипшего состояния — спин выходит (send либо завершается,
/// либо корректно отдаёт Err в наш обработчик).
#[cfg(feature = "display")]
fn i2s_defib() {
    let i2s = unsafe { esp_hal::pac::i2s1::I2S1::steal() };
    i2s.conf().modify(|_, w| w.tx_start().clear_bit());
    i2s.lc_conf().modify(|_, w| w.out_rst().set_bit());
    for _ in 0..80 {
        core::hint::spin_loop();
    }
    i2s.lc_conf().modify(|_, w| w.out_rst().clear_bit());
    i2s.conf().modify(|_, w| w.tx_fifo_reset().set_bit());
    for _ in 0..80 {
        core::hint::spin_loop();
    }
    i2s.conf().modify(|_, w| w.tx_fifo_reset().clear_bit());
    i2s.int_clr().write(|w| {
        w.out_done().clear_bit_by_one();
        w.out_total_eof().clear_bit_by_one();
    });
}

pub fn setup(timer: impl esp_hal::timer::Timer + Into<esp_hal::timer::AnyTimer<'static>>) {
    let mut pt = PeriodicTimer::new(timer);
    pt.set_interrupt_handler(InterruptHandler::new(diag_isr, Priority::max()));
    // Храним таймер ДО arm: ISR не вооружён до listen(), гонки нет.
    // Edition-2024: доступ к static mut только через addr_of_mut!.
    unsafe { *core::ptr::addr_of_mut!(PT_OPT) = Some(pt); }
    let pt_ref = unsafe { &mut *core::ptr::addr_of_mut!(PT_OPT) };
    let Some(pt) = pt_ref else { return };
    if pt.start(Rate::from_hz(1).as_duration()).is_err() {
        log::error!("hang-detect: TIMG0 старт не удался");
        return;
    }
    pt.listen();
    log::info!("hang-detect armed (TIMG0.timer1, 1 s tick, prio max, reset при stall 15 с)");
}

/// Вызывать в начале бута после logger::init(): снимок прошлой смерти, если ISR
/// успел его оставить. Отсутствие снимка при SysRtcWdt = ISR тоже замёрз.
pub fn report_prev() {
    unsafe {
        let m = RTC_SLOW_BASE as *const u32;
        if core::ptr::read_volatile(m) == MAGIC {
            log::error!(
                "ЗАВИСАНИЕ ЗАФИКСИВАНО ISR: tick={} cycle={} frame={} mhz={} ds18={} stall_cycle={}s stall_frame={}s defib={} фазы(disp={},mhz={},ds18={},app={})",
                core::ptr::read_volatile(m.add(1)),
                core::ptr::read_volatile(m.add(2)),
                core::ptr::read_volatile(m.add(3)),
                core::ptr::read_volatile(m.add(4)),
                core::ptr::read_volatile(m.add(5)),
                core::ptr::read_volatile(m.add(6)),
                core::ptr::read_volatile(m.add(7)),
                core::ptr::read_volatile(m.add(8)),
                core::ptr::read_volatile(m.add(9)),
                core::ptr::read_volatile(m.add(10)),
                core::ptr::read_volatile(m.add(11)),
                core::ptr::read_volatile(m.add(12)),
            );
            let w = m as *mut u32;
            for i in 0..13 {
                core::ptr::write_volatile(w.add(i), 0);
            }
        }
    }
}
