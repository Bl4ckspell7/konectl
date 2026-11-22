//! Wire format of the Kone Pro Air vendor HID channel.
//!
//! The mouse exposes a vendor-defined HID interface (usage page `0xFF00`) with
//! 64-byte input and output reports and no report IDs. Every command is one
//! output report; answers arrive as input reports on the same interface.
//!
//! Requests start with `90` (read) or `10` (write), the command byte, the
//! payload length and a block number (`00` for most commands). Writes are
//! answered with a short status reply.
//!
//! Everything here is pure data handling and works without hardware. See
//! `docs/protocol.md` for what is known about each command.

use crate::color::Rgb;
use crate::settings::{
    Firmware, Info, LIGHTING_LEN, Lighting, PROFILE_COUNT, PROFILE_ENTRY_LEN, ProfileEntry,
    SENSOR_LEN, Sensor,
};

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

const OP_READ: u8 = 0x90;
const OP_WRITE: u8 = 0x10;

const CMD_INFO: u8 = 0x00;
const CMD_LIGHTING: u8 = 0x01;
const CMD_ACTIVATE_PROFILE: u8 = 0x03;
const CMD_BATTERY: u8 = 0x0a;
const CMD_LIVE_LEDS: u8 = 0x10;
const CMD_PROFILE_TABLE: u8 = 0x11;
const CMD_SELECT_PROFILE: u8 = 0x13;
const CMD_WRITE_BLOCK: u8 = 0x50;
const CMD_SIGNAL: u8 = 0x70;

const BLOCK_SENSOR: u8 = 0;
const BLOCK_LIGHTING: u8 = 1;

/// Payload the official software sends with the info request; its meaning is
/// unknown. Without it the mouse answers with an error.
const INFO_KEY: [u8; 4] = [0x18, 0x25, 0x51, 0x32];

/// Offset of the payload in a request or data answer.
const PAYLOAD_OFFSET: usize = 4;

/// Payload length of the info answer.
const INFO_LEN: u8 = 0x26;
/// Mouse firmware (major, minor) in the info answer.
const INFO_FIRMWARE_OFFSET: usize = 5;
/// Block 0 bytes 0–18 in the info answer; byte 19 (debounce) is elsewhere.
const INFO_SENSOR_OFFSET: usize = 10;
/// Dongle firmware (major, minor) in the info answer; zero over the cable.
const INFO_DONGLE_FIRMWARE_OFFSET: usize = 30;
const INFO_DEBOUNCE_OFFSET: usize = 41;
const SENSOR_DEBOUNCE: usize = SENSOR_LEN - 1;

/// Start of the "set live LED colours" command: effect `09`, `64 64`,
/// brightness 100, speed 6 (the layout of the saved lighting block).
const LED_HEADER: [u8; 9] = [
    OP_WRITE,
    CMD_LIVE_LEDS,
    0x0b,
    0x00,
    0x09,
    0x64,
    0x64,
    0x64,
    0x06,
];

const BATTERY_LEN: u8 = 0x04;
const BATTERY_OFFSET: usize = 10;
const SIGNAL_OFFSET: usize = 4;
/// Raw signal value reported while the radio is switched off.
const SIGNAL_RADIO_OFF: u8 = 236;

/// Error code of the answer to the bare [`init`] request.
pub(crate) const INIT_ERROR_CODE: u8 = 0x03;

/// Status replies: `<op> <cmd> 03 00 ec <status> [code]`.
const STATUS_LEN: u8 = 0x03;
const STATUS_MARKER: u8 = 0xec;
const STATUS_ACCEPTED: u8 = 0xac;
const STATUS_ERROR: u8 = 0xfa;

/// Builds a report that starts with `prefix` and is zero-padded to [`REPORT_LEN`].
///
/// # Panics
/// If `prefix` is longer than [`REPORT_LEN`].
pub(crate) fn report_with_prefix(prefix: &[u8]) -> Report {
    let mut report: Report = [0; REPORT_LEN];
    report[..prefix.len()].copy_from_slice(prefix);
    report
}

