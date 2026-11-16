//! Indication M5: I2S1 parallel 8-bit + DMA, CPU only at chunk boundaries.
//!
//! Bus layout matches the original IDF project exactly (main/Nixie.cpp):
//! lane order D1A..D4A,D1B..D4B = GPIO 27/26/25/33, 32/12/18/19 (D2B is
//! GPIO12 on this board — a physical jumper pins D2B to GPIO12, confirmed
//! by scope 2026-11-16; the original IDF firmware drove GPIO12 too. The
//! strap only matters at reset where the TTL decoder input is Hi-Z).
//! Byte sample bits 0..3 = bus A code, bits 4..7 = bus B code (ic74141
//! encoder output). TG strobes are plain outputs (active HIGH).
//!
//! Why strobes are not I2S lanes: 16-bit parallel mode needs 16 data lanes
//! + a write-strobe; the board only has 11 signal GPIOs and every remaining
//! free GPIO is already earmarked (button, NeoPixel, SQW, console). 8-bit
//! mode on I2S1 uses exactly the 8 bus GPIOs plus write-strobe GPIO3.
//!
//! Race-free scheme without any strobe lane: every slot transfer ends with a
//! BLANK run (both decoders code 10 -> no output at all), and TG pins are
//! only rewritten during a fully dark window. A strobe write can therefore
//! never illuminate a wrong digit, whatever the CPU latency between chunks.
//! Slot length is held by DMA itself (1056 bytes at 1.27 MHz = ~831 us per
//! slot, ~400 Hz refresh like the original); WiFi load can only lengthen the
//! dark gap between chunks, never glitch a digit.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use embassy_executor::Spawner;

use crate::hangdetect;
use embassy_time::{Instant, Timer};
use esp_hal::i2s::parallel::{I2sParallel, TxEightBits};
use esp_hal::time::Rate;
use esp_hal::{
    dma::DmaTxBuf,
    dma_tx_buffer,
    gpio::{Level, Output, OutputConfig},
    peripherals,
};

use clock_core::display as display_core;

use crate::clock;

/// Byte rate on the bus: 1.27 MHz -> 787 ns per byte (probe FINDINGS).
const SAMPLE_HZ: u32 = 1_270_000;
/// Digit bytes per slot: 1056 * 787 ns = ~831 us -> 400 Hz refresh.
const SLOT_BYTES: usize = 1056;
/// Forced dark gap after every slot (both decoders blanked). Multiples of 4
/// because the 8-bit DMA interleave works on 4-byte groups.
const BLANK_BYTES: usize = 32;

/// Тест-режим индикации (диагностика M5.1): ставится сервером через
/// /display/test, читается display_task каждый кадр. Семантика —
/// clock_core::display::test_samples (0 = выключен).
pub static TEST_MODE: AtomicU8 = AtomicU8::new(0);
/// Стендовый тумблер: /display/off останавливает всю индикацию (DMA/TG молчат),
/// полезный для бисекции крашей; /display/on возобновляет.
pub static DISPLAY_ON: AtomicBool = AtomicBool::new(true);
/// Стендовый тумблер: noblank=1 убирает blank-паузу (данные ламп постоянно
/// на всех 8 линиях между слотами) — удобно смотреть осциллом.
pub static NO_BLANK: AtomicBool = AtomicBool::new(false);

/// Rebuild the peripheral pair after a wedged transfer. Only reachable when
/// the classic-ESP32 I2S flags lied to us: the dropped transfer's Drop has
/// already stopped I2S and the DMA channel, so re-taking the (stateless ZST)
/// peripheral handles via steal() is a clean restart of the data path. TG
/// outputs live in the task and are never part of this rebuild.
fn rebuild(
    par: &mut Option<I2sParallel<'static, esp_hal::Async>>,
    buf: &mut Option<DmaTxBuf>,
) {
    // esp_hal::Peripherals re-export is private; the macro-generated struct
    // lives in esp_hal::peripherals and steal() is unsafe by design (we only
    // rebuild after the previous transfer's Drop has stopped the peripherals).
    let p = unsafe { esp_hal::peripherals::Peripherals::steal() };
    let pins = TxEightBits::new(
        p.GPIO27, p.GPIO26, p.GPIO25, p.GPIO33, p.GPIO32, p.GPIO12, p.GPIO18, p.GPIO19,
    );
    *par = Some(
        I2sParallel::new(p.I2S1, p.DMA_I2S1, Rate::from_hz(SAMPLE_HZ), pins, p.GPIO3).into_async(),
    );
    *buf = Some(dma_tx_buffer!(SLOT_BYTES + BLANK_BYTES).unwrap());
}

