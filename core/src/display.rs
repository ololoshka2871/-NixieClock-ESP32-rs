//! Логика индикации HH:MM:SS для параллельной шины I2S (M5).
//!
//! Железная правда — README «Железо»:
//! - Декодеров 2 (К155ИД1, выходы 0..9): шина A (GPIO 27/26/25/33) кормит лампы
//!   0..2, шина B (GPIO 32/12/18/19) — лампы 3..5.
//! - Стробы TG0..TG2 (GPIO 13/2/14) активны HIGH и выбирают лампы перемаркой
//!   TG(лампа) = [0,1,2,0,1,2] (Ч1 М2), (Ч2 С1), (М1 С2).
//! - Карта цифра->код на входе дешифтора (перемарка разводки катодов) —
//!   оригинальная таблица, подтверждена README: 0->3 1->2 2->4 3->1 4->8
//!   5->13 6->5 7->12 8->9 9->0, гашение -> 10.
//!
//! Слот g (one-hot TG_g): шина A несёт код цифры g, шина B — код цифры g+3;
//! байт семпла: [биты 0..3 = шина A (D1A..D4A LSB..MSB), биты 4..7 = шина B].

/// Цифра 0..9 -> код на входах дешифтора (оригинальная перемарка).
pub const DIGIT_CODES: [u8; 10] = [3, 2, 4, 1, 8, 13, 5, 12, 9, 0];

/// Код «погасить» (К155ИД1: ни один выход не выбран).
pub const BLANK_CODE: u8 = 10;

/// Байт семпла полностью погашенной шины (обе дешифраторы в гашении).
pub const BLANK_BYTE: u8 = BLANK_CODE | (BLANK_CODE << 4);

/// Семп шины: коды дешифраторов A и B в одном байте.
pub const fn slot_sample(code_a: u8, code_b: u8) -> u8 {
    code_a | (code_b << 4)
}

/// Код цифры; всё вне 0..9 трактуем как гашение (на случай сбоя).
pub const fn digit_code(digit: u8) -> u8 {
    if digit < 10 {
        DIGIT_CODES[digit as usize]
    } else {
        BLANK_CODE
    }
}

/// Цифры HHMMSS: [Ч1, Ч2, М1, М2, С1, С2].
pub const fn hhmmss_digits(h: u8, m: u8, s: u8) -> [u8; 6] {
    [h / 10, h % 10, m / 10, m % 10, s / 10, s % 10]
}

/// Дата `ДД ММ ГГ` — `%02d%02d%02d` от оригинала (DateDisplayer.cpp),
/// локальное время, ведущие нули сохраняются (1 сентября 2026 → `010926`).
pub const fn ddmmyy_digits(day: u8, month: u8, year2: u8) -> [u8; 6] {
    [day / 10, day % 10, month / 10, month % 10, year2 / 10, year2 % 10]
}

/// Байты трёх слотов (семп слота g из digits[g] и digits[g+3]).
pub fn slot_samples(d: &[u8; 6]) -> [u8; 3] {
    core::array::from_fn(|g| {
        slot_sample(digit_code(d[g]), digit_code(d[g + 3]))
    })
}

/// Максимальная скважность: столько 16-битных слов из 128-словного блока
/// PWM горит лампа. Блок повторяется драйвером 8 раз внутри окна строба
/// (8 дескрипторов по 128 слов смотрят в один буфер), поэтому скважность
/// `duty / 128` задаёт яркость лампы, а частота ШИМ ≈ 1.27 МГц / 128 ≈ 9.9 кГц.
/// Значение обязано совпадать с `ON_CHUNK_WORDS` прошивки — там const-assert.
pub const DUTY_MAX: u8 = 128;

/// Шаг линейного фейда за один тик. 128 / 4 = 32 тика на половину; при тике
/// 5 мс это ~160 мс вниз и ~160 мс вверх (полный переход ~320 мс).
pub const FADE_STEP: u8 = 4;

