//! Device discovery and the high-level mouse API.

use std::ffi::CString;
use std::fmt;
use std::time::{Duration, Instant};

use hidapi::HidApi;

use crate::backup::{Backup, ProfileBackup};
use crate::color::Rgb;
use crate::error::{Error, Result};
use crate::protocol::{self, Battery, Report, Signal, Status};
use crate::settings::{
    Effect, Info, Lighting, PROFILE_COUNT, ProfileEntry, ProfileSettings, Sensor, profile_in_use,
    profile_index,
};
use crate::transport::{HidTransport, Transport};

/// How long a single read waits for a report.
const READ_TIMEOUT_MS: i32 = 20;
/// How long a request waits for its answer.
const REPLY_DEADLINE: Duration = Duration::from_millis(500);
/// Silence after the last reply at which [`KoneProAir::raw`] stops collecting.
const RAW_QUIET_PERIOD: Duration = Duration::from_millis(100);
/// Upper bound on replies collected by [`KoneProAir::raw`].
const MAX_RAW_REPLIES: usize = 64;
/// Upper bound on stale reports discarded before a request.
const MAX_STALE_REPORTS: usize = 64;

/// How the mouse is connected to the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    /// Through the 2.4 GHz USB dongle.
    Wireless,
    /// Through the USB cable.
    Wired,
}

impl ConnectionMode {
    /// USB product ID the mouse uses in this mode.
    pub const fn product_id(self) -> u16 {
        match self {
            Self::Wireless => protocol::PRODUCT_ID_DONGLE,
            Self::Wired => protocol::PRODUCT_ID_WIRED,
        }
    }

    /// USB interface carrying the vendor channel in this mode.
    pub const fn interface(self) -> i32 {
        match self {
            Self::Wireless => protocol::INTERFACE_DONGLE,
            Self::Wired => protocol::INTERFACE_WIRED,
        }
    }

    /// The mode whose vendor channel is `interface` of `product_id`, if any.
    fn from_ids(product_id: u16, interface: i32) -> Option<Self> {
        [Self::Wired, Self::Wireless]
            .into_iter()
            .find(|mode| mode.product_id() == product_id && mode.interface() == interface)
    }
}

impl fmt::Display for ConnectionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Self::Wireless => "wireless (dongle)",
            Self::Wired => "wired (cable)",
        })
    }
}

/// A connected Kone Pro Air found during discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub mode: ConnectionMode,
    /// Platform path of the vendor channel, e.g. `/dev/hidraw2`.
    pub path: CString,
    pub serial: Option<String>,
}

/// Finds and opens Kone Pro Air devices.
pub struct Manager {
    api: HidApi,
}

impl Manager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: HidApi::new()?,
        })
    }

    /// Re-scans the system for devices.
    pub fn refresh(&mut self) -> Result<()> {
        self.api.refresh_devices()?;
        Ok(())
    }

    /// All connected Kone Pro Air devices, wired ones first.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let mut devices: Vec<DeviceInfo> = self
            .api
            .device_list()
            .filter(|device| device.vendor_id() == protocol::VENDOR_ID)
            .filter_map(|device| {
                let mode =
                    ConnectionMode::from_ids(device.product_id(), device.interface_number())?;
                Some(DeviceInfo {
                    mode,
                    path: device.path().to_owned(),
                    serial: device
                        .serial_number()
                        .filter(|serial| !serial.is_empty())
                        .map(str::to_owned),
                })
            })
            .collect();
        devices.sort_by_key(|device| device.mode != ConnectionMode::Wired);
        devices.dedup_by(|a, b| a.path == b.path);
        devices
    }

    /// Opens and initialises the given device.
    pub fn open(&self, info: &DeviceInfo) -> Result<KoneProAir<HidTransport>> {
        let device = self.api.open_path(&info.path)?;
        KoneProAir::connect(HidTransport::new(device), info.mode)
    }

    /// Opens the first device, preferring a cable connection.
    pub fn open_first(&self) -> Result<KoneProAir<HidTransport>> {
        let info: DeviceInfo = self.devices().into_iter().next().ok_or(Error::NotFound)?;
        self.open(&info)
    }
}