async fn send_chunk(
    par: &mut Option<I2sParallel<'static, esp_hal::Async>>,
    buf: &mut Option<DmaTxBuf>,
    byte: u8,
    len: usize,
    errs: &mut u32,
) {
    let (Some(p), Some(mut b)) = (par.take(), buf.take()) else {
        *errs += 1;
        rebuild(par, buf);
        return;
    };
    {
        let s = b.as_mut_slice();
        let (head, _) = s.split_at_mut(len);
        head.fill(byte);
    }
    match p.send(b) {
        Ok(mut xfer) => {
            hangdetect::beat(&hangdetect::FRAME);
            let mut done = match xfer.wait_for_done().await {
                Ok(()) => true,
                Err(e) => {
                    *errs += 1;
                    log::warn!("display DMA error: {:?}", e);
                    false
                }
            };
            // esp-hal 1.2.2 `wait()` busy-spins `while !tx_idle` (i2s/parallel.rs
            // tx_wait_done). The classic ESP32 tx_idle flag is the documented
            // flaky one: when it misses, that spin starves the embassy
            // executor, the RWDT bites in 30 s and the board reboots — the
            // exact crash loop seen on v0.6.0. So: poll the same flag through
            // a *cooperative* Timer first (bounded, always yields), and only
            // then call wait(), which will return at once. If the flag is
            // stuck, never enter the spin: drop (stops peripherals cleanly)
            // and rebuild.
            if done {
                let mut ticks = 0u32;
                while !xfer.is_done() && ticks < 400 {
                    Timer::after_micros(250).await;
                    ticks += 1;
                }
                done = xfer.is_done();
            }
            if done {
                let (p2, b2) = xfer.wait();
                *par = Some(p2);
                *buf = Some(b2);
            } else {
                *errs += 1;
                log::warn!("display: tx_idle stuck after transfer, rebuilding I2S");
                drop(xfer);
                rebuild(par, buf);
            }
        }
        Err((e, p2, b2)) => {
            *errs += 1;
            log::warn!("display DMA send failed: {:?}", e);
            *par = Some(p2);
            *buf = Some(b2);
            // Never hot-loop a failing send(): keep the executor fed so the
            // watchdog task, HTTP and radios stay alive even if I2S is dead.
            Timer::after_millis(20).await;
        }
    }
}

