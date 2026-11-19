//! Часовая подсистема (M4): гражданский календарь, BCD-регистры DS1307,
//! кодек SNTP (RFC 4330) и модель часов (epoch + монотонная база).
//! Всё чисто арифметическое — тестируется на хосте без embassy/esp.
//!
//! Семантика, снятая с оригинального IDF-проекта (факты железа, не код):
//! - часовой чип на плате — DS1307, I2C0: SDA=GPIO21, SCL=GPIO22, адрес 0x68,
//!   режим 24 ч; SQW 1 Гц на GPIO23 (open-drain, внешняя подтяжка) оставлен
//!   для будущих тиков индикации — сейчас коррекция идёт опросом раз в 60 с;
//! - DS1307 — хозяин времени в offline; сетевая подтяжка (у нас — SNTP плюс
//!   HTTP-установка, как V0 в оригинале) только корректирует;
//! - если чип остановлен или его время моложе времени сборки прошивки —
//!   ставим время сборки (и в модель, и в чип) — оригинальная защита от
//!   «севшей» микросхемы.

/// Отсчёт NTP: секунды от 1900-01-01 до 1970-01-01.
pub const NTP_UNIX_DELTA: u64 = 2_208_988_800;
/// Минимальная эпоха, кодируемая BCD-годом DS1307 (2000-01-01T00:00:00Z).
pub const RTC_MIN_EPOCH: u64 = 946_684_800;
/// Максимальная эпоха чипа (2099-12-31T23:59:59Z, год без века).
pub const RTC_MAX_EPOCH: u64 = 4_102_444_799;
/// Максимально допустимое расхождение ответа SNTP с локальными часами:
/// больше 10 лет — считаем, что часы сбились и ответ не принимается.
pub const MAX_SNTP_SKEW: i64 = 315_360_000;

/// Время сборки прошивки — fallback для остановленной/пустой DS1307
/// (пишет core/build.rs как rustc-env BUILD_EPOCH).
const fn parse_u64(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut v: u64 = 0;
    let mut i = 0;
    while i < b.len() {
        v = v * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    v
}
pub const BUILD_EPOCH: u64 = match option_env!("BUILD_EPOCH") {
    Some(s) => parse_u64(s),
    None => 1_763_000_000, // 2025-11-13, на случай сборки без build.rs
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeError {
    /// DS1307: бит CH=1 — генератор остановлен (часы стоят).
    Stopped,
    /// Чип в 12-часовом режиме (у нас контракт: всегда 24 ч).
    Bad12Hour,
    /// Регистр не валиден как BCD.
    BadBcd,
    /// Невалидная дата (например 31 февраля).
    BadDate,
    /// Эпоха вне диапазона, кодируемого чипом (2000..2099).
    OutOfRange,
    /// Пакет SNTP повреждён/не ответ.
    MalformedPacket,
    /// SNTP stratum=0 (Kiss-of-Death).
    KissOfDeath,
    /// Ответ SNTP расходится с локальными часами больше MAX_SNTP_SKEW.
    TooFarOff,
}

/// Гражданский календарь + время суток (UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    pub month: u8, // 1..12
    pub day: u8, // 1..31
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
}

impl core::fmt::Display for Civil {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.min, self.sec
        )
    }
}

/// Алгоритм Говарда Хиннанта: гражданская дата -> дни от 1970-01-01.
pub const fn days_from_civil(y: i64, m: u8, d: u8) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp as i64 + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Обратный ход Хиннанта: дни -> (год, месяц, день).
pub fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8; // [1, 12]
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

pub fn epoch_to_civil(epoch: u64) -> Civil {
    let days = (epoch / 86400) as i64;
    let rem = epoch % 86400;
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: (rem / 3600) as u8,
        min: (rem % 3600 / 60) as u8,
        sec: (rem % 60) as u8,
    }
}

pub fn civil_to_epoch(c: Civil) -> Option<u64> {
    if c.month < 1 || c.month > 12 || c.day < 1 || c.hour > 23 || c.min > 59 || c.sec > 59 {
        return None;
    }
    if c.year < 1970 || c.day > days_in_month(c.year, c.month) {
        return None;
    }
    let days = days_from_civil(c.year as i64, c.month, c.day);
    let secs = days * 86400 + c.hour as i64 * 3600 + c.min as i64 * 60 + c.sec as i64;
    if secs < 0 {
        return None;
    }
    Some(secs as u64)
}

pub const fn is_leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

