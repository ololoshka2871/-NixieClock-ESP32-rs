//! «Чистая» часть приложения: логика без железа и embassy — компилируется и
//! тестируется на хосте (`cargo test -p clock-core`). Сюда переносится вся
//! бизнес-логика; `firmware/` оставляет только инфраструктуру и задачи.
#![cfg_attr(not(test), no_std)]

/// Программный watchdog: рабочая задача периодически его «кормит», отдельный
/// монитор проверяет дедлайны. Если задача зависла — копятся пропуски
/// (в приложении их видно в логах; время передаётся аргументом — логика
/// тестируется без железа).
pub struct Watchdog {
    period_ms: u64,
    deadline_ms: u64,
    misses: u32,
}

impl Watchdog {
    pub const fn new(period_ms: u64, now_ms: u64) -> Self {
        Self {
            period_ms,
            deadline_ms: now_ms.saturating_add(period_ms),
            misses: 0,
        }
    }

    /// Задача жива: снимаем дедлайн, обнуляем серию пропусков.
    pub fn feed(&mut self, now_ms: u64) {
        self.deadline_ms = now_ms.saturating_add(self.period_ms);
        self.misses = 0;
    }

    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms >= self.deadline_ms
    }

    /// Проверка монитора: пропуск -> фиксируем и перевзводим watchdog
    /// на следующий период (как аппаратный WDT).
    pub fn check(&mut self, now_ms: u64) -> bool {
        if self.expired(now_ms) {
            self.misses = self.misses.saturating_add(1);
            self.deadline_ms = now_ms.saturating_add(self.period_ms);
            true
        } else {
            false
        }
    }

    pub fn misses(&self) -> u32 {
        self.misses
    }
}

pub mod page;

/// Снимок состояния для JSON `/api/status` (SPA-подложка). Формируется в
/// firmware, сериализуется здесь — формат тестируется на хосте.
pub struct StatusSnapshot<'a> {
    pub uptime_secs: u64,
    pub fw: &'a str,
    pub version: &'a str,
    pub ip: Option<&'a str>,
    pub ota_state: &'a str,
    pub pending: bool,
    pub confirmed: bool,
    pub latched: bool,
    pub busy: bool,
}

impl StatusSnapshot<'_> {
    fn write_json(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            out,
            "{{\"uptime\":{},\"fw\":\"{}\",\"version\":\"{}\",\"ip\":",
            self.uptime_secs, self.fw, self.version
        )?;
        match self.ip {
            Some(ip) => write!(out, "\"{}\"", ip)?,
            None => out.write_str("null")?,
        }
        write!(
            out,
            ",\"ota_state\":\"{}\",\"pending\":{},\"confirmed\":{},\"latched\":{},\"busy\":{}}}",
            self.ota_state,
            self.pending,
            self.confirmed,
            self.latched,
            self.busy
        )
    }
}

impl core::fmt::Display for StatusSnapshot<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.write_json(f)
    }
}

/// Пример «чистой» выходной логики: циклический бит из маски моргания.
pub fn blink_pattern(step: u64, pattern: &[bool]) -> bool {
    if pattern.is_empty() {
        return false;
    }
    pattern[(step % pattern.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_not_expired_before_deadline() {
        let wd = Watchdog::new(10_000, 0);
        assert!(!wd.expired(9_999));
        assert!(wd.expired(10_000));
    }

    #[test]
    fn feed_resets_deadline_and_misses() {
        let mut wd = Watchdog::new(1_000, 0);
        assert!(wd.check(1_500));
        assert_eq!(wd.misses(), 1);
        wd.feed(1_500);
        assert_eq!(wd.misses(), 0);
        assert!(!wd.check(2_400));
    }

    #[test]
    fn stalled_task_accumulates_misses() {
        let mut wd = Watchdog::new(1_000, 0);
        let missed = (1..=4).filter(|i| wd.check(i * 1_000)).count();
        assert_eq!(missed, 4);
        assert_eq!(wd.misses(), 4);
    }

    #[test]
    fn blink_pattern_cycles() {
        let p = [true, false, false];
        let mut seq = [false; 7];
        for s in 0..7u64 {
            seq[s as usize] = blink_pattern(s, &p);
        }
        assert_eq!(seq, [true, false, false, true, false, false, true]);
        assert!(!blink_pattern(5, &[]));
    }

    #[test]
    fn watchdog_saturates_near_u64_max() {
        let wd = Watchdog::new(1_000, u64::MAX - 10);
        assert!(!wd.expired(u64::MAX - 5));
        assert!(wd.expired(u64::MAX));
    }

    #[test]
    fn status_json_shape() {
        use super::StatusSnapshot;
        let s = StatusSnapshot {
            uptime_secs: 123,
            fw: "nixieclock-firmware",
            version: "0.1.0",
            ip: Some("192.168.0.123"),
            ota_state: "Valid",
            pending: false,
            confirmed: true,
            latched: false,
            busy: false,
        };
        let json = alloc::format!("{}", s);
        assert_eq!(
            json,
            "{\"uptime\":123,\"fw\":\"nixieclock-firmware\",\"version\":\"0.1.0\",\"ip\":\"192.168.0.123\",\"ota_state\":\"Valid\",\"pending\":false,\"confirmed\":true,\"latched\":false,\"busy\":false}"
        );
        let s2 = StatusSnapshot { ip: None, ota_state: "New", pending: true, ..s };
        let json2 = alloc::format!("{}", s2);
        assert!(json2.contains("\"ip\":null"));
        assert!(json2.contains("\"pending\":true"));
    }
}
