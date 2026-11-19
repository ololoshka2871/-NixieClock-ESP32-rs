//! Часовой пояс: смещение от UTC в минутах.
//!
//! Источник истины — NVS (`settings/tz_min`); в рантайме держим зеркало в
//! атомике, потому что его читают индикация (60 раз в секунду) и логгер, а
//! обращений к flash там быть не должно. Часы внутри (DS1307, epoch, SNTP)
//! всегда живут в UTC — смещение применяется только при показе.

use core::sync::atomic::{AtomicI16, Ordering};

use clock_core::time::{self, Civil};

static TZ_MIN: AtomicI16 = AtomicI16::new(time::TZ_DEFAULT_MIN);

/// Смещение из NVS (или значение по умолчанию, если не сохранено).
pub fn load() -> i16 {
    let v = crate::settings::load_tz().unwrap_or(time::TZ_DEFAULT_MIN);
    TZ_MIN.store(v, Ordering::Relaxed);
    log::info!("tz: пояс {:+.2} ч от UTC", v as f32 / 60.0);
    v
}

pub fn get() -> i16 {
    TZ_MIN.load(Ordering::Relaxed)
}

/// Применить новое смещение (после успешной записи в NVS).
pub fn set(minutes: i16) {
    TZ_MIN.store(minutes, Ordering::Relaxed);
}

/// Локальное гражданское время для индикации/UI/логов.
pub fn civil(epoch: u64) -> Civil {
    time::local_civil(epoch, get())
}

pub fn string(epoch: u64) -> alloc::string::String {
    alloc::format!("{}", civil(epoch))
}
