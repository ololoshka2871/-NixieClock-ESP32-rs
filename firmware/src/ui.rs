//! Кнопка BOOT (GPIO0, активный уровень 0) и режимы индикации.
//!
//! Режимы: часы → CO2 → температура → часы. Клик — короткое нажатие (< 1 с).
//! Через 5 с без нажатий индикация сама возвращается к часам (как
//! `DEFAULT_IDLE_TIMEOUT = 5_s` в оригинальном IDF-проекте).
//!
//! Режим — единственный `AtomicU8`: его читают и кадровый движок индикации
//! (какие цифры формировать), и задача подсветки (какие цвета выставлять).

use core::sync::atomic::{AtomicU8, Ordering};

use embassy_time::{Duration, Instant};

use esp_hal::gpio::{Input, InputConfig, Pull};

/// Часы (по умолчанию).
pub const MODE_CLOCK: u8 = 0;
/// Уровень CO2 на индикаторах + шкала на подсветке.
pub const MODE_CO2: u8 = 1;
/// Температура (`__XXYY`) + цветовая шкала на подсветке.
pub const MODE_TEMP: u8 = 2;

/// Возврат к часам после стольких секунд без нажатий.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Нажатие короче этого считается кликом.
pub const CLICK_MAX: Duration = Duration::from_millis(1_000);
/// Период опроса кнопки.
const POLL: Duration = Duration::from_millis(20);
/// Столько подряд одинаковых опросов — уже не дребезг.
const DEBOUNCE: u8 = 3;

static MODE: AtomicU8 = AtomicU8::new(MODE_CLOCK);

pub fn mode() -> u8 {
    MODE.load(Ordering::Relaxed)
}

pub fn set_mode(m: u8) {
    MODE.store(m, Ordering::Relaxed);
}

/// Следующий режим по кругу (часы → CO2 → температура → часы).
pub fn next_mode() -> u8 {
    let next = match mode() {
        MODE_CO2 => MODE_TEMP,
        MODE_TEMP => MODE_CLOCK,
        _ => MODE_CO2,
    };
    set_mode(next);
    next
}

pub fn mode_name(m: u8) -> &'static str {
    match m {
        MODE_CO2 => "CO2",
        MODE_TEMP => "температура",
        _ => "часы",
    }
}

#[embassy_executor::task]
pub async fn button_task(pin: esp_hal::peripherals::GPIO0<'static>) {
    let btn = Input::new(pin, InputConfig::default().with_pull(Pull::Up));
    let mut stable: u8 = if btn.is_low() { DEBOUNCE } else { 0 };
    let mut pressed = btn.is_low();
    let mut since_press = Instant::now();
    let mut last_activity = Instant::now();
    log::info!("кнопка BOOT: GPIO0, режимы часы/CO2/температура, возврат через 5 с");

    loop {
        let low = btn.is_low();
        // Дребезг: считаем состояние устойчивым после DEBOUNCE одинаковых опросов.
        if low {
            if stable < DEBOUNCE {
                stable += 1;
            }
        } else {
            stable = 0;
        }
        if !pressed && stable >= DEBOUNCE {
            pressed = true;
            since_press = Instant::now();
            last_activity = since_press;
        } else if pressed && stable == 0 {
            pressed = false;
            let held = since_press.elapsed();
            if held < CLICK_MAX {
                let m = next_mode();
                last_activity = Instant::now();
                log::info!("кнопка: режим {}", mode_name(m));
            }
        }

        if mode() != MODE_CLOCK && last_activity.elapsed() >= IDLE_TIMEOUT {
            set_mode(MODE_CLOCK);
            log::info!("кнопка: простой 5 с — возврат к часам");
        }
        embassy_time::Timer::after(POLL).await;
    }
}
