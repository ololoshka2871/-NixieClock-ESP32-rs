#![no_std]
#![no_main]

use core::cell::RefCell;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::CriticalSectionMutex;
use esp_hal::{
    clock::CpuClock,
    rng::Rng,
    timer::{timg::TimerGroup, PeriodicTimer},
    uart::Uart,
    Blocking,
};
use esp_println as _;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

extern crate alloc;

use esp_wifi::EspWifiController;
use nixieclock_esp32_rs as lib;

//-----------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct VoltageCalc {
    r1: f32,
    r2: f32,
}

impl VoltageCalc {
    pub const fn new((r1, r2): (f32, f32)) -> Self {
        Self { r1, r2 }
    }
}

impl lib::sensors::adc::VoltageCalculator<f32> for VoltageCalc {
    fn to_voltage(&self, raw_value: u16) -> f32 {
        // Assuming a 10-bit ADC with a reference voltage of 1.1V with a voltage divider
        (raw_value as f32) * (0.8 / 4096.0) * ((self.r1 + self.r2) / self.r2)
    }
}

struct GlobalContext {
    timer: PeriodicTimer<'static, Blocking>,

    count: u64,
}

impl GlobalContext {
    pub fn new(timer: PeriodicTimer<'static, Blocking>) -> Self {
        Self { timer, count: 0 }
    }

    #[inline(always)]
    pub fn clear_interrupt_flag(&mut self) {
        self.timer.clear_interrupt();
        self.count = self.count.wrapping_add(1);
    }

    pub fn get_count(&self) -> u64 {
        self.count
    }
}

//-----------------------------------------------------------------------------

static mut GLOBAL_CONTEXT: Option<CriticalSectionMutex<RefCell<GlobalContext>>> = None;

//-----------------------------------------------------------------------------

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg1 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timg1.timer0);

    defmt::info!("Embassy initialized!");

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let rng = Rng::new(peripherals.RNG);
    let esp_wifi_ctrl = &*lib::mk_static!(
        EspWifiController<'static>,
        esp_wifi::init(timg0.timer0, rng.clone(), peripherals.RADIO_CLK).unwrap()
    );

    let stack = lib::wifi::start_wifi(esp_wifi_ctrl, peripherals.WIFI, rng, &spawner)
        .await
        .expect("Failed to start wifi");

    let web_app = lib::web::WebApp::default();
    for id in 0..lib::web::WEB_TASK_POOL_SIZE {
        spawner.must_spawn(lib::web::web_task(
            id,
            stack,
            web_app.router,
            web_app.config,
        ));
    }
    defmt::info!("Web server started...");

    {
        use embassy_sync::blocking_mutex::raw::NoopRawMutex;
        use embassy_sync::mutex::Mutex;

        let (tx_pin, rx_pin) = (peripherals.GPIO17, peripherals.GPIO16);
        let config = esp_hal::uart::Config::default()
            .with_baudrate(lib::config::MHZ_19_UART_BUTRATE)
            .with_rx(
                esp_hal::uart::RxConfig::default()
                    .with_fifo_full_threshold(core::mem::size_of::<mh_z19::Packet>() as u16 * 2),
            );
        let uart2 = esp_hal::uart::Uart::new(peripherals.UART2, config)
            .unwrap()
            .with_tx(tx_pin)
            .with_rx(rx_pin)
            .into_async();

        let mhz19 = &*lib::mk_static!(
            Mutex<NoopRawMutex, lib::sensors::mh_z19::MhZ19<Uart<'static, esp_hal::Async>>>,
            Mutex::new(lib::sensors::mh_z19::MhZ19::new(uart2, 0x01))
        );

        let am = {
            use esp_hal::analog::adc::Attenuation;

            let mut config = esp_hal::analog::adc::AdcConfig::default();
            let sens_12v = config.enable_pin(peripherals.GPIO34, Attenuation::_0dB);
            let sens_5v = config.enable_pin(peripherals.GPIO35, Attenuation::_0dB);
            let sens_3v3 = config.enable_pin(peripherals.GPIO33, Attenuation::_0dB);

            let adc = esp_hal::analog::adc::Adc::new(peripherals.ADC1, config);

            lib::sensors::adc::AnalogMeasurments::new(adc, sens_12v, sens_5v, sens_3v3)
        };

        let one_wire_pin = esp_hal::gpio::Flex::new(peripherals.GPIO4);

        let (sda_pin, scl_pin) = (peripherals.GPIO21, peripherals.GPIO22);
        let i2c = esp_hal::i2c::master::I2c::new(
            peripherals.I2C0,
            esp_hal::i2c::master::Config::default(),
        )
        .unwrap()
        .with_sda(sda_pin)
        .with_scl(scl_pin);
        let clocks = &*lib::mk_static!(
            Mutex<NoopRawMutex, ds1307::Ds1307<esp_hal::i2c::master::I2c<'static, esp_hal::Blocking>>>,
            Mutex::new(ds1307::Ds1307::new(i2c))
        );

        spawner.must_spawn(sensor_reader(mhz19, clocks, am));
        spawner.must_spawn(temperature_task(one_wire_pin));
    };

    {
        let mut timer = PeriodicTimer::new(timg0.timer1);
        timer.set_interrupt_handler(esp_hal::interrupt::InterruptHandler::new(
            timer_isr,
            esp_hal::interrupt::Priority::Priority2,
        ));
        timer.enable_interrupt(true);
        timer
            .start(esp_hal::time::Duration::from_micros(
                lib::config::NIXIE_CLOCK_TIMER_PERIOD_US,
            ))
            .unwrap();
        unsafe {
            GLOBAL_CONTEXT = Some(CriticalSectionMutex::new(RefCell::new(GlobalContext::new(
                timer,
            ))))
        };
    }
}

