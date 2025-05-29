use esp_hal::{
    analog::adc::{Adc, AdcChannel, AdcPin, RegisterAccess},
    Blocking,
};

pub trait VoltageCalculator<R> {
    fn to_voltage(&self, raw_value: u16) -> R;
}

pub struct AnalogMeasurments<'d, ADCI, PIN12, PIN5, PIN3V3> {
    adc: Adc<'d, ADCI, Blocking>,
    sens_12v: AdcPin<PIN12, ADCI>,
    sens_5v: AdcPin<PIN5, ADCI>,
    sens_3v3: AdcPin<PIN3V3, ADCI>,
}

impl<'d, ADCI, PIN12, PIN5, PIN3V3> AnalogMeasurments<'d, ADCI, PIN12, PIN5, PIN3V3>
where
    ADCI: RegisterAccess,
    PIN12: AdcChannel,
    PIN5: AdcChannel,
    PIN3V3: AdcChannel,
{
    pub fn new(
        adc: Adc<'d, ADCI, Blocking>,
        sens_12v: AdcPin<PIN12, ADCI>,
        sens_5v: AdcPin<PIN5, ADCI>,
        sens_3v3: AdcPin<PIN3V3, ADCI>,
    ) -> Self {
        Self {
            adc,
            sens_12v,
            sens_5v,
            sens_3v3,
        }
    }

    pub async fn read<R, C>(&mut self, convertors: &[C; 3]) -> Result<[R; 3], ()>
    where
        C: VoltageCalculator<R>,
    {
        async fn read_sensor<'a, ADCI: esp_hal::analog::adc::RegisterAccess>(
            adc: &mut Adc<'a, ADCI, Blocking>,
            sensor: &mut AdcPin<impl AdcChannel, ADCI>,
        ) -> Result<u16, ()> {
            loop {
                match adc.read_oneshot(sensor) {
                    Ok(value) => return Ok(value),
                    Err(nb::Error::WouldBlock) => {
                        // wait 1ms before retrying
                        embassy_time::Timer::after(embassy_time::Duration::from_millis(1)).await;
                        continue;
                    }
                    Err(nb::Error::Other(_)) => return Err(()),
                }
            }
        }

        let adc = &mut self.adc;
        let v12_c = &convertors[0];
        let v5_c = &convertors[1];
        let v3v3_c = &convertors[2];

        let v12 = read_sensor(adc, &mut self.sens_12v)
            .await
            .map(move |v| v12_c.to_voltage(v))?;
        let v5 = read_sensor(adc, &mut self.sens_5v)
            .await
            .map(move |v| v5_c.to_voltage(v))?;
        let v3v3 = read_sensor(adc, &mut self.sens_3v3)
            .await
            .map(move |v| v3v3_c.to_voltage(v))?;

        Ok([v12, v5, v3v3])
    }
}
