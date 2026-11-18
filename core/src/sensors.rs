//! Pure-логика датчиков M5 без железа: MH-Z19 (CO2, UART), DS18x20 (1-Wire
//! температура), конвертеры АЦП-шин. Всё хост-тестируется на x86.
//!
//! Протокольные факты взяты из спеки оригинала (SPEC_display_sensors_ru.md):
//! - MH-Z19: кадр 0xFF + cmd + 4 data + 2 reserved + checksum,
//!   checksum = (0x100 − sum(cmd..reserved)) & 0xFF; ответ 9 байт.
//! - DS18x20: scratchpad 9 байт + CRC8 DALLAS (полином 0x8C reflected),
//!   температура = int16 LE scratchpad[4..6] × 0.0625 °C.
//! - АЦП-шины: GPIO39→3.3V (×5.7), GPIO35→5V (×10.1), GPIO34→12V (×25).

/// MH-Z19: старт кадра.
pub const MHZ19_START: u8 = 0xFF;
/// MH-Z19: запрос концентрации CO2 (ppm).
pub const MHZ19_CMD_READ_PPM: u8 = 0x86;
/// MH-Z19: калибровка нуля.
pub const MHZ19_CMD_CAL_ZERO: u8 = 0x87;
/// MH-Z19: калибровка диапазона (data = ppm эталона, LE в b4..b5? — LE hi/lo в data[0..2]).
pub const MHZ19_CMD_CAL_SPAN: u8 = 0x88;
/// MH-Z19: авторокалибровка ABC (data[0]: 0xA0=48ч, 0x00=выкл, 0xFF=вкл 24ч).
pub const MHZ19_CMD_ABC: u8 = 0x79;

/// Контрольная сумма MH-Z19: (0x100 − sum(байт[1..8])) & 0xFF для 9-байтового кадра.
fn mhz19_checksum(frame9: &[u8; 9]) -> u8 {
    let sum = frame9[1..8].iter().map(|&b| b as u16).sum::<u16>() & 0xFF;
    // Маска обязательна: при sum == 0 разность равна 0x100 и в u8 не влезает
    // (раньше тут стоял try_into().unwrap() -> паника TryFromIntError на каждом
    // 256-м ответе датчика, то есть случайный ребут после пары часов работы).
    ((0x100u16 - sum) & 0xFF) as u8
}

/// Собрать 9-байтовую команду MH-Z19 (cmd + 4 data байта).
pub fn mhz19_command(cmd: u8, data: [u8; 4]) -> [u8; 9] {
    let mut f = [0u8; 9];
    f[0] = MHZ19_START;
    f[1] = 0x01; // command mode
    f[2] = cmd;
    f[3..7].copy_from_slice(&data);
    f[8] = mhz19_checksum(&f);
    f
}

/// Разобранный ответ MH-Z19.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mhz19Reading {
    /// Концентрация CO2, ppm (0..65535).
    pub co2_ppm: u16,
    /// Температура самого датчика, микроградусы (b4 − 40 °C).
    pub sensor_temp_uc: i32,
    /// Статус-байт ответа (0 = OK; 64 = NNC-калибровка, 65 = диапазон,
    /// 66 = ошибка температуры, 67 = обрыв).
    pub status: u8,
}

impl Mhz19Reading {
    /// Разобрать и проверить 9-байтовый ответ (cmd должен быть ожидаемым).
    pub fn parse(resp: &[u8; 9], expect_cmd: u8) -> Option<Self> {
        if resp[0] != MHZ19_START || resp[1] != expect_cmd {
            return None;
        }
        if mhz19_checksum(resp) != resp[8] {
            return None;
        }
        Some(Self {
            co2_ppm: (resp[2] as u16) << 8 | resp[3] as u16,
            sensor_temp_uc: (resp[4] as i8).wrapping_sub(40) as i32 * 1_000_000,
            status: resp[5],
        })
    }
}

// ---------------- DS18x20 ----------------

/// DS18x20: Skip ROM.
pub const DS18_CMD_SKIP_ROM: u8 = 0xCC;
/// DS18x20: Convert T (затем ждать ~750 мс на 12-бит).
pub const DS18_CMD_CONVERT: u8 = 0x44;
/// DS18x20: Read Scratchpad.
pub const DS18_CMD_READ_SCRATCH: u8 = 0xBE;

