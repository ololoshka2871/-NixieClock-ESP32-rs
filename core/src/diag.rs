//! Диагностика Wi-Fi-подключения: лестница ретраев STA и счётчики для WebUI.
//!
//! Правило (зафиксировано в issue #6): **креды никогда не удаляем** — точка
//! доступа может лежать, перестраиваться или менять канал. Вместо лимита
//! попыток растём в паузе между ними: радио не долбит эфир, но сеть
//! восстанавливается сама, как только AP снова станет доступной.
//!
//! Лестница (попытка → пауза до следующей):
//!   1..=10   5 с   — быстрая реакция на кратковременный сбой
//!   11..=20  15 с  — AP скорее всего рестартует/перестраивается
//!   21..=40  60 с  — надолго: раз в минуту
//!   41..=    300 с — фоновый режим, раз в 5 минут
//!
//! Счётчики живут в атомиксах: пишет их `connection`-таск (main.rs), читает
//! HTTP-ответ (`/api/status`) — блокировки нет, значения «на момент чтения».

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Первая пауза лестницы (секунды).
pub const RETRY_FAST_SECS: u64 = 5;
/// Пауза после `RETRY_FAST_ATTEMPTS` неудач.
pub const RETRY_SLOW_SECS: u64 = 15;
/// Пауза «надолго» (после 20 неудач).
pub const RETRY_LONG_SECS: u64 = 60;
/// Фоновая пауза (после 40 неудач).
pub const RETRY_IDLE_SECS: u64 = 300;
/// Сколько попыток держим быструю паузу.
pub const RETRY_FAST_ATTEMPTS: u32 = 10;
/// Сколько попыток держим `RETRY_SLOW_SECS`.
pub const RETRY_SLOW_ATTEMPTS: u32 = 20;
/// Сколько попыток держим `RETRY_LONG_SECS`.
pub const RETRY_LONG_ATTEMPTS: u32 = 40;

/// Пауза (секунды) перед попыткой `attempt` (1-based). Монотонно не убывает.
pub fn backoff_secs(attempt: u32) -> u64 {
    if attempt == 0 {
        return RETRY_FAST_SECS;
    }
    if attempt <= RETRY_FAST_ATTEMPTS {
        RETRY_FAST_SECS
    } else if attempt <= RETRY_SLOW_ATTEMPTS {
        RETRY_SLOW_SECS
    } else if attempt <= RETRY_LONG_ATTEMPTS {
        RETRY_LONG_SECS
    } else {
        RETRY_IDLE_SECS
    }
}

/// Сколько неудачных подключений подряд = порог «пора поднимать SoftAP»
/// (issue #6, п.3: после N неудачных попыток переходим в режим AP).
pub const AP_FALLBACK_AFTER_ATTEMPTS: u32 = RETRY_SLOW_ATTEMPTS;

static CONNECT_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static CONNECT_FAILS: AtomicU32 = AtomicU32::new(0);
static LAST_BACKOFF_SECS: AtomicU32 = AtomicU32::new(0);
/// Целевой SSID попадался в последнем after-failure скане (виден в эфире).
static TARGET_SEEN: AtomicBool = AtomicBool::new(false);
/// Скан после фейда вообще что-то нашёл (0 AP = радио/эфир молчат).
static SCAN_SEEN_ANY: AtomicBool = AtomicBool::new(false);

/// Зарегистрировать попытку подключения; вернуть паузу (сек) перед ней.
pub fn note_attempt() -> u64 {
    let n = CONNECT_ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
    let pause = backoff_secs(n);
    LAST_BACKOFF_SECS.store(pause as u32, Ordering::Relaxed);
    pause
}

pub fn note_connect_ok() {
    CONNECT_FAILS.store(0, Ordering::Relaxed);
    LAST_BACKOFF_SECS.store(0, Ordering::Relaxed);
    TARGET_SEEN.store(false, Ordering::Relaxed);
    SCAN_SEEN_ANY.store(false, Ordering::Relaxed);
}

pub fn note_connect_fail() {
    CONNECT_FAILS.fetch_add(1, Ordering::Relaxed);
}

/// Результат after-failure скана: `seen_any` — найдена хотя бы одна AP,
/// `target_seen` — среди них есть целевая (SSID совпал).
pub fn note_scan(seen_any: bool, target_seen: bool) {
    SCAN_SEEN_ANY.store(seen_any, Ordering::Relaxed);
    TARGET_SEEN.store(target_seen, Ordering::Relaxed);
}

/// (попыток всего, неудач подряд, пауза сейчас, target seen, скан что-то видел)
pub fn snapshot() -> (u32, u32, u64, bool, bool) {
    (
        CONNECT_ATTEMPTS.load(Ordering::Relaxed),
        CONNECT_FAILS.load(Ordering::Relaxed),
        LAST_BACKOFF_SECS.load(Ordering::Relaxed) as u64,
        TARGET_SEEN.load(Ordering::Relaxed),
        SCAN_SEEN_ANY.load(Ordering::Relaxed),
    )
}

/// Пора переходить в SoftAP: N неудач подряд при живых кредах.
pub fn ap_fallback_due() -> bool {
    CONNECT_FAILS.load(Ordering::Relaxed) >= AP_FALLBACK_AFTER_ATTEMPTS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_matches_spec() {
        assert_eq!(backoff_secs(1), 5);
        assert_eq!(backoff_secs(10), 5);
        assert_eq!(backoff_secs(11), 15);
        assert_eq!(backoff_secs(20), 15);
        assert_eq!(backoff_secs(21), 60);
        assert_eq!(backoff_secs(40), 60);
        assert_eq!(backoff_secs(41), 300);
        assert_eq!(backoff_secs(u32::MAX), 300);
    }

    #[test]
    fn ladder_never_shrinks() {
        let mut prev = 0;
        for a in 0..60 {
            let d = backoff_secs(a);
            assert!(d >= prev, "пауза уменьшилась на попытке {a}");
            prev = d;
        }
    }

    #[test]
    fn counters_track_fail_streak() {
        note_connect_ok();
        assert_eq!(snapshot().1, 0);
        for _ in 0..3 {
            note_attempt();
            note_connect_fail();
        }
        let (attempts, fails, pause, _, _) = snapshot();
        assert_eq!(fails, 3);
        assert!(attempts >= 3);
        assert_eq!(pause, 5);
        note_connect_ok();
        assert_eq!(snapshot().1, 0);
        assert_eq!(snapshot().2, 0);
    }

    #[test]
    fn ap_fallback_threshold_is_20() {
        assert_eq!(AP_FALLBACK_AFTER_ATTEMPTS, 20);
    }
}