pub fn days_in_month(y: i32, m: u8) -> u8 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// День недели DS1307: 1=воскресенье .. 7=суббота.
pub fn weekday(epoch: u64) -> u8 {
    let c = epoch_to_civil(epoch);
    let z = days_from_civil(c.year as i64, c.month, c.day);
    let sun0 = ((z + 4) % 7 + 7) % 7; // 1970-01-01 (Thu) -> sun0=4
    (sun0 + 1) as u8
}

const fn is_bcd(v: u8) -> bool {
    (v & 0x0F) <= 9 && ((v >> 4) & 0x07) <= 9
}
const fn unbcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}
const fn to_bcd(v: u8) -> u8 {
    (v / 10) << 4 | v % 10
}

/// Регистры DS1307 0x00..0x06: сек(0x80=CH) мин часы(0x40=12ч) дн.нед дата
/// месяц(0x20=век DS3231) год.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ds1307Regs(pub [u8; 7]);

pub fn regs_to_epoch(regs: &Ds1307Regs) -> Result<u64, TimeError> {
    let r = &regs.0;
    if r[0] & 0x80 != 0 {
        return Err(TimeError::Stopped); // осциллятор остановлен, часы стоят
    }
    let s = r[0] & 0x7F; // секунды — 7 бит (0..59), CH — только бит 7
    let m = r[1] & 0x7F; // минуты — тоже 7 бит (маска 0x3F теряла десятки >=40)
    let h = r[2];
    if h & 0x40 != 0 {
        return Err(TimeError::Bad12Hour);
    }
    let h = h & 0x3F;
    if !is_bcd(s) || !is_bcd(m) || !is_bcd(h) {
        return Err(TimeError::BadBcd);
    }
    let (sec, min, hour) = (unbcd(s), unbcd(m), unbcd(h));
    if sec > 59 || min > 59 || hour > 23 {
        return Err(TimeError::BadBcd);
    }
    let day = r[4] & 0x3F;
    if !is_bcd(day) || {
        let d = unbcd(day);
        d < 1 || d > 31
    } {
        return Err(TimeError::BadDate);
    }
    let mo = r[5] & 0x1F; // век (бит 5) DS3231 не кодируем
    if !is_bcd(mo) || {
        let m = unbcd(mo);
        m < 1 || m > 12
    } {
        return Err(TimeError::BadDate);
    }
    if !is_bcd(r[6]) {
        return Err(TimeError::BadBcd);
    }
    let year = 2000 + unbcd(r[6]) as i32;
    let day = unbcd(day);
    let month = unbcd(mo);
    if day > days_in_month(year, month) {
        return Err(TimeError::BadDate);
    }
    civil_to_epoch(Civil {
        year,
        month,
        day,
        hour,
        min,
        sec,
    })
    .ok_or(TimeError::OutOfRange)
}

pub fn epoch_to_regs(epoch: u64) -> Result<Ds1307Regs, TimeError> {
    if epoch < RTC_MIN_EPOCH || epoch > RTC_MAX_EPOCH {
        return Err(TimeError::OutOfRange);
    }
    let c = epoch_to_civil(epoch);
    Ok(Ds1307Regs([
        to_bcd(c.sec),               // CH=0 -> генератор запущен (аналог ds1307_start)
        to_bcd(c.min),
        to_bcd(c.hour),              // 24-часовой режим (бит 12/24 = 0)
        weekday(epoch),              // 1..7
        to_bcd(c.day),
        to_bcd(c.month),
        to_bcd((c.year - 2000) as u8),
    ]))
}

// ---------------------------------------------------------------- SNTP ----

/// Длинa пакета NTP/SNTP по RFC 4330.
pub const SNTP_PACKET_LEN: usize = 48;

/// Запрос клиента: LI=0, VN=4, Mode=3, остальное нули.
pub fn sntp_request() -> [u8; SNTP_PACKET_LEN] {
    let mut p = [0u8; SNTP_PACKET_LEN];
    p[0] = 0x23;
    p
}