/// HHMMSS on six IN-14 lamps via 3 multiplexed slots, DMA-driven.
#[allow(clippy::too_many_arguments)]
#[embassy_executor::task]
pub async fn display_task(
    i2s: peripherals::I2S1<'static>,
    dma: peripherals::DMA_I2S1<'static>,
    pins: TxEightBits<'static>,
    ws: peripherals::GPIO3<'static>,
    tg13: peripherals::GPIO13<'static>,
    tg2: peripherals::GPIO2<'static>,
    tg14: peripherals::GPIO14<'static>,
) -> ! {
    let mut par = Some(I2sParallel::new(i2s, dma, Rate::from_hz(SAMPLE_HZ), pins, ws).into_async());

    let mut tg0 = Output::new(tg13, Level::Low, OutputConfig::default()); // lamp 0 (Ч1) + lamp 3 (М2)
    let mut tg1 = Output::new(tg2, Level::Low, OutputConfig::default()); // lamp 1 (Ч2) + lamp 4 (С1)
    let mut tg2 = Output::new(tg14, Level::Low, OutputConfig::default()); // lamp 2 (М1) + lamp 5 (С2)

    let mut buf = Some(dma_tx_buffer!(SLOT_BYTES + BLANK_BYTES).unwrap());

    let mut slot: usize = 0;
    let mut frame: u64 = 0;
    let mut errs: u32 = 0;
    let mut log_at = Instant::now();
    // Статистика длительности кадра (целая итерация слота): ловим мерцание —
    // min/avg/max по всем слотам за 60 с.
    let mut fmin: u64 = u64::MAX;
    let mut fmax: u64 = 0;
    let mut fsum: u64 = 0;
    let mut fn_cnt: u64 = 0;
    let mut last_epoch: u64 = u64::MAX;
    let mut last_mode: u8 = 0xFF;
    let mut samples = [10u8 | (10u8 << 4); 3]; // blank до первой секунды
    loop {
        let t0 = Instant::now();
        // Горячий путь без await CLOCK-мьютекса: epoch из атомарного кэша
        // (тикер refresh_cache), samples пересчитываются раз в секунду.
        let epoch = clock::epoch_cached();
        let mode = TEST_MODE.load(Ordering::Relaxed);
        if epoch != last_epoch || mode != last_mode {
            samples = match display_core::test_samples(mode, epoch) {
                Some(s) => s,
                None => {
                    let c = clock_core::time::epoch_to_civil(epoch);
                    display_core::slot_samples(&display_core::hhmmss_digits(
                        c.hour, c.min, c.sec,
                    ))
                }
            };
            last_epoch = epoch;
            last_mode = mode;
        }

        if !DISPLAY_ON.load(Ordering::Relaxed) {
            for tg in [&mut tg0, &mut tg1, &mut tg2] {
                tg.set_level(Level::Low);
            }
            Timer::after_millis(200).await;
            continue;
        }

        for tg in [&mut tg0, &mut tg1, &mut tg2] {
            tg.set_level(Level::Low);
        }
        if mode == 0 || !NO_BLANK.load(Ordering::Relaxed) {
            send_chunk(&mut par, &mut buf, display_core::BLANK_BYTE, BLANK_BYTES, &mut errs).await;
        }

        match slot {
            0 => tg0.set_level(Level::High),
            1 => tg1.set_level(Level::High),
            _ => tg2.set_level(Level::High),
        }
        send_chunk(&mut par, &mut buf, samples[slot], SLOT_BYTES, &mut errs).await;

        slot = (slot + 1) % 3;
        if slot == 0 {
            frame += 1;
        }
        let dt = t0.elapsed().as_micros();
        if dt < fmin {
            fmin = dt;
        }
        if dt > fmax {
            fmax = dt;
        }
        fsum += dt;
        fn_cnt += 1;
        if log_at.elapsed() >= embassy_time::Duration::from_secs(60) {
            log_at = Instant::now();
            log::info!(
                "display frames {} (slot bytes {}, blank bytes {}, DMA errors: {}, slot us min/avg/max: {}/{}/{}, test {})",
                frame,
                SLOT_BYTES,
                BLANK_BYTES,
                errs,
                fmin,
                fsum / fn_cnt.max(1),
                fmax,
                mode,
            );
            fmin = u64::MAX;
            fmax = 0;
            fsum = 0;
            fn_cnt = 0;
        }
    }
}

/// Claimed once from main: builds the pin bundle and spawns the task.
pub fn spawn(
    spawner: &Spawner,
    i2s: peripherals::I2S1<'static>,
    dma: peripherals::DMA_I2S1<'static>,
    gpio27: peripherals::GPIO27<'static>,
    gpio26: peripherals::GPIO26<'static>,
    gpio25: peripherals::GPIO25<'static>,
    gpio33: peripherals::GPIO33<'static>,
    gpio32: peripherals::GPIO32<'static>,
    gpio12: peripherals::GPIO12<'static>,
    gpio18: peripherals::GPIO18<'static>,
    gpio19: peripherals::GPIO19<'static>,
    ws: peripherals::GPIO3<'static>,
    tg13: peripherals::GPIO13<'static>,
    tg2: peripherals::GPIO2<'static>,
    tg14: peripherals::GPIO14<'static>,
) {
    let pins = TxEightBits::new(gpio27, gpio26, gpio25, gpio33, gpio32, gpio12, gpio18, gpio19);
    spawner
        .spawn(display_task(i2s, dma, pins, ws, tg13, tg2, tg14).unwrap());
}
