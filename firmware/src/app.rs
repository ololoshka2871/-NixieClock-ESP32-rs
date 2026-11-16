//! Бизнес-часть приложения: демо-задача + монитор программного watchdog.
//! Замените демо на свою логику, переиспользуемое выносите в крейт clock-core.

use clock_core::Watchdog;
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    mutex::Mutex,
};
use embassy_time::{Duration, Instant, Timer};

/// Период программного watchdog приложения: рабочий цикл обязан кормить его
/// чаще. Монитор логирует пропуски (зависшая задача видна в логах на коллекторе).
const APP_WD_PERIOD_MS: u64 = 10_000;

static APP_WD: Mutex<CriticalSectionRawMutex, Watchdog> =
    Mutex::new(Watchdog::new(APP_WD_PERIOD_MS, 0));

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// Рабочая задача приложения (демо): каждые 2 с кормит watchdog, каждые 5 циклов
/// пишет heartbeat с битом моргания из pure-логики clock-core.
#[embassy_executor::task]
pub async fn app_task() {
    let mut cycle: u64 = 0;
    loop {
        Timer::after(Duration::from_secs(2)).await;
        cycle += 1;
        APP_WD.lock().await.feed(now_ms());
        hangdetect::beat(&hangdetect::CYCLE);
        if cycle % 5 == 0 {
            let led = clock_core::blink_pattern(cycle / 5, &[true, false, false, false]);
            log::info!(
                "app: cycle {} led={} watchdog fed (period {}ms)",
                cycle,
                led as u8,
                APP_WD_PERIOD_MS
            );
        }
    }
}

/// Монитор программного watchdog: раз в 3 с проверяет дедлайн рабочей задачи.
#[embassy_executor::task]
pub async fn watchdog_monitor_task() {
    loop {
        Timer::after(Duration::from_secs(3)).await;
        let (missed, misses) = {
            let mut wd = APP_WD.lock().await;
            (wd.check(now_ms()), wd.misses())
        };
        if missed {
            log::warn!("watchdog: app task missed deadline, misses={}", misses);
        }
    }
}
