//! «Хлебные крошки» в RTC slow memory.
//!
//! Зачем: зависание платы иногда не оставляет вообще ничего — ни строки паники
//! (текст не успел в NVS, потому что flash-операция сама и повесила ядро), ни
//! снимка hangdetect (ISR не отработал). RTC slow memory переживает
//! software_reset, RWDT и хардлок, пишется парой `s32c1i`-независимых
//! `memw`+`s32i` и не требует ни flash, ни сети.
//!
//! Раскладка (база 0x5000_0000, первые 512 Б):
//!   0x000..0x020 — снимок hangdetect (см. hangdetect.rs);
//!   0x040        — MAGIC метки;
//!   0x044        — тег (T_*);
//!   0x048        — значение;
//!   0x080        — MAGIC текста паники;
//!   0x084        — длина;
//!   0x088..0x150 — байты текста паники (UTF-8, без NUL).

const BASE: usize = 0x5000_0000;

const MARK_OFF: usize = 0x40;
const MARK_MAGIC: u32 = 0x4D41_524B; // "MARK"
const MARK_TAG: usize = MARK_OFF + 4;
const MARK_VAL: usize = MARK_OFF + 8;

const PANIC_OFF: usize = 0x80;
const PANIC_MAGIC: u32 = 0x5041_4E43; // "PANC"
const PANIC_LEN: usize = PANIC_OFF + 4;
const PANIC_DATA: usize = PANIC_OFF + 8;
const PANIC_MAX: usize = 200;

/// Счётчики в RTC (переживают brown-out/software reset): сколько раз плата
/// поднималась и сколько из этих подъёмов было после срабатывания BOD.
///
/// MAGIC обязателен: RTC slow memory после холодного старта (новая плата,
/// полная потеря питания) содержит мусор — без проверки первый бут печатал
/// `boot #2838895972 (brown-out сбросов: 2002480577)`.
const BOOTS_OFF: usize = 0x50;
const BROWNOUTS_OFF: usize = 0x54;
const COUNTERS_MAGIC_OFF: usize = 0x58;
const COUNTERS_MAGIC: u32 = 0x424F_4F54; // "BOOT"

/// Какие метки бывают. Значение — свободное поле.
pub const T_HTTP: u32 = 1;
pub const T_APPLY: u32 = 2;
pub const T_LOG: u32 = 3;

/// Значения для T_LOG.
pub const L_POST: u32 = 1;

/// Идентификаторы эндпоинтов для T_HTTP (в младшем байте значения).
pub const H_ROOT: u32 = 1;
pub const H_STATUS: u32 = 2;
pub const H_LOGS: u32 = 3;
pub const H_TEST: u32 = 4;
pub const H_ONOFF: u32 = 5;
pub const H_OTA: u32 = 6;
pub const H_REVERT: u32 = 7;
pub const H_POST: u32 = 8;
pub const H_OTHER: u32 = 9;

#[inline]
unsafe fn wr(off: usize, v: u32) {
    unsafe { core::ptr::write_volatile((BASE + off) as *mut u32, v) };
}

#[inline]
unsafe fn rd(off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((BASE + off) as *const u32) }
}

/// Записать метку «мы были здесь». Вызывать перед рискованным шагом:
/// последняя запись = последнее, что успел сделать код.
pub fn mark(tag: u32, value: u32) {
    unsafe {
        wr(MARK_TAG, tag);
        wr(MARK_VAL, value);
        wr(MARK_OFF, MARK_MAGIC); // magic последним: метка либо целая, либо старая
    }
}

/// Стереть метку (успешное завершение рискованного шага).
pub fn clear() {
    unsafe { wr(MARK_OFF, 0) };
}

/// Отметить бут; вернуть (всего бустов, из них после brown-out).
///
/// Счётчики живут в RTC slow memory, поэтому не сбрасываются ни BOD-сбросом,
/// ни software_reset — по ним видно, помогла ли аппаратная правка питания:
/// если `brownouts` перестал расти, починка сработала.
pub fn boot_tick(brownout: bool) -> (u32, u32) {
    unsafe {
        if rd(COUNTERS_MAGIC_OFF) != COUNTERS_MAGIC {
            wr(BOOTS_OFF, 0);
            wr(BROWNOUTS_OFF, 0);
            wr(COUNTERS_MAGIC_OFF, COUNTERS_MAGIC);
        }
        let boots = rd(BOOTS_OFF).wrapping_add(1);
        wr(BOOTS_OFF, boots);
        let mut brownouts = rd(BROWNOUTS_OFF);
        if brownout {
            brownouts = brownouts.wrapping_add(1);
            wr(BROWNOUTS_OFF, brownouts);
        }
        (boots, brownouts)
    }
}

/// Текущие счётчики без инкремента (для /status).
pub fn counters() -> (u32, u32) {
    unsafe {
        if rd(COUNTERS_MAGIC_OFF) != COUNTERS_MAGIC {
            (0, 0)
        } else {
            (rd(BOOTS_OFF), rd(BROWNOUTS_OFF))
        }
    }
}

/// Сохранить текст паники в RTC (до попытки записи в NVS).
pub fn save_panic(msg: &str) {
    let bytes = msg.as_bytes();
    let n = bytes.len().min(PANIC_MAX);
    unsafe {
        for (i, b) in bytes[..n].iter().enumerate() {
            core::ptr::write_volatile((BASE + PANIC_DATA + i) as *mut u8, *b);
        }
        wr(PANIC_LEN, n as u32);
        wr(PANIC_OFF, PANIC_MAGIC);
    }
}

fn tag_name(tag: u32) -> &'static str {
    match tag {
        T_HTTP => "http",
        T_APPLY => "display-apply",
        T_LOG => "log-post",
        _ => "?",
    }
}

fn http_name(v: u32) -> &'static str {
    match v & 0xFF {
        H_ROOT => "/",
        H_STATUS => "/status",
        H_LOGS => "/logs",
        H_TEST => "/display/test",
        H_ONOFF => "/display/off|on",
        H_OTA => "/ota",
        H_REVERT => "/revert",
        H_POST => "POST /api/*",
        _ => "other",
    }
}

/// Вызывать в начале бута после logger::init(): выложить и очистить крошки.
pub fn report() {
    unsafe {
        if rd(MARK_OFF) == MARK_MAGIC {
            let tag = rd(MARK_TAG);
            let val = rd(MARK_VAL);
            if tag == T_HTTP {
                log::error!(
                    "ПОСЛЕДНИЙ HTTP-ЗАПРОС: {} (id={}, mode={})",
                    http_name(val),
                    val & 0xFF,
                    (val >> 8) & 0xFF
                );
            } else {
                log::error!(
                    "ПОСЛЕДНЯЯ МЕТКА: {} (tag={}, value=0x{:08x})",
                    tag_name(tag),
                    tag,
                    val
                );
            }
            wr(MARK_OFF, 0);
            wr(MARK_TAG, 0);
            wr(MARK_VAL, 0);
        }
        if rd(PANIC_OFF) == PANIC_MAGIC {
            let n = (rd(PANIC_LEN) as usize).min(PANIC_MAX);
            let mut s: heapless::String<PANIC_MAX> = heapless::String::new();
            for i in 0..n {
                let b = core::ptr::read_volatile((BASE + PANIC_DATA + i) as *const u8);
                if s.push(b as char).is_err() {
                    break;
                }
            }
            log::error!("ПАНИКА (текст из RTC): {}", s.as_str());
            wr(PANIC_OFF, 0);
            wr(PANIC_LEN, 0);
        }
    }
}
