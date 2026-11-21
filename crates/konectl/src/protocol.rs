//! Wire format of the Kone Pro Air vendor HID channel.
//!
//! The mouse exposes a vendor-defined HID interface (usage page `0xFF00`) with
//! 64-byte input and output reports and no report IDs. Every command is one
//! output report; answers arrive as input reports on the same interface.
//!
//! Everything here is pure data handling and works without hardware. See
//! `docs/protocol.md` for what is known about each command.

use crate::color::Rgb;

/// USB vendor ID of ROCCAT.
pub const VENDOR_ID: u16 = 0x1e7d;
/// Product ID of the Kone Pro Air wireless dongle.
pub const PRODUCT_ID_DONGLE: u16 = 0x2c8e;
/// Product ID of the Kone Pro Air connected with the USB cable.
pub const PRODUCT_ID_WIRED: u16 = 0x2c92;
/// USB interface carrying the vendor channel on the dongle.
pub const INTERFACE_DONGLE: i32 = 2;
/// USB interface carrying the vendor channel over the cable.
pub const INTERFACE_WIRED: i32 = 1;

/// Length of every report on the vendor channel.
pub const REPORT_LEN: usize = 64;

/// One report on the vendor channel, without a report ID.
pub type Report = [u8; REPORT_LEN];

const CMD_STATUS: u8 = 0x90;
const STATUS_INIT: u8 = 0x00;
const STATUS_BATTERY: u8 = 0x0a;
const STATUS_SIGNAL: u8 = 0x70;

/// Start of the "set live LED colours" command; the bytes after the command
/// byte are constant and not understood yet.
const LED_HEADER: [u8; 9] = [0x10, 0x10, 0x0b, 0x00, 0x09, 0x64, 0x64, 0x64, 0x06];

const BATTERY_OFFSET: usize = 10;
const SIGNAL_OFFSET: usize = 4;
/// Raw signal value reported while the radio is switched off.
const SIGNAL_RADIO_OFF: u8 = 236;

/// Builds a report that starts with `prefix` and is zero-padded to [`REPORT_LEN`].
///
/// # Panics
/// If `prefix` is longer than [`REPORT_LEN`].
pub(crate) fn report_with_prefix(prefix: &[u8]) -> Report {
    let mut report: Report = [0; REPORT_LEN];
    report[..prefix.len()].copy_from_slice(prefix);
    report
}

/// Initialisation command, sent once after opening the device.
pub fn init() -> Report {
    report_with_prefix(&[CMD_STATUS, STATUS_INIT])
}

/// Asks for the battery level; answered by a report [`parse_battery`] accepts.
pub fn query_battery() -> Report {
    report_with_prefix(&[CMD_STATUS, STATUS_BATTERY])
}

/// Asks for the radio signal; answered by a report [`parse_signal`] accepts.
pub fn query_signal() -> Report {
    report_with_prefix(&[CMD_STATUS, STATUS_SIGNAL])
}

/// Sets both LED zones to fixed colours until the mouse sleeps (volatile).
pub fn set_leds(left: Rgb, right: Rgb) -> Report {
    let mut report: Report = report_with_prefix(&LED_HEADER);
    let start: usize = LED_HEADER.len();
    report[start..start + 3].copy_from_slice(&left.to_bytes());
    report[start + 3..start + 6].copy_from_slice(&right.to_bytes());
    report
}

fn is_status_reply(report: &Report, status: u8) -> bool {
    report[0] == CMD_STATUS && report[1] == status
}

/// `true` if `report` answers [`init`].
pub fn is_init_reply(report: &Report) -> bool {
    is_status_reply(report, STATUS_INIT)
}

/// Battery state, if `report` answers [`query_battery`].
pub fn parse_battery(report: &Report) -> Option<Battery> {
    is_status_reply(report, STATUS_BATTERY).then(|| Battery {
        raw: report[BATTERY_OFFSET],
    })
}

/// Battery state as reported by the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// Raw battery byte; 0–100 is a percentage, other values are not understood yet.
    pub raw: u8,
}

