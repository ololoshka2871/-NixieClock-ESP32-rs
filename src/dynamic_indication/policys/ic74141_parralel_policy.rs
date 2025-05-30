use crate::dynamic_indication::*;

const BITS_PRE_CHANNEL: usize = 4;

pub struct IC74141ParallelPolicy<B> {
    data_bus: B,
    channels_pre_data_bus: usize,
    group_count: usize,
}

impl<B: OutputBus> IC74141ParallelPolicy<B> {
    pub fn new(data_bus: B, group_count: usize) -> Self {
        let channels_pre_data_bus = data_bus.width() / BITS_PRE_CHANNEL;
        Self {
            data_bus,
            channels_pre_data_bus,
            group_count,
        }
    }
}

impl<B: OutputBus<DataType = u8>> DataPolicy for IC74141ParallelPolicy<B> {
    type BusType = B;
    type DataType = u8;
    type FramebuferElementType = u8;

    fn set_data(&mut self, data: &[Self::DataType], group: u8) {
        let mut bus_val: Self::DataType = 0;
        for ch in 0..self.channels_pre_data_bus {
            if let Some(d) = data.get(group as usize + ch * self.group_count) {
                bus_val |= *d << (ch * BITS_PRE_CHANNEL);
            } else {
                defmt::warn!(
                    "dataSrc.get({}) is None, group: {}, ch: {}, group_count: {}",
                    group as usize + ch * self.group_count,
                    group,
                    ch,
                    self.group_count
                );
                return;
            }
        }
        self.data_bus.set_data(bus_val);
    }

    fn frame_buffer_size(&self) -> usize {
        self.channels_pre_data_bus * self.group_count
    }
}
