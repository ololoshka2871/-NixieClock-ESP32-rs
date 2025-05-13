#![no_std]
#![no_main]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
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

        let mhz19 = lib::sensors::mh_z19::MhZ19::new(uart2, 0x01);
        spawner.must_spawn(mh_z19_task(mhz19));
    };

    {
        let one_wire_pin = esp_hal::gpio::Flex::new(peripherals.GPIO4);
        spawner.must_spawn(temperature_task(one_wire_pin));
    }
}

#[embassy_executor::task]
async fn mh_z19_task(
    mut mhz19: lib::sensors::mh_z19::MhZ19<esp_hal::uart::Uart<'static, esp_hal::Async>>,
) {
    loop {
        match mhz19.read_gas_concentration_ppm().await {
            Ok(ppm) => {
                defmt::info!("CO2 concentration: {} ppm", ppm);
            }
            Err(e) => {
                defmt::error!("Error reading CO2 concentration: {:?}", e);
            }
        }

        embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
    }
}

#[embassy_executor::task]
async fn temperature_task(mut one_wire_pin: esp_hal::gpio::Flex<'static>) {
    defmt::info!("Starting temperature task...");

    one_wire_pin.set_as_open_drain(esp_hal::gpio::Pull::None);
    let mut bus = onecable::OneWire::new(&mut one_wire_pin);
    let mut delay = embassy_time::Delay;

    // scan bus
    let sensor = match bus.search_rom_iter(&mut delay).next() {
        Some(rom) => {
            defmt::info!("Found device with ROM: {}", defmt::Debug2Format(&rom));
            onecable::ds18b20::DS18B20::try_from(rom).unwrap()
        }
        None => {
            defmt::error!("No devices found on the bus");
            return;
        }
    };

    loop {
        match sensor.read_temperature(&mut bus, &mut delay, &mut embassy_time::Delay) {
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