/// Builds a request with a header and `payload`.
///
/// # Panics
/// If `payload` does not fit into a report.
fn request(op: u8, command: u8, block: u8, payload: &[u8]) -> Report {
    let length: u8 = u8::try_from(payload.len()).expect("payload length fits into a byte");
    let mut report: Report = report_with_prefix(&[op, command, length, block]);
    report[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload.len()].copy_from_slice(payload);
    report
}

/// Initialisation command, sent once after opening the device. The mouse
/// answers with an error status (`90 00 03 00 ec fa 03`), which is enough to
/// synchronise with it.
pub fn init() -> Report {
    report_with_prefix(&[OP_READ, CMD_INFO])
}

/// Asks for the battery level; answered by a report [`parse_battery`] accepts.
pub fn query_battery() -> Report {
    report_with_prefix(&[OP_READ, CMD_BATTERY])
}

/// Asks for the radio signal; answered by a report [`parse_signal`] accepts.
pub fn query_signal() -> Report {
    report_with_prefix(&[OP_READ, CMD_SIGNAL])
}

/// Asks for the firmware versions and block 0 of the selected profile;
/// answered by a report [`parse_info`] accepts.
pub fn query_info() -> Report {
    request(OP_READ, CMD_INFO, 0, &INFO_KEY)
}

/// Asks for the saved lighting of the selected profile; answered by a report
/// [`parse_lighting`] accepts.
pub fn query_lighting() -> Report {
    report_with_prefix(&[OP_READ, CMD_LIGHTING])
}

/// Asks for the profile table; answered by a report
/// [`parse_profile_table`] accepts.
pub fn query_profile_table() -> Report {
    report_with_prefix(&[OP_READ, CMD_PROFILE_TABLE])
}

/// Selects the profile (1–5) whose settings the following reads return.
pub fn select_profile(profile: u8) -> Report {
    request(OP_WRITE, CMD_SELECT_PROFILE, 0, &[profile])
}

/// Makes `profile` (1–5) the active profile.
pub fn activate_profile(profile: u8) -> Report {
    request(OP_WRITE, CMD_ACTIVATE_PROFILE, 0, &[profile])
}

/// Writes entry `profile` (1–5) of the profile table. The official software
/// sends it before the settings blocks of that profile.
pub fn write_profile_entry(profile: u8, entry: &ProfileEntry) -> Report {
    let mut payload: [u8; 1 + PROFILE_ENTRY_LEN] = [0; 1 + PROFILE_ENTRY_LEN];
    payload[0] = profile;
    payload[1..].copy_from_slice(entry);
    request(OP_WRITE, CMD_PROFILE_TABLE, 0, &payload)
}

/// Writes settings block 0 of the profile named in its first byte.
pub fn write_sensor(sensor: &Sensor) -> Report {
    request(
        OP_WRITE,
        CMD_WRITE_BLOCK,
        BLOCK_SENSOR,
        &sensor.write_bytes(),
    )
}

/// Writes settings block 1 (saved lighting). Follows the profile table entry
/// and block 0 of the same profile.
pub fn write_lighting(lighting: &Lighting) -> Report {
    request(OP_WRITE, CMD_WRITE_BLOCK, BLOCK_LIGHTING, &lighting.bytes())
}

/// Sets both LED zones to fixed colours until the mouse sleeps (volatile).
pub fn set_leds(left: Rgb, right: Rgb) -> Report {
    let mut report: Report = report_with_prefix(&LED_HEADER);
    let start: usize = LED_HEADER.len();
    report[start..start + 3].copy_from_slice(&left.to_bytes());
    report[start + 3..start + 6].copy_from_slice(&right.to_bytes());
    report
}

/// `true` if `report` echoes the operation and command of `request`.
fn is_reply_to(report: &Report, request: &Report) -> bool {
    report[..2] == request[..2]
}

fn is_reply(report: &Report, op: u8, command: u8) -> bool {
    report[0] == op && report[1] == command
}

/// `true` if `report` is a data answer of `command` with `length` payload bytes.
fn is_data_reply(report: &Report, command: u8, length: u8) -> bool {
    is_reply(report, OP_READ, command) && report[2] == length && report[3] == 0
}

/// Outcome reported by a status reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `ec ac`: the request was carried out.
    Accepted,
    /// `ec fa <code>`: the request failed.
    Error(u8),
}

