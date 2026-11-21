//! Raw report I/O, abstracted so device logic can be tested without hardware.

use crate::error::{Error, Result};
use crate::protocol::{REPORT_LEN, Report};

/// Sends and receives reports on the mouse's vendor channel.
pub trait Transport {
    /// Sends one output report.
    fn write_report(&mut self, report: &Report) -> Result<()>;

    /// Waits up to `timeout_ms` (0 = do not wait) for one input report.
    /// Returns `Ok(None)` when nothing arrived in time.
    fn read_report(&mut self, timeout_ms: i32) -> Result<Option<Report>>;
}

/// [`Transport`] over a `hidapi` device (hidraw on Linux).
pub struct HidTransport {
    device: hidapi::HidDevice,
}

impl HidTransport {
    pub fn new(device: hidapi::HidDevice) -> Self {
        Self { device }
    }
}

impl Transport for HidTransport {
    fn write_report(&mut self, report: &Report) -> Result<()> {
        // The channel has no numbered reports, so hidapi expects report ID 0 first.
        let mut buffer: [u8; REPORT_LEN + 1] = [0; REPORT_LEN + 1];
        buffer[1..].copy_from_slice(report);
        let written: usize = self.device.write(&buffer)?;
        if written != buffer.len() {
            return Err(Error::ShortWrite {
                written,
                expected: buffer.len(),
            });
        }
        Ok(())
    }

    fn read_report(&mut self, timeout_ms: i32) -> Result<Option<Report>> {
        let mut report: Report = [0; REPORT_LEN];
        let length: usize = self.device.read_timeout(&mut report, timeout_ms)?;
        Ok((length > 0).then_some(report))
    }
}