/// Фаза плавной смены одного разряда.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Out,
    In,
}

/// Плавная смена кода одной лампы: линейно вниз до полной тьмы, затем
/// линейно вверх уже с новым кодом. Старый и новый код никогда не горят
/// одновременно: код меняется ровно в момент `duty == 0`.
#[derive(Clone, Copy, Debug)]
pub struct LampFade {
    shown: u8,
    target: u8,
    duty: u8,
    phase: Phase,
}

impl LampFade {
    /// Лампа уже горит кодом `code` на полной яркости.
    pub const fn new(code: u8) -> Self {
        Self {
            shown: code,
            target: code,
            duty: DUTY_MAX,
            phase: Phase::Idle,
        }
    }

    /// Погашенная лампа (дисплей выключен).
    pub const fn dark() -> Self {
        Self {
            shown: BLANK_CODE,
            target: BLANK_CODE,
            duty: 0,
            phase: Phase::Idle,
        }
    }

    pub const fn code(&self) -> u8 {
        self.shown
    }

    pub const fn duty(&self) -> u8 {
        self.duty
    }

    /// Сразу показать код на полной яркости, без анимации.
    pub fn snap(&mut self, code: u8) {
        self.shown = code;
        self.target = code;
        self.duty = DUTY_MAX;
        self.phase = Phase::Idle;
    }

    /// Погасить без анимации.
    pub fn blank(&mut self) {
        self.shown = BLANK_CODE;
        self.target = BLANK_CODE;
        self.duty = 0;
        self.phase = Phase::Idle;
    }

    /// Один тик анимации. `animate = false` — мгновенная смена (эффект
    /// выключен). Возвращает `true`, если видимое состояние (код/яркость)
    /// изменилось и буферы слота надо переписать.
    pub fn step(&mut self, target: u8, animate: bool) -> bool {
        let before = (self.shown, self.duty);
        self.target = target;
        if !animate {
            self.snap(target);
        } else {
            match self.phase {
                Phase::Idle => {
                    if target != self.shown {
                        self.phase = Phase::Out;
                    }
                }
                Phase::Out => {
                    if target == self.shown {
                        // Цель вернулась к текущему коду — не гасим зря, поднимаем.
                        self.phase = Phase::In;
                    } else if self.duty > FADE_STEP {
                        self.duty -= FADE_STEP;
                    } else {
                        // Старый код гаснет ровно в ноль; только теперь подмена.
                        self.duty = 0;
                        self.shown = target;
                        self.phase = Phase::In;
                    }
                }
                Phase::In => {
                    if target != self.shown {
                        // Новое изменение на полпути: сначала снова погасить.
                        self.phase = Phase::Out;
                    } else if self.duty >= DUTY_MAX - FADE_STEP {
                        self.duty = DUTY_MAX;
                        self.phase = Phase::Idle;
                    } else {
                        self.duty += FADE_STEP;
                    }
                }
            }
        }
        before != (self.shown, self.duty)
    }
}

/// Семпл шины для позиции `i` внутри 128-словного блока PWM: лампа A
/// (младший ниббл) горит, пока `i < duty_a`, лампа B — пока `i < duty_b`.
/// Каналы независимы: при обеих полных скважностях слово несёт оба кода,
/// то есть выглядит ровно как статическая индикация.
///
/// `inline(always)` обязателен: функция вызывается из ISR в `.rwtext`, и
/// отдельная копия в flash не переживёт запись OTA (кэш выключен).
#[inline(always)]
pub const fn pwm_sample(i: u8, code_a: u8, duty_a: u8, code_b: u8, duty_b: u8) -> u8 {
    let a = if i < duty_a { code_a } else { BLANK_CODE };
    let b = if i < duty_b { code_b } else { BLANK_CODE };
    a | (b << 4)
}

