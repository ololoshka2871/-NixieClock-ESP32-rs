pub mod controller;
pub use controller::DIController;

pub mod policys;
pub use policys::{IC74141ParallelPolicy, ParallelOutputBus, OneOfSelector};

use alloc::{string::String, vec::Vec};
use num::{Num, One, Zero};

pub trait DataPolicy {
    type BusType: OutputBus;
    type DataType;
    type FramebuferElementType;

    fn set_data(&mut self, data: &[Self::DataType], group: u8);
    fn frame_buffer_size(&self) -> usize;
}

pub trait Effect<T> {
    fn set_animation_duration(&mut self, full_animation_frame_count: usize);
    fn next_frame(&mut self);
    fn set_destination_data(&mut self, destination: &[T]);
    fn init_buffers(&mut self, fbsize: usize);
    fn frame_op<F: FnOnce(&mut [T])>(&self, op: F);
}

// TODO: NoEffect, Fade

pub trait EncoderPolicy {
    type OutputType;
    type EncoderMapType;

    fn encode(&self, input: &String) -> Vec<Self::OutputType>;
    fn get_unknown_char_value() -> Self::OutputType; // ???
}

pub trait OutputBus {
    type DataType: Zero + One;

    fn set_data(&mut self, data: Self::DataType);
    fn get_data(&self) -> Self::DataType;
    fn width(&self) -> usize;
    fn set_inverted(&mut self, inverted: bool);
    fn is_inverted(&self) -> bool;
}

pub trait SelectorPolicy {
    type BusType: OutputBus;
    type DataType: One;

    fn one() -> Self::DataType {
        Self::DataType::one()
    }

    fn next_element(&mut self) -> usize;

    fn set_enabled(&mut self, enabled: bool);
    fn is_enabled(&self) -> bool;

    fn group_count(&self) -> usize;

    fn is_configured(&self) -> bool {
        self.group_count() > 0
    }

    fn is_first_element(&self) -> bool;
}

pub trait Controller {
    fn set_text(&mut self, text: String) -> &mut Self;
    fn set_enabled(&mut self, enabled: bool) -> &mut Self;
    fn set_update_interval(&mut self, interval: esp_hal::time::Duration) -> &mut Self;

    fn get_enabled(&self) -> bool;
    fn get_update_interval(&self) -> esp_hal::time::Duration;

    fn next_group(&mut self);
}
