//! Питание: встроенный brown-out детектор (BOD) ESP32.
//!
//! Плата питается от ~4.6 В (5 В минус входной диод), ESP32 — от 3.3 В.
//! Если под нагрузкой рельс проседает, BOD — единственное, что отличит
//! «просадку питания» от программного зависания: при срабатывании следующий
//! бут скажет `boot reason: Some(SysBrownOut)` вместо загадочного зависания
//! или `ChipPowerOn` (когда успевает потеряться и RTC-домен).
//!
//! ROM-загрузчик ESP32 BOD не включает, esp-hal его тоже не трогает. Поэтому
//! на каждом буте печатаем конфиг, а если детектор выключен — включаем на
//! 2.9 В с аппаратным сбросом. 2.9 В — это ~350 мВ ниже измеренных 3.25 В,
//! то есть штатные просадки WiFi (~100 мВ) его не тронут, а реальная авария
//! будет видна в логе.

use esp32::RTC_CNTL;

/// Порог по TRM ESP32: 0=2.7 В, 1=2.8, 2=2.9, 3=3.0, 4=3.1, 5=3.2,
/// 6=3.3, 7=3.4 В.
///
/// Берём самый низкий: цель — поймать реальный провал, а не сбрасывать плату
/// на каждом законном проседании под импульсом WiFi. Именно 0xf
/// (`RTCWDT_BROWN_OUT_RESET`) и увидел владелец при просадке 3.3 В.
const THRES_2V7: u8 = 0;

/// Вызывать сразу после `esp_hal::init`, до включения индикации.
pub fn brownout_report() {
    let regs = unsafe { &*RTC_CNTL::ptr() };
    let r = regs.brown_out().read();
    let ena = r.ena().bit_is_set();
    let thres = r.dbrown_out_thres().bits();
    let rst_ena = r.rst_ena().bit_is_set();
    let det = r.det().bit_is_set();

    if ena {
        log::info!(
            "brown-out: включён, порог={} ({}), rst_ena={}, det={}",
            thres,
            match thres {
                0 => "2.7V",
                1 => "2.8V",
                2 => "2.9V",
                3 => "3.0V",
                4 => "3.1V",
                5 => "3.2V",
                6 => "3.3V",
                _ => "3.4V",
            },
            rst_ena,
            det,
        );
        return;
    }

    regs.brown_out().modify(|_, w| unsafe {
        w.dbrown_out_thres()
            .bits(THRES_2V7)
            .rst_wait()
            .bits(0x3ff)
            .rst_ena()
            .set_bit()
            .pd_rf_ena()
            .set_bit()
            .close_flash_ena()
            .set_bit()
            .ena()
            .set_bit()
    });
    log::warn!("brown-out: был выключен — включил на 2.7 В со сбросом (просадка теперь = SysBrownOut)");
}

/// Минимум 3.3 В за всё время работы (мВ, 0 = ещё не мерили).
pub static MIN_V33_MV: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Последнее окно измерения «под нагрузкой» (1 с, сэмпл каждые 2 мс):
/// среднее и минимум 3.3 В, сколько сэмплов ниже 3.0 В и сколько всего.
/// Видно в `/status` — браузерная самопроверка показывает питание по каждой
/// цифре, не заставляя читать логи.
pub static WIN_V33_AVG: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
pub static WIN_V33_MIN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
pub static WIN_V33_LOW: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
pub static WIN_V33_N: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn set_window(avg: u32, min: u32, low: u32, n: u32) {
    use core::sync::atomic::Ordering;
    WIN_V33_AVG.store(avg, Ordering::Relaxed);
    WIN_V33_MIN.store(min, Ordering::Relaxed);
    WIN_V33_LOW.store(low, Ordering::Relaxed);
    WIN_V33_N.store(n, Ordering::Relaxed);
}

/// Запомнить очередное измерение 3.3 В (атомарный минимум).
pub fn note_v33(mv: u32) {
    use core::sync::atomic::Ordering;
    let mut cur = MIN_V33_MV.load(Ordering::Relaxed);
    loop {
        if cur != 0 && mv >= cur {
            return;
        }
        match MIN_V33_MV.compare_exchange_weak(cur, mv, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(c) => cur = c,
        }
    }
}

pub fn min_v33_mv() -> u32 {
    MIN_V33_MV.load(core::sync::atomic::Ordering::Relaxed)
}

/// Текущее состояние детектора (для /status): (включён, порог, сброс включён, детект).
pub fn brownout_state() -> (bool, u8, bool, bool) {
    let regs = unsafe { &*RTC_CNTL::ptr() };
    let r = regs.brown_out().read();
    (
        r.ena().bit_is_set(),
        r.dbrown_out_thres().bits(),
        r.rst_ena().bit_is_set(),
        r.det().bit_is_set(),
    )
}
