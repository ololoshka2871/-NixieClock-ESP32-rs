//! Crash-лог: текст паники пишется в NVS (ключ `settings/last_panic`),
//! печатается на UART и сбрасывается чип через software_reset.
//!
//! why: штатный обработчик esp-backtrace печатает бэктрейс в UART (коллектор
//! его не видит) и крутится в бесконечном цикле до RTC-WDT (~30 с) — в логах
//! collect'ора смерть выглядела как молчаливое SysRtcWdt без причины.
//! Наш обработчик: (1) причина+локация в NVS, (2) быстрый software_reset,
//! (3) на следующем буте dump_previous_panic() выкладывает текст в collector.
//!
//! Оговорка: если паника случилась внутри flash-операции или до
//! flashcell::init, запись в NVS не состоится — зависание до RWDT, как раньше.

use core::panic::PanicInfo;

use esp_nvs::Key;
use heapless::String;

const NS: Key = const { Key::from_str("settings") };
const K_PANIC: Key = const { Key::from_str("last_panic") };

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    use core::fmt::Write;
    let mut msg: String<240> = String::new();
    if let Some(loc) = info.location() {
        let _ = write!(&mut msg, "{}:{}: ", loc.file(), loc.line());
    }
    // На esp-тулчейне PanicInfo::message() -> PanicMessage (реализует Display).
    let _ = write!(&mut msg, "{}", info.message());
    // Для тех, кто сидит с serial-консолью.
    esp_println::println!("PANIC: {}", msg.as_str());
    // RTC slow memory пишется всегда и мгновенно: даже если запись в NVS
    // повесит ядро (flash-операция), причина уедет в следующий бут.
    crate::rtcmark::save_panic(msg.as_str());
    // Лучшее, что можно сделать без аллокаций и логгера:
    let saved = crate::settings::with_nvs(|nvs| nvs.set(&NS, &K_PANIC, msg.as_str()));
    esp_println::println!(
        "crashlog: NVS save ok={}",
        saved.map(|r| r.is_ok()).unwrap_or(false)
    );
    esp_hal::system::software_reset();
}

/// Вызывать после `flashcell::init()`: если прошлый бут умер по панике —
/// логируем причину (лог-буфер доедет в collector после поднятия WiFi) и
/// очищаем ключ, чтобы не повторять на каждом буте.
pub fn dump_previous_panic() {
    let got = crate::settings::with_nvs(|nvs| {
        match nvs.get::<alloc::string::String>(&NS, &K_PANIC) {
            Ok(s) if !s.is_empty() => {
                let _ = nvs.set(&NS, &K_PANIC, "");
                Some(s)
            }
            _ => None,
        }
    });
    match got {
        Some(Some(s)) => log::error!("ПРОШЛЫЙ БУТ УМЕР ОТ ПАНИКИ: {}", s),
        Some(None) => {}
        None => log::warn!("crashlog: NVS недоступен, причину прошлой паники не прочитать"),
    }
}