/// Разбор ответа сервера: проверяем заголовок и Receive Timestamp (смещение
/// 40, секунды NTP от 1900 как u32). 32-битные часы NTP-make переворачиваются
/// в 2036 — эпоху выбираем по ближайшей к `ref_epoch` candidate.
pub fn sntp_parse(buf: &[u8], ref_epoch: u64) -> Result<u64, TimeError> {
    if buf.len() < SNTP_PACKET_LEN {
        return Err(TimeError::MalformedPacket);
    }
    let flags = buf[0];
    if (flags >> 6) & 3 == 3 {
        return Err(TimeError::MalformedPacket); // LI=11 — дата недействительна
    }
    if flags & 7 != 4 {
        return Err(TimeError::MalformedPacket); // не ответ сервера
    }
    let stratum = buf[1];
    if stratum == 0 {
        return Err(TimeError::KissOfDeath);
    }
    if stratum > 15 {
        return Err(TimeError::MalformedPacket);
    }
    let ntp32 = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]);
    // Кандидаты эпох 0..2: ntp32 + era*2^32 - дельта. Выбираем ближайший
    // к локальным часам (референс не даёт уйти на +/- 68 лет).
    let mut best: Option<u64> = None;
    let mut best_dist = u64::MAX;
    for era in 0u64..3 {
        let secs = ntp32 as u64 + era * (1u64 << 32);
        if secs < NTP_UNIX_DELTA {
            continue;
        }
        let cand = secs - NTP_UNIX_DELTA;
        let dist = (cand as i128 - ref_epoch as i128).unsigned_abs() as u64;
        if dist < best_dist {
            best_dist = dist;
            best = Some(cand);
        }
    }
    let epoch = best.ok_or(TimeError::MalformedPacket)?;
    if best_dist > MAX_SNTP_SKEW as u64 {
        return Err(TimeError::TooFarOff);
    }
    Ok(epoch)
}

/// Разбор тела `{"epoch": <сек>}` или `{"epoch_ms": <мс>}` для ручного
/// установки времени (HTTP-путь, аналог V0 в оригинале). Диапазон — тот же,
/// что кодирует чип.
pub fn parse_time_json(body: &str) -> Result<u64, &'static str> {
    let digits_after = |key: &str| -> Option<u64> {
        let at = body.find(key)?;
        let rest = body[at + key.len()..].trim_start().strip_prefix(':')?.trim_start();
        let mut v: u64 = 0;
        let mut any = false;
        for c in rest.bytes() {
            if c.is_ascii_digit() {
                v = v.checked_mul(10)?.checked_add((c - b'0') as u64)?;
                any = true;
            } else {
                break;
            }
        }
        if any { Some(v) } else { None }
    };
    let epoch = if let Some(ms) = digits_after("\"epoch_ms\"") {
        ms / 1000
    } else if let Some(s) = digits_after("\"epoch\"") {
        s
    } else {
        return Err("нет поля epoch (сек) или epoch_ms (мс)");
    };
    if epoch < RTC_MIN_EPOCH || epoch > RTC_MAX_EPOCH {
        return Err("время вне диапазона 2000..2099");
    }
    Ok(epoch)
}

// ------------------------------------------------------------- Clock ------

/// Откуда взялся текущий epoch модели.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSource {
    /// fallback времени сборки (часы ещё не синхронизированы)
    Build,
    /// прочитано из DS1307 (holdover/коррекция дрейфа)
    Rtc,
    /// получено по SNTP
    Ntp,
    /// установлено вручную через HTTP
    Http,
}

impl TimeSource {
    pub const fn as_str(&self) -> &'static str {
        match self {
            TimeSource::Build => "build",
            TimeSource::Rtc => "rtc",
            TimeSource::Ntp => "ntp",
            TimeSource::Http => "http",
        }
    }
}

/// Модельwall-clock без абсолютного времени у embassy-time: храним пару
/// (epoch_base, монотонные секунды на момент установки) и считаем
/// now = epoch_base + (mono - mono_base). Синхронизация — переустановка пары.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    epoch_base: u64,
    mono_base_secs: u64,
    source: TimeSource,
    synced: bool, // хоть раз получили сетевое/аппаратно-подтверждённое время
}

impl Clock {
    pub const fn new(build_epoch: u64, mono_now_secs: u64) -> Self {
        Self {
            epoch_base: build_epoch,
            mono_base_secs: mono_now_secs,
            source: TimeSource::Build,
            synced: false,
        }
    }

    pub fn now_secs(&self, mono_now_secs: u64) -> u64 {
        let delta = mono_now_secs.saturating_sub(self.mono_base_secs);
        self.epoch_base.saturating_add(delta)
    }

    /// Полная переустановка (hard-set) — при большой расходимости или первом
    /// подтверждении.
    pub fn set(&mut self, epoch: u64, mono_now_secs: u64, source: TimeSource) {
        self.epoch_base = epoch;
        self.mono_base_secs = mono_now_secs;
        self.source = source;
        self.synced = true;
    }

