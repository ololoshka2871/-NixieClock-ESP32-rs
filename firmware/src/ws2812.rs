//! Подсветка: полоса WS2812 на GPIO15 под лампами (6 светодиодов, как в
//! оригинале — FastLED `NEOPIXEL` на том же пине).
//!
//! Драйвер — RMT с источником 80 МГц: одна 32-битная посылка на бит, кадр
//! 6×24 бита = 144 посылки, дальше канал сам вытягивает линию из своей RAM
//! (memsize 4 блока). Пауза между кадрами ≥ 50 мкс обеспечена самим темпом
//! (10 Гц) плюс idle-уровень Low — это и есть reset-импульс WS2812.
//!
//! Логика цветов — `clock_core::led` (хост-тесты): шкала CO2 400…2000 ppm,
//! шкала температуры 0…50 °C, обратный порядок светодиодов относительно ламп.

use embassy_time::{Duration, Timer};
use esp_hal::gpio::Level;
use esp_hal::rmt::{PulseCode, Rmt, TxChannelConfig, TxChannelCreator};
use esp_hal::time::Rate;

use clock_core::led;

/// WS2812: T0H/T0L = 0.35/0.90 мкс, T1H/T1L = 0.70/0.55 мкс (по 100 тактов 80 МГц).
const T0H: u16 = 28;
const T0L: u16 = 72;
const T1H: u16 = 56;
const T1L: u16 = 44;

/// Битов в кадре: 6 светодиодов × 24.
pub const BITS: usize = led::LEDS * 24;
/// Посылок в буфере: биты кадра + end-marker (без него RMT отказывается
/// передавать: `Error::EndMarkerMissing`).
pub const CODES: usize = BITS + 1;
/// Байт в кадре: 6 × 3 (G, R, B — порядок провода WS2812).
pub const FRAME_BYTES: usize = led::LEDS * 3;

/// Период пересчёта статического кадра: датчики обновляются раз в 10 с.
const PERIOD: Duration = Duration::from_millis(200);
/// Период для эффектов подсветки часов (30 Гц — достаточно для «дыхания»).
const PERIOD_ANIM: Duration = Duration::from_millis(30);

/// Кодирование байтов кадра в посылки RMT (MSB первым).
pub fn encode(bytes: &[u8; FRAME_BYTES]) -> [PulseCode; CODES] {
    const ONE: PulseCode = PulseCode::new(Level::High, T1H, Level::Low, T1L);
    const ZERO: PulseCode = PulseCode::new(Level::High, T0H, Level::Low, T0L);
    let mut out = [ZERO; CODES];
    out[CODES - 1] = PulseCode::end_marker();
    let mut i = 0;
    for b in bytes.iter() {
        let mut mask = 0x80u8;
        while mask != 0 {
            if b & mask != 0 {
                out[i] = ONE;
            }
            i += 1;
            mask >>= 1;
        }
    }
    out
}

/// Цвет подсветки часов: сохранённый пользователем, иначе цвет месяца.
fn clock_base(c: clock_core::time::Civil) -> led::Rgb {
    match crate::settings::clock_color() {
        Some(v) => led::rgb_from_u32(v),
        None => led::month_color(c.month),
    }
}

/// Текущий кадр под режим индикации (G, R, B на каждый светодиод).
pub fn current_frame(t_ms: u32) -> [u8; FRAME_BYTES] {
    let mode = crate::ui::mode();
    let c = crate::tz::civil(crate::clock::epoch_cached());
    let colors = match mode {
        crate::ui::MODE_CO2 => led::co2_leds(crate::sensors::co2_ppm_now()),
        crate::ui::MODE_TEMP => led::temp_leds(crate::sensors::temp_mc_now()),
        crate::ui::MODE_DATE => led::date_leds(c.month),
        _ => led::clock_effect(clock_base(c), crate::settings::led_effect(), t_ms),
    };
    let mut out = [0u8; FRAME_BYTES];
    for (i, (r, g, b)) in colors.iter().enumerate() {
        out[i * 3] = *g;
        out[i * 3 + 1] = *r;
        out[i * 3 + 2] = *b;
    }
    out
}

#[embassy_executor::task]
pub async fn led_task(
    rmt: esp_hal::peripherals::RMT<'static>,
    pin: esp_hal::peripherals::GPIO15<'static>,
) {
    let peripheral = match Rmt::new(rmt, Rate::from_mhz(80)) {
        Ok(p) => p,
        Err(e) => {
            log::error!("подсветка: RMT init failed: {:?}", e);
            return;
        }
    };
    let cfg = TxChannelConfig::default()
        .with_clk_divider(1)
        .with_idle_output(true)
        .with_idle_output_level(Level::Low)
        .with_memsize(4);
    let mut chan = match peripheral.channel0.configure_tx(&cfg).map(|c| c.with_pin(pin)) {
        Ok(c) => c,
        Err(e) => {
            log::error!("подсветка: канал RMT не настроен: {:?}", e);
            return;
        }
    };

    let mut last = [0xFFu8; FRAME_BYTES];
    let mut warned = false; // один warning за бут: иначе лог забивается
    let mut t_ms: u32 = 0;
    log::info!("подсветка: WS2812 на GPIO15, светодиодов: {}", led::LEDS);
    loop {
        let frame = current_frame(t_ms);
        let anim = crate::ui::mode() == crate::ui::MODE_CLOCK
            && crate::settings::led_effect() != led::EFFECT_OFF;
        let period = if anim { PERIOD_ANIM } else { PERIOD };
        if frame != last {
            let codes = encode(&frame);
            match chan.transmit(&codes[..]) {
                Ok(tx) => {
                    // Канал возвращается только при успехе: ошибку ожидания
                    // пережить нельзя, поэтому просто прекращаем мигать.
                    chan = match tx.wait() {
                        Ok(c) => c,
                        Err(e) => {
                            log::error!("подсветка: передача не завершилась: {:?}", e);
                            return;
                        }
                    };
                    last = frame;
                }
                Err((e, c)) => {
                    chan = c;
                    if !warned {
                        warned = true;
                        log::warn!("подсветка: кадр не отправлен: {:?}", e);
                    }
                }
            }
        }
        Timer::after(period).await;
        t_ms = t_ms.wrapping_add(period.as_millis() as u32);
    }
}
