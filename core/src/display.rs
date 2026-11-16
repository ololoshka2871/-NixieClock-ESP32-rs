//! Логика индикации HH:MM:SS для параллельной шины I2S (M5).
//!
//! Железная правда — README «Железо»:
//! - Декодеров 2 (К155ИД1, выходы 0..9): шина A (GPIO 27/26/25/33) кормит лампы
//!   0..2, шина B (GPIO 32/5/18/19) — лампы 3..5.
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
    fn гашение_это_10_и_10() {
        assert_eq!(BLANK_BYTE, 0xAA);
        assert_eq!(slot_sample(BLANK_CODE, BLANK_CODE), BLANK_BYTE);
    }
}
