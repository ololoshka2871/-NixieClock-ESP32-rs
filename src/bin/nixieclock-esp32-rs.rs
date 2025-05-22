#![no_std]
#![no_main]

use embassy_executor::Spawner;
use esp_hal::{clock::CpuClock, rng::Rng, timer::timg::TimerGroup, uart::Uart};
use esp_println as _;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

extern crate alloc;

use esp_wifi::EspWifiController;
use nixieclock_esp32_rs as lib;

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

        let one_wire_pin = esp_hal::gpio::Flex::new(peripherals.GPIO4);
        let one_wire_timer = esp_hal::timer::OneShotTimer::new(timg0.timer1);

        spawner.must_spawn(sensor_reader(mhz19, one_wire_pin, one_wire_timer));
    };
}

#[embassy_executor::task]
async fn sensor_reader(
    mhz19: &'static embassy_sync::mutex::Mutex<
        embassy_sync::blocking_mutex::raw::NoopRawMutex,
        lib::sensors::mh_z19::MhZ19<esp_hal::uart::Uart<'static, esp_hal::Async>>,
    >,
    mut one_wire_pin: esp_hal::gpio::Flex<'static>,
    mut microseconds_timer: esp_hal::timer::OneShotTimer<'static, esp_hal::Blocking>,
) {
    use embassy_time::Delay;

    one_wire_pin.set_as_open_drain(esp_hal::gpio::Pull::None);
    let mut one_wire_bus = onecable::OneWire::new(&mut one_wire_pin);

    // scan bus
    let sensor = match one_wire_bus.search_rom_iter(&mut microseconds_timer).next() {
        Some(rom) => {
            defmt::info!("Found device with ROM: {}", defmt::Debug2Format(&rom));
            onecable::ds18b20::DS18B20::try_from(rom)
        }
        None => {
            defmt::error!("No devices found on the bus");
            Err(onecable::FamilyCodeError::MismatchedFamilyCode {
                expected: 0,
                received: 0,
            })
        }
    }
    .ok();

    loop {
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

        if let Some(sensor) = &sensor {
            match sensor.read_temperature(&mut one_wire_bus, &mut microseconds_timer, &mut Delay) {
                Ok(temp) => {
                    defmt::info!("Temperature: {} °C", temp.to_num::<f32>());
                }
                Err(e) => {
                    defmt::error!("Error reading temperature: {}", defmt::Debug2Format(&e));
                }
            }
        }

        embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
    }
}
