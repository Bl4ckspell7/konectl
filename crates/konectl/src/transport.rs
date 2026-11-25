//! Raw report I/O, abstracted so device logic can be tested without hardware.
//!
//! [`HidTransport`] logs every report it sends and reads at the `trace` level
//! (`RUST_LOG=konectl=trace` with a logger such as env_logger).

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
        log::trace!("→ {}", hex(report));
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
        if length > 0 {
            log::trace!("← {}", hex(&report));
        }
        Ok((length > 0).then_some(report))
    }
}

/// A report as hex bytes without its trailing zeros (most reports are short),
/// but always with the two header bytes (operation and command).
fn hex(report: &[u8]) -> String {
    let end: usize = report
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |last| last + 1)
        .max(2)
        .min(report.len());
    report[..end]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_drops_the_trailing_zeros() {
        let mut report: Report = [0; REPORT_LEN];
        report[..6].copy_from_slice(&[0x10, 0x50, 0x0b, 0x01, 0x00, 0x64]);
        assert_eq!(hex(&report), "10 50 0b 01 00 64");
        let mut info: Report = [0; REPORT_LEN];
        info[0] = 0x90;
        assert_eq!(hex(&info), "90 00");
    }
}
