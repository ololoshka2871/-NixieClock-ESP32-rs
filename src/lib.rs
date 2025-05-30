#![no_std]
#![feature(impl_trait_in_assoc_type)]

extern crate alloc;

pub mod net;
pub mod sensors;
pub mod dynamic_indication;

pub mod config;

pub use net::{web, wifi};

#[macro_export]
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}
