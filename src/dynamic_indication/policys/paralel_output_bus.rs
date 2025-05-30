use alloc::boxed::Box;
use embedded_hal::digital::OutputPin;

use crate::dynamic_indication::*;

pub struct ParallelOutputBus<T: num::Num, E, const SIZE: usize> {
    // The pins that make up the bus
    pins: [Box<dyn OutputPin<Error = E>>; SIZE],
    // Inverted state of the bus
    inverted: bool,
    // current data on the bus
    value: T,
}

impl<T, E, const SIZE: usize> ParallelOutputBus<T, E, SIZE>
where
    T: num::Num + Copy + core::ops::Shl<usize, Output = T> + core::ops::BitAnd<Output = T>,
    E: embedded_hal::digital::Error,
{
    pub fn new(pins: [Box<dyn OutputPin<Error = E>>; SIZE]) -> Self {
        let mut res = Self {
            pins,
            inverted: false,
            value: T::zero(),
        };

        res.set_data(T::zero());

        res
    }
}

impl<T, E, const SIZE: usize> OutputBus for ParallelOutputBus<T, E, SIZE>
where
    T: num::Num + Copy + core::ops::Shl<usize, Output = T> + core::ops::BitAnd<Output = T>,
    E: embedded_hal::digital::Error,
{
    type DataType = T;

    fn set_data(&mut self, data: Self::DataType) {
        self.value = data;
        for (i, pin) in self.pins.iter_mut().enumerate() {
            let bit = (data & (T::one() << i)) != T::zero();
            let _ = pin.set_state((bit ^ self.inverted).into());
        }
    }

    fn get_data(&self) -> Self::DataType {
        self.value
    }

    fn width(&self) -> usize {
        SIZE
    }

    fn set_inverted(&mut self, inverted: bool) {
        self.inverted = inverted;
    }

    fn is_inverted(&self) -> bool {
        self.inverted
    }
}
