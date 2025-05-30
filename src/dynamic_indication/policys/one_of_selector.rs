use crate::dynamic_indication::*;

pub struct OneOfSelector<B> {
    bus: B,
    channel: usize,
}

impl<B: OutputBus> OneOfSelector<B> {
    pub fn new(bus: B) -> Self {
        Self { bus, channel: 0 }
    }

    pub fn bus(&self) -> &B {
        &self.bus
    }
}

impl<B: OutputBus> SelectorPolicy for OneOfSelector<B>
where
    B::DataType: Copy
        + core::ops::Shl<usize, Output = B::DataType>
        + core::ops::BitAnd<Output = B::DataType>
        + Into<usize>,
{
    type BusType = B;
    type DataType = B::DataType;

    fn next_element(&mut self) -> usize {
        self.channel = (self.channel + Self::one().into()) % self.bus.width();
        if self.is_enabled() {
            self.bus.set_data(Self::one() << self.channel);
        }
        self.channel
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.bus.set_data(if enabled {
            Self::one() << self.channel
        } else {
            Self::zero()
        });
    }

    fn is_enabled(&self) -> bool {
        self.bus.get_data() != Self::zero()
    }

    fn group_count(&self) -> usize {
        self.bus.width()
    }

    fn is_first_element(&self) -> bool {
        self.channel == 0
    }
}
