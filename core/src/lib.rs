//! «Чистая» часть приложения: логика без железа и embassy — компилируется и
//! тестируется на хосте (`cargo test -p clock-core`). Сюда переносится вся
//! бизнес-логика; `firmware/` оставляет только инфраструктуру и задачи.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

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

pub mod display;
pub mod led;
pub mod icmp;
pub mod page;
pub mod sensors;
pub mod settings;
pub mod time;
pub mod urlenc;

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
    pub wifi_ssid: Option<&'a str>,
    pub wifi_saved: bool,
    /// Форматированное локальное время (UTC + смещение пояса; None, если нет).
    pub time: Option<&'a str>,
    /// Смещение пояса от UTC в минутах (показ; сами часы всегда UTC).
    pub tz_min: i16,
    pub time_epoch: u64,
    /// Источник текущего времени: build|rtc|ntp|http.
    pub time_source: &'a str,
    /// Хоть раз подтверждено сетью/чипом (не просто время сборки).
    pub time_synced: bool,
    /// Сохранённый пользователем NTP-сервер (None — встроенные пулы).
    pub ntp_server: Option<&'a str>,
    /// Default gateway из DHCP. None без учёта dhcp_done двусмысленно:
    /// либо DHCP ещё не завершился, либо роутер не присылает option 3.
    pub gateway: Option<&'a str>,
    /// DHCP-гандшейк завершён (стек имеет конфигурацию).
    pub dhcp_done: bool,
    /// Первый DNS-сервер из DHCP (тот же смысл None, что у gateway).
    pub dns: Option<&'a str>,
    /// M5-датчики (None, пока нет первого успешного замера).
    /// Температура DS18x20, миллиградусы Цельсия.
    pub temp_mc: Option<i32>,
    /// CO2 с MH-Z19, ppm.
    pub co2_ppm: Option<u16>,
    /// Температура самого MH-Z19, миллиградусы.
    pub co2_temp_mc: Option<i32>,
    /// Напряжение шины 3.3 В (АЦП GPIO39 × делитель 5.7), милливольты.
    pub v33_mv: Option<u32>,
    /// Напряжение шины 5 В (АЦП GPIO35 × делитель 10.1), милливольты.
    pub v5_mv: Option<u32>,
    /// Напряжение шины 12 В (АЦП GPIO34 × делитель 25), милливольты.
    pub v12_mv: Option<u32>,
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
        match self.wifi_ssid {
            Some(ssid) => write!(out, ",\"wifi_ssid\":\"{}\"", ssid)?,
            None => out.write_str(",\"wifi_ssid\":null")?,
        }
        write!(
            out,
            ",\"wifi_saved\":{},\"time\":",
            self.wifi_saved,
        )?;
        match self.time {
            Some(t) => write!(out, "\"{}\"", t)?,
            None => out.write_str("null")?,
        }
        write!(
            out,
            ",\"time_epoch\":{},\"tz_min\":{},\"time_source\":\"{}\",\"time_synced\":{},\"ota_state\":\"{}\",\"pending\":{},\"confirmed\":{},\"latched\":{},\"busy\":{}",
            self.time_epoch,
            self.tz_min,
            self.time_source,
            self.time_synced,
            self.ota_state,
            self.pending,
            self.confirmed,
            self.latched,
            self.busy
        )?;
        match self.ntp_server {
            Some(s) => write!(out, ",\"ntp_server\":\"{}\"", s)?,
            None => out.write_str(",\"ntp_server\":null")?,
        }
        match self.gateway {
            Some(g) => write!(out, ",\"gateway\":\"{}\"", g)?,
            None => out.write_str(",\"gateway\":null")?,
        }
        write!(out, ",\"dhcp_done\":{}", self.dhcp_done)?;
        match self.dns {
            Some(d) => write!(out, ",\"dns\":\"{}\"", d)?,
            None => out.write_str(",\"dns\":null")?,
        }
        match self.temp_mc {
            Some(v) => write!(out, ",\"temp_mc\":{}", v)?,
            None => out.write_str(",\"temp_mc\":null")?,
        }
        match self.co2_ppm {
            Some(v) => write!(out, ",\"co2_ppm\":{}", v)?,
            None => out.write_str(",\"co2_ppm\":null")?,
        }
        match self.co2_temp_mc {
            Some(v) => write!(out, ",\"co2_temp_mc\":{}", v)?,
            None => out.write_str(",\"co2_temp_mc\":null")?,
        }
        write!(
            out,
            ",\"v33_mv\":{},\"v5_mv\":{},\"v12_mv\":{}",
            opt_u32(self.v33_mv),
            opt_u32(self.v5_mv),
            opt_u32(self.v12_mv),
        )?;
        out.write_str("}")
    }
}

/// None -> "null", Some -> число (для компактной встав в JSON).
struct OptU32(Option<u32>);
impl core::fmt::Display for OptU32 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{}", v),
            None => f.write_str("null"),
        }
    }
}
fn opt_u32(v: Option<u32>) -> OptU32 {
    OptU32(v)
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
            wifi_ssid: Some("Mokrot-5G"),
            wifi_saved: true,
            time: Some("2026-11-13 12:00:00"),
            time_epoch: 1_763_035_200,
            tz_min: 180,
            time_source: "ntp",
            time_synced: true,
            ntp_server: Some("192.168.0.28"),
            gateway: Some("192.168.0.1"),
            dhcp_done: true,
            dns: Some("192.168.0.1"),
            temp_mc: Some(25_000),
            co2_ppm: Some(500),
            co2_temp_mc: Some(25_000),
            v33_mv: Some(3300),
            v5_mv: Some(5050),
            v12_mv: Some(12100),
        };
        let json = alloc::format!("{}", s);
        assert_eq!(
            json,
            "{\"uptime\":123,\"fw\":\"nixieclock-firmware\",\"version\":\"0.1.0\",\"ip\":\"192.168.0.123\",\"wifi_ssid\":\"Mokrot-5G\",\"wifi_saved\":true,\"time\":\"2026-11-13 12:00:00\",\"time_epoch\":1763035200,\"tz_min\":180,\"time_source\":\"ntp\",\"time_synced\":true,\"ota_state\":\"Valid\",\"pending\":false,\"confirmed\":true,\"latched\":false,\"busy\":false,\"ntp_server\":\"192.168.0.28\",\"gateway\":\"192.168.0.1\",\"dhcp_done\":true,\"dns\":\"192.168.0.1\",\"temp_mc\":25000,\"co2_ppm\":500,\"co2_temp_mc\":25000,\"v33_mv\":3300,\"v5_mv\":5050,\"v12_mv\":12100}"
        );
        let s2 = StatusSnapshot {
            ip: None,
            time: None,
            ota_state: "New",
            pending: true,
            temp_mc: None,
            co2_ppm: None,
            co2_temp_mc: None,
            v33_mv: None,
            v5_mv: None,
            v12_mv: None,
            ..s
        };
        let json2 = alloc::format!("{}", s2);
        assert!(json2.contains("\"ip\":null"));
        assert!(json2.contains("\"time\":null"));
        assert!(json2.contains("\"pending\":true"));
        assert!(json2.contains("\"temp_mc\":null"));
        assert!(json2.contains("\"co2_ppm\":null"));
        assert!(json2.contains("\"v12_mv\":null"));
    }
}
