//! M5: датчики. Три independent embassy-задачи, общее состояние — статический
//! `SENSORS` (const-init Mutex, как CLOCK в clock.rs — без StaticCell).
//!
//! - DS18x20 на GPIO4: hand-bitten 1-Wire мастер (в vendored registry нет
//!   ни одного 1-Wire crate; проверено зондом /work/probe_onewire). Тайминги
//!   slot'ами через Delay (busy-wait); в release-профиле Instant калиброван
//!   по циклам. Конверсия T (750 мс) — асинхронный Timer.sleep, шина в это
//!   время отпущена на pull-up, executor не блокируется.
//! - MH-Z19 CO2 на UART2 (RX=GPIO16, TX=GPIO17, 9600 8N1): async Uart
//!   (into_async сам регистрирует ISR-waker, см. зонд /work/probe_mhz19).
//! - АЦП-шины 3.3/5/12 В: ADC1 GPIO39/35/34 (input-only пины),
//!   калибровка по eFuse AdcCalLine -> read_oneshot отдаёт милливольты на пине;
//!   на классическом ESP32 нет async ADC, поэтому блокирующие one-shot'ы
//!   внутри таска (nb::block). Каналы читаются строго последовательно.
//!
//! Кодеки протоколов и пересчёты — в clock_core::sensors (хост-тесты).

use embassy_executor::task;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::{Read, Write};
use esp_hal::Async;
use esp_hal::Blocking;
use esp_hal::analog::adc::{Adc, AdcCalLine, AdcPin};
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Flex, InputConfig, OutputConfig, Pin, Pull};
use esp_hal::peripherals::{ADC1, GPIO34, GPIO35, GPIO39, GPIO4};
use esp_hal::uart::Uart;
use nb::block;

use clock_core::sensors as codec;

/// Текущие показания датчиков. None = ещё не измерено/датчик не отвечает.
#[derive(Clone, Copy)]
pub struct SensorData {
    /// DS18x20, миллиградусы Цельсия.
    pub temp_mc: Option<i32>,
    /// MH-Z19 CO2, ppm.
    pub co2_ppm: Option<u16>,
    /// Температура самого MH-Z19, миллиградусы.
    pub co2_temp_mc: Option<i32>,
    /// Шина 3.3 В, милливольты (GPIO39 × 5.7).
    pub v33_mv: Option<u32>,
    /// Шина 5 В, милливольты (GPIO35 × 10.1).
    pub v5_mv: Option<u32>,
    /// Шина 12 В, милливольты (GPIO34 × 25).
    pub v12_mv: Option<u32>,
}

impl SensorData {
    pub const fn new() -> Self {
        Self {
            temp_mc: None,
            co2_ppm: None,
            co2_temp_mc: None,
            v33_mv: None,
            v5_mv: None,
            v12_mv: None,
        }
    }
}

pub static SENSORS: Mutex<CriticalSectionRawMutex, SensorData> = Mutex::new(SensorData::new());

// ---------------- DS18x20: 1-Wire мастер (GPIO4) ----------------

/// Hand-bitten мастер по мотивам compile-зонда /work/probe_onewire (esp-hal 1.2.2).
/// Flex: TX = output-enable + drive low (open-drain), RX = output отключён,
/// линия на pull-up, сэмпл is_low(). Единственный владелец GPIO4 — эта задача.
struct OneWire {
    pin: Flex<'static>,
    delay: Delay,
}

impl OneWire {
    fn new(pin: impl Pin + 'static) -> Self {
        let mut pin = Flex::new(pin);
        // Idle-high: внешний ~4.7k pull-up предпочтителен; внутренний как страховка.
        pin.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
        pin.apply_output_config(&OutputConfig::default().with_drive_mode(DriveMode::OpenDrain));
        pin.set_high();
        pin.set_output_enable(true);
        pin.set_input_enable(true);
        Self { pin, delay: Delay::new() }
    }

    fn drive_low(&mut self) {
        self.pin.set_low();
        self.pin.set_output_enable(true);
    }

    fn release(&mut self) {
        self.pin.set_output_enable(false);
        self.delay.delay_micros(1);
    }

    /// Reset: >=480 мкс low, отпускание, presence-импульс через 70 мкс.
    fn reset(&mut self) -> bool {
        self.release();
        self.delay.delay_micros(50);
        self.drive_low();
        self.delay.delay_micros(500);
        self.release();
        self.delay.delay_micros(70);
        let presence = self.pin.is_low();
        self.delay.delay_micros(410);
        presence
    }

    fn write_bit(&mut self, bit: bool) {
        self.drive_low();
        if bit {
            self.delay.delay_micros(6);
            self.release();
            self.delay.delay_micros(59);
        } else {
            self.delay.delay_micros(60);
            self.release();
            self.delay.delay_micros(5);
        }
    }

    fn read_bit(&mut self) -> bool {
        self.drive_low();
        self.delay.delay_micros(2);
        self.release();
        self.delay.delay_micros(8);
        let v = self.pin.is_high();
        self.delay.delay_micros(50);
        v
    }

    fn write_byte(&mut self, b: u8) {
        for i in 0..8 {
            self.write_bit((b >> i) & 1 != 0);
        }
    }

    fn read_byte(&mut self) -> u8 {
        let mut b = 0u8;
        for i in 0..8 {
            if self.read_bit() {
                b |= 1u8 << i;
            }
        }
        b
    }
}