/// The status, if `report` is a status reply to `request`.
pub fn parse_status(report: &Report, request: &Report) -> Option<Status> {
    if !is_reply_to(report, request)
        || report[2] != STATUS_LEN
        || report[3] != 0
        || report[4] != STATUS_MARKER
    {
        return None;
    }
    match report[5] {
        STATUS_ACCEPTED => Some(Status::Accepted),
        STATUS_ERROR => Some(Status::Error(report[6])),
        _ => None,
    }
}

/// `true` if `report` answers [`init`].
pub fn is_init_reply(report: &Report) -> bool {
    is_reply(report, OP_READ, CMD_INFO)
}

/// `true` if `report` is the answer to [`query_info`] the dongle sends on its
/// own while the mouse is asleep or switched off (`90 00 39 00` and zeros).
pub fn is_info_without_mouse(report: &Report) -> bool {
    is_reply(report, OP_READ, CMD_INFO) && report[2] != INFO_LEN && report[2] != STATUS_LEN
}

/// Firmware versions and block 0, if `report` answers [`query_info`].
pub fn parse_info(report: &Report) -> Option<Info> {
    if !is_data_reply(report, CMD_INFO, INFO_LEN) {
        return None;
    }
    let mut sensor: [u8; SENSOR_LEN] = [0; SENSOR_LEN];
    sensor[..SENSOR_DEBOUNCE]
        .copy_from_slice(&report[INFO_SENSOR_OFFSET..INFO_SENSOR_OFFSET + SENSOR_DEBOUNCE]);
    sensor[SENSOR_DEBOUNCE] = report[INFO_DEBOUNCE_OFFSET];
    let firmware_at = |offset: usize| Firmware {
        major: report[offset],
        minor: report[offset + 1],
    };
    let dongle_firmware: Firmware = firmware_at(INFO_DONGLE_FIRMWARE_OFFSET);
    Some(Info {
        firmware: firmware_at(INFO_FIRMWARE_OFFSET),
        // Over the USB cable these bytes are zero.
        dongle_firmware: (!dongle_firmware.is_zero()).then_some(dongle_firmware),
        sensor: Sensor::from_bytes(sensor),
    })
}

/// Saved lighting, if `report` answers [`query_lighting`].
pub fn parse_lighting(report: &Report) -> Option<Lighting> {
    // LIGHTING_LEN is 11, so the cast cannot truncate.
    if !is_data_reply(report, CMD_LIGHTING, LIGHTING_LEN as u8) {
        return None;
    }
    let mut bytes: [u8; LIGHTING_LEN] = [0; LIGHTING_LEN];
    bytes.copy_from_slice(&report[PAYLOAD_OFFSET..PAYLOAD_OFFSET + LIGHTING_LEN]);
    Some(Lighting::from_bytes(bytes))
}

/// The five profile table entries, if `report` answers [`query_profile_table`].
pub fn parse_profile_table(report: &Report) -> Option<[ProfileEntry; PROFILE_COUNT as usize]> {
    const TABLE_LEN: usize = PROFILE_ENTRY_LEN * PROFILE_COUNT as usize;
    // TABLE_LEN is 25, so the cast cannot truncate.
    if !is_data_reply(report, CMD_PROFILE_TABLE, TABLE_LEN as u8) {
        return None;
    }
    Some(std::array::from_fn(|index| {
        let start: usize = PAYLOAD_OFFSET + index * PROFILE_ENTRY_LEN;
        let mut entry: ProfileEntry = [0; PROFILE_ENTRY_LEN];
        entry.copy_from_slice(&report[start..start + PROFILE_ENTRY_LEN]);
        entry
    }))
}