    pub fn source(&self) -> TimeSource {
        self.source
    }

    pub fn synced(&self) -> bool {
        self.synced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates_match_epoch() {
        assert_eq!(
            civil_to_epoch(Civil {
                year: 2000,
                month: 1,
                day: 1,
                hour: 0,
                min: 0,
                sec: 0
            }),
            Some(RTC_MIN_EPOCH)
        );
        // 2024-02-29 (высокосный)
        assert_eq!(
            civil_to_epoch(Civil {
                year: 2024,
                month: 2,
                day: 29,
                hour: 0,
                min: 0,
                sec: 0
            }),
            Some(1_709_164_800)
        );
        // конец диапазона чипа
        assert_eq!(
            civil_to_epoch(Civil {
                year: 2099,
                month: 12,
                day: 31,
                hour: 23,
                min: 59,
                sec: 59
            }),
            Some(RTC_MAX_EPOCH)
        );
        assert_eq!(epoch_to_civil(RTC_MIN_EPOCH).year, 2000);
        assert_eq!(epoch_to_civil(RTC_MAX_EPOCH).month, 12);
    }

    #[test]
    fn civil_roundtrip_full_months() {
        for y in 1970..=2100i32 {
            for m in 1..=12u8 {
                for d in [1u8, 15, days_in_month(y, m)] {
                    let c = Civil {
                        year: y,
                        month: m,
                        day: d,
                        hour: 23,
                        min: 59,
                        sec: 58,
                    };
                    let e = civil_to_epoch(c).unwrap();
                    assert_eq!(epoch_to_civil(e), c, "{y:04}-{m:02}-{d:02}");
                }
            }
        }
    }

    #[test]
    fn rejects_invalid_dates() {
        assert_eq!(civil_to_epoch(Civil { year: 2025, month: 2, day: 29, hour: 0, min: 0, sec: 0 }), None);
        assert_eq!(civil_to_epoch(Civil { year: 2024, month: 2, day: 30, hour: 0, min: 0, sec: 0 }), None);
        assert_eq!(civil_to_epoch(Civil { year: 2025, month: 13, day: 1, hour: 0, min: 0, sec: 0 }), None);
        assert_eq!(civil_to_epoch(Civil { year: 2025, month: 1, day: 32, hour: 0, min: 0, sec: 0 }), None);
        assert_eq!(civil_to_epoch(Civil { year: 1969, month: 1, day: 1, hour: 0, min: 0, sec: 0 }), None);
    }

    #[test]
    fn weekdays_match_calendar() {
        // 2000-01-01 — суббота (DS1307: 7), 2026-11-13 — пятница (6),
        // 2026-11-15 — воскресенье (1).
        assert_eq!(weekday(946_684_800), 7);
        let fri = civil_to_epoch(Civil { year: 2026, month: 11, day: 13, hour: 9, min: 44, sec: 0 }).unwrap();
        assert_eq!(weekday(fri), 6);
        assert_eq!(weekday(fri + 2 * 86400), 1);
    }

    #[test]
    fn ds1307_regs_roundtrip() {
        for epoch in [RTC_MIN_EPOCH, 1_763_030_096, RTC_MAX_EPOCH, 3_500_000_000] {
            let regs = epoch_to_regs(epoch).unwrap();
            assert_eq!(regs.0[0] & 0x80, 0, "CH должен быть 0");
            assert_eq!(regs.0[2] & 0x40, 0, "24-часовой режим");
            assert_eq!(regs_to_epoch(&regs).unwrap(), epoch);
        }
        // Побайтовая сверка известного кадра: 2026-11-13 09:44:31, пятница.
        let e = civil_to_epoch(Civil { year: 2026, month: 11, day: 13, hour: 9, min: 44, sec: 31 }).unwrap();
        assert_eq!(epoch_to_regs(e).unwrap().0, [0x31, 0x44, 0x09, 0x06, 0x13, 0x11, 0x26]);
    }

    #[test]
    fn ds1307_rejects_bad_regs() {
        // CH=1 — часы стоят
        assert_eq!(regs_to_epoch(&Ds1307Regs([0x81, 0, 0, 1, 1, 1, 26])), Err(TimeError::Stopped));
        // 12-часовой режим
        assert_eq!(regs_to_epoch(&Ds1307Regs([0, 0, 0x44, 1, 1, 1, 26])), Err(TimeError::Bad12Hour));
        // не-BCD секунда (0x79)
        assert_eq!(regs_to_epoch(&Ds1307Regs([0x79, 0, 0, 1, 1, 1, 26])), Err(TimeError::BadBcd));
        // час 24
        assert_eq!(regs_to_epoch(&Ds1307Regs([0, 0, 0x24, 1, 1, 1, 26])), Err(TimeError::BadBcd));
        // 31 февраля
        assert_eq!(regs_to_epoch(&Ds1307Regs([0, 0, 0, 1, 0x31, 0x02, 0x26])), Err(TimeError::BadDate));
        // месяц 13
        assert_eq!(regs_to_epoch(&Ds1307Regs([0, 0, 0, 1, 1, 0x13, 0x26])), Err(TimeError::BadDate));
        // эпоха вне 2000..2099
        assert!(matches!(epoch_to_regs(RTC_MIN_EPOCH - 1), Err(TimeError::OutOfRange)));
        assert!(matches!(epoch_to_regs(RTC_MAX_EPOCH + 1), Err(TimeError::OutOfRange)));
    }

    #[test]
    fn sntp_request_shape() {
        let r = sntp_request();
        assert_eq!(r[0], 0x23);
        assert_eq!(r.len(), 48);
        assert!(r[1..].iter().all(|b| *b == 0));
    }

    #[test]
    fn sntp_parse_response_current_era() {
        let epoch = civil_to_epoch(Civil { year: 2026, month: 11, day: 13, hour: 12, min: 0, sec: 0 }).unwrap();
        let mut pkt = [0u8; 48];
        pkt[0] = 0x24; // LI=0 VN=4 Mode=4 (ответ)
        pkt[1] = 2; // stratum 2
        let ntp = epoch + NTP_UNIX_DELTA;
        pkt[40..44].copy_from_slice(&(ntp as u32).to_be_bytes());
        assert_eq!(sntp_parse(&pkt, epoch + 30).unwrap(), epoch);
    }

    #[test]
    fn sntp_parse_picks_era_after_2036_rollover() {
        // После 2036-02-07 32-битные часы NTP обнуляются: timestamp 100_000_000
        // в era 1 -> epoch 2039-05, рядом с референсом 2039 года.
        let mut pkt = [0u8; 48];
        pkt[0] = 0x24;
        pkt[1] = 1;
        pkt[40..44].copy_from_slice(&100_000_000u32.to_be_bytes());
        let ref_epoch = 2_185_978_496 - 100; // ~2039-05
        assert_eq!(sntp_parse(&pkt, ref_epoch).unwrap(), 2_185_978_496);
    }

    #[test]
    fn sntp_parse_rejects_junk() {
        let mut pkt = [0u8; 48];
        pkt[0] = 0x24;
        pkt[1] = 2;
        // короткая
        assert_eq!(sntp_parse(&pkt[..20], 1_763_000_000), Err(TimeError::MalformedPacket));
        // не ответ (mode=3)
        pkt[0] = 0x23;
        assert_eq!(sntp_parse(&pkt, 1_763_000_000), Err(TimeError::MalformedPacket));
        // stratum 0 — Kiss-of-Death
        pkt[0] = 0x24;
        pkt[1] = 0;
        assert_eq!(sntp_parse(&pkt, 1_763_000_000), Err(TimeError::KissOfDeath));
        // LI=11
        pkt[0] = 0xE4;
        pkt[1] = 2;
        assert_eq!(sntp_parse(&pkt, 1_763_000_000), Err(TimeError::MalformedPacket));
        // timestamp 0 -> кандидат слишком далеко от локальных часов
        pkt[0] = 0x24;
        assert_eq!(sntp_parse(&pkt, 1_763_000_000), Err(TimeError::TooFarOff));
    }

    #[test]
    fn parse_time_body_accepts_epoch_and_ms() {
        assert_eq!(parse_time_json(r#"{"epoch_ms":1763030096000}"#).unwrap(), 1_763_030_096);
        assert_eq!(parse_time_json(r#"{ "epoch" : 1763030096 }"#).unwrap(), 1_763_030_096);
        assert!(parse_time_json(r#"{"epoch_ms":999999999}"#).is_err()); // до 2001
        assert!(parse_time_json("{}").is_err());
        assert!(parse_time_json(r#"{"epoch":}"#).is_err());
    }

    #[test]
    fn clock_model_rebases_on_sync() {
        let mut c = Clock::new(1000, 100);
        assert_eq!(c.now_secs(160), 1060); // идем от build-эпохи по монотонному
        assert_eq!(c.source(), TimeSource::Build);
        assert!(!c.synced());
        c.set(5000, 200, TimeSource::Ntp);
        assert_eq!(c.now_secs(260), 5060);
        assert_eq!(c.source(), TimeSource::Ntp);
        assert!(c.synced());
        // коррекция из RTC не делает «прыжка» назад по монотонным часам
        c.set(5058, 300, TimeSource::Rtc);
        assert_eq!(c.now_secs(310), 5068);
    }

    #[test]
    fn civil_display_pads() {
        let c = epoch_to_civil(civil_to_epoch(Civil { year: 2026, month: 11, day: 3, hour: 9, min: 4, sec: 5 }).unwrap());
        assert_eq!(alloc::format!("{}", c), "2026-11-03 09:04:05");
    }
}

// ------------------------------------------------------------ часовой пояс ----

/// Границы смещения от UTC (минуты): UTC-12:00 … UTC+14:00 — как в tzdata,
/// больше не бывает ни у одного обитаемого пояса.
pub const TZ_MIN_LIMIT: i32 = -12 * 60;
pub const TZ_MAX_LIMIT: i32 = 14 * 60;
/// По умолчанию — Москва (UTC+3): часы должны сразу показывать местное время.
pub const TZ_DEFAULT_MIN: i16 = 180;

/// Проверка и приведение смещения из настроек/JSON к i16.
pub fn validate_tz_min(minutes: i32) -> Result<i16, &'static str> {
    if !(TZ_MIN_LIMIT..=TZ_MAX_LIMIT).contains(&minutes) {
        return Err("tz: смещение от UTC должно быть в диапазоне -720..=840 минут");
    }
    Ok(minutes as i16)
}

/// Сдвинутая шкала: UTC-эпоха + смещение пояса (может уйти на сутки назад).
/// Нижняя граница — 0, чтобы не получить отрицательную эпоху на краю.
pub fn local_epoch(epoch: u64, tz_min: i16) -> u64 {
    let shifted = epoch as i64 + tz_min as i64 * 60;
    if shifted < 0 { 0 } else { shifted as u64 }
}

/// Локальное гражданское время для показа на индикаторе и в UI.
pub fn local_civil(epoch: u64, tz_min: i16) -> Civil {
    epoch_to_civil(local_epoch(epoch, tz_min))
}

#[cfg(test)]
mod tz_tests {
    use super::*;

    fn utc(y: i32, mo: u8, d: u8, h: u8, mi: u8) -> u64 {
        civil_to_epoch(Civil { year: y, month: mo, day: d, hour: h, min: mi, sec: 0 }).expect("civil")
    }

    #[test]
    fn moscow_is_three_hours_ahead() {
        let c = local_civil(utc(2026, 11, 19, 12, 0), 180);
        assert_eq!((c.year, c.month, c.day, c.hour, c.min), (2026, 11, 19, 15, 0));
    }

    #[test]
    fn negative_offset_wraps_to_previous_day() {
        let c = local_civil(utc(2026, 11, 19, 2, 0), -300);
        assert_eq!((c.month, c.day, c.hour), (11, 18, 21));
    }

    #[test]
    fn positive_offset_wraps_to_next_year() {
        let c = local_civil(utc(2026, 12, 31, 23, 30), 180);
        assert_eq!((c.year, c.month, c.day, c.hour, c.min), (2027, 1, 1, 2, 30));
    }

    #[test]
    fn half_hour_zones_are_supported() {
        let c = local_civil(utc(2026, 11, 19, 12, 0), 330); // UTC+5:30
        assert_eq!((c.hour, c.min), (17, 30));
    }

    #[test]
    fn limits_are_enforced() {
        assert_eq!(validate_tz_min(-720), Ok(-720));
        assert_eq!(validate_tz_min(840), Ok(840));
        assert_eq!(validate_tz_min(0), Ok(0));
        assert!(validate_tz_min(-721).is_err());
        assert!(validate_tz_min(841).is_err());
        assert!(validate_tz_min(-20_000).is_err());
    }

    #[test]
    fn local_epoch_never_underflows() {
        assert_eq!(local_epoch(60, -720), 0);
        assert_eq!(local_epoch(0, 180), 10_800);
    }
}
