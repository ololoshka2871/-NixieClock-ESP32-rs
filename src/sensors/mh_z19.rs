use embedded_io::Error;
use mh_z19::*;

pub struct MhZ19<U>
where
    U: embedded_io_async::Write + embedded_io_async::Read,
{
    uart: U,
    device_number: u8,
}

impl<U> MhZ19<U>
where
    U: embedded_io_async::Write + embedded_io_async::Read,
{
    pub fn new(uart: U, device_number: u8) -> Self {
        MhZ19 {
            uart,
            device_number,
        }
    }

    /// Create a command to enable or disable Automatic Baseline Correction (ABC)
    pub async fn set_automatic_baseline_correction(
        &mut self,
        enabled: bool,
    ) -> Result<(), U::Error> {
        let pocket = set_automatic_baseline_correction(self.device_number, enabled);
        self.uart.write_all(&pocket).await
        // no response expected
    }

    /// Create a command to calibrate the span point.
    ///
    /// Quoting the datasheet: "Note: Pls do ZERO calibration before span calibration
    /// Please make sure the sensor worked under a certain level co2 for over 20 minutes.
    ///
    /// Suggest using 2000ppm as span, at least 1000ppm"
    pub async fn calibrate_span_point(&mut self, value: u16) -> Result<(), U::Error> {
        let pocket = calibrate_span_point(self.device_number, value);
        self.uart.write_all(&pocket).await
        // no response expected
    }

    /// Create a command to set the sensor detection range (MH-Z19B only).
    ///
    /// Quoting the datasheet: "Detection range is 2000 or 5000ppm"
    pub async fn set_detection_range(&mut self, value: u16) -> Result<(), U::Error> {
        let pocket = set_detection_range(self.device_number, value);
        self.uart.write_all(&pocket).await
        // no response expected
    }

    /// Create a command to calibrate the zero point.
    ///
    /// Quoting the datasheet: "Note：Zero point is 400ppm, please make sure the sensor has
    /// been worked under 400ppm for over 20 minutes"
    pub async fn calibrate_zero_point(&mut self) -> Result<(), U::Error> {
        let pocket = calibrate_zero_point(self.device_number);
        self.uart.write_all(&pocket).await
        // no response expected
    }

    /// Read the CO2 concentration in ppm.
    pub async fn read_gas_concentration_ppm(&mut self) -> Result<u32, embedded_io_async::ErrorKind> {
        let pocket = read_gas_concentration(self.device_number);
        self.uart.flush().await.map_err(|e| e.kind())?;
        self.uart.write_all(&pocket).await.map_err(|e| e.kind())?;

        // Read the response or timeout async
        let mut buffer = [0u8; 9];
        match embassy_time::with_timeout(
            embassy_time::Duration::from_millis(10),
            embedded_io_async::Read::read_exact(&mut self.uart, &mut buffer),
        )
        .await
        {
            Ok(Ok(())) => parse_gas_concentration_ppm(&buffer)
                .map_err(|_| embedded_io_async::ErrorKind::InvalidData),
            Ok(Err(_e)) => Err(embedded_io_async::ErrorKind::InvalidInput),
            Err(_e) => Err(embedded_io_async::ErrorKind::TimedOut),
        }
    }
}
