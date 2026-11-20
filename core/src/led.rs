//! Подсветка WS2812 под лампами (6 светодиодов на GPIO15) — порт логики
//! оригинального IDF-проекта.
//!
//! Индекс в массиве светодиодов — **обратный** порядку ламп: `led[5]` стоит под
//! лампой 0 (самой левой), `led[0]` — под лампой 5. Именно так оригинал
//! заполняет полосу (`_leds[5 - i]`), и так же разложены цвета режима
//! температуры (тусклые — под сотыми, яркие — под целыми).
//!
//! Режимы:
//! - часы: полоса окрашена в «фоновый» цвет (по умолчанию чёрный);
//! - CO2: шкала 400…2000 ppm зелёный→красный, ниже 400 — один зелёный,
//!   выше 2000 — вся полоса красным с яркостью `30·ln(ppm-1999)` (дышит);
//! - температура: тон по шкале 0…50 °C синий→красный, тусклый низ под сотыми,
//!   яркий верх под целыми, крайний левый — красный (или фиолетовый при <0).

/// Цвет в порядке проводов WS2812: (R, G, B).
pub type Rgb = (u8, u8, u8);

/// Светодиодов в полосе — по одному под каждой лампой.
pub const LEDS: usize = 6;

pub const BLACK: Rgb = (0, 0, 0);
pub const RED: Rgb = (255, 0, 0);
pub const GREEN: Rgb = (0, 255, 0);
pub const BLUE: Rgb = (0, 0, 255);
/// CRGB::BlueViolet — знак «минус» в режиме температуры.
pub const BLUE_VIOLET: Rgb = (138, 43, 226);

/// FastLED `hsv2rgb_rainbow()` — восемь секторов по 32 hue-единицы.
///
/// Это даёт именно тот круг, который был в оригинале: 0 красный, 96 зелёный,
/// 160 синий. Переход 255→0 остаётся плавным, потому что последний шаг колеса
/// уже почти красный, а не произвольная точка палитры.
const HUE_THIRD_SCALE: u8 = 85;
const HUE_TWO_THIRDS_SCALE: u8 = 170;

pub fn hsv2rgb(hue: u8, saturation: u8, value: u8) -> Rgb {
    let offset8 = (hue & 0x1F) << 3;
    let third = scale8(offset8, HUE_THIRD_SCALE);

    let (mut r, mut g, mut b) = match (hue & 0x80, hue & 0x40, hue & 0x20) {
        (0, 0, 0) => (255 - third, third, 0),
        (0, 0, 0x20) => (171, 85 + third, 0),
        (0, 0x40, 0) => {
            let twothirds = scale8(offset8, HUE_TWO_THIRDS_SCALE);
            (171 - twothirds, 170 + third, 0)
        }
        (0, 0x40, 0x20) => (0, 255 - third, third),
        (0x80, 0, 0) => {
            let twothirds = scale8(offset8, HUE_TWO_THIRDS_SCALE);
            (0, 171 - twothirds, 85 + twothirds)
        }
        (0x80, 0, 0x20) => (third, 0, 255 - third),
        (0x80, 0x40, 0) => (85 + third, 0, 171 - third),
        _ => (170 + third, 0, 85 - third),
    };

    if saturation != 255 {
        if saturation == 0 {
            return (255, 255, 255);
        }
        let desat = scale8_video(255 - saturation, 255 - saturation);
        let satscale = 255 - desat;
        r = scale8(r, satscale);
        g = scale8(g, satscale);
        b = scale8(b, satscale);
        r += desat;
        g += desat;
        b += desat;
    }

    if value != 255 {
        let value = scale8_video(value, value);
        if value == 0 {
            return BLACK;
        }
        r = scale8(r, value);
        g = scale8(g, value);
        b = scale8(b, value);
    }

    (r, g, b)
}

/// FastLED fixed `scale8()`: x * (scale + 1) / 256.
fn scale8(x: u8, scale: u8) -> u8 {
    ((x as u16 * (scale as u16 + 1)) >> 8) as u8
}