impl Battery {
    /// Charge in percent, or `None` if the raw value is not a percentage.
    pub fn percent(self) -> Option<u8> {
        (self.raw <= 100).then_some(self.raw)
    }
}

/// Radio signal, if `report` answers [`query_signal`].
pub fn parse_signal(report: &Report) -> Option<Signal> {
    is_status_reply(report, STATUS_SIGNAL).then(|| Signal {
        raw: report[SIGNAL_OFFSET],
    })
}

/// Radio signal between mouse and dongle as reported by the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    /// Raw value from the mouse; its unit is not documented.
    pub raw: u8,
}

impl Signal {
    /// `false` while the mouse reports its radio as switched off.
    pub fn radio_enabled(self) -> bool {
        self.raw != SIGNAL_RADIO_OFF
    }

    /// Rough signal quality in percent (0 when the radio is off).
    ///
    /// The scale of [`Signal::raw`] is not documented; this assumes lower raw
    /// values mean a better signal, with 28 or less counting as 100 %.
    pub fn percent(self) -> u8 {
        if !self.radio_enabled() {
            return 0;
        }
        let estimate: i16 = 128 - i16::from(self.raw);
        // Clamped to 0..=100, so the cast cannot truncate.
        estimate.clamp(0, 100) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_queries() {
        assert_eq!(init()[..2], [0x90, 0x00]);
        assert_eq!(query_battery()[..2], [0x90, 0x0a]);
        assert_eq!(query_signal()[..2], [0x90, 0x70]);
        for report in [init(), query_battery(), query_signal()] {
            assert!(report[2..].iter().all(|&byte| byte == 0));
        }
    }

    #[test]
    fn led_report_layout() {
        let report = set_leds(Rgb::new(1, 2, 3), Rgb::new(4, 5, 6));
        assert_eq!(
            report[..15],
            [
                0x10, 0x10, 0x0b, 0x00, 0x09, 0x64, 0x64, 0x64, 0x06, 1, 2, 3, 4, 5, 6
            ]
        );
        assert!(report[15..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn parses_battery_reply() {
        // Captured from the dongle at 54 %.
        let reply = report_with_prefix(&[
            0x90, 0x0a, 0x04, 0x00, 0x00, 0x32, 0x00, 0x00, 0xe4, 0x0a, 0x36,
        ]);
        let battery = parse_battery(&reply).expect("battery reply");
        assert_eq!(battery.raw, 54);
        assert_eq!(battery.percent(), Some(54));
    }

    #[test]
    fn battery_out_of_range_is_not_a_percentage() {
        assert_eq!(Battery { raw: 100 }.percent(), Some(100));
        assert_eq!(Battery { raw: 101 }.percent(), None);
        assert_eq!(Battery { raw: 0xff }.percent(), None);
    }

    #[test]
    fn recognises_init_reply() {
        assert!(is_init_reply(&report_with_prefix(&[
            0x90, 0x00, 0x03, 0x00, 0xec, 0xfa, 0x03
        ])));
        assert!(!is_init_reply(&report_with_prefix(&[0x90, 0x0a])));
    }

    #[test]
    fn ignores_foreign_replies() {
        let signal_reply = report_with_prefix(&[0x90, 0x70, 0, 0, 40]);
        assert_eq!(parse_battery(&signal_reply), None);
        assert_eq!(parse_signal(&report_with_prefix(&[0x10, 0x0a])), None);
        assert_eq!(parse_battery(&[0; REPORT_LEN]), None);
    }

    #[test]
    fn parses_signal_reply() {
        let reply = report_with_prefix(&[0x90, 0x70, 0, 0, 40]);
        let signal = parse_signal(&reply).expect("signal reply");
        assert_eq!(signal.raw, 40);
        assert!(signal.radio_enabled());
        assert_eq!(signal.percent(), 88);
    }

    #[test]
    fn signal_percent_edges() {
        assert_eq!(Signal { raw: 0 }.percent(), 100);
        assert_eq!(Signal { raw: 200 }.percent(), 0);
        let off = Signal { raw: 236 };
        assert!(!off.radio_enabled());
        assert_eq!(off.percent(), 0);
    }
}