#[embassy_executor::task]
async fn sensor_reader(
    mhz19: &'static embassy_sync::mutex::Mutex<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        lib::sensors::mh_z19::MhZ19<esp_hal::uart::Uart<'static, esp_hal::Async>>,
    >,
    ds1307: &'static embassy_sync::mutex::Mutex<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        ds1307::Ds1307<esp_hal::i2c::master::I2c<'static, Blocking>>,
    >,
    mut am: nixieclock_esp32_rs::sensors::adc::AnalogMeasurments<
        'static,
        esp_hal::peripherals::ADC1,
        esp_hal::gpio::GpioPin<34>,
        esp_hal::gpio::GpioPin<35>,
        esp_hal::gpio::GpioPin<33>,
    >,
) {
    const VOLTAGE_CONVERTORS: [VoltageCalc; 3] = [
        VoltageCalc::new(lib::config::V12_DEVIDER),
        VoltageCalc::new(lib::config::V5_DEVIDER),
        VoltageCalc::new(lib::config::V3V3_DEVIDER),
    ];

    loop {
        let start = embassy_time::Instant::now();

        let result = {
            let mut mhz19_guard = mhz19.lock().await;
            mhz19_guard.read_gas_concentration_ppm().await
        };

        match result {
            Ok(ppm) => {
                defmt::info!("CO2 concentration: {} ppm", ppm);
            }
            Err(e) => {
                defmt::error!("Error reading CO2 concentration: {:?}", e);
            }
        }

        {
            use ds1307::Rtcc;

            let mut guard = ds1307.lock().await;
            match guard.time() {
                Ok(time) => {
                    defmt::info!("Time: {}", defmt::Display2Format(&time));
                }
                Err(e) => {
                    defmt::error!("Error reading time: {}", defmt::Debug2Format(&e));
                }
            }
        }

        {
            match am.read(&VOLTAGE_CONVERTORS).await {
                Ok(v) => {
                    defmt::info!("Voltages: 12V: {}, 5V: {}, 3.3V: {}", v[0], v[1], v[2]);
                }
                Err(_) => {
                    defmt::error!("Error reading voltages from ADC");
                }
            };
        }

        {
            #[allow(static_mut_refs)]
            if let Some(ctx) = unsafe { &GLOBAL_CONTEXT } {
                ctx.lock(|c| {
                    let c = c.borrow().get_count();
                    defmt::info!("Timer count: {}", c);
                });
            }
        }

        let elapsed = start.elapsed();

        embassy_time::Timer::after(embassy_time::Duration::from_secs(1) - elapsed).await;
    }
}

#[embassy_executor::task]
async fn temperature_task(mut one_wire_pin: esp_hal::gpio::Flex<'static>) {
    defmt::info!("Starting temperature task...");

    one_wire_pin.set_as_open_drain(esp_hal::gpio::Pull::None);
    let mut bus = onecable::OneWire::new(&mut one_wire_pin);

    // scan bus
    let sensor = {
        loop {
            match bus.search_rom_iter(&mut embassy_time::Delay).next() {
                Some(rom) => {
                    defmt::info!("Found device with ROM: {}", defmt::Debug2Format(&rom));
                    break onecable::ds18b20::DS18B20::try_from(rom).unwrap();
                }
                None => {
                    defmt::error!("No 1-Wire devices found on the bus");
                    continue;
                }
            }
        }
    };

    loop {
        match sensor.read_temperature(&mut bus, &mut embassy_time::Delay, &mut embassy_time::Delay)
        {
            Ok(temp) => {
                defmt::info!("Temperature: {} °C", temp.to_num::<f32>());
            }
            Err(e) => {
                defmt::error!("Error reading temperature: {}", defmt::Debug2Format(&e));
            }
        }

        embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
    }
}

extern "C" fn timer_isr() {
    #[allow(static_mut_refs)]
    if let Some(mutex) = unsafe { &GLOBAL_CONTEXT } {
        mutex.lock(|ctx| {
            ctx.borrow_mut().clear_interrupt_flag();
        });
    } else {
        defmt::panic!("Timer context is not initialized!");
    }
}
