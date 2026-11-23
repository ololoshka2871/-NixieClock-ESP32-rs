//! Единый embassy-net device, распределяющий трафик по живому радио-интерфейсу.
//!
//! embassy-net 0.9.1: `Runner::run()` расходится (diverging) и таск спавнится
//! один раз — второй Runner на AP-режим создать нельзя, поэтому режим
//! STA<->SoftAP переключается внутри одного стека диспетчером (пробный crate
//! `softap_probe` на сборочной машине: типизация подтверждена на закреплённых
//! версиях esp-radio 1.0.0-beta.1 / embassy-net 0.9.1; поведение на плате —
//! стендовый тест, не проверено статически).
//!
//! esp-radio держит синглтоны STA и AP раздельно (`Interface::station()`,
//! `Interface::access_point()`), AP-кадры приходят тем же Ethernet-обёрнутым
//! видом через `recv_cb_ap` -> `DATA_QUEUE_RX_AP` — отдельной среды не нужно
//! (`medium-ethernet` уже включён в Cargo.toml).
//!
//! Порядок переключения (в connection-таске, он единственный владелец
//! контроллера): `RADIO_MODE.store` -> `controller.set_config` ->
//! `stack.set_config_v4`. `set_config` рвёт радио и все открытые сокеты —
//! задачи сервера/логгера/SNTP переживают это штатными ретраями.
//! `embassy_net::Runner::poll` перечитывает `hardware_address()` каждый поллин
//! (lib.rs:856-868) и сам обновляет MAC в smoltcp — MAC AP подхватится сам.

use core::sync::atomic::{AtomicU8, Ordering};
use core::task::Context;

use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState};
use esp_radio::wifi::Interface;

pub const MODE_STA: u8 = 0;
pub const MODE_AP: u8 = 1;

/// Пишет connection-таск ДО `controller.set_config()`; читает диспетчер.
pub static RADIO_MODE: AtomicU8 = AtomicU8::new(MODE_STA);
/// 1 = SoftAP поднят (шлюза/DNS нет): гейт для логгера/SNTP/OTA и /api/status.
pub static AP_ACTIVE: AtomicU8 = AtomicU8::new(0);

pub const fn ap_is_active() -> bool {
    AP_ACTIVE.load(Ordering::SeqCst) == 1
}

pub struct NetDevice {
    sta: Interface,
    ap: Interface,
}

impl NetDevice {
    /// Оба синглтона создаются на старте: esp-radio позволяет держать STA и AP
    /// интерфейсы одновременно (STA_BIT/AP_BIT независимы, mod.rs:1521-1547).
    pub fn new() -> Self {
        Self {
            sta: Interface::station(),
            ap: Interface::access_point(),
        }
    }

    fn current(&mut self) -> &mut Interface {
        if RADIO_MODE.load(Ordering::SeqCst) == MODE_AP {
            &mut self.ap
        } else {
            &mut self.sta
        }
    }

    fn current_ref(&self) -> &Interface {
        if RADIO_MODE.load(Ordering::SeqCst) == MODE_AP {
            &self.ap
        } else {
            &self.sta
        }
    }
}

impl Driver for NetDevice {
    type RxToken<'a> = esp_radio::wifi::WifiRxToken where Self: 'a;
    type TxToken<'a> = esp_radio::wifi::WifiTxToken where Self: 'a;

    fn receive(&mut self, cx: &mut Context<'_>) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        Driver::receive(self.current(), cx)
    }

    fn transmit(&mut self, cx: &mut Context<'_>) -> Option<Self::TxToken<'_>> {
        Driver::transmit(self.current(), cx)
    }

    fn link_state(&mut self, cx: &mut Context<'_>) -> LinkState {
        Driver::link_state(self.current(), cx)
    }

    fn capabilities(&self) -> Capabilities {
        Driver::capabilities(self.current_ref())
    }

    /// Перечитывается Runner::poll каждый поллин — MAC AP после переключения
    /// подхватывается без пересоздания стека.
    fn hardware_address(&self) -> HardwareAddress {
        Driver::hardware_address(self.current_ref())
    }
}
