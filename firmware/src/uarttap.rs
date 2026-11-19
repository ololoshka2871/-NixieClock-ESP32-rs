//! Неблокирующий вывод в UART0 для лог-хука.
//!
//! Зачем не `esp_println`: на ESP32 он пишет каждый байт ROM-функцией
//! `uart_tx_one_char`, которая КРУТИТСЯ, пока в 128-байтном TX-FIFO нет места.
//! Строка лога ~180 байт => 4..15 мс ожидания прямо в вызывающем коде (а лог
//! пишется и из обработчиков, и из середины сетевых операций). На 1.27 Мслов/с
//! у I2S кадр идёт 2.5 мс, у WiFi MAC — свои микросекундные дедлайны, так что
//! такие паузы ломают и картинку, и радио.
//!
//! Здесь пишем прямо в FIFO и **выбрасываем** хвост, если FIFO полон: лог
//! никогда не ждёт. Для отладки через serial этого достаточно (при 115200
//! теряются только всплески), а коллектор всё равно получает полный текст по
//! WiFi. Порядок байт в строке сохраняется, пока FIFO не переполнится.
//!
//! Паника (`crashlog.rs`) по-прежнему использует `esp_println`: там важнее
//! гарантированно вытолкнуть сообщение, а не скорость.

/// UART0: `UART_FIFO_REG` (offset 0x00) и `UART_STATUS_REG` (0x1C).
const UART0_BASE: usize = 0x3FF4_0000;
const FIFO_LIMIT: u32 = 120; // из 128: два байта уходят, пока мы пишем слово

/// Сколько байт выброшено из-за полного FIFO (для диагностики).
pub static DROPPED_BYTES: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

#[inline]
fn fifo_count() -> u32 {
    unsafe { core::ptr::read_volatile((UART0_BASE + 0x1C) as *const u32) >> 16 & 0xFF }
}

/// Best-effort запись: никогда не блокируется, хвост может потеряться.
pub fn write(bytes: &[u8]) {
    use core::sync::atomic::Ordering;
    let fifo = (UART0_BASE) as *mut u32;
    let mut lost = 0u32;
    for &b in bytes {
        if fifo_count() >= FIFO_LIMIT {
            lost = lost.wrapping_add(1);
            continue;
        }
        unsafe { core::ptr::write_volatile(fifo, b as u32) };
    }
    if lost != 0 {
        DROPPED_BYTES.fetch_add(lost, Ordering::Relaxed);
    }
}

/// Строка целиком (дописывает `\n`).
pub fn line(s: &str) {
    write(s.as_bytes());
    write(b"\n");
}