/// CRC8 DALLAS/Maxim (полином x^8+x^5+x^4+1, reflected, init 0).
pub fn crc8_dallas(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in bytes {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8C } else { crc >> 1 };
        }
    }
    crc
}

/// Разобрать scratchpad DS18x20 (Read Scratchpad 0xBE).
/// DS18B20: 9 байт, CRC8 на позиции 8; DS18S20: 8 байт + заполнитель.
/// Возвращает температуру в микроградусах (raw × 62_500).
/// Декод Read Scratchpad DS18x20. Принимает 9 байт и поддерживает обе раскладки:
/// DS18B20 (канон: CRC8 на индексе 8 поверх байтов 0..7) и легаси DS18S20
/// (CRC8 на индексе 7 поверх байтов 0..6). Температура — raw[1:0] в 1/16 °C.
pub fn ds18x20_scratchpad(sp: &[u8; 9]) -> Option<i32> {
    let b20 = crc8_dallas(&sp[..8]) == sp[8];
    let s20 = crc8_dallas(&sp[..7]) == sp[7];
    if !b20 && !s20 {
        return None;
    }
    let raw = i16::from_le_bytes([sp[0], sp[1]]);
    Some(raw as i32 * 62_500)
}

/// Проверка ROM-кода DS18x20 (8 байт: family + serial[6] + CRC8).
pub fn ds18x20_check_rom(rom8: &[u8; 8]) -> bool {
    crc8_dallas(&rom8[..7]) == rom8[7]
}

// ---------------- АЦП-шины ----------------

/// Полное шкальное напряжение АЦП на пине, мВ (аттенюация 0 у ESP32-classic).
pub const ADC_FS_AT0_MV: u32 = 1100;
/// Отношение делителя ×1000: GPIO39 → шина 3.3 В.
pub const RATIO_3V3_X1000: u32 = 5_700;
/// Отношение делителя ×1000: GPIO35 → шина 5 В.
pub const RATIO_5V_X1000: u32 = 10_100;
/// Отношение делителя ×1000: GPIO34 → шина 12 В.
pub const RATIO_12V_X1000: u32 = 25_000;

/// Пересчёт raw АЦП в напряжение шины, милливольты.
/// `bits` — разрядность (10 или 12), `ratio_x1000` — отношение делителя ×1000.
pub fn adc_raw_to_rail_mv(raw: u16, bits: u8, fs_mv: u32, ratio_x1000: u32) -> u32 {
    let full = (1u32 << bits) - 1;
    let pin_mv = raw as u32 * fs_mv / full;
    pin_mv * ratio_x1000 / 1000
}