/// Battery state, if `report` answers [`query_battery`]. Error replies (mouse
/// switched off) are not battery states.
pub fn parse_battery(report: &Report) -> Option<Battery> {
    is_data_reply(report, CMD_BATTERY, BATTERY_LEN).then(|| Battery {
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

/// Radio signal, if `report` answers [`query_signal`]. The error reply sent
/// without a radio link (`ec` at offset 4) counts as "radio off".
pub fn parse_signal(report: &Report) -> Option<Signal> {
    is_reply(report, OP_READ, CMD_SIGNAL).then(|| Signal {
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
pub(crate) mod tests {
    use super::*;
    use crate::settings::{Effect, LiftOff, PollingRate};

    /// Info answer captured at the start (profile 1, 600 DPI on stage 2,
    /// mouse firmware 4.32, dongle firmware 4.19, debounce 2 ms).
    pub(crate) const INFO_REPLY: [u8; 42] = [
        0x90, 0x00, 0x26, 0x00, 0x00, 0x04, 0x20, 0x00, 0x92, 0x2c, 0x01, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x86, 0x0f, 0x00, 0x00,
        0x04, 0x13, 0x00, 0x00, 0x01, 0x04, 0x00, 0x00, 0x01, 0x02, 0x00, 0x02,
    ];

    /// Info answer captured over the USB cable (profile 1, 900 DPI on stage 2,
    /// mouse firmware 4.32, no dongle firmware).
    pub(crate) const CABLE_INFO_REPLY: [u8; 42] = [
        0x90, 0x00, 0x26, 0x00, 0x00, 0x04, 0x20, 0x00, 0x92, 0x2c, 0x01, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x06, 0x0f, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
    ];

    /// Lighting answer captured at the start (profile 1, wave).
    pub(crate) const LIGHTING_REPLY: [u8; 15] = [
        0x90, 0x01, 0x0b, 0x00, 0x06, 0x64, 0x64, 0x46, 0x04, 0x00, 0xff, 0x00, 0x00, 0xff, 0x64,
    ];

    /// Profile table answer: slots 1–3 in use.
    pub(crate) const PROFILE_TABLE_REPLY: [u8; 16] = [
        0x90, 0x11, 0x19, 0x00, 0x01, 0x64, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x00, 0x00, 0x01,
        0x64,
    ];

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
    fn settings_requests_match_captures() {
        let cases: [(Report, &[u8]); 6] = [
            (
                query_info(),
                &[0x90, 0x00, 0x04, 0x00, 0x18, 0x25, 0x51, 0x32],
            ),
            (query_lighting(), &[0x90, 0x01]),
            (query_profile_table(), &[0x90, 0x11]),
            (select_profile(2), &[0x10, 0x13, 0x01, 0x00, 0x02]),
            (activate_profile(2), &[0x10, 0x03, 0x01, 0x00, 0x02]),
            (
                write_profile_entry(1, &[0x01, 0x64, 0x00, 0x00, 0x00]),
                &[0x10, 0x11, 0x06, 0x00, 0x01, 0x01, 0x64],
            ),
        ];
        for (report, expected) in cases {
            assert_eq!(report, report_with_prefix(expected));
        }
    }

    #[test]
    fn block_writes_match_captures() {
        let info = parse_info(&report_with_prefix(&INFO_REPLY)).expect("info");
        let lighting = parse_lighting(&report_with_prefix(&LIGHTING_REPLY)).expect("lighting");
        // Written back unchanged, except bit 7 of byte 16 (capture 01 without the DPI change).
        assert_eq!(
            write_sensor(&info.sensor),
            report_with_prefix(&[
                0x10, 0x50, 0x14, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x06, 0x0f, 0x00, 0x02
            ])
        );
        assert_eq!(
            write_lighting(&lighting),
            report_with_prefix(&[
                0x10, 0x50, 0x0b, 0x01, 0x06, 0x64, 0x64, 0x46, 0x04, 0x00, 0xff, 0x00, 0x00, 0xff,
                0x64
            ])
        );
    }

    #[test]
    fn parses_info_reply() {
        let info = parse_info(&report_with_prefix(&INFO_REPLY)).expect("info reply");
        assert_eq!(info.firmware.to_string(), "4.32");
        assert_eq!(
            info.dongle_firmware.expect("dongle firmware").to_string(),
            "4.19"
        );
        assert_eq!(info.sensor.profile(), 1);
        assert_eq!(info.sensor.polling_rate(), Some(PollingRate::Hz1000));
        assert_eq!(
            info.sensor.dpi_stages(),
            [None, Some(600), None, None, None]
        );
        assert_eq!(info.sensor.lift_off(), LiftOff::Low);
        assert_eq!(info.sensor.debounce_ms(), 2);
        assert_eq!(info.sensor.bytes()[16], 0x86);
    }

    #[test]
    fn parses_lighting_and_profile_table() {
        let lighting = parse_lighting(&report_with_prefix(&LIGHTING_REPLY)).expect("lighting");
        assert_eq!(lighting.effect(), Effect::Wave);
        let table =
            parse_profile_table(&report_with_prefix(&PROFILE_TABLE_REPLY)).expect("profile table");
        assert_eq!(table[0], [0x01, 0x64, 0x00, 0x00, 0x00]);
        assert_eq!(table[2], [0x01, 0x64, 0x00, 0x00, 0x00]);
        assert_eq!(table[3], [0; PROFILE_ENTRY_LEN]);
    }

    #[test]
    fn init_error_reply_is_not_info() {
        let init_reply = report_with_prefix(&[0x90, 0x00, 0x03, 0x00, 0xec, 0xfa, 0x03]);
        assert!(is_init_reply(&init_reply));
        assert_eq!(parse_info(&init_reply), None);
        assert_eq!(
            parse_status(&init_reply, &query_info()),
            Some(Status::Error(0x03))
        );
    }

    #[test]
    fn cable_info_has_no_dongle_firmware() {
        let info = parse_info(&report_with_prefix(&CABLE_INFO_REPLY)).expect("info reply");
        assert_eq!(info.dongle_firmware, None);
        assert_eq!(info.firmware.to_string(), "4.32");
        assert_eq!(info.sensor.profile(), 1);
        assert_eq!(info.sensor.dpi(2).expect("stage 2"), Some(900));
    }

    #[test]
    fn recognises_info_answer_without_mouse() {
        // Captured while the mouse was asleep.
        let reply = report_with_prefix(&[0x90, 0x00, 0x39, 0x00]);
        assert!(is_info_without_mouse(&reply));
        assert_eq!(parse_info(&reply), None);
        assert!(!is_info_without_mouse(&report_with_prefix(&INFO_REPLY)));
        assert!(!is_info_without_mouse(&report_with_prefix(&[
            0x90, 0x00, 0x03, 0x00, 0xec, 0xfa, 0x03
        ])));
    }

    #[test]
    fn parses_status_replies() {
        let request = write_lighting(&Lighting::from_bytes([0; LIGHTING_LEN]));
        let accepted = report_with_prefix(&[0x10, 0x50, 0x03, 0x00, 0xec, 0xac]);
        assert_eq!(parse_status(&accepted, &request), Some(Status::Accepted));
        let other_command = report_with_prefix(&[0x10, 0x11, 0x03, 0x00, 0xec, 0xac]);
        assert_eq!(parse_status(&other_command, &request), None);
        let data = report_with_prefix(&LIGHTING_REPLY);
        assert_eq!(parse_status(&data, &query_lighting()), None);
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
    fn battery_error_reply_is_not_a_battery_state() {
        let reply = report_with_prefix(&[0x90, 0x0a, 0x03, 0x00, 0xec, 0xfa, 0x02]);
        assert_eq!(parse_battery(&reply), None);
        assert_eq!(
            parse_status(&reply, &query_battery()),
            Some(Status::Error(0x02))
        );
    }

    #[test]
    fn battery_out_of_range_is_not_a_percentage() {
        assert_eq!(Battery { raw: 100 }.percent(), Some(100));
        assert_eq!(Battery { raw: 101 }.percent(), None);
        assert_eq!(Battery { raw: 0xff }.percent(), None);
    }

    #[test]
    fn ignores_foreign_replies() {
        let signal_reply = report_with_prefix(&[0x90, 0x70, 0, 0, 40]);
        assert_eq!(parse_battery(&signal_reply), None);
        assert_eq!(parse_signal(&report_with_prefix(&[0x10, 0x0a])), None);
        assert_eq!(parse_battery(&[0; REPORT_LEN]), None);
        assert_eq!(parse_lighting(&signal_reply), None);
        assert_eq!(parse_info(&report_with_prefix(&[0x90, 0x00])), None);
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
    fn signal_error_reply_means_radio_off() {
        let reply = report_with_prefix(&[0x90, 0x70, 0x03, 0x00, 0xec, 0xfa, 0x02]);
        let signal = parse_signal(&reply).expect("signal reply");
        assert!(!signal.radio_enabled());
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
