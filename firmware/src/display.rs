//! Indication M5: I2S1 parallel 8-bit + DMA, CPU only at chunk boundaries.
//!
//! Bus layout matches the original IDF project exactly (main/Nixie.cpp):
//! lane order D1A..D4A,D1B..D4B = GPIO 27/26/25/33, 32/5/18/19 (D2B is
//! GPIO5 on this board; GPIO12 is the VDD_SDIO strap and stays untouched).
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

use embassy_executor::Spawner;
use embassy_time::Instant;
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

async fn send_chunk(
    mut par: I2sParallel<'static, esp_hal::Async>,
    mut buf: DmaTxBuf,
    byte: u8,
    len: usize,
) -> (I2sParallel<'static, esp_hal::Async>, DmaTxBuf) {
    {
        let s = buf.as_mut_slice();
        let (head, _) = s.split_at_mut(len);
        head.fill(byte);
    }
    match par.send(buf) {
        Ok(mut xfer) => match xfer.wait_for_done().await {
            Ok(()) => xfer.wait(),
            Err(e) => {
                log::warn!("display DMA error: {:?}", e);
                xfer.wait()
            }
        },
        Err((e, par, buf)) => {
            log::warn!("display DMA send failed: {:?}", e);
            (par, buf)
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
    let mut par = I2sParallel::new(i2s, dma, Rate::from_hz(SAMPLE_HZ), pins, ws).into_async();

    let mut tg0 = Output::new(tg13, Level::Low, OutputConfig::default()); // lamp 0 (Ч1) + lamp 3 (М2)
    let mut tg1 = Output::new(tg2, Level::Low, OutputConfig::default()); // lamp 1 (Ч2) + lamp 4 (С1)
    let mut tg2 = Output::new(tg14, Level::Low, OutputConfig::default()); // lamp 2 (М1) + lamp 5 (С2)

    let mut buf = dma_tx_buffer!(SLOT_BYTES + BLANK_BYTES).unwrap();

    let mut slot: usize = 0;
    let mut frame: u64 = 0;
    let mut log_at = Instant::now();
    loop {
        let epoch = clock::now_secs().await;
        let c = clock_core::time::epoch_to_civil(epoch);
        let samples = display_core::slot_samples(&display_core::hhmmss_digits(
            c.hour, c.min, c.sec,
        ));

        for tg in [&mut tg0, &mut tg1, &mut tg2] {
            tg.set_level(Level::Low);
        }
        let (par_, buf_) = send_chunk(par, buf, display_core::BLANK_BYTE, BLANK_BYTES).await;
        par = par_;
        buf = buf_;

        match slot {
            0 => tg0.set_level(Level::High),
            1 => tg1.set_level(Level::High),
            _ => tg2.set_level(Level::High),
        }
        let (par_, buf_) = send_chunk(par, buf, samples[slot], SLOT_BYTES).await;
        par = par_;
        buf = buf_;

        slot = (slot + 1) % 3;
        if slot == 0 {
            frame += 1;
        }
        if log_at.elapsed() >= embassy_time::Duration::from_secs(60) {
            log_at = Instant::now();
            log::info!(
                "display frames {} (slot {} bytes {}, blank {}, strobes TG0..2 low-active-HIGH)",
                frame,
                SLOT_BYTES,
                BLANK_BYTES,
                1
            );
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
    gpio5: peripherals::GPIO5<'static>,
    gpio18: peripherals::GPIO18<'static>,
    gpio19: peripherals::GPIO19<'static>,
    ws: peripherals::GPIO3<'static>,
    tg13: peripherals::GPIO13<'static>,
    tg2: peripherals::GPIO2<'static>,
    tg14: peripherals::GPIO14<'static>,
) {
    let pins = TxEightBits::new(gpio27, gpio26, gpio25, gpio33, gpio32, gpio5, gpio18, gpio19);
    spawner
        .spawn(display_task(i2s, dma, pins, ws, tg13, tg2, tg14).unwrap());
}