/// FastLED `scale8_video()`: ненулевое значение не схлопывается в 0.
fn scale8_video(x: u8, scale: u8) -> u8 {
    (((x as u16 * scale as u16) >> 8) as u8) + if x != 0 && scale != 0 { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// CO2
// ---------------------------------------------------------------------------

pub const CO2_MIN: i32 = 400;
pub const CO2_MAX: i32 = 2000;

/// Натуральный логарифм для no_std: степень двойки + ряд для мантиссы.
/// Точность ~1e-4 на диапазоне 1..10⁵, этого более чем достаточно для яркости.
pub fn ln(x: f32) -> f32 {
    if x <= 0.0 {
        return -100.0;
    }
    let bits = x.to_bits();
    let mut e = ((bits >> 23) & 0xff) as i32 - 127;
    let mut m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000); // [1, 2)
    if m > 1.414_213_6 {
        m *= 0.5;
        e += 1;
    }
    // ln(m) = 2·atanh((m-1)/(m+1)), |z| <= 0.1716 → четырёх членов хватает
    let z = (m - 1.0) / (m + 1.0);
    let z2 = z * z;
    let ln_m = 2.0 * z * (1.0 + z2 / 3.0 + z2 * z2 / 5.0 + z2 * z2 * z2 / 7.0);
    e as f32 * core::f32::consts::LN_2 + ln_m
}

/// Сколько светодиодов горит полностью и каким цветом (оригинальная шкала).
/// Возвращает (последний полный индекс, цвет заливки, цвет «догоняющего»).
fn co2_bar(level: i32) -> (usize, Rgb, Rgb) {
    if level < CO2_MIN {
        (1, GREEN, BLACK)
    } else if level > CO2_MAX {
        let bright = 30.0 * ln((level - (CO2_MAX - 1)) as f32);
        let v = if bright < 255.0 { (255.0 - bright) as u8 } else { 0 };
        let color = hsv2rgb(0, 255, v);
        (5, color, color)
    } else {
        let rel = level - CO2_MIN;
        // k = (0 - 96) / 1600 → тон плавно уходит от зелёного к красному
        let h = 96.0 - 96.0 * rel as f32 / (CO2_MAX - CO2_MIN) as f32;
        let count = (rel / 320) as usize;
        let over = rel - (count as i32) * 320;
        let v = (255 * over / 320) as u8;
        let fill = hsv2rgb(h as u8, 255, 255);
        let add = hsv2rgb(h as u8, 255, v);
        (count, fill, add)
    }
}

/// Полоса для уровня CO2 (ppm). `None`/отрицательное — всё погашено.
pub fn co2_leds(level: Option<i32>) -> [Rgb; LEDS] {
    let Some(level) = level else {
        return [BLACK; LEDS];
    };
    if level < 0 {
        return [BLACK; LEDS];
    }
    let (count, fill, add) = co2_bar(level);
    let mut out = [BLACK; LEDS];
    let mut i = 0usize;
    while i <= count && i < LEDS {
        out[LEDS - 1 - i] = fill;
        i += 1;
    }
    if i < LEDS - 1 {
        out[LEDS - 1 - i] = add;
    }
    out
}

// ---------------------------------------------------------------------------
// Температура
// ---------------------------------------------------------------------------

pub const TEMP_MIN: f32 = 0.0;
pub const TEMP_MAX: f32 = 50.0;

/// Тон по температуре: 0 °C — синий (hue 160), 50 °C — красный (hue 0).
/// `low` — приглушённый вариант (sat 200, value 100) как в оригинале.
fn temp_hue(t: f32) -> u8 {
    if t <= TEMP_MIN {
        return 160;
    }
    if t >= TEMP_MAX {
        return 0;
    }
    let k = (0.0 - 160.0) / (TEMP_MAX - TEMP_MIN);
    (160.0 + k * t) as u8
}

fn temp_color(t: f32, low: bool) -> Rgb {
    if t <= TEMP_MIN {
        return BLUE;
    }
    if t >= TEMP_MAX {
        return RED;
    }
    hsv2rgb(temp_hue(t), if low { 200 } else { 255 }, if low { 100 } else { 255 })
}

/// Полоса для температуры (милли-°C). Порядок: led[0..2] — сотые (тускло),
/// led[2..5] — целые (ярко), led[5] — знак.
pub fn temp_leds(temp_mc: Option<i32>) -> [Rgb; LEDS] {
    let Some(mc) = temp_mc else {
        return [BLACK; LEDS];
    };
    let t = mc as f32 / 1000.0;
    let dim = temp_color(t, true);
    let bright = temp_color(t, false);
    let sign = if t < 0.0 { BLUE_VIOLET } else { RED };
    [dim, dim, bright, bright, bright, sign]
}

/// Фон режима часов (по умолчанию выключен, как в оригинале).
pub fn clock_leds(bg: Option<Rgb>) -> [Rgb; LEDS] {
    [bg.unwrap_or(BLACK); LEDS]
}

// ---------------------------------------------------------------------------
// Цифры для индикаторов
// ---------------------------------------------------------------------------

/// Погашенная лампа (любой код, кроме 0..9, трактуется как гашение).
pub const BLANK: u8 = 10;

/// `%6d` от оригинала: значение прижато вправо, ведущие позиции погашены.
pub fn co2_digits(level: Option<i32>) -> [u8; 6] {
    let mut out = [BLANK; 6];
    let Some(level) = level.filter(|v| *v >= 0) else {
        return out;
    };
    let mut n = level.min(999_999) as u32;
    let mut i = 6;
    loop {
        i -= 1;
        out[i] = (n % 10) as u8;
        n /= 10;
        if n == 0 || i == 0 {
            break;
        }
    }
    out
}

/// `" % 5d"` от оригинала: позиция 0 пустая, сотые в позициях 4..5.
/// 30.87 °C → `[_, _, 3, 0, 8, 7]`.
pub fn temp_digits(temp_mc: Option<i32>) -> [u8; 6] {
    let mut out = [BLANK; 6];
    let Some(mc) = temp_mc else {
        return out;
    };
    let centi = ((mc.unsigned_abs() + 5) / 10).min(9_999) as u32;
    let mut n = centi;
    let mut i = 6;
    while i > 1 {
        i -= 1;
        out[i] = (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out
}


// ---------------------------------------------------------------------------
// Таблица цвета по месяцу (оригинал: DateDisplayer.cpp, month2Color[tm_mon])
// ---------------------------------------------------------------------------

/// 12 цветов, индекс 0 = январь ... 11 = декабрь; все 6 диодов одним цветом.
pub const MONTH_COLORS: [Rgb; 12] = [
    (0x00, 0x00, 0xff), // январь
    (0x00, 0xbf, 0xff), // февраль
    (0x00, 0xff, 0xff), // март
    (0x99, 0xff, 0xe6), // апрель
    (0x00, 0xff, 0x00), // май
    (0xff, 0xff, 0x99), // июнь
    (0xff, 0x00, 0x00), // июль
    (0xff, 0xb3, 0x66), // август
    (0xcc, 0xcc, 0x00), // сентябрь
    (0x66, 0x33, 0x00), // октябрь
    (0xa3, 0x00, 0xcc), // ноябрь
    (0x77, 0x33, 0xff), // декабрь
];

/// Цвет месяца; вне 1..12 — чёрный (без подсветки).
pub fn month_color(month: u8) -> Rgb {
    if (1..=12).contains(&month) {
        MONTH_COLORS[(month - 1) as usize]
    } else {
        BLACK
    }
}

/// Подсветка режима «дата»: вся полоса цветом месяца.
pub fn date_leds(month: u8) -> [Rgb; LEDS] {
    [month_color(month); LEDS]
}

/// 0xRRGGBB -> (R,G,B) и обратно.
pub fn rgb_from_u32(v: u32) -> Rgb {
    ((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub fn rgb_to_u32(c: Rgb) -> u32 {
    ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32
}

pub fn rgb_hex(c: Rgb) -> alloc::string::String {
    alloc::format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

// ---------------------------------------------------------------------------
// Эффекты подсветки часов (в оригинале их нет — новая фича)
// ---------------------------------------------------------------------------

pub const EFFECT_OFF: u8 = 0;
pub const EFFECT_BREATHING: u8 = 1;
pub const EFFECT_WHEEL: u8 = 2;
pub const EFFECT_WAVE: u8 = 3;

pub const BREATH_PERIOD_MS: u32 = 4_000;
pub const WHEEL_PERIOD_MS: u32 = 24_000;
pub const WAVE_PERIOD_MS: u32 = 3_000;

/// Пользовательская настройка эффектов. Частота — процент от базовой скорости,
/// амплитуда — максимальная яркость эффекта относительно выбранного цвета.
pub const FREQ_PCT_MIN: u8 = 10;
pub const FREQ_PCT_MAX: u8 = 200;
pub const FREQ_PCT_DEFAULT: u8 = 100;
pub const AMP_PCT_MIN: u8 = 0;
pub const AMP_PCT_MAX: u8 = 100;
pub const AMP_PCT_DEFAULT: u8 = 100;

pub fn effect_name(e: u8) -> &'static str {
    match e {
        EFFECT_BREATHING => "breathing",
        EFFECT_WHEEL => "wheel",
        EFFECT_WAVE => "wave",
        _ => "off",
    }
}

pub fn effect_from_name(n: &str) -> Option<u8> {
    match n {
        "off" => Some(EFFECT_OFF),
        "breathing" => Some(EFFECT_BREATHING),
        "wheel" => Some(EFFECT_WHEEL),
        "wave" => Some(EFFECT_WAVE),
        _ => None,
    }
}

/// sin на [-pi,pi] (в core нет libm; ряд Тейлора, ошибка < 1e-5 на этом диапазоне).
fn sinf(x: f32) -> f32 {
    const PI: f32 = 3.14159265;
    const TWO_PI: f32 = 6.28318531;
    let mut a = x % TWO_PI;
    if a > PI {
        a -= TWO_PI;
    } else if a < -PI {
        a += TWO_PI;
    }
    let x2 = a * a;
    // Тейлор до x^17: на [-pi,pi] ошибка < 1e-6.
    let mut term = a;
    let mut sum = a;
    for n in 1..=8u32 {
        term = -term * x2 / ((2 * n) as f32 * (2 * n + 1) as f32);
        sum += term;
    }
    sum
}

fn scale(c: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    (
        (c.0 as f32 * k) as u8,
        (c.1 as f32 * k) as u8,
        (c.2 as f32 * k) as u8,
    )
}

/// Кадр подсветки часов: статический цвет или один из эффектов (настройки по
/// умолчанию; совместимость со старым вызовом).
pub fn clock_effect(base: Rgb, effect: u8, t_ms: u32) -> [Rgb; LEDS] {
    clock_effect_tuned(
        base,
        effect,
        t_ms,
        FREQ_PCT_DEFAULT,
        AMP_PCT_DEFAULT,
    )
}

/// Период эффекта при пользовательской частоте в процентах от базовой скорости.
fn effect_period(base_ms: u32, freq_pct: u8) -> u32 {
    let f = freq_pct.clamp(FREQ_PCT_MIN, FREQ_PCT_MAX) as u64;
    ((base_ms as u64 * 100) / f).max(1) as u32
}

/// Кадр подсветки часов: статический цвет или один из эффектов.
/// `t_ms` — время с начала работы (монотонные миллисекунды).
/// `freq_pct` — скорость 10..200 % от базового периода; `amp_pct` — максимальная
/// яркость 0..100 % эффекта. Для выключенного эффекта амплитуда не применяется:
/// подсветка показывает выбранный цвет целиком.
pub fn clock_effect_tuned(
    base: Rgb,
    effect: u8,
    t_ms: u32,
    freq_pct: u8,
    amp_pct: u8,
) -> [Rgb; LEDS] {
    let freq_pct = freq_pct.clamp(FREQ_PCT_MIN, FREQ_PCT_MAX);
    let amp_pct = amp_pct.clamp(AMP_PCT_MIN, AMP_PCT_MAX);
    let max_brightness = amp_pct as f32 / 100.0;

    match effect {
        EFFECT_BREATHING => {
            let period = effect_period(BREATH_PERIOD_MS, freq_pct);
            let ph = (t_ms % period) as f32 / period as f32 * 6.28318531;
            // 0.10 .. 1.00: почти гасит и разгорается, затем амплитуда-яркость.
            let k = (0.55 + 0.45 * sinf(ph)) * max_brightness;
            [scale(base, k); LEDS]
        }
        EFFECT_WHEEL => {
            let period = effect_period(WHEEL_PERIOD_MS, freq_pct);
            // колесо FastLED: hue 0..255 за полный оборот
            let h = (((t_ms % period) * 255) / period) as u8;
            [scale(hsv2rgb(h, 255, 255), max_brightness); LEDS]
        }
        EFFECT_WAVE => {
            let period = effect_period(WAVE_PERIOD_MS, freq_pct);
            let ph = (t_ms % period) as f32 / period as f32 * 6.28318531;
            core::array::from_fn(|i| {
                let s = 0.5 + 0.5 * sinf(ph + i as f32 * 3.14159265 / 3.0);
                scale(base, (0.30 + 0.70 * s) * max_brightness)
            })
        }
        _ => [base; LEDS],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leds_eq(a: [Rgb; LEDS], b: [Rgb; LEDS]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| x == y)
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv2rgb(0, 255, 255), RED);
        assert_eq!(hsv2rgb(96, 255, 255), GREEN);
        assert_eq!(hsv2rgb(160, 255, 255), BLUE);
        assert_eq!(hsv2rgb(96, 0, 255), (255, 255, 255));
        assert_eq!(hsv2rgb(0, 255, 0), BLACK);
    }

    #[test]
    fn ln_matches_reference() {
        for (x, want) in [(1.0f32, 0.0f32), (2.718_282, 1.0), (10.0, 2.302_585), (100.0, 4.605_17)] {
            let got = ln(x);
            assert!((got - want).abs() < 1e-3, "ln({}) = {}, ждали {}", x, got, want);
        }
        assert!(ln(0.0) < -10.0);
    }

    #[test]
    fn co2_low_is_two_green() {
        // Как в оригинале: count=1 при <= и цикл `i <= count` → 2 светодиода.
        let l = co2_leds(Some(380));
        assert_eq!(l[5], GREEN);
        assert_eq!(l[4], GREEN);
        assert_eq!(l[3], BLACK);
    }

    #[test]
    fn co2_mid_gradient_and_count() {
        let l = co2_leds(Some(1200)); // rel=800 → count=2, over=160
        assert_eq!(l[5], l[4]); // три полных — одинаковый цвет
        assert_eq!(l[4], l[3]);
        assert_ne!(l[5], BLACK);
        // догоняющий тусклее полного, но не чёрный
        assert!(l[2].1 <= l[3].1);
        assert_eq!(l[1], BLACK);
        assert_eq!(l[0], BLACK);
        // тон ближе к красному, чем зелёный: красная составляющая выросла
        assert!(l[5].0 > 100);
    }

    #[test]
    fn co2_high_all_red_and_fades_out() {
        let dark = co2_leds(Some(3000));
        assert!(dark.iter().all(|c| c.0 == 0 || c.0 < 120), "при 3000 ppm яркость уже низкая: {:?}", dark);
        let all = co2_leds(Some(2050));
        assert!(all.iter().all(|c| *c != BLACK));
        let none = co2_leds(Some(20_000));
        assert_eq!(none, [BLACK; LEDS], "выше ~6900 ppm полоса гаснет");
    }

    #[test]
    fn co2_unknown_off() {
        assert_eq!(co2_leds(None), [BLACK; LEDS]);
        assert_eq!(co2_leds(Some(-1)), [BLACK; LEDS]);
    }

    #[test]
    fn temp_layout_and_sign() {
        let warm = temp_leds(Some(30_870));
        assert_eq!(warm[0], warm[1]); // сотые — тускло
        assert_eq!(warm[2], warm[3]);
        assert_eq!(warm[3], warm[4]);
        assert_eq!(warm[4], warm[2]); // целые — ярко
        assert_eq!(warm[5], RED); // знак плюс
        assert!(warm[2].1 > warm[0].1, "яркий вариант светлее тусклого");

        let cold = temp_leds(Some(-5_000));
        assert_eq!(cold[5], BLUE_VIOLET);
        assert_eq!(cold[2], BLUE); // <= 0 °C — чистый синий
    }

    #[test]
    fn temp_unknown_off() {
        assert_eq!(temp_leds(None), [BLACK; LEDS]);
    }

    #[test]
    fn digits_co2_right_aligned() {
        assert_eq!(co2_digits(Some(740)), [BLANK, BLANK, BLANK, 7, 4, 0]);
        assert_eq!(co2_digits(Some(1234)), [BLANK, BLANK, 1, 2, 3, 4]);
        assert_eq!(co2_digits(None), [BLANK; 6]);
        assert_eq!(co2_digits(Some(0)), [BLANK, BLANK, BLANK, BLANK, BLANK, 0]);
    }

    #[test]
    fn digits_temp_hundredths() {
        assert_eq!(temp_digits(Some(30_870)), [BLANK, BLANK, 3, 0, 8, 7]);
        assert_eq!(temp_digits(Some(30_860)), [BLANK, BLANK, 3, 0, 8, 6]);
        assert_eq!(temp_digits(Some(29_250)), [BLANK, BLANK, 2, 9, 2, 5]);
        assert_eq!(temp_digits(Some(-4_500)), [BLANK, BLANK, BLANK, 4, 5, 0]);
        assert_eq!(temp_digits(Some(5_000)), [BLANK, BLANK, BLANK, 5, 0, 0]);
        assert_eq!(temp_digits(None), [BLANK; 6]);
    }

    #[test]
    fn clock_leds_default_off() {
        assert_eq!(clock_leds(None), [BLACK; LEDS]);
        assert!(leds_eq(clock_leds(Some(RED)), [RED; LEDS]));
    }

    #[test]
    fn month_table_matches_original() {
        // 1-в-1 с DateDisplayer.cpp
        assert_eq!(month_color(1), (0x00, 0x00, 0xff));
        assert_eq!(month_color(5), (0x00, 0xff, 0x00));
        assert_eq!(month_color(7), (0xff, 0x00, 0x00));
        assert_eq!(month_color(11), (0xa3, 0x00, 0xcc));
        assert_eq!(month_color(12), (0x77, 0x33, 0xff));
        assert_eq!(month_color(0), BLACK);
        assert_eq!(month_color(13), BLACK);
        assert_eq!(date_leds(7), [(0xff, 0x00, 0x00); LEDS]);
    }

    #[test]
    fn color_u32_roundtrip() {
        let c = rgb_from_u32(0x1a2b3c);
        assert_eq!(c, (0x1a, 0x2b, 0x3c));
        assert_eq!(rgb_to_u32(c), 0x1a2b3c);
        assert_eq!(rgb_hex(c), "#1a2b3c");
    }

    #[test]
    fn sin_matches_known_values() {
        assert!((sinf(0.0)).abs() < 1e-6);
        assert!((sinf(1.5707963) - 1.0).abs() < 1e-4);
        assert!((sinf(3.14159265)).abs() < 1e-4);
        assert!((sinf(-1.5707963) + 1.0).abs() < 1e-4);
    }

    #[test]
    fn effect_off_is_static() {
        let c = (0x12, 0x34, 0x56);
        for t in [0u32, 700, 2_000, 9_999] {
            assert_eq!(clock_effect(c, EFFECT_OFF, t), [c; LEDS]);
        }
    }

    #[test]
    fn breathing_between_dark_and_full() {
        let c = (100u8, 200u8, 50u8);
        let mut min = 255u8;
        let mut max = 0u8;
        for t in (0..BREATH_PERIOD_MS).step_by(50) {
            let f = clock_effect(c, EFFECT_BREATHING, t);
            let v = f[0].0;
            min = min.min(v);
            max = max.max(v);
        }
        assert!(min < 20, "дыхание почти гасит: min={min}");
        assert!(max > 95, "дыхание доходит до полного: max={max}");
    }

    #[test]
    fn wheel_covers_red_and_blue() {
        let f0 = clock_effect(BLACK, EFFECT_WHEEL, 0);
        assert_eq!(f0[0], RED);
        let f_blue = clock_effect(BLACK, EFFECT_WHEEL, WHEEL_PERIOD_MS * 160 / 255);
        assert_eq!(f_blue[0].0, 0);
        assert!(f_blue[0].2 > 200, "около синей точки: {:?}", f_blue[0]);
        assert_eq!(hsv2rgb(160, 255, 255), BLUE);
        // полный оборот возвращает к красному
        let f_end = clock_effect(BLACK, EFFECT_WHEEL, WHEEL_PERIOD_MS);
        assert_eq!(f_end[0], RED);
    }

    #[test]
    fn wheel_has_no_color_jump() {
        fn max_delta(a: Rgb, b: Rgb) -> i32 {
            (a.0 as i32 - b.0 as i32)
                .abs()
                .max((a.1 as i32 - b.1 as i32).abs())
                .max((a.2 as i32 - b.2 as i32).abs())
        }

        let mut worst = 0;
        let mut prev = hsv2rgb(0, 255, 255);
        for h in 1..=255 {
            let next = hsv2rgb(h, 255, 255);
            worst = worst.max(max_delta(prev, next));
            prev = next;
        }

        // FastLED на каждом шаге hue двигает каналы маленькими порциями.
        // 85/32 ≈ 2,6, плюс целочисленные ступеньки scale8().
        assert!(worst <= 8, "прыжок цвета на шаге колеса: {}", worst);

        // Именно этот стык видел пользователь: фиолетовый/красный в конце круга.
        let seam = max_delta(hsv2rgb(255, 255, 255), hsv2rgb(0, 255, 255));
        assert!(seam <= 8, "стык 255→0: {}", seam);
    }

    #[test]
    fn wave_has_gradient_not_uniform() {
        let f = clock_effect(RED, EFFECT_WAVE, 500);
        assert!(f.iter().any(|c| *c != f[0]), "волна даёт разные яркости вдоль полосы");
        assert!(f.iter().all(|c| c.0 > 0 && c.1 == 0 && c.2 == 0), "градиент по красному");
        assert!(f[0].0 > 200, "начало полосы почти полное: {}", f[0].0);
    }

    #[test]
    fn effect_names_roundtrip() {
        for e in [EFFECT_OFF, EFFECT_BREATHING, EFFECT_WHEEL, EFFECT_WAVE] {
            assert_eq!(effect_from_name(effect_name(e)), Some(e));
        }
        assert_eq!(effect_from_name("nope"), None);
    }

    #[test]
    fn tuning_period_changes_with_frequency_pct() {
        assert_eq!(effect_period(BREATH_PERIOD_MS, 100), 4_000);
        assert_eq!(effect_period(BREATH_PERIOD_MS, 50), 8_000);
        assert_eq!(effect_period(BREATH_PERIOD_MS, 10), 40_000);
        assert_eq!(effect_period(WHEEL_PERIOD_MS, 200), 12_000);
        assert_eq!(effect_period(WAVE_PERIOD_MS, 200), 1_500);
    }

    #[test]
    fn tuning_changes_effect_phase_speed() {
        let c = (200u8, 0u8, 0u8);
        let normal = clock_effect_tuned(c, EFFECT_BREATHING, 3_000, 100, 100);
        let slow = clock_effect_tuned(c, EFFECT_BREATHING, 3_000, 50, 100);
        // При периоде 8 с точка t=3000 ещё на подъёме, а при 4 с уже на спаде.
        assert!(slow[0].0 > normal[0].0, "частота 50 % должна отставать: {normal:?} {slow:?}");
    }

    #[test]
    fn amplitude_caps_maximum_brightness() {
        let c = (255u8, 128u8, 64u8);

        let full = clock_effect_tuned(BLACK, EFFECT_WHEEL, 0, 100, 100);
        assert_eq!(full[0], RED);

        let half = clock_effect_tuned(BLACK, EFFECT_WHEEL, 0, 100, 50);
        assert!(120 <= half[0].0 && half[0].0 <= 130, "колесо 50 %: {:?}", half[0]);
        assert_eq!(half[0].1, 0);
        assert_eq!(half[0].2, 0);

        let off = clock_effect_tuned(BLACK, EFFECT_WHEEL, 0, 100, 0);
        assert_eq!(off[0], BLACK);

        let static_full = clock_effect_tuned(c, EFFECT_OFF, 0, 100, 50);
        assert_eq!(static_full[0], c);

        let full_wave = clock_effect_tuned(c, EFFECT_WAVE, 500, 100, 100);
        let half_wave = clock_effect_tuned(c, EFFECT_WAVE, 500, 100, 50);
        assert!(half_wave[0].0 < full_wave[0].0 && half_wave[0].0 > 0);
    }
}