/// Режимы тестовой индикации (диагностика M5.1, эндпоинт /display/test):
/// 0 = выключен (показываем время); 1..=9 = фиксированная цифра d на всех
/// шести лампах; 10 = цикл 1..9 по 1 с (привязан к настенной секунде);
/// 16..=31 = сырой код (16+code, code 0..15) прямо на оба дешифратора —
/// карта «код -> что реально горит» без перемарки; 32 = цикл сырых кодов
/// 0..15 по 1 с.
pub const TEST_CYCLE: u8 = 10;
pub const TEST_ZERO: u8 = 11;
pub const TEST_RAW_BASE: u8 = 16;
pub const TEST_RAW_CYCLE: u8 = 32;

/// Цифра тест-цикла: секунда времени -> цифра 0..9 (0,1,..9,0,1,..).
pub const fn test_digit_at_epoch(epoch: u64) -> u8 {
    (epoch % 10) as u8
}

/// Байты трёх слотов в тест-режиме; None — обычный часовой режим.
pub fn test_samples(mode: u8, epoch: u64) -> Option<[u8; 3]> {
    match mode {
        0 => None,
        1..=9 => {
            let c = digit_code(mode);
            Some([slot_sample(c, c); 3])
        }
        TEST_ZERO => {
            let c = digit_code(0);
            Some([slot_sample(c, c); 3])
        }
        TEST_CYCLE => {
            let c = digit_code(test_digit_at_epoch(epoch));
            Some([slot_sample(c, c); 3])
        }
        TEST_RAW_CYCLE => {
            let c = (epoch % 16) as u8;
            Some([slot_sample(c, c); 3])
        }
        m if (TEST_RAW_BASE..=(TEST_RAW_BASE + 15)).contains(&m) => {
            let c = m - TEST_RAW_BASE;
            Some([slot_sample(c, c); 3])
        }
        _ => None,
    }
}

