use super::*;

pub struct DIController<DP, SP, ENC, EFFECT> {
    datapolicy: DP,
    selector: SP,
    encoder: ENC,
    effector: EFFECT,
    update_interval: esp_hal::time::Duration,
}

impl<DP, SP, ENC, EFFECT> DIController<DP, SP, ENC, EFFECT>
where
    DP: DataPolicy,
    SP: SelectorPolicy,
    ENC: EncoderPolicy,
    EFFECT: Effect<DP::DataType>,
{
    pub fn new(datapolicy: DP, selector: SP, encoder: ENC, mut effector: EFFECT) -> Self {
        effector.init_buffers(datapolicy.frame_buffer_size());

        Self {
            datapolicy,
            selector,
            encoder,
            effector,
            update_interval: esp_hal::time::Duration::from_micros(10000), // Default update interval
        }
    }
}

impl<DP, SP, ENC, EFFECT> Controller for DIController<DP, SP, ENC, EFFECT>
where
    DP: DataPolicy,
    SP: SelectorPolicy,
    ENC: EncoderPolicy,
    EFFECT: Effect<DP::DataType>,
{
    fn set_text(&mut self, text: String) -> &mut Self {
        todo!()
    }

    fn set_enabled(&mut self, enabled: bool) -> &mut Self {
        todo!()
    }

    fn set_update_interval(&mut self, interval: esp_hal::time::Duration) -> &mut Self {
        todo!()
    }

    fn get_enabled(&self) -> bool {
        todo!()
    }

    fn get_update_interval(&self) -> esp_hal::time::Duration {
        todo!()
    }

    fn next_group(&mut self) {
        todo!()
    }
}