#[task]
pub async fn onewire_task(pin: GPIO4<'static>) {
    let mut ow = OneWire::new(pin);
    // Присутствие логируем только при изменении — по нему видно жив ли датчик,
    // не подключён ли, или битые провода. Цикл 10 с: удобно ловить осциллографом.
    let mut last_presence: Option<bool> = None;
    loop {
        Timer::after_secs(10).await;
        let mut temp_mc: Option<i32> = None;
        let mut sp: Option<[u8; 9]> = None;
        let mut reset2 = false;
        let present = ow.reset();
        if last_presence != Some(present) {
            log::info!("ds18x20: presence на GPIO4 = {}", present);
            last_presence = Some(present);
        }
        if present {
            ow.write_byte(codec::DS18_CMD_SKIP_ROM);
            ow.write_byte(codec::DS18_CMD_CONVERT);
            ow.release();
            // 12-битная конверсия — ждём асинхронно, шина высоко на pull-up.
            Timer::after_millis(750).await;
            reset2 = ow.reset();
            if reset2 {
                ow.write_byte(codec::DS18_CMD_SKIP_ROM);
                ow.write_byte(codec::DS18_CMD_READ_SCRATCH);
                // Канон ds18x20 datasheet: TL TH CONF SCRATCH RA R1 R2 CRC8 [EXTCRC]
                let mut buf = [0u8; 9];
                for b in buf.iter_mut() {
                    *b = ow.read_byte();
                }
                ow.release();
                sp = Some(buf);
                temp_mc = codec::ds18x20_scratchpad(&buf[..8].try_into().unwrap());
            }
        }
        match temp_mc {
            Some(mc) => log::info!("ds18x20: {}.{} C", mc / 1000, (mc % 1000).abs()),
            None => log::warn!(
                "ds18x20: нет чтения (presence={}, reset2={}, sp={:02X?})",
                present,
                reset2,
                sp.unwrap_or([0u8; 9])
            ),
        }
        SENSORS.lock().await.temp_mc = temp_mc;
    }
}

// ---------------- MH-Z19: CO2 на UART2 ----------------

#[task]
pub async fn mhz19_task(mut uart: Uart<'static, Async>) {
    let mut buf = [0u8; 9];
    // Датчику нужно несколько секунд на прогрев лампы после подачи питания.
    Timer::after_secs(20).await;
    loop {
        let cmd = codec::mhz19_command(codec::MHZ19_CMD_READ_PPM, [0, 0, 0, 0]);
        match uart.write_all(&cmd).await {
            Ok(()) => match with_timeout(Duration::from_secs(3), uart.read_exact(&mut buf)).await {
                Ok(Ok(())) => match codec::Mhz19Reading::parse(&buf, codec::MHZ19_CMD_READ_PPM) {
                    Some(r) => {
                        if r.status != 0 {
                            log::warn!("mhz19: status {} (калибровка/ошибка)", r.status);
                        }
                        log::info!("mhz19: {} ppm, датчик {} C", r.co2_ppm, r.sensor_temp_uc / 1_000_000);
                        let mut s = SENSORS.lock().await;
                        s.co2_ppm = Some(r.co2_ppm);
                        s.co2_temp_mc = Some(r.sensor_temp_uc / 1000);
                    }
                    None => log::warn!("mhz19: битый кадр: {:02X?}", buf),
                },
                Ok(Err(e)) => log::warn!("mhz19: read: {:?}", e),
                Err(_elapsed) => log::warn!("mhz19: нет ответа 3 с (датчик не подключён к UART2?)"),
            },
            Err(e) => log::warn!("mhz19: write: {:?}", e),
        }
        Timer::after_secs(2).await;
    }
}

// ---------------- АЦП: шины 3.3/5/12 В ----------------

/// Пины с eFuse-калибровкой (AdcCalLine): read_oneshot отдаёт милливольты на пине.
type CalPin<P> = AdcPin<P, ADC1<'static>, AdcCalLine<ADC1<'static>>>;

#[task]
pub async fn adc_task(
    mut adc: Adc<'static, ADC1<'static>, Blocking>,
    mut p33: CalPin<GPIO39<'static>>,
    mut p5: CalPin<GPIO35<'static>>,
    mut p12: CalPin<GPIO34<'static>>,
) {
    loop {
        Timer::after_secs(30).await;
        // SAR-конверсии строго по очереди (чужая конверсия = WouldBlock).
        let r33 = block!(adc.read_oneshot(&mut p33)).ok();
        let r5 = block!(adc.read_oneshot(&mut p5)).ok();
        let r12 = block!(adc.read_oneshot(&mut p12)).ok();
        let mut s = SENSORS.lock().await;
        s.v33_mv = r33.map(|mv| codec::pin_mv_to_rail_mv(mv, codec::RATIO_3V3_X1000));
        s.v5_mv = r5.map(|mv| codec::pin_mv_to_rail_mv(mv, codec::RATIO_5V_X1000));
        s.v12_mv = r12.map(|mv| codec::pin_mv_to_rail_mv(mv, codec::RATIO_12V_X1000));
        match (s.v33_mv, s.v5_mv, s.v12_mv) {
            (Some(a), Some(b), Some(c)) => log::info!("adc: 3v3={}mV 5v={}mV 12v={}mV", a, b, c),
            _ => log::warn!("adc: часть каналов не читается"),
        }
    }
}
