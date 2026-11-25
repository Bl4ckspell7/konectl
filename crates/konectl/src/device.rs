//! Device discovery and the high-level mouse API.

use std::ffi::CString;
use std::fmt;
use std::time::{Duration, Instant};

use hidapi::HidApi;

use crate::backup::{Backup, ProfileBackup};
use crate::buttons::{ButtonAction, Buttons, Layer};
use crate::color::Rgb;
use crate::error::{Error, Result};
use crate::macros::Macro;
use crate::protocol::{self, Battery, FunctionEvent, Report, Signal, Status};
use crate::settings::{
    Effect, Firmware, Info, LiftOff, Lighting, PROFILE_COUNT, ProfileEntry, ProfileSettings,
    Sensor, check_debounce_ms, profile_in_use, profile_index, set_profile_color,
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
/// How long the reset command waits for its answer (about 1.5 s in the
/// captures).
const RESET_DEADLINE: Duration = if cfg!(test) {
    Duration::from_millis(30)
} else {
    Duration::from_secs(5)
};
/// How long the macro commit waits for its answer (about 200 ms in the
/// captures). It is not repeated: what a second commit does is not known.
const MACRO_COMMIT_DEADLINE: Duration = if cfg!(test) {
    Duration::from_millis(30)
} else {
    Duration::from_secs(2)
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
/// How long a read-back that differed waits for late answers before it reads
/// again.
const LATE_ANSWER_WAIT_MS: i32 = 50;

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

    /// Opens the first device connected in `mode`.
    pub fn open_mode(&self, mode: ConnectionMode) -> Result<KoneProAir<HidTransport>> {
        let info: DeviceInfo = self
            .devices()
            .into_iter()
            .find(|device| device.mode == mode)
            .ok_or(Error::NotFound)?;
        self.open(&info)
    }

    /// Opens the first device, preferring a cable connection.
    pub fn open_first(&self) -> Result<KoneProAir<HidTransport>> {
        let info: DeviceInfo = self.devices().into_iter().next().ok_or(Error::NotFound)?;
        self.open(&info)
    }
}

/// How long the mouse has to be moved during the lift-off calibration (the
/// official software asks for 10 seconds).
pub const LIFT_OFF_CALIBRATION_SECONDS: u64 = 12;

/// What [`KoneProAir::remove_profile`] changed besides removing the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedProfile {
    /// Profiles that moved to another slot, as (old number, new number).
    pub moved: Vec<(u8, u8)>,
    /// Button functions that switched to the removed profile (or to an
    /// unused slot that another profile moved into) and are now disabled.
    pub disabled_buttons: usize,
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
                // An answer to a bare init request, which shares the header: a
                // late one of ours, or one of another program (hidraw gives
                // every input report to every open handle). The info query
                // itself has not been seen to fail with this code.
                Some(Status::Error(protocol::INIT_ERROR_CODE)) => None,
                Some(Status::Error(code)) => Some(Err(Error::Rejected {
                    command: WHAT,
                    code,
                })),
                Some(Status::Accepted) | None => None,
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
    /// surface for [`LIFT_OFF_CALIBRATION_SECONDS`], then call
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

    /// Ends the lift-off calibration and keeps the measured distance; call it
    /// [`LIFT_OFF_CALIBRATION_SECONDS`] after
    /// [`Self::start_lift_off_calibration`] while the mouse is being moved.
    ///
    /// Not repeated when unanswered: whether applying twice is harmless is
    /// not known.
    pub fn finish_lift_off_calibration(&mut self) -> Result<()> {
        let report: Report = protocol::apply_lift_off_calibration();
        self.exchange_with(
            &report,
            "lift-off calibration",
            0,
            REPLY_DEADLINE,
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

    /// Resets the mouse to its factory settings: the profiles in use get
    /// default settings, lighting, buttons and colours (the number of
    /// profiles stays the same), and energy saving is switched on.
    ///
    /// Not repeated when unanswered.
    pub fn reset(&mut self) -> Result<()> {
        const WHAT: &str = "reset";
        let report: Report = protocol::reset();
        self.exchange_with(
            &report,
            WHAT,
            0,
            RESET_DEADLINE,
            |reply| match protocol::parse_status(reply, &report) {
                Some(Status::Accepted) => Some(Ok(())),
                Some(Status::Error(code)) => Some(Err(Error::Rejected {
                    command: WHAT,
                    code,
                })),
                None => None,
            },
        )?;
        std::thread::sleep(WRITE_SETTLE);
        Ok(())
    }

    /// Puts the dongle into pairing mode. Only through the dongle: the mouse
    /// is then paired by holding its profile button for three seconds while it
    /// runs on battery, and the dongle is plugged in again afterwards.
    pub fn start_pairing(&mut self) -> Result<()> {
        if self.mode != ConnectionMode::Wireless {
            return Err(Error::InvalidValue(
                "pairing works through the dongle, not over the USB cable".to_owned(),
            ));
        }
        self.command(&protocol::start_pairing(), "pairing start")
    }

    /// Sets the colour the DPI LED flashes when the mouse switches to
    /// `profile`; `None` removes it.
    pub fn set_profile_color(&mut self, profile: u8, color: Option<Rgb>) -> Result<()> {
        let mut entry: ProfileEntry = self.profile_entry(profile)?;
        set_profile_color(&mut entry, color);
        self.command(
            &protocol::write_profile_entry(profile, &entry),
            "profile table write",
        )?;
        if self.profile_entry(profile)? != entry {
            return Err(Error::VerifyFailed("profile colour"));
        }
        Ok(())
    }

    /// Adds a profile in the first free slot as a copy of `copy_from` (its
    /// settings and buttons, without the profile colour) and returns its number.
    pub fn add_profile(&mut self, copy_from: u8) -> Result<u8> {
        let table = self.profile_table()?;
        let source: usize = profile_index(copy_from)?;
        if !profile_in_use(&table[source]) {
            return Err(Error::UnusedProfile(copy_from));
        }
        let free: usize = table
            .iter()
            .position(|entry| !profile_in_use(entry))
            .ok_or_else(|| {
                Error::InvalidValue(format!("all {PROFILE_COUNT} profile slots are in use"))
            })?;
        // Index 0–4, so the cast cannot truncate.
        let number: u8 = free as u8 + 1;
        let (settings, buttons) = self.read_profile_and_buttons(copy_from)?;
        let mut entry: ProfileEntry = table[source];
        set_profile_color(&mut entry, None);
        self.write_blocks(
            &entry,
            &settings.sensor.with_profile(number),
            Some(&settings.lighting),
            Some(&buttons.with_profile(number)),
            &[],
        )?;
        Ok(number)
    }

    /// Removes `profile`, like the official software: the remaining profiles
    /// move into slots 1, 2, … (with their colours), and the slots after them
    /// are cleared. The active profile and the only profile in use cannot be
    /// removed.
    ///
    /// Buttons that switch to a profile (`profile:N`) are renumbered to keep
    /// pointing at the same profile; those that switched to the removed one are
    /// disabled.
    pub fn remove_profile(&mut self, profile: u8) -> Result<RemovedProfile> {
        let table = self.profile_table()?;
        let in_use: Vec<u8> = (1u8..)
            .zip(&table)
            .filter(|(_, entry)| profile_in_use(entry))
            .map(|(number, _)| number)
            .collect();
        if !in_use.contains(&profile) {
            return Err(Error::UnusedProfile(profile));
        }
        if in_use.len() == 1 {
            return Err(Error::InvalidValue(format!(
                "profile {profile} is the only profile"
            )));
        }
        let active: u8 = self.active_profile()?;
        if profile == active {
            return Err(Error::InvalidValue(format!(
                "profile {profile} is active: activate another profile first"
            )));
        }

        // Read every remaining profile before anything is written: the later
        // ones move, and any of them may have buttons to renumber.
        let remaining: Vec<u8> = in_use
            .iter()
            .copied()
            .filter(|&number| number != profile)
            .collect();
        let new_number = |old: u8| -> Option<u8> {
            if old == profile {
                return None;
            }
            match (1u8..).zip(&remaining).find(|(_, number)| **number == old) {
                Some((target, _)) => Some(target),
                // An unused slot that a moved profile fills: the button would
                // switch to that profile, so it is disabled.
                None if usize::from(old) <= remaining.len() => None,
                None => Some(old),
            }
        };
        let mut writes: Vec<(u8, bool, ProfileEntry, ProfileSettings, Buttons)> = Vec::new();
        let mut disabled: usize = 0;
        for (target, &source) in (1u8..).zip(&remaining) {
            let (settings, mut buttons) = self.read_profile_and_buttons(source)?;
            let (renumbered, cleared) = buttons.renumber_profiles(new_number);
            disabled += cleared;
            let moves: bool = target != source;
            if moves || renumbered > 0 {
                let entry: ProfileEntry = table[usize::from(source - 1)];
                writes.push((target, moves, entry, settings, buttons));
            }
        }
        for (target, moves, entry, settings, buttons) in &writes {
            // A profile that stays in its slot keeps its lighting as it is.
            self.write_blocks(
                entry,
                &settings.sensor.with_profile(*target),
                moves.then_some(&settings.lighting),
                Some(&buttons.with_profile(*target)),
                &[],
            )?;
        }
        // The active profile moved with its settings.
        let moved_active: u8 = (1u8..)
            .zip(&remaining)
            .find(|(_, number)| **number == active)
            .map_or(active, |(target, _)| target);
        if moved_active != active {
            self.set_active_profile(moved_active)?;
        }
        for &number in &in_use {
            if usize::from(number) > remaining.len() {
                self.command(
                    &protocol::clear_profile_entry(number),
                    "profile table write",
                )?;
            }
        }
        Ok(RemovedProfile {
            moved: (1u8..)
                .zip(&remaining)
                .filter(|(target, source)| *target != **source)
                .map(|(target, source)| (*source, target))
                .collect(),
            disabled_buttons: disabled,
        })
    }

    /// `false` once the device is gone, e.g. the dongle was unplugged:
    /// reading from it fails. Sends nothing; drops queued reports.
    pub fn is_present(&mut self) -> bool {
        self.discard_pending(0).is_ok()
    }

    /// The functions the mouse reported by itself since the last call, e.g.
    /// when its profile button switched profiles. Sends nothing; other queued
    /// reports are dropped (reports read while waiting for an answer are not
    /// kept). Fails once the device is gone.
    pub fn take_function_events(&mut self) -> Result<Vec<FunctionEvent>> {
        let mut events: Vec<FunctionEvent> = Vec::new();
        for _ in 0..MAX_STALE_REPORTS {
            match self.transport.read_report(0)? {
                Some(report) => events.extend(protocol::parse_function_event(&report)),
                None => break,
            }
        }
        Ok(events)
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
        self.read_profile(profile, Self::read_selected_settings)
    }

    /// Reads the button assignments of `profile` (1–5), or of the active
    /// profile with `None`. Selects the profile like [`Self::settings`].
    pub fn buttons(&mut self, profile: Option<u8>) -> Result<Buttons> {
        self.read_profile(profile, |mouse, target| {
            // Blocks 2–4 do not name their profile; block 0 does.
            mouse.read_selected_sensor(target)?;
            mouse.read_selected_buttons(target)
        })
    }

    /// Writes button assignments to the profile they belong to and checks
    /// that they read back unchanged. A newly assigned macro plays whatever
    /// the mouse has stored under its number; use [`Self::write_macro`] to
    /// write its keys too.
    ///
    /// Block writes go to the profile named in the last block 0 written, so
    /// this writes the profile's table entry and block 0 as read, then
    /// settings blocks 2, 3 and 4, the order of the official software. The
    /// saved lighting is left alone.
    pub fn write_buttons(&mut self, buttons: &Buttons) -> Result<()> {
        let profile: u8 = buttons.profile();
        let entry: ProfileEntry = self.profile_entry(profile)?;
        let sensor: Sensor = self.read_profile(Some(profile), Self::read_selected_sensor)?;
        self.write_blocks(&entry, &sensor, None, Some(buttons), &[])
    }

    /// Writes `macro_` as the macro that `button` plays in `layer` of the
    /// profile `buttons` belong to, together with `buttons`, which must assign
    /// a macro to that button (see [`Buttons::set_action`]). The buttons are
    /// checked by reading them back; the macro itself cannot be read back.
    ///
    /// Macros are shared by all profiles: every profile whose `button` plays
    /// a macro in `layer` plays this one afterwards.
    pub fn write_macro(
        &mut self,
        buttons: &Buttons,
        button: u8,
        layer: Layer,
        macro_: &Macro,
    ) -> Result<()> {
        if !matches!(buttons.action(button, layer)?, ButtonAction::Macro(_)) {
            return Err(Error::InvalidValue(format!(
                "button {button} does not play a macro in these assignments"
            )));
        }
        let number: u8 = Buttons::macro_number(button, layer)?
            .ok_or_else(|| Error::InvalidValue(format!("button {button} cannot play a macro")))?;
        let name: String = match layer {
            Layer::Standard => format!("button {button}"),
            Layer::EasyShift => format!("button {button} easy-shift"),
        };
        let mut reports: Vec<Report> = (0u8..)
            .zip(macro_.record(number, &name))
            .map(|(index, chunk)| protocol::write_macro_chunk(number, index, &chunk))
            .collect();
        reports.push(protocol::commit_macro(number));

        let profile: u8 = buttons.profile();
        let entry: ProfileEntry = self.profile_entry(profile)?;
        let sensor: Sensor = self.read_profile(Some(profile), Self::read_selected_sensor)?;
        self.write_blocks(&entry, &sensor, None, Some(buttons), &reports)
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
        self.write_blocks(
            &entry,
            &settings.sensor,
            Some(&settings.lighting),
            None,
            &[],
        )
    }

    /// Writes only block 0 ([`Sensor`]: polling rate, DPI stages and AUTO
    /// flags, lift-off, angle snapping, LED time-out, debounce) of the profile it
    /// belongs to, after the profile's table entry, and checks that it reads
    /// back unchanged. The saved lighting is left alone.
    pub fn write_sensor(&mut self, sensor: &Sensor) -> Result<()> {
        let entry: ProfileEntry = self.profile_entry(sensor.profile())?;
        self.write_blocks(&entry, sensor, None, None, &[])
    }

    /// Writes what changed from `before` to `after` (the same profile, as
    /// read with [`Self::settings`]) and returns whether anything was written.
    ///
    /// Only block 0 is written when the lighting did not change: with the AIMO
    /// effect the mouse reports the colours it shows, so writing the lighting
    /// back would save those.
    pub fn write_changes(
        &mut self,
        before: &ProfileSettings,
        after: &ProfileSettings,
    ) -> Result<bool> {
        if before.profile() != after.profile() {
            return Err(Error::InvalidValue(format!(
                "the settings of profile {} cannot replace those of profile {}",
                after.profile(),
                before.profile()
            )));
        }
        if before == after {
            return Ok(false);
        }
        if before.lighting == after.lighting {
            self.write_sensor(&after.sensor)?;
        } else {
            self.write_settings(after)?;
        }
        Ok(true)
    }

    /// Sets the lift-off distance in every profile in use, as the official
    /// software does after a calibration. Returns the profiles that changed.
    pub fn set_lift_off_all(&mut self, lift_off: LiftOff) -> Result<Vec<u8>> {
        let mut changed: Vec<u8> = Vec::new();
        for profile in self.profiles()? {
            let mut sensor: Sensor = self.settings(Some(profile))?.sensor;
            if sensor.lift_off() != lift_off {
                sensor.set_lift_off(lift_off)?;
                self.write_sensor(&sensor)?;
                changed.push(profile);
            }
        }
        Ok(changed)
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

    /// Reads the settings and button assignments of all profiles in use.
    pub fn backup(&mut self) -> Result<Backup> {
        let info: Info = self.info()?;
        let table = self.profile_table()?;
        let mut profiles: Vec<ProfileBackup> = Vec::new();
        for (entry, number) in table.into_iter().zip(1u8..) {
            if profile_in_use(&entry) {
                let (settings, buttons) = self.read_profile_and_buttons(number)?;
                profiles.push(ProfileBackup {
                    entry,
                    settings,
                    buttons: Some(buttons),
                });
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
    /// must match the mouse's, and the dongle's when both are known. Button
    /// assignments must belong to the profile they are stored with, the
    /// active profile must be in the backup, and every profile must be marked
    /// in use and appear only once (as [`Backup::from_text`] checks).
    pub fn check_backup(&mut self, backup: &Backup) -> Result<()> {
        let mut numbers: Vec<u8> = Vec::new();
        for profile in &backup.profiles {
            let number: u8 = profile.settings.profile();
            profile_index(number)
                .map_err(|_| Error::InvalidBackup(format!("it has a profile numbered {number}")))?;
            if numbers.contains(&number) {
                return Err(Error::InvalidBackup(format!(
                    "it has profile {number} twice"
                )));
            }
            if !profile_in_use(&profile.entry) {
                return Err(Error::InvalidBackup(format!(
                    "the table entry of profile {number} marks it as unused"
                )));
            }
            numbers.push(number);
        }
        if !backup
            .profiles
            .iter()
            .any(|profile| profile.settings.profile() == backup.active_profile)
        {
            return Err(Error::InvalidBackup(format!(
                "its active profile {} is not in it",
                backup.active_profile
            )));
        }
        for profile in &backup.profiles {
            if let Some(buttons) = &profile.buttons
                && buttons.profile() != profile.settings.profile()
            {
                return Err(Error::InvalidBackup(format!(
                    "the buttons of profile {} are stored with profile {}",
                    buttons.profile(),
                    profile.settings.profile()
                )));
            }
        }
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
    /// after [`Self::check_backup`]. Button assignments are written if the
    /// backup has them (backups of konectl 0.2 and older do not). Profile
    /// slots in use that the backup does not have are cleared afterwards.
    pub fn restore(&mut self, backup: &Backup) -> Result<()> {
        self.check_backup(backup)?;
        for profile in &backup.profiles {
            let settings: &ProfileSettings = &profile.settings;
            self.write_blocks(
                &profile.entry,
                &settings.sensor,
                Some(&settings.lighting),
                profile.buttons.as_ref(),
                &[],
            )?;
        }
        self.set_active_profile(backup.active_profile)?;
        let kept: Vec<u8> = backup
            .profiles
            .iter()
            .map(|profile| profile.settings.profile())
            .collect();
        for (number, entry) in (1u8..).zip(self.profile_table()?) {
            if profile_in_use(&entry) && !kept.contains(&number) {
                self.command(
                    &protocol::clear_profile_entry(number),
                    "profile table write",
                )?;
            }
        }
        Ok(())
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

    /// Runs `read` with `profile` (default: the active one) selected for reads,
    /// passing the profile number.
    ///
    /// Reading another profile selects it and selects the active profile
    /// again afterwards, even if `read` failed, as the official software does.
    fn read_profile<R>(
        &mut self,
        profile: Option<u8>,
        read: impl FnOnce(&mut Self, u8) -> Result<R>,
    ) -> Result<R> {
        let active: u8 = self.active_profile()?;
        let target: u8 = profile.unwrap_or(active);
        if target == active {
            return read(self, target);
        }
        self.profile_entry(target)?;
        if let Err(error) = self.command(&protocol::select_profile(target), "profile selection") {
            // The mouse may have selected it with only the answer lost.
            let _ = self.command(&protocol::select_profile(active), "profile selection");
            return Err(error);
        }
        let value: Result<R> = read(self, target);
        let restored: Result<()> =
            self.command(&protocol::select_profile(active), "profile selection");
        let value: R = value?;
        restored?;
        Ok(value)
    }

    /// Reads the settings and buttons of `profile` with one selection.
    fn read_profile_and_buttons(&mut self, profile: u8) -> Result<(ProfileSettings, Buttons)> {
        self.read_profile(Some(profile), |mouse, target| {
            let settings: ProfileSettings = mouse.read_selected_settings(target)?;
            Ok((settings, mouse.read_selected_buttons(target)?))
        })
    }

    /// Reads block 0 of the selected profile, which must be `profile`.
    fn read_selected_sensor(&mut self, profile: u8) -> Result<Sensor> {
        let sensor: Sensor = self.info()?.sensor;
        if sensor.profile() != profile {
            return Err(Error::VerifyFailed("profile selection"));
        }
        Ok(sensor)
    }

    /// Reads block 1 of the selected profile.
    fn read_selected_lighting(&mut self) -> Result<Lighting> {
        self.request(
            &protocol::query_lighting(),
            "lighting query",
            protocol::parse_lighting,
        )
    }

    /// Reads blocks 0 and 1 of the selected profile, which must be `profile`.
    fn read_selected_settings(&mut self, profile: u8) -> Result<ProfileSettings> {
        let sensor: Sensor = self.read_selected_sensor(profile)?;
        let lighting: Lighting = self.read_selected_lighting()?;
        Ok(ProfileSettings { sensor, lighting })
    }

    /// Reads blocks 2, 3 and 4 of the selected profile, `profile`.
    fn read_selected_buttons(&mut self, profile: u8) -> Result<Buttons> {
        let functions = self.request(
            &protocol::query_button_functions(),
            "button functions query",
            protocol::parse_button_functions,
        )?;
        let standard = self.request(
            &protocol::query_button_entries(Layer::Standard),
            "button query",
            |reply| protocol::parse_button_entries(reply, Layer::Standard),
        )?;
        let easy_shift = self.request(
            &protocol::query_button_entries(Layer::EasyShift),
            "Easy-Shift button query",
            |reply| protocol::parse_button_entries(reply, Layer::EasyShift),
        )?;
        Ok(Buttons::from_blocks(
            profile, functions, standard, easy_shift,
        ))
    }

    /// Writes the table entry, block 0 and (if given) block 1 and blocks 2–4
    /// of the profile named in `sensor`, then reads them back to check them.
    ///
    /// Block 0 comes first: the mouse writes blocks 1–4 into the profile named
    /// in the last block 0 it received (the table entry does not select it).
    ///
    /// `macros` (macro record chunks and commits) are sent after the blocks,
    /// before the read-back, as the official software does.
    fn write_blocks(
        &mut self,
        entry: &ProfileEntry,
        sensor: &Sensor,
        lighting: Option<&Lighting>,
        buttons: Option<&Buttons>,
        macros: &[Report],
    ) -> Result<()> {
        let profile: u8 = sensor.profile();
        profile_index(profile)?;
        if let Some(buttons) = buttons
            && buttons.profile() != profile
        {
            return Err(Error::InvalidValue(format!(
                "the buttons of profile {} cannot be written to profile {profile}",
                buttons.profile()
            )));
        }
        self.command(
            &protocol::write_profile_entry(profile, entry),
            "profile table write",
        )?;
        self.command(&protocol::write_sensor(sensor), "sensor settings write")?;
        if let Some(lighting) = lighting {
            self.command(&protocol::write_lighting(lighting), "lighting write")?;
        }
        if let Some(buttons) = buttons {
            for report in protocol::write_buttons(buttons) {
                self.command(&report, "button write")?;
            }
        }
        for report in macros {
            if protocol::is_macro_commit(report) {
                self.command_once(report, "macro commit", MACRO_COMMIT_DEADLINE)?;
            } else {
                self.command(report, "macro write")?;
            }
        }

        // The dongle sometimes delivers an answer twice, and a late copy of
        // an earlier read can stand in for the read-back: on a difference,
        // let late answers arrive, drop them and read once more.
        if self
            .read_back(profile, sensor, lighting, buttons)?
            .is_none()
        {
            return Ok(());
        }
        self.discard_pending(LATE_ANSWER_WAIT_MS)?;
        match self.read_back(profile, sensor, lighting, buttons)? {
            None => Ok(()),
            Some(what) => Err(Error::VerifyFailed(what)),
        }
    }

    /// Reads back exactly the blocks that were written; the name of the
    /// first one that differs, if any.
    fn read_back(
        &mut self,
        profile: u8,
        sensor: &Sensor,
        lighting: Option<&Lighting>,
        buttons: Option<&Buttons>,
    ) -> Result<Option<&'static str>> {
        let (stored_sensor, stored_lighting, stored_buttons) =
            self.read_profile(Some(profile), |mouse, target| {
                let sensor: Sensor = mouse.read_selected_sensor(target)?;
                let lighting: Option<Lighting> = lighting
                    .map(|_| mouse.read_selected_lighting())
                    .transpose()?;
                let buttons: Option<Buttons> = buttons
                    .map(|_| mouse.read_selected_buttons(target))
                    .transpose()?;
                Ok((sensor, lighting, buttons))
            })?;
        if stored_sensor.bytes() != sensor.write_bytes() {
            return Ok(Some("sensor settings"));
        }
        if let (Some(lighting), Some(stored)) = (lighting, stored_lighting) {
            // With the AIMO effect the mouse reports what it currently shows
            // instead of the saved values, so only the effect can be compared.
            let lighting_stored: bool = if lighting.effect() == Effect::Aimo {
                stored.effect() == Effect::Aimo
            } else {
                stored == *lighting
            };
            if !lighting_stored {
                return Ok(Some("lighting settings"));
            }
        }
        if let (Some(buttons), Some(stored)) = (buttons, stored_buttons)
            && stored != *buttons
        {
            return Ok(Some("buttons"));
        }
        Ok(None)
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
        self.exchange_with(report, what, REQUEST_RETRIES, REPLY_DEADLINE, classify)
    }

    /// [`Self::exchange`] with an explicit number of retries and deadline.
    fn exchange_with<R>(
        &mut self,
        report: &Report,
        what: &'static str,
        retries: usize,
        deadline: Duration,
        classify: impl Fn(&Report) -> Option<Result<R>>,
    ) -> Result<R> {
        for attempt in 0..=retries {
            self.discard_pending(0)?;
            self.transport.write_report(report)?;
            if let Some(outcome) = self.await_reply_within(deadline, &classify)? {
                if attempt > 0 {
                    // The first attempt may still be answered late.
                    self.discard_pending(DUPLICATE_WAIT_MS)?;
                }
                return outcome;
            }
        }
        Err(Error::Timeout(what))
    }

    /// Sends a write command once (no repeat), waits up to `deadline` until
    /// the mouse accepts it and gives the mouse [`WRITE_SETTLE`].
    fn command_once(
        &mut self,
        report: &Report,
        what: &'static str,
        deadline: Duration,
    ) -> Result<()> {
        self.exchange_with(
            report,
            what,
            0,
            deadline,
            |reply| match protocol::parse_status(reply, report) {
                Some(Status::Accepted) => Some(Ok(())),
                Some(Status::Error(code)) => Some(Err(Error::Rejected {
                    command: what,
                    code,
                })),
                None => None,
            },
        )?;
        std::thread::sleep(WRITE_SETTLE);
        Ok(())
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
        self.await_reply_within(REPLY_DEADLINE, parse)
    }

    /// [`Self::await_reply`] with another deadline.
    fn await_reply_within<R>(
        &mut self,
        within: Duration,
        parse: impl Fn(&Report) -> Option<R>,
    ) -> Result<Option<R>> {
        let deadline: Instant = Instant::now() + within;
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
    use crate::buttons::tests::start_buttons;
    use crate::buttons::{MacroMode, MouseFunction, Special};
    use crate::protocol::report_with_prefix;
    use crate::protocol::tests::{
        BUTTON_FUNCTIONS_REPLY, BUTTONS_REPLY, CABLE_INFO_REPLY, EASY_SHIFT_BUTTONS_REPLY,
        INFO_REPLY, LIGHTING_REPLY, PROFILE_TABLE_REPLY,
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

    #[test]
    fn takes_the_functions_the_mouse_reported() {
        // Captured: Easy-Shift held (slot 5), then the profile-previous
        // function; a stale battery answer in between is dropped.
        let mut mouse = KoneProAir {
            transport: MockTransport::new(Vec::new()),
            mode: ConnectionMode::Wireless,
        };
        mouse.transport.pending.extend([
            report_with_prefix(&[0x90, 0x07, 0x04, 0x00, 0x45, 0x00, 0x00, 0x05]),
            battery_reply(80),
            report_with_prefix(&[0x90, 0x07, 0x04, 0x00, 0x47, 0x00, 0x00, 0x06]),
        ]);
        let events: Vec<FunctionEvent> = mouse.take_function_events().expect("events");
        assert_eq!(
            events,
            [
                FunctionEvent {
                    code: 0x45,
                    entry: [0, 0],
                    slot: 5
                },
                FunctionEvent {
                    code: 0x47,
                    entry: [0, 0],
                    slot: 6
                },
            ]
        );
        assert!(mouse.take_function_events().expect("none").is_empty());
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

    /// The read-back part of [`write_batches`]: what the mouse reports.
    fn read_back_batches(stored: &ProfileSettings) -> Vec<Vec<Report>> {
        write_batches(stored).split_off(4)
    }

    #[test]
    fn write_that_does_not_read_back_fails() {
        // Read back twice: the second read shows the same.
        let mut batches = write_batches(&start_settings());
        batches.extend(read_back_batches(&start_settings()));
        let mut mouse = connect(batches);
        assert!(matches!(
            mouse.write_settings(&changed_settings()),
            Err(Error::VerifyFailed("sensor settings"))
        ));
    }

    #[test]
    fn a_late_copy_of_an_earlier_answer_is_read_again() {
        // The first read-back gets an old lighting answer; the second the
        // written one.
        let mut stale = changed_settings();
        stale.lighting = start_settings().lighting;
        let mut batches = write_batches(&stale);
        batches.extend(read_back_batches(&changed_settings()));
        let mut mouse = connect(batches);
        mouse.write_settings(&changed_settings()).expect("verified");
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

    /// Answers to the three block reads of `buttons`.
    fn button_replies(buttons: &Buttons) -> Vec<Vec<Report>> {
        let reply = |prefix: [u8; 4], bytes: &[u8]| {
            let mut reply: Report = report_with_prefix(&prefix);
            reply[4..4 + bytes.len()].copy_from_slice(bytes);
            reply
        };
        let [functions, standard, easy_shift] = protocol::write_buttons(buttons);
        vec![
            vec![reply([0x90, 0x02, 0x1c, 0x00], &functions[4..32])],
            vec![reply([0x90, 0x04, 0x18, 0x00], &standard[4..28])],
            vec![reply([0x90, 0x05, 0x18, 0x00], &easy_shift[4..28])],
        ]
    }

    fn button_queries() -> [Report; 3] {
        [
            protocol::query_button_functions(),
            protocol::query_button_entries(Layer::Standard),
            protocol::query_button_entries(Layer::EasyShift),
        ]
    }

    #[test]
    fn buttons_are_read_from_blocks_2_to_4() {
        let mut mouse = connect(vec![
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&BUTTON_FUNCTIONS_REPLY)],
            vec![report_with_prefix(&BUTTONS_REPLY)],
            vec![report_with_prefix(&EASY_SHIFT_BUTTONS_REPLY)],
        ]);
        assert_eq!(mouse.buttons(None).expect("buttons"), start_buttons());
        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(
            written[..2],
            [protocol::query_info(), protocol::query_info()]
        );
        assert_eq!(written[2..], button_queries());
    }

    /// Replies for writing buttons to profile 1 (active) with the start
    /// settings, after which the mouse reports `stored` buttons.
    fn write_button_batches(stored: &Buttons) -> Vec<Vec<Report>> {
        let settings = start_settings();
        let stored_sensor = Sensor::from_bytes(settings.sensor.write_bytes());
        let mut batches = vec![
            // table entry, then block 0 as read
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            // entry, block 0, blocks 2–4
            vec![accepted(0x11)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            // read back: block 0 and blocks 2–4
            vec![info_reply(&stored_sensor)],
            vec![info_reply(&stored_sensor)],
        ];
        batches.extend(button_replies(stored));
        batches
    }

    #[test]
    fn write_buttons_writes_block_0_first() {
        let mut buttons = start_buttons();
        buttons
            .set_action(7, Layer::Standard, ButtonAction::Mouse(MouseFunction::Back))
            .expect("allowed");
        let mut mouse = connect(write_button_batches(&buttons));
        mouse.write_buttons(&buttons).expect("write");

        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written.len(), 13);
        assert_eq!(written[0], protocol::query_profile_table());
        assert_eq!(
            written[3],
            report_with_prefix(&[0x10, 0x11, 0x06, 0x00, 0x01, 0x01, 0x64])
        );
        // Block 0 as read (it names the profile the following blocks go to),
        // then blocks 2–4; block 1 is neither written nor read.
        assert_eq!(written[4], protocol::write_sensor(&start_settings().sensor));
        assert_eq!(written[5..8], protocol::write_buttons(&buttons));
        // Button 7 is slot 4 in block 3: browser back (capture 56).
        assert_eq!(written[6][4 + 9..4 + 12], [0x00, 0x04, 0x00]);
        assert!(written.iter().all(|report| {
            report[..4] != [0x10, 0x50, 0x0b, 0x01] && *report != protocol::query_lighting()
        }));
        assert_eq!(written[10..], button_queries());
    }

    #[test]
    fn write_buttons_to_another_profile_writes_its_block_0() {
        // Profile 1 is active; the buttons belong to profile 2.
        let start = start_buttons();
        let buttons = Buttons::from_blocks(
            2,
            *start.functions(),
            *start.entries(Layer::Standard),
            *start.entries(Layer::EasyShift),
        );
        let mut bytes = start_settings().sensor.bytes();
        bytes[0] = 2;
        let sensor_2 = Sensor::from_bytes(bytes);
        let stored_2 = Sensor::from_bytes(sensor_2.write_bytes());
        let table = report_with_prefix(&PROFILE_TABLE_REPLY);
        let mut batches = vec![
            vec![table],
            // block 0 of profile 2, selected for the read
            vec![report_with_prefix(&INFO_REPLY)],
            vec![table],
            vec![accepted(0x13)],
            vec![info_reply(&sensor_2)],
            vec![accepted(0x13)],
            // entry, block 0, blocks 2–4
            vec![accepted(0x11)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            // read back, selected again
            vec![report_with_prefix(&INFO_REPLY)],
            vec![table],
            vec![accepted(0x13)],
            vec![info_reply(&stored_2)],
        ];
        batches.extend(button_replies(&buttons));
        batches.push(vec![accepted(0x13)]);
        let mut mouse = connect(batches);
        mouse.write_buttons(&buttons).expect("write");

        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written[6][..5], [0x10, 0x11, 0x06, 0x00, 0x02]);
        // Block 0 of profile 2 comes before the button blocks.
        assert_eq!(written[7], protocol::write_sensor(&sensor_2));
        assert_eq!(written[7][4], 2);
        assert_eq!(written[8..11], protocol::write_buttons(&buttons));
        assert_eq!(written.last(), Some(&protocol::select_profile(1)));
    }

    #[test]
    fn write_macro_sends_the_record_after_the_blocks() {
        let mut buttons = start_buttons();
        buttons
            .set_action(
                7,
                Layer::Standard,
                ButtonAction::Macro(MacroMode::OnPress { repeat: 1 }),
            )
            .expect("allowed");
        let macro_: Macro = "down:c, 50ms, up:c".parse().expect("macro");
        let stored = Sensor::from_bytes(start_settings().sensor.write_bytes());
        let mut batches = vec![
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![accepted(0x11)],
        ];
        batches.extend(vec![vec![accepted(0x50)]; 4]);
        batches.extend([
            vec![accepted(0x23)],
            vec![accepted(0x23)],
            vec![accepted(0x60)],
        ]);
        batches.extend([vec![info_reply(&stored)], vec![info_reply(&stored)]]);
        batches.extend(button_replies(&buttons));
        let mut mouse = connect(batches);
        mouse
            .write_macro(&buttons, 7, Layer::Standard, &macro_)
            .expect("write");

        let written: &[Report] = &mouse.transport.written[1..];
        // Entry, block 0, blocks 2-4, then the record of macro 4 (capture 67).
        assert_eq!(written[4], protocol::write_sensor(&start_settings().sensor));
        assert_eq!(written[5..8], protocol::write_buttons(&buttons));
        assert_eq!(
            written[8][..8],
            [0x10, 0x23, 0x3c, 0x00, 0x04, 0x09, 0x04, 0x38]
        );
        assert_eq!(
            written[9][..19],
            [
                0x10, 0x23, 0x3c, 0x01, 0x02, 0x0a, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x32, 0x00,
                0x81, 0x00, 0x06, 0x00, 0x00
            ]
        );
        assert_eq!(written[10], protocol::commit_macro(4));
        assert_eq!(written[13..], button_queries());
    }

    #[test]
    fn macro_commit_is_not_repeated() {
        let mut buttons = start_buttons();
        buttons
            .set_action(
                7,
                Layer::Standard,
                ButtonAction::Macro(MacroMode::OnPress { repeat: 1 }),
            )
            .expect("allowed");
        let macro_: Macro = "a".parse().expect("macro");
        let mut batches = vec![
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![accepted(0x11)],
        ];
        batches.extend(vec![vec![accepted(0x50)]; 4]);
        batches.extend([vec![accepted(0x23)], vec![accepted(0x23)]]);
        let mut mouse = connect(batches);
        assert!(matches!(
            mouse.write_macro(&buttons, 7, Layer::Standard, &macro_),
            Err(Error::Timeout("macro commit"))
        ));
        let commits: usize = mouse
            .transport
            .written
            .iter()
            .filter(|report| protocol::is_macro_commit(report))
            .count();
        assert_eq!(commits, 1);
    }

    #[test]
    fn write_macro_needs_a_macro_assignment() {
        let macro_: Macro = "a".parse().expect("macro");
        let mut mouse = connect(vec![]);
        assert!(matches!(
            mouse.write_macro(&start_buttons(), 7, Layer::Standard, &macro_),
            Err(Error::InvalidValue(_))
        ));
        assert_eq!(mouse.transport.written.len(), 1);
    }

    #[test]
    fn buttons_that_do_not_read_back_fail() {
        let mut buttons = start_buttons();
        buttons
            .set_action(7, Layer::Standard, ButtonAction::Disabled)
            .expect("allowed");
        // Read back twice: the second read shows the same.
        let mut batches = write_button_batches(&start_buttons());
        let read_back: Vec<Vec<Report>> = batches[batches.len() - 5..].to_vec();
        batches.extend(read_back);
        let mut mouse = connect(batches);
        assert!(matches!(
            mouse.write_buttons(&buttons),
            Err(Error::VerifyFailed("buttons"))
        ));
    }

    #[test]
    fn buttons_of_an_unselected_profile_are_refused() {
        // The info answer after the selection still names profile 1.
        let mut mouse = connect(vec![
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![accepted(0x13)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![accepted(0x13)],
        ]);
        assert!(matches!(
            mouse.buttons(Some(2)),
            Err(Error::VerifyFailed("profile selection"))
        ));
        // The active profile is selected again.
        assert_eq!(
            mouse.transport.written.last(),
            Some(&protocol::select_profile(1))
        );
    }

    /// Profile 1 of the start state, as a one-profile backup.
    fn start_backup() -> Backup {
        Backup {
            firmware: firmware(4, 32),
            dongle_firmware: firmware(4, 19),
            active_profile: 1,
            profiles: vec![ProfileBackup {
                entry: [0x01, 0x64, 0x00, 0x00, 0x00],
                settings: start_settings(),
                buttons: Some(start_buttons()),
            }],
        }
    }

    #[test]
    fn backup_includes_the_buttons() {
        let settings = start_settings();
        let mut batches = vec![
            vec![report_with_prefix(&INFO_REPLY)],
            vec![single_profile_table()],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![lighting_reply(&settings.lighting)],
        ];
        batches.extend(button_replies(&start_buttons()));
        let mut mouse = connect(batches);
        assert_eq!(mouse.backup().expect("backup"), start_backup());
    }

    #[test]
    fn restore_writes_the_buttons() {
        let backup = start_backup();
        let settings = start_settings();
        let stored_sensor = Sensor::from_bytes(settings.sensor.write_bytes());
        let mut batches = vec![
            // check_backup
            vec![report_with_prefix(&INFO_REPLY)],
            // entry and blocks 0–4 in the official order
            vec![accepted(0x11)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            // read back
            vec![info_reply(&stored_sensor)],
            vec![info_reply(&stored_sensor)],
            vec![lighting_reply(&settings.lighting)],
        ];
        batches.extend(button_replies(&start_buttons()));
        // activation of the backup's active profile, then the table check
        batches.extend([
            vec![single_profile_table()],
            vec![accepted(0x03)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![single_profile_table()],
        ]);
        let mut mouse = connect(batches);
        mouse.restore(&backup).expect("restore");
        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written[2], protocol::write_sensor(&settings.sensor));
        assert_eq!(written[3], protocol::write_lighting(&settings.lighting));
        assert_eq!(written[4..7], protocol::write_buttons(&start_buttons()));
        // Activation, then the check for slots the backup does not have.
        assert_eq!(written.last(), Some(&protocol::query_profile_table()));
    }

    #[test]
    fn restore_without_buttons_leaves_them_alone() {
        let mut backup = start_backup();
        backup.profiles[0].buttons = None;
        let settings = start_settings();
        let stored_sensor = Sensor::from_bytes(settings.sensor.write_bytes());
        let mut mouse = connect(vec![
            vec![report_with_prefix(&INFO_REPLY)],
            vec![accepted(0x11)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![info_reply(&stored_sensor)],
            vec![info_reply(&stored_sensor)],
            vec![lighting_reply(&settings.lighting)],
            vec![single_profile_table()],
            vec![accepted(0x03)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![single_profile_table()],
        ]);
        mouse.restore(&backup).expect("restore");
        let button_requests: usize = mouse.transport.written[1..]
            .iter()
            .filter(|report| {
                (report[..2] == [0x10, 0x50] && report[3] >= 2) || button_queries().contains(report)
            })
            .count();
        assert_eq!(button_requests, 0);
    }

    #[test]
    fn restore_refuses_buttons_of_another_profile() {
        let mut backup = start_backup();
        let start = start_buttons();
        backup.profiles[0].buttons = Some(Buttons::from_blocks(
            2,
            *start.functions(),
            *start.entries(Layer::Standard),
            *start.entries(Layer::EasyShift),
        ));
        let mut mouse = connect(vec![]);
        assert!(matches!(
            mouse.restore(&backup),
            Err(Error::InvalidBackup(_))
        ));
        // Nothing was sent.
        assert_eq!(mouse.transport.written.len(), 1);
    }

    #[test]
    fn failed_selection_selects_the_active_profile_again() {
        let mut mouse = connect(vec![
            vec![report_with_prefix(&INFO_REPLY)],
            vec![report_with_prefix(&PROFILE_TABLE_REPLY)],
            vec![report_with_prefix(&[
                0x10, 0x13, 0x03, 0x00, 0xec, 0xfa, 0x05,
            ])],
            vec![accepted(0x13)],
        ]);
        assert!(matches!(
            mouse.buttons(Some(2)),
            Err(Error::Rejected { .. })
        ));
        assert_eq!(
            mouse.transport.written.last(),
            Some(&protocol::select_profile(1))
        );
    }

    /// Profile table with slots 1 to `count` in use.
    fn table_with(count: usize) -> Report {
        let mut reply: Report = report_with_prefix(&[0x90, 0x11, 0x19, 0x00]);
        for slot in 0..count {
            reply[4 + slot * 5..6 + slot * 5].copy_from_slice(&[0x01, 0x64]);
        }
        reply
    }

    /// The start settings and buttons, moved to profile `profile`.
    fn profile_data(profile: u8) -> (ProfileSettings, Buttons) {
        let settings = start_settings();
        let settings = ProfileSettings {
            sensor: settings.sensor.with_profile(profile),
            lighting: settings.lighting,
        };
        (settings, start_buttons().with_profile(profile))
    }

    /// Replies for reading the settings and buttons of `profile` while profile
    /// 1 is active.
    fn read_profile_batches(profile: u8, table: Report) -> Vec<Vec<Report>> {
        let (settings, buttons) = profile_data(profile);
        let mut batches = vec![vec![report_with_prefix(&INFO_REPLY)]];
        if profile != 1 {
            batches.extend([vec![table], vec![accepted(0x13)]]);
        }
        batches.extend([
            vec![info_reply(&settings.sensor)],
            vec![lighting_reply(&settings.lighting)],
        ]);
        batches.extend(button_replies(&buttons));
        if profile != 1 {
            batches.push(vec![accepted(0x13)]);
        }
        batches
    }

    /// Replies for writing all blocks of `profile` (not active) with the start
    /// data, read back the same way.
    fn write_profile_batches(profile: u8, table: Report) -> Vec<Vec<Report>> {
        let mut batches = vec![vec![accepted(0x11)]];
        batches.extend(vec![vec![accepted(0x50)]; 5]);
        let (settings, buttons) = profile_data(profile);
        let stored = Sensor::from_bytes(settings.sensor.write_bytes());
        batches.extend([
            vec![report_with_prefix(&INFO_REPLY)],
            vec![table],
            vec![accepted(0x13)],
            vec![info_reply(&stored)],
            vec![lighting_reply(&settings.lighting)],
        ]);
        batches.extend(button_replies(&buttons));
        batches.push(vec![accepted(0x13)]);
        batches
    }

    #[test]
    fn add_profile_copies_into_the_first_free_slot() {
        let mut batches = vec![vec![table_with(3)]];
        batches.extend(read_profile_batches(1, table_with(3)));
        batches.extend(write_profile_batches(4, table_with(4)));
        let mut mouse = connect(batches);
        assert_eq!(mouse.add_profile(1).expect("add"), 4);

        let written: &[Report] = &mouse.transport.written[1..];
        let entry_write: usize = written
            .iter()
            .position(|report| report[..2] == [0x10, 0x11])
            .expect("table entry written");
        // Capture 61: the entry of the new slot, then blocks 0–4 for it.
        assert_eq!(
            written[entry_write],
            report_with_prefix(&[0x10, 0x11, 0x06, 0x00, 0x04, 0x01, 0x64])
        );
        let (settings, buttons) = profile_data(4);
        assert_eq!(
            written[entry_write + 1],
            protocol::write_sensor(&settings.sensor)
        );
        assert_eq!(written[entry_write + 1][4], 4);
        assert_eq!(
            written[entry_write + 2],
            protocol::write_lighting(&settings.lighting)
        );
        assert_eq!(
            written[entry_write + 3..entry_write + 6],
            protocol::write_buttons(&buttons)
        );
    }

    #[test]
    fn add_profile_needs_a_free_slot() {
        let mut mouse = connect(vec![vec![table_with(5)]]);
        assert!(matches!(mouse.add_profile(1), Err(Error::InvalidValue(_))));
        let mut mouse = connect(vec![vec![table_with(3)]]);
        assert!(matches!(mouse.add_profile(4), Err(Error::UnusedProfile(4))));
    }

    #[test]
    fn remove_profile_moves_the_later_ones_down() {
        // Profiles 1–3, 1 active; removing 2 moves 3 to 2 and clears slot 3
        // (capture 63).
        let mut batches = vec![vec![table_with(3)], vec![report_with_prefix(&INFO_REPLY)]];
        // Profile 1 stays (no buttons to renumber), profile 3 moves.
        batches.extend(read_profile_batches(1, table_with(3)));
        batches.extend(read_profile_batches(3, table_with(3)));
        batches.extend(write_profile_batches(2, table_with(3)));
        batches.push(vec![accepted(0x11)]);
        let mut mouse = connect(batches);
        assert_eq!(
            mouse.remove_profile(2).expect("remove"),
            RemovedProfile {
                moved: vec![(3, 2)],
                disabled_buttons: 0
            }
        );

        let written: &[Report] = &mouse.transport.written[1..];
        assert!(written.contains(&report_with_prefix(&[
            0x10, 0x11, 0x06, 0x00, 0x02, 0x01, 0x64
        ])));
        let (settings, _) = profile_data(2);
        assert!(written.contains(&protocol::write_sensor(&settings.sensor)));
        assert_eq!(written.last(), Some(&protocol::clear_profile_entry(3)));
        // The active profile did not move.
        assert!(written.iter().all(|report| report[..2] != [0x10, 0x03]));
    }

    #[test]
    fn remove_profile_with_a_gap_renumbers_by_slot() {
        // Slots 1, 3 and 4 in use, profile 1 active. Profile 1 has buttons
        // for profile 3 and the empty slot 2; removing 4 moves 3 to 2.
        let mut table: Report = table_with(4);
        table[9..14].copy_from_slice(&[0; 5]);
        let mut buttons_1 = start_buttons();
        for (button, target) in [(3, 3), (6, 2)] {
            buttons_1
                .set_action(
                    button,
                    Layer::Standard,
                    ButtonAction::Special(Special::Profile(target)),
                )
                .expect("allowed");
        }
        let mut batches = vec![vec![table], vec![report_with_prefix(&INFO_REPLY)]];
        // Profile 1 (active): read directly.
        let settings_1 = start_settings();
        batches.extend([
            vec![report_with_prefix(&INFO_REPLY)],
            vec![info_reply(&settings_1.sensor)],
            vec![lighting_reply(&settings_1.lighting)],
        ]);
        batches.extend(button_replies(&buttons_1));
        // Profile 3: selected for reading.
        let (settings_3, buttons_3) = profile_data(3);
        batches.extend([
            vec![report_with_prefix(&INFO_REPLY)],
            vec![table],
            vec![accepted(0x13)],
            vec![info_reply(&settings_3.sensor)],
            vec![lighting_reply(&settings_3.lighting)],
        ]);
        batches.extend(button_replies(&buttons_3));
        batches.push(vec![accepted(0x13)]);
        // Profile 1's entry, block 0 and blocks 2-4 are accepted; the read-back
        // after them is not answered, so the plan is checked up to there.
        batches.push(vec![accepted(0x11)]);
        batches.extend(vec![vec![accepted(0x50)]; 4]);
        let mut mouse = connect(batches);
        let _ = mouse.remove_profile(4);

        let written: &[Report] = &mouse.transport.written[1..];
        let entry_write: usize = written
            .iter()
            .position(|report| report[..2] == [0x10, 0x11])
            .expect("profile 1 is written first");
        assert_eq!(written[entry_write][4], 1);
        let button_functions: Report = *written
            .iter()
            .find(|report| report[..4] == [0x10, 0x50, 0x1c, 0x02])
            .expect("block 2 of profile 1");
        // Button 3 (slot 1): profile 3 -> 2; button 6 (slot 5): the empty
        // slot 2 is filled by profile 3, so it is disabled.
        assert_eq!(button_functions[4 + 2], 0x49);
        assert_eq!(button_functions[4 + 4], 0x00);
    }

    #[test]
    fn remove_profile_activates_the_moved_active_profile() {
        // Profiles 1–3, 3 active; removing 1 moves 2 to 1 and 3 to 2.
        let profile_3 = profile_data(3).0.sensor;
        let table = table_with(3);
        let mut batches = vec![vec![table], vec![info_reply(&profile_3)]];
        for source in [2, 3] {
            let (settings, buttons) = profile_data(source);
            batches.push(vec![info_reply(&profile_3)]);
            if source != 3 {
                batches.extend([vec![table], vec![accepted(0x13)]]);
            }
            batches.extend([
                vec![info_reply(&settings.sensor)],
                vec![lighting_reply(&settings.lighting)],
            ]);
            batches.extend(button_replies(&buttons));
            if source != 3 {
                batches.push(vec![accepted(0x13)]);
            }
        }
        for target in [1, 2] {
            batches.push(vec![accepted(0x11)]);
            batches.extend(vec![vec![accepted(0x50)]; 5]);
            let (settings, buttons) = profile_data(target);
            let stored = Sensor::from_bytes(settings.sensor.write_bytes());
            batches.extend([
                vec![info_reply(&profile_3)],
                vec![table],
                vec![accepted(0x13)],
                vec![info_reply(&stored)],
                vec![lighting_reply(&settings.lighting)],
            ]);
            batches.extend(button_replies(&buttons));
            batches.push(vec![accepted(0x13)]);
        }
        let moved_active = profile_data(2).0.sensor;
        batches.extend([
            vec![table],
            vec![accepted(0x03)],
            vec![info_reply(&moved_active)],
            vec![accepted(0x11)],
        ]);
        let mut mouse = connect(batches);
        mouse.remove_profile(1).expect("remove");

        let written: &[Report] = &mouse.transport.written[1..];
        let activation: usize = written
            .iter()
            .position(|report| *report == protocol::activate_profile(2))
            .expect("activation");
        // Activated before its old slot is cleared.
        assert_eq!(written[activation + 2], protocol::clear_profile_entry(3));
        assert_eq!(written.last(), Some(&protocol::clear_profile_entry(3)));
    }

    #[test]
    fn remove_profile_refuses_the_active_and_the_only_profile() {
        let mut mouse = connect(vec![
            vec![table_with(3)],
            vec![report_with_prefix(&INFO_REPLY)],
        ]);
        assert!(matches!(
            mouse.remove_profile(1),
            Err(Error::InvalidValue(_))
        ));
        let mut mouse = connect(vec![vec![table_with(1)]]);
        assert!(matches!(
            mouse.remove_profile(1),
            Err(Error::InvalidValue(_))
        ));
        let mut mouse = connect(vec![vec![table_with(3)]]);
        assert!(matches!(
            mouse.remove_profile(4),
            Err(Error::UnusedProfile(4))
        ));
        // Nothing but table and info reads was sent.
        assert!(
            mouse.transport.written[1..]
                .iter()
                .all(|report| report[0] == 0x90)
        );
    }

    #[test]
    fn profile_colour_is_written_to_the_table_entry() {
        let mut coloured = table_with(3);
        coloured[9..14].copy_from_slice(&[0x01, 0x64, 0xf4, 0x00, 0x00]);
        let mut mouse = connect(vec![
            vec![table_with(3)],
            vec![accepted(0x11)],
            vec![coloured],
        ]);
        mouse
            .set_profile_color(2, Some(Rgb::new(0xf4, 0, 0)))
            .expect("colour");
        // Capture 58: red for profile slot 2.
        assert_eq!(
            mouse.transport.written[2],
            report_with_prefix(&[0x10, 0x11, 0x06, 0x00, 0x02, 0x01, 0x64, 0xf4])
        );
    }

    #[test]
    fn restore_clears_profiles_the_backup_does_not_have() {
        let mut backup = start_backup();
        backup.profiles[0].buttons = None;
        let settings = start_settings();
        let stored_sensor = Sensor::from_bytes(settings.sensor.write_bytes());
        let mut mouse = connect(vec![
            vec![report_with_prefix(&INFO_REPLY)],
            vec![accepted(0x11)],
            vec![accepted(0x50)],
            vec![accepted(0x50)],
            vec![info_reply(&stored_sensor)],
            vec![info_reply(&stored_sensor)],
            vec![lighting_reply(&settings.lighting)],
            vec![table_with(4)],
            vec![accepted(0x03)],
            vec![report_with_prefix(&INFO_REPLY)],
            vec![table_with(4)],
            vec![accepted(0x11)],
            vec![accepted(0x11)],
            vec![accepted(0x11)],
        ]);
        mouse.restore(&backup).expect("restore");
        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(
            written[written.len() - 3..],
            [
                protocol::clear_profile_entry(2),
                protocol::clear_profile_entry(3),
                protocol::clear_profile_entry(4)
            ]
        );
    }

    #[test]
    fn reset_waits_long_and_is_not_repeated() {
        let mut mouse = connect(vec![vec![accepted(0x62)]]);
        mouse.reset().expect("reset");
        assert_eq!(mouse.transport.written[1], protocol::reset());
        let mut mouse = connect(vec![]);
        assert!(matches!(mouse.reset(), Err(Error::Timeout("reset"))));
        assert_eq!(mouse.transport.written.len(), 2);
    }

    #[test]
    fn pairing_needs_the_dongle() {
        let mut mouse = connect(vec![vec![accepted(0x09)]]);
        mouse.start_pairing().expect("pairing");
        assert_eq!(mouse.transport.written[1], protocol::start_pairing());
        let transport = MockTransport::new(vec![vec![init_reply()]]);
        let mut wired =
            KoneProAir::connect(transport, ConnectionMode::Wired).expect("mock connect");
        assert!(matches!(wired.start_pairing(), Err(Error::InvalidValue(_))));
        assert_eq!(wired.transport.written.len(), 1);
    }

    #[test]
    fn late_init_reply_is_skipped_by_the_info_query() {
        // A late init answer, or one of another program on the same device.
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
        // Table, entry, block 0, then block 0 read back (no lighting read).
        assert_eq!(written.len(), 5);
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
            ..start_backup()
        };
        connect(vec![vec![report_with_prefix(info_reply)]]).check_backup(&backup)
    }

    #[test]
    fn backup_profiles_must_be_in_use_and_unique() {
        let mut unused = start_backup();
        unused.profiles[0].entry = [0; 5];
        let mut twice = start_backup();
        twice.profiles.push(twice.profiles[0]);
        for backup in [unused, twice] {
            let mut mouse = connect(vec![]);
            assert!(matches!(
                mouse.check_backup(&backup),
                Err(Error::InvalidBackup(_))
            ));
            assert_eq!(mouse.transport.written.len(), 1);
        }
    }

    #[test]
    fn backup_must_contain_its_active_profile() {
        let backup = Backup {
            active_profile: 2,
            ..start_backup()
        };
        assert!(matches!(
            connect(vec![]).check_backup(&backup),
            Err(Error::InvalidBackup(_))
        ));
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
        assert_eq!(written.len(), 9);
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
        // Write profile 2, then read its block 0 back the same way.
        batches.extend([vec![table], vec![accepted(0x11)], vec![accepted(0x50)]]);
        batches.extend([
            vec![info_reply(&profile_1)],
            vec![table],
            vec![accepted(0x13)],
            vec![info_reply(&written_2)],
            vec![accepted(0x13)],
        ]);
        let mut mouse = connect(batches);
        mouse.set_debounce(3).expect("set debounce");

        let written: &[Report] = &mouse.transport.written[1..];
        assert_eq!(written.len(), 18);
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
