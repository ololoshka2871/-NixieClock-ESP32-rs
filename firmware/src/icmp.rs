//! ICMP echo-резолвер на уровне Driver embassy-net.
//!
//! smoltcp принимает Echo Request только в IcmpSocket с привязанным identifier,
//! поэтому произвольный ping (ident = PID отправителя) до стека не доходит.
//! Обёртка над интерфейсом esp-radio: Echo Request к нам отвечает сразу
//! paired-TxToken'ом драйвера (на это прямо указывает документация
//! embassy-net-driver про receive()), остальные кадры передаёт стеку как есть.

use core::cell::RefCell;
use core::task::Context;

use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

/// Хватает на кадр Ethernet MTU 1500 + заголовок.
const MAX_FRAME: usize = 1524;

struct FrameSlot {
    len: usize, // 0 — слот пуст
    buf: [u8; MAX_FRAME],
}

const fn new_slot() -> FrameSlot {
    FrameSlot { len: 0, buf: [0u8; MAX_FRAME] }
}

static SLOT: Mutex<CriticalSectionRawMutex, RefCell<FrameSlot>> = Mutex::new(RefCell::new(new_slot()));
static REPLY: Mutex<CriticalSectionRawMutex, RefCell<[u8; MAX_FRAME]>> =
    Mutex::new(RefCell::new([0u8; MAX_FRAME]));
static OUR_IP: Mutex<CriticalSectionRawMutex, RefCell<[u8; 4]>> = Mutex::new(RefCell::new([0u8; 4]));
static BUSY_WAKER: Mutex<CriticalSectionRawMutex, RefCell<Option<core::task::Waker>>> =
    Mutex::new(RefCell::new(None));

/// Наш текущий IPv4 (для фильтра Echo Request). Вызывать при получении IP.
pub fn set_our_ip(ip: [u8; 4]) {
    OUR_IP.lock(|m| *m.borrow_mut() = ip);
}

/// RX-токен: кадр лежит в SLOT, consume отдаёт его стеку и освобождает слот.
pub struct SlotRxToken;

impl RxToken for SlotRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let (had, res) = SLOT.lock(|m| {
            let mut s = m.borrow_mut();
            let n = s.len;
            let res = f(&mut s.buf[..n]);
            let had = n != 0;
            s.len = 0;
            (had, res)
        });
        if had {
            if let Some(w) = BUSY_WAKER.lock(|w| w.borrow_mut().take()) {
                w.wake();
            }
        }
        res
    }
}

enum Outcome<T> {
    Replied,
    Pass(T),
}

pub struct IcmpDevice<D> {
    inner: D,
}

impl<D: Driver> IcmpDevice<D> {
    pub fn new(inner: D) -> Self {
        Self { inner }
    }
}

impl<D: Driver> Driver for IcmpDevice<D> {
    type RxToken<'a> = SlotRxToken where D: 'a;
    type TxToken<'a> = D::TxToken<'a> where D: 'a;

    fn receive(&mut self, cx: &mut Context) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if SLOT.lock(|m| m.borrow().len != 0) {
            BUSY_WAKER.lock(|w| *w.borrow_mut() = Some(cx.waker().clone()));
            return None;
        }
        let our_mac = match self.inner.hardware_address() {
            HardwareAddress::Ethernet(m) => m,
            _ => [0u8; 6],
        };
        let our_ip = OUR_IP.lock(|m| *m.borrow());

        let (irx, itx) = self.inner.receive(cx)?;
        let outcome = irx.consume(move |buf| {
            if our_ip != [0u8; 4] {
                let rlen = REPLY.lock(|m| {
                    let mut s = m.borrow_mut();
                    clock_core::icmp::build_echo_reply(buf, our_mac, our_ip, &mut s[..])
                });
                if let Some(rlen) = rlen {
                    REPLY.lock(|m| {
                        let s = m.borrow();
                        itx.consume(rlen, |t| {
                            let n = rlen.min(t.len());
                            t[..n].copy_from_slice(&s[..rlen]);
                        });
                    });
                    return Outcome::Replied;
                }
            }
            // Не наш ICMP: кадр идёт стеку целиком.
            let n = buf.len().min(MAX_FRAME);
            SLOT.lock(|m| {
                let mut s = m.borrow_mut();
                s.buf[..n].copy_from_slice(&buf[..n]);
                s.len = n;
            });
            Outcome::Pass(itx)
        });
        match outcome {
            Outcome::Replied => None, // ответ уже отправлен, стеку ICMP не нужен
            Outcome::Pass(tx) => Some((SlotRxToken, tx)),
        }
    }

    fn transmit(&mut self, cx: &mut Context) -> Option<Self::TxToken<'_>> {
        self.inner.transmit(cx)
    }

    fn link_state(&mut self, cx: &mut Context) -> LinkState {
        self.inner.link_state(cx)
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn hardware_address(&self) -> HardwareAddress {
        self.inner.hardware_address()
    }
}