/// An opened ROCCAT Kone Pro Air.
pub struct KoneProAir<T: Transport> {
    transport: T,
    mode: ConnectionMode,
}

impl<T: Transport> KoneProAir<T> {
    /// Wraps an opened vendor channel and sends the init command.
    pub fn connect(transport: T, mode: ConnectionMode) -> Result<Self> {
        let mut mouse = Self { transport, mode };
        mouse.discard_pending(0)?;
        mouse.transport.write_report(&protocol::init())?;
        // Consume the init answer so it cannot be mistaken for the reply to a
        // later request. Its content is not used and a missing answer is not fatal.
        mouse.await_reply(|reply| protocol::is_init_reply(reply).then_some(()))?;
        Ok(mouse)
    }

    pub fn mode(&self) -> ConnectionMode {
        self.mode
    }

    /// Battery state.
    pub fn battery(&mut self) -> Result<Battery> {
        self.request(
            &protocol::query_battery(),
            "battery query",
            protocol::parse_battery,
        )
    }

    /// Radio signal between mouse and dongle.
    pub fn signal(&mut self) -> Result<Signal> {
        self.request(
            &protocol::query_signal(),
            "signal query",
            protocol::parse_signal,
        )
    }

    /// Sets both LED zones. Not saved: the mouse goes back to its stored
    /// lighting after it wakes from sleep.
    pub fn set_leds(&mut self, left: Rgb, right: Rgb) -> Result<()> {
        self.transport
            .write_report(&protocol::set_leds(left, right))
    }

    /// Firmware version and the sensor settings of the active profile.
    ///
    /// Every settings read starts here, so it fails with [`Error::NotReachable`]
    /// while the mouse sleeps or is switched off.
    pub fn info(&mut self) -> Result<Info> {
        const WHAT: &str = "info query";
        let request: Report = protocol::query_info();
        self.exchange(&request, WHAT, |reply| {
            if let Some(info) = protocol::parse_info(reply) {
                return Some(Ok(info));
            }
            if protocol::is_info_without_mouse(reply) {
                return Some(Err(Error::NotReachable));
            }
            match protocol::parse_status(reply, &request) {
                // A late answer to the bare init request, which shares the header.
                Some(Status::Error(protocol::INIT_ERROR_CODE)) | None => None,
                Some(Status::Error(code)) => Some(Err(Error::Rejected {
                    command: WHAT,
                    code,
                })),
                Some(Status::Accepted) => None,
            }
        })
    }

    /// Number (1–5) of the active profile.
    pub fn active_profile(&mut self) -> Result<u8> {
        Ok(self.info()?.sensor.profile())
    }

    /// The profile table: one entry per slot, see [`profile_in_use`].
    pub fn profile_table(&mut self) -> Result<[ProfileEntry; PROFILE_COUNT as usize]> {
        self.request(
            &protocol::query_profile_table(),
            "profile table query",
            protocol::parse_profile_table,
        )
    }

    /// Numbers of the profiles in use, e.g. `[1, 2, 3]`.
    pub fn profiles(&mut self) -> Result<Vec<u8>> {
        Ok(self
            .profile_table()?
            .iter()
            .zip(1u8..)
            .filter(|(entry, _)| profile_in_use(entry))
            .map(|(_, number)| number)
            .collect())
    }

    /// Makes `profile` (1–5) the active profile.
    pub fn set_active_profile(&mut self, profile: u8) -> Result<()> {
        self.profile_entry(profile)?;
        self.command(&protocol::activate_profile(profile), "profile activation")?;
        if self.active_profile()? != profile {
            return Err(Error::VerifyFailed("active profile"));
        }
        Ok(())
    }

    /// Reads the settings of `profile` (1–5), or of the active profile with `None`.
    ///
    /// Reading another profile selects it for the reads and selects the active
    /// profile again afterwards, as the official software does. If the program
    /// is interrupted in between, the mouse keeps reporting the other profile
    /// as active until a profile is activated with [`Self::set_active_profile`].
    pub fn settings(&mut self, profile: Option<u8>) -> Result<ProfileSettings> {
        let active: u8 = self.active_profile()?;
        let target: u8 = profile.unwrap_or(active);
        if target == active {
            return self.read_selected();
        }
        self.profile_entry(target)?;
        self.command(&protocol::select_profile(target), "profile selection")?;
        let settings: Result<ProfileSettings> = self.read_selected();
        // Select the active profile again even if the reads failed.
        let restored: Result<()> =
            self.command(&protocol::select_profile(active), "profile selection");
        let settings: ProfileSettings = settings?;
        restored?;
        if settings.profile() != target {
            return Err(Error::VerifyFailed("profile selection"));
        }
        Ok(settings)
    }

