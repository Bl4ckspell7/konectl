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
    Effect, Firmware, Info, Lighting, PROFILE_COUNT, ProfileEntry, ProfileSettings, Sensor,
    check_debounce_ms, profile_in_use, profile_index,
};
use crate::transport::{HidTransport, Transport};

/// How long a single read waits for a report.
const READ_TIMEOUT_MS: i32 = 20;
/// The official software reads the calibration state one second after
/// starting the lift-off calibration.
const CALIBRATION_START_DELAY: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_secs(1)
};
/// Quiet time before starting the calibration: its start is not acknowledged,
/// so it cannot be sent again if the mouse drops it.
const CALIBRATION_QUIET: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_millis(200)
};
/// How long a request waits for its answer.
const REPLY_DEADLINE: Duration = if cfg!(test) {
    Duration::from_millis(30)
} else {
    Duration::from_millis(500)
};
/// Silence after the last reply at which [`KoneProAir::raw`] stops collecting.
const RAW_QUIET_PERIOD: Duration = Duration::from_millis(100);
/// How often a request that gets no answer is sent again. The mouse
/// sometimes drops a request that arrives right after a state change; these
/// requests read or set an absolute value, so repeating them does no harm.
/// The lift-off calibration commands are never repeated.
const REQUEST_RETRIES: usize = 2;
/// Pause after a write command before the next request. The mouse may drop a
/// request that arrives within a few milliseconds of a profile switch or a
/// block write (the official software leaves 60–90 ms).
const WRITE_SETTLE: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_millis(40)
};
/// After a repeated request, how long to wait for a late duplicate answer to
/// the first attempt, so it is not taken for the answer to the next request.
const DUPLICATE_WAIT_MS: i32 = if cfg!(test) { 0 } else { 100 };
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

    /// Firmware versions and the sensor settings of the active profile.
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

    /// Whether energy saving is on. One setting for all profiles.
    pub fn energy_saving(&mut self) -> Result<bool> {
        self.request(
            &protocol::query_energy_saving(),
            "energy saving query",
            protocol::parse_energy_saving,
        )?
        .map_err(|raw| Error::InvalidValue(format!("unknown energy saving value {raw:#04x}")))
    }

    /// Turns energy saving on or off and checks that it reads back.
    pub fn set_energy_saving(&mut self, enabled: bool) -> Result<()> {
        self.command(&protocol::set_energy_saving(enabled), "energy saving write")?;
        if self.energy_saving()? != enabled {
            return Err(Error::VerifyFailed("energy saving"));
        }
        Ok(())
    }

    /// Starts the lift-off calibration. Move the mouse continuously on the
    /// surface for about 10 seconds, then call
    /// [`Self::finish_lift_off_calibration`] and set [`LiftOff::Custom`] to
    /// use the measured distance.
    ///
    /// [`LiftOff::Custom`]: crate::LiftOff::Custom
    pub fn start_lift_off_calibration(&mut self) -> Result<()> {
        std::thread::sleep(CALIBRATION_QUIET);
        self.discard_pending(0)?;
        // The mouse does not acknowledge the start; like the official software,
        // read the calibration state a second later instead.
        self.transport
            .write_report(&protocol::start_lift_off_calibration())?;
        std::thread::sleep(CALIBRATION_START_DELAY);
        self.request(
            &protocol::query_lift_off_calibration(),
            "lift-off calibration query",
            protocol::parse_lift_off_calibration,
        )?;
        Ok(())
    }

    /// Ends the lift-off calibration and keeps the measured distance.
    ///
    /// Not repeated when unanswered: whether applying twice is harmless is
    /// not known.
    pub fn finish_lift_off_calibration(&mut self) -> Result<()> {
        let report: Report = protocol::apply_lift_off_calibration();
        self.exchange_with(
            &report,
            "lift-off calibration",
            0,
            |reply| match protocol::parse_status(reply, &report) {
                Some(Status::Accepted) => Some(Ok(())),
                Some(Status::Error(code)) => Some(Err(Error::Rejected {
                    command: "lift-off calibration",
                    code,
                })),
                None => None,
            },
        )?;
        std::thread::sleep(WRITE_SETTLE);
        Ok(())
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

    /// Writes only block 0 ([`Sensor`]: polling rate, DPI stages and AUTO
    /// flags, lift-off, angle snapping, LED time-out, debounce) of the profile it
    /// belongs to, after the profile's table entry, and checks that it reads
    /// back unchanged. The saved lighting is left alone.
    pub fn write_sensor(&mut self, sensor: &Sensor) -> Result<()> {
        let entry: ProfileEntry = self.profile_entry(sensor.profile())?;
        self.write_blocks(&entry, sensor, None)
    }

    /// Sets the debounce time (0–10 ms) in every profile in use, as the
    /// official software does: it shows a single value. Profiles that already
    /// have it are not written.
    pub fn set_debounce(&mut self, ms: u8) -> Result<()> {
        check_debounce_ms(ms)?;
        for profile in self.profiles()? {
            let mut sensor: Sensor = self.settings(Some(profile))?.sensor;
            if sensor.debounce_ms() != ms {
                sensor.set_debounce_ms(ms)?;
                self.write_sensor(&sensor)?;
            }
        }
        Ok(())
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
            firmware: (!info.firmware.is_zero()).then_some(info.firmware),
            dongle_firmware: info.dongle_firmware,
            active_profile: info.sensor.profile(),
            profiles,
        })
    }

    /// Checks that `backup` fits the mouse: the firmware versions it records
    /// must match the mouse's, and the dongle's when both are known.
    pub fn check_backup(&mut self, backup: &Backup) -> Result<()> {
        let info: Info = self.info()?;
        let mismatch = |device: &str, saved: Firmware, now: Firmware| {
            Error::InvalidBackup(format!(
                "it was made with {device} firmware {saved}, the {device} has {now}"
            ))
        };
        if let Some(saved) = backup.firmware
            && !info.firmware.is_zero()
            && saved != info.firmware
        {
            return Err(mismatch("mouse", saved, info.firmware));
        }
        if let (Some(saved), Some(now)) = (backup.dongle_firmware, info.dongle_firmware)
            && saved != now
        {
            return Err(mismatch("dongle", saved, now));
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
    /// recognises (`Some`); other replies are skipped. A request that gets no
    /// answer is sent again up to [`REQUEST_RETRIES`] times.
    fn exchange<R>(
        &mut self,
        report: &Report,
        what: &'static str,
        classify: impl Fn(&Report) -> Option<Result<R>>,
    ) -> Result<R> {
        self.exchange_with(report, what, REQUEST_RETRIES, classify)
    }

    /// [`Self::exchange`] with an explicit number of retries.
    fn exchange_with<R>(
        &mut self,
        report: &Report,
        what: &'static str,
        retries: usize,
        classify: impl Fn(&Report) -> Option<Result<R>>,
    ) -> Result<R> {
        for attempt in 0..=retries {
            self.discard_pending(0)?;
            self.transport.write_report(report)?;
            if let Some(outcome) = self.await_reply(&classify)? {
                if attempt > 0 {
                    // The first attempt may still be answered late.
                    self.discard_pending(DUPLICATE_WAIT_MS)?;
                }
                return outcome;
            }
        }
        Err(Error::Timeout(what))
    }

    /// Sends a write command, waits until the mouse accepts it and gives the
    /// mouse [`WRITE_SETTLE`] before the next request.
    fn command(&mut self, report: &Report, what: &'static str) -> Result<()> {
        self.request(report, what, |reply| {
            (protocol::parse_status(reply, report) == Some(Status::Accepted)).then_some(())
        })?;
        std::thread::sleep(WRITE_SETTLE);
        Ok(())
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
    use crate::protocol::tests::{
        CABLE_INFO_REPLY, INFO_REPLY, LIGHTING_REPLY, PROFILE_TABLE_REPLY,
    };
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

    fn firmware(major: u8, minor: u8) -> Option<Firmware> {
        Some(Firmware { major, minor })
    }

    /// Checks a backup with these firmware versions against `info_reply`.
    fn check_backup(
        info_reply: &[u8],
        mouse: Option<Firmware>,
        dongle: Option<Firmware>,
    ) -> Result<()> {
        let backup = Backup {
            firmware: mouse,
            dongle_firmware: dongle,
            active_profile: 1,
            profiles: Vec::new(),
        };
        connect(vec![vec![report_with_prefix(info_reply)]]).check_backup(&backup)
    }

    #[test]
    fn backup_must_match_the_firmware() {
        // INFO_REPLY: mouse 4.32, dongle 4.19.
        assert!(check_backup(&INFO_REPLY, firmware(4, 32), firmware(4, 19)).is_ok());
        assert!(check_backup(&INFO_REPLY, None, None).is_ok());
        assert!(matches!(
            check_backup(&INFO_REPLY, firmware(4, 31), firmware(4, 19)),
            Err(Error::InvalidBackup(_))
        ));
        // A konectl 0.1 backup records only the dongle firmware.
        assert!(check_backup(&INFO_REPLY, None, firmware(4, 19)).is_ok());
        assert!(matches!(
            check_backup(&INFO_REPLY, None, firmware(4, 18)),
            Err(Error::InvalidBackup(_))
        ));
        // Over the cable the dongle firmware cannot be compared.
        assert!(check_backup(&CABLE_INFO_REPLY, firmware(4, 32), firmware(4, 18)).is_ok());
        assert!(matches!(
            check_backup(&CABLE_INFO_REPLY, firmware(4, 31), None),
            Err(Error::InvalidBackup(_))
        ));
    }

    /// Profile table with only slot 1 in use.
    fn single_profile_table() -> Report {
        report_with_prefix(&[0x90, 0x11, 0x19, 0x00, 0x01, 0x64])
    }

    /// Replies for reading profile 1 (active) with `sensor`.
    fn read_batches(sensor: &Sensor) -> Vec<Vec<Report>> {
        let lighting = start_settings().lighting;
        vec![
            vec![info_reply(sensor)],
            vec![info_reply(sensor)],
            vec![lighting_reply(&lighting)],
        ]
    }

    #[test]
    fn set_debounce_writes_profiles_that_differ() {
        let start = start_settings().sensor;
        let mut changed = start;
        changed.set_debounce_ms(3).expect("valid debounce");
        let changed = Sensor::from_bytes(changed.write_bytes());

        let mut batches = vec![vec![single_profile_table()]];
        batches.extend(read_batches(&start));
        batches.extend([
            vec![single_profile_table()],
            vec![accepted(0x11)],
            vec![accepted(0x50)],
        ]);
        batches.extend(read_batches(&changed));
        let mut mouse = connect(batches);
        mouse.set_debounce(3).expect("set debounce");

        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written.len(), 10);
        assert_eq!(written[6], protocol::write_sensor(&changed));
        assert_eq!(written[6][4 + 19], 3);
    }

    #[test]
    fn set_debounce_writes_a_non_active_profile() {
        // Slots 1 and 2 in use; profile 1 (active) already has 3 ms, profile 2 has 2 ms.
        let table = report_with_prefix(&[
            0x90, 0x11, 0x19, 0x00, 0x01, 0x64, 0x00, 0x00, 0x00, 0x01, 0x64,
        ]);
        let mut profile_1 = start_settings().sensor;
        profile_1.set_debounce_ms(3).expect("valid debounce");
        let mut bytes = start_settings().sensor.bytes();
        bytes[0] = 2;
        let profile_2 = Sensor::from_bytes(bytes);
        let mut written_2 = profile_2;
        written_2.set_debounce_ms(3).expect("valid debounce");
        let written_2 = Sensor::from_bytes(written_2.write_bytes());
        let lighting = lighting_reply(&start_settings().lighting);

        let mut batches = vec![vec![table]];
        // Profile 1: active, read directly; already 3 ms.
        batches.extend(read_batches(&profile_1));
        // Profile 2: selected for reading, then the active profile again.
        batches.extend([
            vec![info_reply(&profile_1)],
            vec![table],
            vec![accepted(0x13)],
            vec![info_reply(&profile_2)],
            vec![lighting],
            vec![accepted(0x13)],
        ]);
        // Write profile 2, then read it back the same way.
        batches.extend([vec![table], vec![accepted(0x11)], vec![accepted(0x50)]]);
        batches.extend([
            vec![info_reply(&profile_1)],
            vec![table],
            vec![accepted(0x13)],
            vec![info_reply(&written_2)],
            vec![lighting],
            vec![accepted(0x13)],
        ]);
        let mut mouse = connect(batches);
        mouse.set_debounce(3).expect("set debounce");

        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written.len(), 19);
        assert_eq!(written[6], protocol::select_profile(2));
        assert_eq!(written[9], protocol::select_profile(1));
        assert_eq!(written[11][..5], [0x10, 0x11, 0x06, 0x00, 0x02]);
        assert_eq!(written[12], protocol::write_sensor(&written_2));
        // Only profile 2 was written.
        let sensor_writes: usize = written
            .iter()
            .filter(|report| report[..4] == [0x10, 0x50, 0x14, 0x00])
            .count();
        assert_eq!(sensor_writes, 1);
    }

    #[test]
    fn set_debounce_skips_profiles_that_match() {
        let start = start_settings().sensor;
        let mut batches = vec![vec![single_profile_table()]];
        batches.extend(read_batches(&start));
        let mut mouse = connect(batches);
        mouse
            .set_debounce(start.debounce_ms())
            .expect("set debounce");
        assert_eq!(mouse.transport.written.len(), 1 + 4);
    }

    #[test]
    fn set_debounce_rejects_invalid_values_before_writing() {
        let mut mouse = connect(vec![]);
        assert!(matches!(
            mouse.set_debounce(11),
            Err(Error::InvalidValue(_))
        ));
        assert_eq!(mouse.transport.written.len(), 1);
    }

    #[test]
    fn sleeping_mouse_is_not_reachable() {
        let mut mouse = connect(vec![vec![report_with_prefix(&[0x90, 0x00, 0x39, 0x00])]]);
        assert!(matches!(mouse.settings(None), Err(Error::NotReachable)));
    }

    #[test]
    fn energy_saving_is_written_and_verified() {
        let mut mouse = connect(vec![
            vec![accepted(0x72)],
            vec![report_with_prefix(&[0x90, 0x72, 0x01, 0x00, 0xaa])],
        ]);
        mouse.set_energy_saving(true).expect("set energy saving");
        assert_eq!(
            mouse.transport.written[1],
            protocol::set_energy_saving(true)
        );
        assert_eq!(mouse.transport.written[2], protocol::query_energy_saving());
    }

    #[test]
    fn energy_saving_that_does_not_read_back_fails() {
        let mut mouse = connect(vec![
            vec![accepted(0x72)],
            vec![report_with_prefix(&[0x90, 0x72, 0x01, 0x00, 0x00])],
        ]);
        assert!(matches!(
            mouse.set_energy_saving(true),
            Err(Error::VerifyFailed("energy saving"))
        ));
    }

    #[test]
    fn lift_off_calibration_sequence() {
        let mut mouse = connect(vec![
            vec![],
            vec![report_with_prefix(&[0x90, 0x08, 0x01, 0x00, 0x00])],
            vec![accepted(0x15)],
        ]);
        mouse.start_lift_off_calibration().expect("start");
        mouse.finish_lift_off_calibration().expect("finish");
        assert_eq!(
            mouse.transport.written[1..],
            [
                protocol::start_lift_off_calibration(),
                protocol::query_lift_off_calibration(),
                protocol::apply_lift_off_calibration()
            ]
        );
    }

    #[test]
    fn unanswered_reads_are_sent_again() {
        // The first info query is dropped, the second one is answered.
        let mut mouse = connect(vec![vec![], vec![report_with_prefix(&INFO_REPLY)]]);
        assert_eq!(mouse.info().expect("info").sensor.profile(), 1);
        assert_eq!(
            mouse.transport.written[1..],
            [protocol::query_info(), protocol::query_info()]
        );
    }

    #[test]
    fn unanswered_writes_are_sent_again() {
        // No answer at all: the write is sent 1 + REQUEST_RETRIES times.
        let mut mouse = connect(vec![]);
        assert!(matches!(
            mouse.set_energy_saving(true),
            Err(Error::Timeout("energy saving write"))
        ));
        assert_eq!(mouse.transport.written.len(), 1 + 1 + REQUEST_RETRIES);
        // The second attempt is answered.
        let mut mouse = connect(vec![
            vec![],
            vec![accepted(0x72)],
            vec![report_with_prefix(&[0x90, 0x72, 0x01, 0x00, 0xaa])],
        ]);
        mouse.set_energy_saving(true).expect("set energy saving");
        assert_eq!(
            mouse.transport.written[1..],
            [
                protocol::set_energy_saving(true),
                protocol::set_energy_saving(true),
                protocol::query_energy_saving()
            ]
        );
    }

    #[test]
    fn calibration_apply_is_not_sent_again() {
        let mut mouse = connect(vec![]);
        assert!(matches!(
            mouse.finish_lift_off_calibration(),
            Err(Error::Timeout("lift-off calibration"))
        ));
        assert_eq!(mouse.transport.written.len(), 2);
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
