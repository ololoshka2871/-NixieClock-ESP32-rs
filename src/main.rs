#![no_main]
#![no_std]

use rtic_monotonics::esp32c3::prelude::*;
esp32c3_systimer_monotonic!(Mono);
use esp_hal as _;

use esp_backtrace as _;
use esp_println as _;

#[rtic::app(device = esp32c3, dispatchers = [])]
mod app {
    use super::*;

    #[shared]
    struct Shared {}

    #[local]
    struct Local {}

    #[init]
    fn init(cx: init::Context) -> (Shared, Local) {
        defmt::info!("init");
        let timer = cx.device.SYSTIMER;

        Mono::start(timer);

        foo::spawn().unwrap();

        (Shared {}, Local {})
    }

    #[task]
    async fn foo(_cx: foo::Context) {
        defmt::info!("hello from foo");
        Mono::delay(2_u64.secs()).await;
        defmt::info!("bye from foo");
    }
}