    /// Writes `settings` to the profile they belong to and checks that they
    /// read back unchanged.
    ///
    /// Like the official software, this writes the profile's table entry (as
    /// read), then settings blocks 0 and 1. Bytes konectl does not understand
    /// are written back as they were read.
    ///
    /// With the AIMO effect the mouse reports the colours it currently shows
    /// instead of the saved ones; writing such lighting back saves those
    /// colours. Use [`Self::write_sensor`] when only the sensor settings changed.
    pub fn write_settings(&mut self, settings: &ProfileSettings) -> Result<()> {
        let entry: ProfileEntry = self.profile_entry(settings.profile())?;
        self.write_blocks(&entry, &settings.sensor, Some(&settings.lighting))
    }

    /// Writes only block 0 (polling rate, DPI, lift-off) of the profile it
    /// belongs to, after the profile's table entry, and checks that it reads
    /// back unchanged. The saved lighting is left alone.
    pub fn write_sensor(&mut self, sensor: &Sensor) -> Result<()> {
        let entry: ProfileEntry = self.profile_entry(sensor.profile())?;
        self.write_blocks(&entry, sensor, None)
    }

    /// Reads the settings of all profiles in use.
    pub fn backup(&mut self) -> Result<Backup> {
        let info: Info = self.info()?;
        let table = self.profile_table()?;
        let mut profiles: Vec<ProfileBackup> = Vec::new();
        for (entry, number) in table.into_iter().zip(1u8..) {
            if profile_in_use(&entry) {
                let settings: ProfileSettings = self.settings(Some(number))?;
                profiles.push(ProfileBackup { entry, settings });
            }
        }
        Ok(Backup {
            firmware: info.firmware,
            active_profile: info.sensor.profile(),
            profiles,
        })
    }

    /// Checks that `backup` fits the mouse: if both the backup and the mouse
    /// name a firmware version, they must match.
    pub fn check_backup(&mut self, backup: &Backup) -> Result<()> {
        if let (Some(mouse_firmware), Some(backup_firmware)) =
            (self.info()?.firmware, backup.firmware)
            && mouse_firmware != backup_firmware
        {
            return Err(Error::InvalidBackup(format!(
                "it was made with firmware {backup_firmware}, the mouse has {mouse_firmware}"
            )));
        }
        Ok(())
    }

    /// Writes a backup back to the mouse and activates its active profile,
    /// after [`Self::check_backup`].
    pub fn restore(&mut self, backup: &Backup) -> Result<()> {
        self.check_backup(backup)?;
        for profile in &backup.profiles {
            let settings: &ProfileSettings = &profile.settings;
            self.write_blocks(&profile.entry, &settings.sensor, Some(&settings.lighting))?;
        }
        self.set_active_profile(backup.active_profile)
    }

    /// Table entry of `profile`; fails if the slot is not in use.
    fn profile_entry(&mut self, profile: u8) -> Result<ProfileEntry> {
        let index: usize = profile_index(profile)?;
        let entry: ProfileEntry = self.profile_table()?[index];
        if !profile_in_use(&entry) {
            return Err(Error::UnusedProfile(profile));
        }
        Ok(entry)
    }

    /// Reads blocks 0 and 1 of the selected profile.
    fn read_selected(&mut self) -> Result<ProfileSettings> {
        let sensor = self.info()?.sensor;
        let lighting = self.request(
            &protocol::query_lighting(),
            "lighting query",
            protocol::parse_lighting,
        )?;
        Ok(ProfileSettings { sensor, lighting })
    }