/// То же, но с маской ламп: `Some(0..=5)` оставляет цифру только на этой
/// лампе, `None` — на всех шести.
///
/// Зачем: `/display/test?digit=N` зажигает N на всех лампах сразу, то есть
/// тянет из питания ток шести ламп одного и того же разряда. Если плата
/// «складывается» именно от тока (а не от кода на шине), это видно сразу:
/// одна лампа — норма, шесть — просадка. Маска позволяет перебрать лампы
/// по одной и найти виновника (`?digit=2&lamp=3`).
///
/// Раскладка слотов: слот g несёт лампы g (младший ниббл) и g+3 (старший).
pub fn test_samples_lamp(mode: u8, epoch: u64, lamp: Option<u8>) -> Option<[u8; 3]> {
    let mut s = test_samples(mode, epoch)?;
    let Some(l) = lamp.filter(|l| *l < 6) else {
        return Some(s);
    };
    let target_slot = (l % 3) as usize;
    let high_nibble = l >= 3;
    for (g, slot) in s.iter_mut().enumerate() {
        if g != target_slot {
            *slot = BLANK_BYTE; // этой лампе в слоте нечего показывать
        } else if high_nibble {
            // слот несёт лампу g в младшем ниббле и g+3 в старшем
            *slot = (*slot & 0xF0) | BLANK_CODE;
        } else {
            *slot = (*slot & 0x0F) | (BLANK_CODE << 4);
        }
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn перемарка_цифр_совпадает_с_оригиналом() {
        assert_eq!(DIGIT_CODES, [3, 2, 4, 1, 8, 13, 5, 12, 9, 0]);
        assert_eq!(digit_code(5), 13);
        assert_eq!(digit_code(7), 12);
        assert_eq!(digit_code(10), BLANK_CODE);
    }

    #[test]
    fn цифры_часов() {
        assert_eq!(hhmmss_digits(23, 59, 7), [2, 3, 5, 9, 0, 7]);
        assert_eq!(hhmmss_digits(0, 0, 0), [0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn цифры_даты() {
        assert_eq!(ddmmyy_digits(9, 11, 26), [0, 9, 1, 1, 2, 6]);
        assert_eq!(ddmmyy_digits(31, 12, 99), [3, 1, 1, 2, 9, 9]);
    }

    #[test]
    fn цифры_даты_как_в_оригинале() {
        // 19 ноября 2026 → 19 11 26
        assert_eq!(ddmmyy_digits(19, 11, 26), [1, 9, 1, 1, 2, 6]);
        // 1 сентября 2026 → ведущие нули сохраняются: 01 09 26
        assert_eq!(ddmmyy_digits(1, 9, 26), [0, 1, 0, 9, 2, 6]);
        assert_eq!(ddmmyy_digits(31, 12, 99), [3, 1, 1, 2, 9, 9]);
    }

    #[test]
    fn слоты_несут_свои_лампы_по_перемарке_tg() {
        // 23:59:07: слот0 = Ч1=2 (код4) + М2=9 (код0);
        // слот1 = Ч2=3 (код1) + С1=0 (код3); слот2 = М1=5 (код13) + С2=7 (код12).
        let s = slot_samples(&hhmmss_digits(23, 59, 7));
        assert_eq!(s[0], 4 | (0 << 4));
        assert_eq!(s[1], 1 | (3 << 4));
        assert_eq!(s[2], 13 | (12 << 4));
    }

    #[test]
    fn тест_фиксированная_цифра_и_цикл() {
        assert_eq!(test_samples(0, 7), None);
        assert_eq!(test_samples(3, 0), Some([1u8 | (1u8 << 4); 3])); // цифра 3 -> код 1
        assert_eq!(test_samples(5, 0), Some([13u8 | (13u8 << 4); 3]));
        assert_eq!(test_digit_at_epoch(0), 0);
        assert_eq!(test_digit_at_epoch(8), 8);
        assert_eq!(test_digit_at_epoch(9), 9);
        assert_eq!(test_digit_at_epoch(10), 0);
        assert_eq!(test_samples(TEST_CYCLE, 10), Some([3u8 | (3u8 << 4); 3])); // цифра 0 -> код 3
        assert_eq!(test_samples(TEST_ZERO, 0), Some([3u8 | (3u8 << 4); 3]));
    }

    #[test]
    fn тест_сырые_коды_и_их_цикл() {
        assert_eq!(test_samples(TEST_RAW_BASE, 0), Some([0u8; 3]));
        assert_eq!(
            test_samples(TEST_RAW_BASE + 15, 0),
            Some([15u8 | (15u8 << 4); 3])
        );
        assert_eq!(test_samples(TEST_RAW_CYCLE, 16), Some([0u8; 3]));
        assert_eq!(test_samples(TEST_RAW_CYCLE, 17), Some([1u8 | (1u8 << 4); 3]));
        assert_eq!(test_samples(255, 0), None);
    }

    #[test]
    fn тест_одна_лампа() {
        // digit=5 -> код 13 на всех лампах
        let all = test_samples(5, 0).unwrap();
        assert_eq!(all, [13u8 | (13u8 << 4); 3]);
        // лампа 0: слот 0, младший ниббл живёт, старший погашен, слоты 1/2 пусты
        assert_eq!(
            test_samples_lamp(5, 0, Some(0)).unwrap(),
            [13u8 | (BLANK_CODE << 4), BLANK_BYTE, BLANK_BYTE]
        );
        // лампа 4: слот 1, старший ниббл
        assert_eq!(
            test_samples_lamp(5, 0, Some(4)).unwrap(),
            [BLANK_BYTE, BLANK_CODE | (13u8 << 4), BLANK_BYTE]
        );
        // лампа 5: слот 2, старший ниббл
        assert_eq!(
            test_samples_lamp(5, 0, Some(5)).unwrap(),
            [BLANK_BYTE, BLANK_BYTE, BLANK_CODE | (13u8 << 4)]
        );
        // без маски и с маской вне 0..5 — все шесть
        assert_eq!(test_samples_lamp(5, 0, None).unwrap(), all);
        assert_eq!(test_samples_lamp(5, 0, Some(9)).unwrap(), all);
        // в обычном режиме маска не действует (нет тест-семплов)
        assert_eq!(test_samples_lamp(0, 0, Some(1)), None);
    }

    #[test]
    fn гашение_это_10_и_10() {
        assert_eq!(BLANK_BYTE, 0xAA);
        assert_eq!(slot_sample(BLANK_CODE, BLANK_CODE), BLANK_BYTE);
    }

    #[test]
    fn фейд_гасит_до_нуля_и_поднимает_новый_код() {
        let mut f = LampFade::new(3);
        assert_eq!((f.code(), f.duty()), (3, DUTY_MAX));
        let mut prev = f.duty();
        // Гасим до нуля: код не меняется, яркость строго не растёт.
        loop {
            f.step(7, true);
            if f.code() == 7 {
                assert_eq!(f.duty(), 0, "код меняется только на нуле");
                break;
            }
            assert_eq!(f.code(), 3);
            assert!(f.duty() <= prev);
            prev = f.duty();
        }
        // Поднимаем новый код до полной яркости.
        let mut guard = 0;
        while f.duty() < DUTY_MAX {
            assert_eq!(f.code(), 7);
            f.step(7, true);
            guard += 1;
            assert!(guard < 100);
        }
        assert_eq!((f.code(), f.duty()), (7, DUTY_MAX));
        assert!(!f.step(7, true), "на полной яркости тик ничего не меняет");
    }

    #[test]
    fn фейд_выключен_меняет_мгновенно() {
        let mut f = LampFade::new(3);
        assert!(f.step(7, false));
        assert_eq!((f.code(), f.duty()), (7, DUTY_MAX));
        assert!(!f.step(7, false));
    }

    #[test]
    fn смена_цели_на_полпути_не_смешивает_коды() {
        let mut f = LampFade::new(3);
        for _ in 0..10 {
            f.step(7, true);
        }
        assert_eq!(f.code(), 3);
        assert!(f.duty() > 0 && f.duty() < DUTY_MAX);
        // Новая цель 8: старый код гаснет до нуля, затем поднимается 8.
        for _ in 0..100 {
            f.step(8, true);
            if f.code() == 8 && f.duty() == DUTY_MAX {
                break;
            }
            if f.code() == 8 {
                // новый код появляется только после полного нуля
                assert_eq!(f.duty(), 0);
                break;
            }
            assert_eq!(f.code(), 3);
        }
        while f.duty() < DUTY_MAX {
            f.step(8, true);
        }
        assert_eq!((f.code(), f.duty()), (8, DUTY_MAX));
    }

    #[test]
    fn возврат_цели_поднимает_яркость_обратно() {
        let mut f = LampFade::new(3);
        for _ in 0..10 {
            f.step(7, true);
        }
        let dipped = f.duty();
        assert!(dipped < DUTY_MAX);
        for _ in 0..100 {
            f.step(3, true);
            if f.duty() == DUTY_MAX {
                break;
            }
        }
        assert_eq!((f.code(), f.duty()), (3, DUTY_MAX));
    }

    #[test]
    fn pwm_скважность_независима_по_каналам() {
        // Полная скважность — оба кода в каждом слове.
        for i in 0..DUTY_MAX {
            assert_eq!(
                pwm_sample(i, 13, DUTY_MAX, 4, DUTY_MAX),
                13 | (4 << 4)
            );
        }
        // Нулевая — полное гашение обоих дешифраторов.
        assert_eq!(pwm_sample(0, 13, 0, 4, 0), BLANK_BYTE);
        // A горит 64 слова, B — 32, каналы не мешают друг другу.
        let (mut a, mut b) = (0, 0);
        for i in 0..DUTY_MAX {
            let s = pwm_sample(i, 13, 64, 4, 32);
            if s & 0x0F == 13 {
                a += 1;
            }
            if s >> 4 == 4 {
                b += 1;
            }
        }
        assert_eq!((a, b), (64, 32));
    }
}