/// Откалиброванный по eFuse пин (милливольты на пине) -> напряжение шины.
pub fn pin_mv_to_rail_mv(pin_mv: u16, ratio_x1000: u32) -> u32 {
    pin_mv as u32 * ratio_x1000 / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mhz19_checksum_wraps_at_256() {
        // сумма байт[1..8] = 0x100 -> контрольная сумма 0x00 (раньше: паника PosOverflow)
        // 0x01 + 0xFF + 0x00*4 + 0x00 = 0x100
        let frame = [MHZ19_START, 0x01, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(mhz19_checksum(&frame), 0x00);
        // сумма = 0x1FF -> 0x01
        let frame2 = [MHZ19_START, 0x01, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(mhz19_checksum(&frame2), 0x01);
        // обычный кадр не изменился
        assert_eq!(mhz19_checksum(&mhz19_command(MHZ19_CMD_READ_PPM, [0, 0, 0, 0])), 0x79);
    }

    #[test]
    fn mhz19_read_command_frame() {
        assert_eq!(
            mhz19_command(MHZ19_CMD_READ_PPM, [0, 0, 0, 0]),
            [0xFF, 0x01, 0x86, 0, 0, 0, 0, 0, 0x79]
        );
    }

    #[test]
    fn mhz19_abc_and_span_frames() {
        assert_eq!(
            mhz19_command(MHZ19_CMD_ABC, [0xA0, 0, 0, 0]),
            [0xFF, 0x01, 0x79, 0xA0, 0, 0, 0, 0, 0xE6]
        );
        assert_eq!(
            mhz19_command(MHZ19_CMD_CAL_SPAN, [0x00, 0x0F, 0xA0, 0]),
            [0xFF, 0x01, 0x88, 0x00, 0x0F, 0xA0, 0, 0, 0xC8]
        );
    }

    #[test]
    fn mhz19_parse_ok() {
        let resp = [0xFF, 0x86, 0x01, 0xF4, 65, 0, 0, 0, 0x44];
        let r = Mhz19Reading::parse(&resp, MHZ19_CMD_READ_PPM).unwrap();
        assert_eq!(r.co2_ppm, 500);
        assert_eq!(r.sensor_temp_uc, 25_000_000);
        assert_eq!(r.status, 0);
    }

    #[test]
    fn mhz19_parse_error_status_and_bad_crc() {
        let resp = [0xFF, 0x86, 0x03, 0xE8, 60, 66, 0, 0, 0x11];
        let r = Mhz19Reading::parse(&resp, MHZ19_CMD_READ_PPM).unwrap();
        assert_eq!(r.co2_ppm, 1000);
        assert_eq!(r.status, 66); // ошибка температуры датчика
        // подкрученный checksum
        let mut bad = resp;
        bad[8] ^= 0xFF;
        assert!(Mhz19Reading::parse(&bad, MHZ19_CMD_READ_PPM).is_none());
        // чужой cmd
        assert!(Mhz19Reading::parse(&resp, MHZ19_CMD_CAL_ZERO).is_none());
    }

    #[test]
    fn crc8_dallas_check_value() {
        // каноническое проверочное значение для "123456789"
        assert_eq!(crc8_dallas(b"123456789"), 0xA1);
    }

    #[test]
    fn ds18x20_scratchpad_positive_and_negative() {
        // канон DS18S20-раскладки: +25.0000 °C (raw 400 = 0x0190), CRC на b7
        let sp = [0x90u8, 0x01, 0x1F, 0x00, 0xFF, 0x10, 0x10, 0xEB, 0xFF];
        assert_eq!(ds18x20_scratchpad(&sp), Some(25_000_000));
        // −10.0625 °C (raw −161 = 0xFF5F)
        let neg = [0x5Fu8, 0xFF, 0x1F, 0x00, 0xFF, 0x10, 0x10, 0x98, 0xFF];
        assert_eq!(ds18x20_scratchpad(&neg), Some(-10_062_500));
        // живой кадр DS18B20 с платы (v0.5.2 диагностика): CRC на b8, 28.3125 °C
        let b20 = [0xC5u8, 0x01, 0x55, 0x05, 0x7F, 0x7E, 0x81, 0x66, 0xDD];
        assert_eq!(ds18x20_scratchpad(&b20), Some(28_312_500));
        // битый CRC (обе раскладки не сходятся)
        let mut bad = b20;
        bad[8] = 0;
        bad[7] ^= 0x20;
        assert_eq!(ds18x20_scratchpad(&bad), None);
        // ROM-код: family 0x28
        assert!(ds18x20_check_rom(&[0x28, 0xE0, 0x02, 0xFF, 0xFF, 0x00, 0x00, 0x8D]));
        let mut badrom = [0x28, 0xE0, 0x02, 0xFF, 0xFF, 0x00, 0x00, 0x8D];
        badrom[1] ^= 1;
        assert!(!ds18x20_check_rom(&badrom));
    }

    #[test]
    fn adc_rail_conversion() {
        // 10-бит, аттенюация 0 (полный шкала 1100 мВ на пине).
        // Полный raw: 1100 мВ × 5.7 = 6270 мВ на шине 3.3 В (макс ~6.3 В по спеке)
        assert_eq!(adc_raw_to_rail_mv(1023, 10, ADC_FS_AT0_MV, RATIO_3V3_X1000), 6270);
        // raw=538 → 578 мВ на пине → ×5.7 ≈ 3294 мВ (3.3 В шина)
        let v33 = adc_raw_to_rail_mv(538, 10, ADC_FS_AT0_MV, RATIO_3V3_X1000);
        assert!((3250..=3350).contains(&v33), "v33={v33}");
        // 12-бит: 5 В шина при raw 1600 из 4095 (1600*1100/4095=430 мВ ×10.1 = 4343)
        let v5 = adc_raw_to_rail_mv(1600, 12, ADC_FS_AT0_MV, RATIO_5V_X1000);
        assert!((4300..=4400).contains(&v5), "v5={v5}");
        // 12 В шина: raw 950/4095 → 255 мВ ×25 = 6381 мВ
        let v12 = adc_raw_to_rail_mv(950, 12, ADC_FS_AT0_MV, RATIO_12V_X1000);
        assert!((6300..=6450).contains(&v12), "v12={v12}");
    }
}