    /// Writes the table entry, block 0 and (if given) block 1 of the profile
    /// named in `sensor`, then reads them back to check them.
    fn write_blocks(
        &mut self,
        entry: &ProfileEntry,
        sensor: &Sensor,
        lighting: Option<&Lighting>,
    ) -> Result<()> {
        let profile: u8 = sensor.profile();
        profile_index(profile)?;
        self.command(
            &protocol::write_profile_entry(profile, entry),
            "profile table write",
        )?;
        self.command(&protocol::write_sensor(sensor), "sensor settings write")?;
        if let Some(lighting) = lighting {
            self.command(&protocol::write_lighting(lighting), "lighting write")?;
        }

        let stored: ProfileSettings = self.settings(Some(profile))?;
        if stored.sensor.bytes() != sensor.write_bytes() {
            return Err(Error::VerifyFailed("sensor settings"));
        }
        if let Some(lighting) = lighting {
            // With the AIMO effect the mouse reports what it currently shows
            // instead of the saved values, so only the effect can be compared.
            let lighting_stored: bool = if lighting.effect() == Effect::Aimo {
                stored.lighting.effect() == Effect::Aimo
            } else {
                stored.lighting == *lighting
            };
            if !lighting_stored {
                return Err(Error::VerifyFailed("lighting settings"));
            }
        }
        Ok(())
    }

    /// Sends an arbitrary report and returns every reply that follows, until
    /// the channel has been quiet for 100 ms (or for 500 ms if nothing came).
    ///
    /// Debugging aid for protocol work: unknown commands may change or break
    /// the mouse configuration.
    pub fn raw(&mut self, report: &Report) -> Result<Vec<Report>> {
        self.discard_pending(0)?;
        self.transport.write_report(report)?;
        let mut replies: Vec<Report> = Vec::new();
        let mut last_activity: Instant = Instant::now();
        while replies.len() < MAX_RAW_REPLIES {
            let quiet_limit: Duration = if replies.is_empty() {
                REPLY_DEADLINE
            } else {
                RAW_QUIET_PERIOD
            };
            if last_activity.elapsed() >= quiet_limit {
                break;
            }
            if let Some(reply) = self.transport.read_report(READ_TIMEOUT_MS)? {
                replies.push(reply);
                last_activity = Instant::now();
            }
        }
        Ok(replies)
    }

    /// Sends `report` and returns the first reply `parse` accepts.
    /// An error status reply to `report` fails the request with [`Error::Rejected`].
    fn request<R>(
        &mut self,
        report: &Report,
        what: &'static str,
        parse: impl Fn(&Report) -> Option<R>,
    ) -> Result<R> {
        self.exchange(report, what, |reply| {
            if let Some(value) = parse(reply) {
                return Some(Ok(value));
            }
            match protocol::parse_status(reply, report) {
                Some(Status::Error(code)) => Some(Err(Error::Rejected {
                    command: what,
                    code,
                })),
                _ => None,
            }
        })
    }

    /// Sends `report` and returns the outcome of the first reply `classify`
    /// recognises (`Some`); other replies are skipped.
    fn exchange<R>(
        &mut self,
        report: &Report,
        what: &'static str,
        classify: impl Fn(&Report) -> Option<Result<R>>,
    ) -> Result<R> {
        self.discard_pending(0)?;
        self.transport.write_report(report)?;
        self.await_reply(classify)?
            .unwrap_or(Err(Error::Timeout(what)))
    }

    /// Sends a write command and waits until the mouse accepts it.
    fn command(&mut self, report: &Report, what: &'static str) -> Result<()> {
        self.request(report, what, |reply| {
            (protocol::parse_status(reply, report) == Some(Status::Accepted)).then_some(())
        })
    }

