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

/// Байты трёх слотов (семп слота g из digits[g] и digits[g+3]).
pub fn slot_samples(d: &[u8; 6]) -> [u8; 3] {
    core::array::from_fn(|g| {
        slot_sample(digit_code(d[g]), digit_code(d[g + 3]))
    })
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
}