    /// Reads until `parse` accepts a report, skipping all others.
    /// Returns `Ok(None)` when nothing matched within [`REPLY_DEADLINE`].
    fn await_reply<R>(&mut self, parse: impl Fn(&Report) -> Option<R>) -> Result<Option<R>> {
        let deadline: Instant = Instant::now() + REPLY_DEADLINE;
        while Instant::now() < deadline {
            if let Some(reply) = self.transport.read_report(READ_TIMEOUT_MS)?
                && let Some(value) = parse(&reply)
            {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Drops queued input reports so a request is not answered by an old reply.
    /// Waits up to `first_timeout_ms` for the first one.
    fn discard_pending(&mut self, first_timeout_ms: i32) -> Result<()> {
        let mut timeout_ms: i32 = first_timeout_ms;
        for _ in 0..MAX_STALE_REPORTS {
            if self.transport.read_report(timeout_ms)?.is_none() {
                break;
            }
            timeout_ms = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::protocol::report_with_prefix;
    use crate::protocol::tests::{INFO_REPLY, LIGHTING_REPLY, PROFILE_TABLE_REPLY};
    use crate::settings::{Lighting, Sensor};

    /// Replays canned replies: each write releases the next batch of replies.
    struct MockTransport {
        written: Vec<Report>,
        batches: VecDeque<Vec<Report>>,
        pending: VecDeque<Report>,
    }

    impl MockTransport {
        fn new(batches: Vec<Vec<Report>>) -> Self {
            Self {
                written: Vec::new(),
                batches: batches.into(),
                pending: VecDeque::new(),
            }
        }
    }

    impl Transport for MockTransport {
        fn write_report(&mut self, report: &Report) -> Result<()> {
            self.written.push(*report);
            if let Some(batch) = self.batches.pop_front() {
                self.pending.extend(batch);
            }
            Ok(())
        }

        fn read_report(&mut self, _timeout_ms: i32) -> Result<Option<Report>> {
            Ok(self.pending.pop_front())
        }
    }

    fn init_reply() -> Report {
        report_with_prefix(&[0x90, 0x00, 0x03, 0x00, 0xec, 0xfa, 0x03])
    }

    fn battery_reply(raw: u8) -> Report {
        let mut reply = report_with_prefix(&[0x90, 0x0a, 0x04]);
        reply[10] = raw;
        reply
    }

    fn accepted(command: u8) -> Report {
        report_with_prefix(&[0x10, command, 0x03, 0x00, 0xec, 0xac])
    }

    fn info_reply(sensor: &Sensor) -> Report {
        let mut reply: Report = report_with_prefix(&INFO_REPLY);
        let bytes = sensor.bytes();
        reply[10..29].copy_from_slice(&bytes[..19]);
        reply[41] = bytes[19];
        reply
    }

    fn lighting_reply(lighting: &Lighting) -> Report {
        let mut reply: Report = report_with_prefix(&[0x90, 0x01, 0x0b, 0x00]);
        reply[4..15].copy_from_slice(&lighting.bytes());
        reply
    }

    fn start_settings() -> ProfileSettings {
        ProfileSettings {
            sensor: protocol::parse_info(&report_with_prefix(&INFO_REPLY))
                .expect("info")
                .sensor,
            lighting: protocol::parse_lighting(&report_with_prefix(&LIGHTING_REPLY))
                .expect("lighting"),
        }
    }

    /// The settings at the start, with stage 2 at 900 DPI and static red/blue lighting.
    fn changed_settings() -> ProfileSettings {
        let mut settings = start_settings();
        settings.sensor.set_dpi(2, Some(900)).expect("valid DPI");
        settings
            .lighting
            .set_effect(Effect::Static)
            .expect("known effect");
        settings.lighting.set_left(Rgb::new(255, 0, 0));
        settings.lighting.set_right(Rgb::new(0, 0, 255));
        settings
    }

    /// Replies for a write to profile 1 (active) after which the mouse reports `stored`.
    fn write_batches(stored: &ProfileSettings) -> Vec<Vec<Report>> {
        let stored_sensor = Sensor::from_bytes(stored.sensor.write_bytes());
        vec![
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![accepted(0x11)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![info_reply(&stored_sensor)],
            vec![info_reply(&stored_sensor)],
            vec![lighting_reply(&stored.lighting)],
        ]
    }

    #[test]
    fn write_settings_follows_the_official_sequence() {
        let settings = changed_settings();
        let mut mouse = connect(write_batches(&settings));
        mouse.write_settings(&settings).expect("write");
        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written[0], protocol::query_profile_table());
        assert_eq!(
            written[1],
            report_with_prefix(&[0x10, 0x11, 0x06, 0x00, 0x01, 0x01, 0x64])
        );
        // Capture 02: stage 2 at 900 DPI, bit 7 of byte 16 cleared.
        assert_eq!(
            written[2],
            report_with_prefix(&[
                0x10, 0x50, 0x14, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x06, 0x0f, 0x00, 0x02
            ])
        );
        assert_eq!(
            written[3],
            report_with_prefix(&[
                0x10, 0x50, 0x0b, 0x01, 0x01, 0x64, 0x64, 0x46, 0x04, 0xff, 0x00, 0x00, 0x00, 0x00,
                0xff
            ])
        );
        assert_eq!(
            written[4..],
            [
                protocol::query_info(),
                protocol::query_info(),
                protocol::query_lighting()
            ]
        );
    }

    #[test]
    fn rejected_write_is_an_error() {
        let mut mouse = connect(vec![
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![report_with_prefix(&[
                0x10, 0x11, 0x03, 0x00, 0xec, 0xfa, 0x05,
            ])],
        ]);
        assert!(matches!(
            mouse.write_settings(&changed_settings()),
            Err(Error::Rejected {
                command: "profile table write",
                code: 0x05
            })
        ));
    }

    #[test]
    fn write_that_does_not_read_back_fails() {
        let mut mouse = connect(write_batches(&start_settings()));
        assert!(matches!(
            mouse.write_settings(&changed_settings()),
            Err(Error::VerifyFailed("sensor settings"))
        ));
    }

    #[test]
    fn aimo_lighting_is_verified_by_effect_only() {
        let mut settings = start_settings();
        settings
            .lighting
            .set_effect(Effect::Aimo)
            .expect("known effect");
        let mut shown = settings;
        shown.lighting.set_left(Rgb::new(1, 2, 3));
        let mut mouse = connect(write_batches(&shown));
        mouse.write_settings(&settings).expect("write");
    }

    #[test]
    fn unused_profile_is_not_written() {
        let mut settings = start_settings();
        let mut bytes = settings.sensor.bytes();
        bytes[0] = 4;
        settings.sensor = Sensor::from_bytes(bytes);
        let mut mouse = connect(vec![vec![report_with_prefix(&PROFILE_TABLE_REPLY)]]);
        assert!(matches!(
            mouse.write_settings(&settings),
            Err(Error::UnusedProfile(4))
        ));
        assert_eq!(mouse.transport.written.len(), 2);
    }

    #[test]
    fn reading_another_profile_selects_it_and_restores_the_active_one() {
        let mut profile_2 = start_settings();
        let mut bytes = profile_2.sensor.bytes();
        bytes[0] = 2;
        profile_2.sensor = Sensor::from_bytes(bytes);
        let mut mouse = connect(vec![
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![accepted(0x13)],
            vec![info_reply(&profile_2.sensor)],
            vec![lighting_reply(&profile_2.lighting)],
            vec![accepted(0x13)],
        ]);
        assert_eq!(mouse.settings(Some(2)).expect("settings"), profile_2);
        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written[2], protocol::select_profile(2));
        assert_eq!(written[5], protocol::select_profile(1));
    }

    #[test]
    fn late_init_reply_is_skipped_by_the_info_query() {
        let late_init: Report = init_reply();
        let mut mouse = connect(vec![vec![late_init, report_with_prefix(&INFO_REPLY)]]);
        assert_eq!(mouse.info().expect("info").sensor.profile(), 1);
    }

    #[test]
    fn write_sensor_leaves_the_lighting_alone() {
        let mut settings = start_settings();
        settings.sensor.set_dpi(2, Some(900)).expect("valid DPI");
        let mut batches = write_batches(&settings);
        batches.remove(3); // no lighting write, so no third acknowledgement
        let mut mouse = connect(batches);
        mouse.write_sensor(&settings.sensor).expect("write");
        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written.len(), 6);
        assert_eq!(written[2], protocol::write_sensor(&settings.sensor));
        assert!(
            written
                .iter()
                .all(|report| report[..4] != [0x10, 0x50, 0x0b, 0x01])
        );
    }

    #[test]
    fn sleeping_mouse_is_not_reachable() {
        let mut mouse = connect(vec![vec![report_with_prefix(&[0x90, 0x00, 0x39, 0x00])]]);
        assert!(matches!(mouse.settings(None), Err(Error::NotReachable)));
    }

    #[test]
    fn battery_error_reply_is_rejected() {
        let mut mouse = connect(vec![vec![report_with_prefix(&[
            0x90, 0x0a, 0x03, 0x00, 0xec, 0xfa, 0x02,
        ])]]);
        assert!(matches!(
            mouse.battery(),
            Err(Error::Rejected {
                command: "battery query",
                code: 0x02
            })
        ));
    }

    fn foreign_report() -> Report {
        report_with_prefix(&[0x10, 0x10, 0x03])
    }

    /// Connects a mock whose init is answered; `batches` answer the later writes.
    fn connect(batches: Vec<Vec<Report>>) -> KoneProAir<MockTransport> {
        let mut all_batches: Vec<Vec<Report>> = vec![vec![init_reply()]];
        all_batches.extend(batches);
        KoneProAir::connect(MockTransport::new(all_batches), ConnectionMode::Wireless)
            .expect("mock connect")
    }

    #[test]
    fn connect_sends_init_and_consumes_its_reply() {
        let mouse = connect(vec![]);
        assert_eq!(mouse.transport.written, vec![protocol::init()]);
        assert!(mouse.transport.pending.is_empty());
    }

    #[test]
    fn connect_skips_reports_before_init_reply() {
        let transport = MockTransport::new(vec![vec![foreign_report(), init_reply()]]);
        let mouse = KoneProAir::connect(transport, ConnectionMode::Wireless).expect("mock connect");
        assert!(mouse.transport.pending.is_empty());
    }

    #[test]
    fn battery_skips_foreign_replies() {
        let mut mouse = connect(vec![vec![foreign_report(), battery_reply(57)]]);
        assert_eq!(mouse.battery().expect("battery").percent(), Some(57));
        assert_eq!(mouse.transport.written[1], protocol::query_battery());
    }

    #[test]
    fn many_foreign_reports_do_not_exhaust_the_request() {
        let mut batch: Vec<Report> = vec![foreign_report(); 100];
        batch.push(battery_reply(42));
        let mut mouse = connect(vec![batch]);
        assert_eq!(mouse.battery().expect("battery").percent(), Some(42));
    }

    #[test]
    fn stale_replies_are_not_used() {
        let mut mouse = connect(vec![vec![battery_reply(80)]]);
        mouse.transport.pending.push_back(battery_reply(10));
        assert_eq!(mouse.battery().expect("battery").percent(), Some(80));
    }

    #[test]
    fn missing_reply_times_out() {
        let mut mouse = connect(vec![]);
        assert!(matches!(
            mouse.signal(),
            Err(Error::Timeout("signal query"))
        ));
    }

    #[test]
    fn signal_is_parsed() {
        let reply = report_with_prefix(&[0x90, 0x70, 0, 0, 40]);
        let mut mouse = connect(vec![vec![reply]]);
        assert_eq!(mouse.signal().expect("signal").raw, 40);
    }

    #[test]
    fn set_leds_writes_led_report() {
        let mut mouse = connect(vec![]);
        let (left, right) = (Rgb::new(255, 0, 0), Rgb::new(0, 0, 255));
        mouse.set_leds(left, right).expect("set leds");
        assert_eq!(mouse.transport.written[1], protocol::set_leds(left, right));
    }

    #[test]
    fn raw_collects_all_replies() {
        let replies = vec![report_with_prefix(&[1]), report_with_prefix(&[2])];
        let mut mouse = connect(vec![replies.clone()]);
        let request = report_with_prefix(&[0xaa]);
        assert_eq!(mouse.raw(&request).expect("raw"), replies);
        assert_eq!(mouse.transport.written[1], request);
    }

    #[test]
    fn modes_match_usb_ids() {
        assert_eq!(
            ConnectionMode::from_ids(0x2c8e, 2),
            Some(ConnectionMode::Wireless)
        );
        assert_eq!(
            ConnectionMode::from_ids(0x2c92, 1),
            Some(ConnectionMode::Wired)
        );
        assert_eq!(ConnectionMode::from_ids(0x2c8e, 1), None);
        assert_eq!(ConnectionMode::from_ids(0x2c88, 2), None);
    }
}
