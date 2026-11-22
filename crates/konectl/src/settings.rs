//! Typed views of the settings stored on the mouse.
//!
//! Each type keeps the raw bytes it was read from and changes only the fields
//! it understands, so bytes whose meaning is unknown are written back unchanged.
//! See `docs/protocol.md` for the block layouts.

use std::fmt;
use std::str::FromStr;

use crate::color::Rgb;
use crate::error::{Error, Result};

/// Number of profile slots on the mouse.
pub const PROFILE_COUNT: u8 = 5;
/// Number of DPI stages per profile.
pub const DPI_STAGE_COUNT: u8 = 5;
/// Lowest DPI value the official software offers.
pub const DPI_MIN: u16 = 50;
/// Highest DPI value the official software offers.
pub const DPI_MAX: u16 = 19_000;
/// DPI values are stored in steps of this size.
pub const DPI_STEP: u16 = 50;
/// Highest lighting brightness, in percent.
pub const BRIGHTNESS_MAX: u8 = 100;
/// Slowest lighting effect speed.
pub const SPEED_MIN: u8 = 1;
/// Fastest lighting effect speed.
pub const SPEED_MAX: u8 = 11;
/// Longest debounce time the official software offers, in milliseconds.
pub const DEBOUNCE_MAX_MS: u8 = 10;

/// Fails unless `ms` is a debounce time the official software offers (0–10).
pub(crate) fn check_debounce_ms(ms: u8) -> Result<()> {
    if ms > DEBOUNCE_MAX_MS {
        return Err(Error::InvalidValue(format!(
            "invalid debounce time {ms} ms: use 0 to {DEBOUNCE_MAX_MS}"
        )));
    }
    Ok(())
}

/// Length of settings block 0 (sensor and DPI).
pub(crate) const SENSOR_LEN: usize = 20;
/// Length of settings block 1 (saved lighting).
pub(crate) const LIGHTING_LEN: usize = 11;
/// Length of one profile table entry.
pub(crate) const PROFILE_ENTRY_LEN: usize = 5;

/// One entry of the profile table (`90 11`). Its bytes are not understood;
/// they are only read and written back.
pub type ProfileEntry = [u8; PROFILE_ENTRY_LEN];

/// `true` if the profile table entry belongs to a slot in use.
///
/// Slots used by the official software hold `01 64 00 00 00`, unused ones are zero.
pub fn profile_in_use(entry: &ProfileEntry) -> bool {
    entry[0] != 0
}

/// Checks a 1-based profile number and returns its 0-based index.
pub(crate) fn profile_index(profile: u8) -> Result<usize> {
    if (1..=PROFILE_COUNT).contains(&profile) {
        Ok(usize::from(profile - 1))
    } else {
        Err(Error::InvalidValue(format!(
            "profile {profile} does not exist (1–{PROFILE_COUNT})"
        )))
    }
}

/// A firmware version (of the mouse or of the dongle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Firmware {
    pub major: u8,
    pub minor: u8,
}

impl Firmware {
    /// `0.00`: the device reported no version.
    pub(crate) const fn is_zero(self) -> bool {
        self.major == 0 && self.minor == 0
    }
}

impl fmt::Display for Firmware {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.major, self.minor)
    }
}

impl FromStr for Firmware {
    type Err = Error;

    /// Parses `major.minor`, e.g. `4.19`.
    fn from_str(s: &str) -> Result<Self> {
        let invalid = || Error::InvalidValue(format!("invalid firmware version {s:?}"));
        let (major, minor) = s.split_once('.').ok_or_else(invalid)?;
        Ok(Self {
            major: major.parse().map_err(|_| invalid())?,
            minor: minor.parse().map_err(|_| invalid())?,
        })
    }
}

/// Answer to the device info request: firmware versions and the selected
/// profile's block 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    /// Firmware of the mouse (what the official software shows over the cable).
    pub firmware: Firmware,
    /// Firmware of the wireless dongle; `None` over the USB cable.
    pub dongle_firmware: Option<Firmware>,
    /// Block 0 of the selected profile; outside of a profile read this is the
    /// active profile.
    pub sensor: Sensor,
}

/// The settings konectl reads and writes for one profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileSettings {
    pub sensor: Sensor,
    pub lighting: Lighting,
}

impl ProfileSettings {
    /// Profile number (1–5) the settings belong to.
    pub fn profile(&self) -> u8 {
        self.sensor.profile()
    }
}

/// How often the mouse reports to the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollingRate {
    Hz125,
    Hz250,
    Hz500,
    Hz1000,
}

impl PollingRate {
    /// The rate in Hz.
    pub const fn hz(self) -> u16 {
        match self {
            Self::Hz125 => 125,
            Self::Hz250 => 250,
            Self::Hz500 => 500,
            Self::Hz1000 => 1000,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Hz1000 => 0,
            Self::Hz500 => 1,
            Self::Hz250 => 2,
            Self::Hz125 => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        [Self::Hz1000, Self::Hz500, Self::Hz250, Self::Hz125]
            .into_iter()
            .find(|rate| rate.code() == code)
    }
}

impl fmt::Display for PollingRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.hz())
    }
}

impl FromStr for PollingRate {
    type Err = Error;

    /// Parses the rate in Hz: `125`, `250`, `500` or `1000`.
    fn from_str(s: &str) -> Result<Self> {
        [Self::Hz125, Self::Hz250, Self::Hz500, Self::Hz1000]
            .into_iter()
            .find(|rate| s.trim_end_matches("hz").trim_end_matches("Hz") == rate.hz().to_string())
            .ok_or_else(|| {
                Error::InvalidValue(format!(
                    "invalid polling rate {s:?}: use 125, 250, 500 or 1000"
                ))
            })
    }
}

/// Lift-off distance: how high the mouse can be lifted before it stops tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiftOff {
    VeryLow,
    Low,
    /// A value konectl does not know, e.g. from the official software's calibration.
    Other(u8),
}

impl LiftOff {
    fn code(self) -> u8 {
        match self {
            Self::VeryLow => 0x06,
            Self::Low => 0x07,
            Self::Other(code) => code,
        }
    }

    fn from_code(code: u8) -> Self {
        [Self::VeryLow, Self::Low]
            .into_iter()
            .find(|distance| distance.code() == code)
            .unwrap_or(Self::Other(code))
    }
}

impl fmt::Display for LiftOff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VeryLow => f.pad("very low"),
            Self::Low => f.pad("low"),
            Self::Other(code) => write!(f, "unknown ({code:#04x})"),
        }
    }
}

impl FromStr for LiftOff {
    type Err = Error;

    /// Parses `very-low` or `low`.
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "very-low" => Ok(Self::VeryLow),
            "low" => Ok(Self::Low),
            _ => Err(Error::InvalidValue(format!(
                "invalid lift-off distance {s:?}: use very-low or low"
            ))),
        }
    }
}

/// Settings block 0: polling rate, DPI stages, lift-off, angle snapping and
/// debounce time of one profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sensor {
    bytes: [u8; SENSOR_LEN],
}

impl Sensor {
    const PROFILE: usize = 0;
    const POLLING_RATE: usize = 1;
    const ANGLE_SNAPPING: usize = 2;
    const ACTIVE_STAGE: usize = 3;
    const DPI_STAGES: usize = 4;
    const LIFT_OFF: usize = 14;
    /// Unknown field; the official software clears bit 7 before writing it back.
    const FLAGS: usize = 16;
    const DEBOUNCE: usize = 19;

    pub const fn from_bytes(bytes: [u8; SENSOR_LEN]) -> Self {
        Self { bytes }
    }

    /// The block as read from the mouse.
    pub const fn bytes(&self) -> [u8; SENSOR_LEN] {
        self.bytes
    }

    /// The block as konectl writes it. Bit 7 of byte 16 is cleared, as the
    /// official software does.
    pub(crate) fn write_bytes(&self) -> [u8; SENSOR_LEN] {
        let mut bytes: [u8; SENSOR_LEN] = self.bytes;
        bytes[Self::FLAGS] &= 0x7f;
        bytes
    }

    /// Profile number (1–5) this block belongs to.
    pub const fn profile(&self) -> u8 {
        self.bytes[Self::PROFILE]
    }

    /// Polling rate, or `None` for a value konectl does not know.
    pub fn polling_rate(&self) -> Option<PollingRate> {
        PollingRate::from_code(self.bytes[Self::POLLING_RATE])
    }

    pub fn set_polling_rate(&mut self, rate: PollingRate) {
        self.bytes[Self::POLLING_RATE] = rate.code();
    }

    pub const fn angle_snapping(&self) -> bool {
        self.bytes[Self::ANGLE_SNAPPING] != 0
    }

    pub fn set_angle_snapping(&mut self, enabled: bool) {
        self.bytes[Self::ANGLE_SNAPPING] = u8::from(enabled);
    }

    /// Debounce time in milliseconds. Stored per profile; the official
    /// software keeps it equal in all profiles.
    pub const fn debounce_ms(&self) -> u8 {
        self.bytes[Self::DEBOUNCE]
    }

    /// Sets the debounce time, 0–10 ms. See
    /// [`KoneProAir::set_debounce`](crate::KoneProAir::set_debounce) to set it
    /// in all profiles, as the official software does.
    pub fn set_debounce_ms(&mut self, ms: u8) -> Result<()> {
        check_debounce_ms(ms)?;
        self.bytes[Self::DEBOUNCE] = ms;
        Ok(())
    }

    pub fn lift_off(&self) -> LiftOff {
        LiftOff::from_code(self.bytes[Self::LIFT_OFF])
    }

    /// Sets the lift-off distance; [`LiftOff::Other`] is rejected.
    pub fn set_lift_off(&mut self, distance: LiftOff) -> Result<()> {
        if let LiftOff::Other(code) = distance {
            return Err(Error::InvalidValue(format!(
                "unknown lift-off value {code:#04x}"
            )));
        }
        self.bytes[Self::LIFT_OFF] = distance.code();
        Ok(())
    }

    /// Active DPI stage, 1-based.
    pub const fn active_stage(&self) -> u8 {
        self.bytes[Self::ACTIVE_STAGE].saturating_add(1)
    }

    /// Makes `stage` (1–5) the active DPI stage; it must be enabled.
    pub fn set_active_stage(&mut self, stage: u8) -> Result<()> {
        let index: usize = stage_index(stage)?;
        if self.dpi(stage)?.is_none() {
            return Err(Error::InvalidValue(format!(
                "DPI stage {stage} is disabled; give it a DPI value first"
            )));
        }
        // index < DPI_STAGE_COUNT, so it fits into a u8.
        self.bytes[Self::ACTIVE_STAGE] = index as u8;
        Ok(())
    }

    /// DPI of `stage` (1–5), or `None` if the stage is disabled.
    pub fn dpi(&self, stage: u8) -> Result<Option<u16>> {
        let offset: usize = Self::DPI_STAGES + 2 * stage_index(stage)?;
        let steps: u16 = u16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]]);
        Ok((steps != 0).then(|| steps.saturating_mul(DPI_STEP)))
    }

    /// DPI of all five stages; `None` = disabled.
    pub fn dpi_stages(&self) -> [Option<u16>; DPI_STAGE_COUNT as usize] {
        std::array::from_fn(|index| {
            let offset: usize = Self::DPI_STAGES + 2 * index;
            let steps: u16 = u16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]]);
            (steps != 0).then(|| steps.saturating_mul(DPI_STEP))
        })
    }

    /// Sets the DPI of `stage` (1–5), or disables it with `None`.
    ///
    /// The DPI must be a multiple of 50 between 50 and 19000. The active stage
    /// cannot be disabled.
    pub fn set_dpi(&mut self, stage: u8, dpi: Option<u16>) -> Result<()> {
        let index: usize = stage_index(stage)?;
        let steps: u16 = match dpi {
            Some(dpi) => {
                if !(DPI_MIN..=DPI_MAX).contains(&dpi) || !dpi.is_multiple_of(DPI_STEP) {
                    return Err(Error::InvalidValue(format!(
                        "invalid DPI {dpi}: use a multiple of {DPI_STEP} from {DPI_MIN} to {DPI_MAX}"
                    )));
                }
                dpi / DPI_STEP
            }
            None if stage == self.active_stage() => {
                return Err(Error::InvalidValue(format!(
                    "DPI stage {stage} is the active stage; activate another stage before disabling it"
                )));
            }
            None => 0,
        };
        let offset: usize = Self::DPI_STAGES + 2 * index;
        self.bytes[offset..offset + 2].copy_from_slice(&steps.to_le_bytes());
        Ok(())
    }
}

fn stage_index(stage: u8) -> Result<usize> {
    if (1..=DPI_STAGE_COUNT).contains(&stage) {
        Ok(usize::from(stage - 1))
    } else {
        Err(Error::InvalidValue(format!(
            "DPI stage {stage} does not exist (1–{DPI_STAGE_COUNT})"
        )))
    }
}

/// Lighting effect stored on the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Off,
    /// Steady colours ("fully lit" in the official software).
    Static,
    Breathing,
    Heartbeat,
    Wave,
    Blinking,
    /// "AIMO intelligent lighting" in the official software.
    Aimo,
    /// Shows the battery level.
    Battery,
    /// An effect konectl does not know.
    Other(u8),
}

impl Effect {
    const KNOWN: [Self; 8] = [
        Self::Off,
        Self::Static,
        Self::Breathing,
        Self::Heartbeat,
        Self::Wave,
        Self::Blinking,
        Self::Aimo,
        Self::Battery,
    ];

    fn code(self) -> u8 {
        match self {
            Self::Off => 0x00,
            Self::Static => 0x01,
            Self::Breathing => 0x02,
            Self::Heartbeat => 0x05,
            Self::Wave => 0x06,
            Self::Blinking => 0x08,
            Self::Aimo => 0x09,
            Self::Battery => 0x0a,
            Self::Other(code) => code,
        }
    }

    fn from_code(code: u8) -> Self {
        Self::KNOWN
            .into_iter()
            .find(|effect| effect.code() == code)
            .unwrap_or(Self::Other(code))
    }

    fn name(self) -> Option<&'static str> {
        match self {
            Self::Off => Some("off"),
            Self::Static => Some("static"),
            Self::Breathing => Some("breathing"),
            Self::Heartbeat => Some("heartbeat"),
            Self::Wave => Some("wave"),
            Self::Blinking => Some("blinking"),
            Self::Aimo => Some("aimo"),
            Self::Battery => Some("battery"),
            Self::Other(_) => None,
        }
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.pad(name),
            None => write!(f, "unknown ({:#04x})", self.code()),
        }
    }
}

impl FromStr for Effect {
    type Err = Error;

    /// Parses an effect name: off, static, breathing, heartbeat, wave,
    /// blinking, aimo or battery.
    fn from_str(s: &str) -> Result<Self> {
        Self::KNOWN
            .into_iter()
            .find(|effect| effect.name() == Some(s))
            .ok_or_else(|| {
                let names: Vec<&str> = Self::KNOWN
                    .iter()
                    .filter_map(|effect| effect.name())
                    .collect();
                Error::InvalidValue(format!(
                    "invalid effect {s:?}: use one of {}",
                    names.join(", ")
                ))
            })
    }
}

/// Settings block 1: the lighting stored in one profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lighting {
    bytes: [u8; LIGHTING_LEN],
}

impl Lighting {
    const EFFECT: usize = 0;
    const BRIGHTNESS: usize = 3;
    const SPEED: usize = 4;
    const LEFT: usize = 5;
    const RIGHT: usize = 8;

    pub const fn from_bytes(bytes: [u8; LIGHTING_LEN]) -> Self {
        Self { bytes }
    }

    pub const fn bytes(&self) -> [u8; LIGHTING_LEN] {
        self.bytes
    }

    pub fn effect(&self) -> Effect {
        Effect::from_code(self.bytes[Self::EFFECT])
    }

    /// Sets the effect; [`Effect::Other`] is rejected. Like the official
    /// software, the battery effect also sets the brightness to 100 %.
    pub fn set_effect(&mut self, effect: Effect) -> Result<()> {
        if let Effect::Other(code) = effect {
            return Err(Error::InvalidValue(format!("unknown effect {code:#04x}")));
        }
        self.bytes[Self::EFFECT] = effect.code();
        if effect == Effect::Battery {
            self.bytes[Self::BRIGHTNESS] = BRIGHTNESS_MAX;
        }
        Ok(())
    }

    /// Brightness in percent.
    pub const fn brightness(&self) -> u8 {
        self.bytes[Self::BRIGHTNESS]
    }

    /// Sets the brightness, 0–100 %.
    pub fn set_brightness(&mut self, percent: u8) -> Result<()> {
        if percent > BRIGHTNESS_MAX {
            return Err(Error::InvalidValue(format!(
                "invalid brightness {percent}: use 0 to {BRIGHTNESS_MAX}"
            )));
        }
        self.bytes[Self::BRIGHTNESS] = percent;
        Ok(())
    }

    /// Effect speed, 1 (slow) to 11 (fast).
    pub const fn speed(&self) -> u8 {
        self.bytes[Self::SPEED]
    }

    /// Sets the effect speed, 1–11.
    pub fn set_speed(&mut self, speed: u8) -> Result<()> {
        if !(SPEED_MIN..=SPEED_MAX).contains(&speed) {
            return Err(Error::InvalidValue(format!(
                "invalid speed {speed}: use {SPEED_MIN} to {SPEED_MAX}"
            )));
        }
        self.bytes[Self::SPEED] = speed;
        Ok(())
    }

    pub fn left(&self) -> Rgb {
        self.colour(Self::LEFT)
    }

    pub fn right(&self) -> Rgb {
        self.colour(Self::RIGHT)
    }

    pub fn set_left(&mut self, colour: Rgb) {
        self.bytes[Self::LEFT..Self::LEFT + 3].copy_from_slice(&colour.to_bytes());
    }

    pub fn set_right(&mut self, colour: Rgb) {
        self.bytes[Self::RIGHT..Self::RIGHT + 3].copy_from_slice(&colour.to_bytes());
    }

    fn colour(&self, offset: usize) -> Rgb {
        Rgb::new(
            self.bytes[offset],
            self.bytes[offset + 1],
            self.bytes[offset + 2],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Block 0 of profile 1 as read at the start of the captures (600 DPI on
    /// stage 2, 1000 Hz, lift-off low, debounce 2 ms, byte 16 with bit 7 set).
    const SENSOR_START: [u8; SENSOR_LEN] = [
        0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
        0x00, 0x86, 0x0f, 0x00, 0x02,
    ];

    /// Block 1 of profile 1 at the start of the captures (wave).
    const LIGHTING_START: [u8; LIGHTING_LEN] = [
        0x06, 0x64, 0x64, 0x46, 0x04, 0x00, 0xff, 0x00, 0x00, 0xff, 0x64,
    ];

    fn sensor() -> Sensor {
        Sensor::from_bytes(SENSOR_START)
    }

    #[test]
    fn reads_sensor_fields() {
        let sensor = sensor();
        assert_eq!(sensor.profile(), 1);
        assert_eq!(sensor.polling_rate(), Some(PollingRate::Hz1000));
        assert!(!sensor.angle_snapping());
        assert_eq!(sensor.active_stage(), 2);
        assert_eq!(sensor.dpi_stages(), [None, Some(600), None, None, None]);
        assert_eq!(sensor.dpi(2).expect("stage 2"), Some(600));
        assert_eq!(sensor.lift_off(), LiftOff::Low);
        assert_eq!(sensor.debounce_ms(), 2);
    }

    #[test]
    fn dpi_change_matches_official_write() {
        // Capture 02: stage 2 set to 900; the software cleared bit 7 of byte 16.
        let mut sensor = sensor();
        sensor.set_dpi(2, Some(900)).expect("valid DPI");
        assert_eq!(
            sensor.write_bytes(),
            [
                0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x07, 0x00, 0x06, 0x0f, 0x00, 0x02
            ]
        );
    }

    #[test]
    fn dpi_above_255_steps_is_little_endian() {
        // Capture 03: 12800 DPI = 256 steps = 00 01.
        let mut sensor = sensor();
        sensor.set_dpi(2, Some(12_800)).expect("valid DPI");
        assert_eq!(sensor.bytes()[6..8], [0x00, 0x01]);
        assert_eq!(sensor.dpi(2).expect("stage 2"), Some(12_800));
    }

    #[test]
    fn stage_enable_disable_and_activate() {
        // Captures 05–07: stage 1 set to 400, then made active.
        let mut sensor = sensor();
        sensor.set_dpi(1, Some(400)).expect("valid DPI");
        assert_eq!(sensor.bytes()[4..6], [0x08, 0x00]);
        sensor.set_active_stage(1).expect("enabled stage");
        assert_eq!(sensor.bytes()[3], 0x00);
        sensor.set_active_stage(2).expect("enabled stage");
        sensor.set_dpi(1, None).expect("inactive stage");
        assert_eq!(sensor.bytes()[4..6], [0x00, 0x00]);
    }

    #[test]
    fn rejects_invalid_dpi_changes() {
        let mut sensor = sensor();
        for dpi in [0, 49, 925, 19_050] {
            assert!(sensor.set_dpi(2, Some(dpi)).is_err(), "{dpi} accepted");
        }
        assert!(sensor.set_dpi(0, Some(800)).is_err());
        assert!(sensor.set_dpi(6, Some(800)).is_err());
        assert!(sensor.set_dpi(2, None).is_err(), "active stage disabled");
        assert!(
            sensor.set_active_stage(3).is_err(),
            "disabled stage activated"
        );
        assert!(sensor.set_active_stage(6).is_err());
        assert_eq!(sensor.bytes(), SENSOR_START);
    }

    #[test]
    fn angle_snapping_and_debounce_match_official_writes() {
        // Capture 12: angle snapping on → byte 2 = 01.
        let mut sensor = sensor();
        sensor.set_angle_snapping(true);
        assert_eq!(sensor.bytes()[2], 0x01);
        assert!(sensor.angle_snapping());
        sensor.set_angle_snapping(false);
        assert_eq!(sensor.bytes()[2], 0x00);
        // Capture 13: debounce 2 → 3 ms → byte 19 = 03.
        sensor.set_debounce_ms(3).expect("valid debounce");
        assert_eq!(sensor.bytes()[19], 0x03);
        assert_eq!(sensor.debounce_ms(), 3);
        sensor.set_debounce_ms(0).expect("valid debounce");
        sensor
            .set_debounce_ms(DEBOUNCE_MAX_MS)
            .expect("valid debounce");
        assert!(sensor.set_debounce_ms(DEBOUNCE_MAX_MS + 1).is_err());
        assert_eq!(sensor.debounce_ms(), DEBOUNCE_MAX_MS);
    }

    #[test]
    fn polling_rate_codes() {
        // Captures 08–10.
        let mut sensor = sensor();
        for (rate, code) in [
            (PollingRate::Hz500, 1),
            (PollingRate::Hz125, 3),
            (PollingRate::Hz250, 2),
            (PollingRate::Hz1000, 0),
        ] {
            sensor.set_polling_rate(rate);
            assert_eq!(sensor.bytes()[1], code);
            assert_eq!(sensor.polling_rate(), Some(rate));
        }
        assert_eq!(
            "500".parse::<PollingRate>().expect("valid"),
            PollingRate::Hz500
        );
        assert!("600".parse::<PollingRate>().is_err());
    }

    #[test]
    fn lift_off_codes() {
        // Capture 11.
        let mut sensor = sensor();
        sensor.set_lift_off(LiftOff::VeryLow).expect("known value");
        assert_eq!(sensor.bytes()[14], 0x06);
        assert_eq!(sensor.lift_off(), LiftOff::VeryLow);
        assert!(sensor.set_lift_off(LiftOff::Other(0x09)).is_err());
        assert_eq!(LiftOff::from_code(0x09), LiftOff::Other(0x09));
        assert_eq!(
            "very-low".parse::<LiftOff>().expect("valid"),
            LiftOff::VeryLow
        );
    }

    #[test]
    fn reads_lighting_fields() {
        let lighting = Lighting::from_bytes(LIGHTING_START);
        assert_eq!(lighting.effect(), Effect::Wave);
        assert_eq!(lighting.brightness(), 70);
        assert_eq!(lighting.speed(), 4);
        assert_eq!(lighting.left(), Rgb::new(0x00, 0xff, 0x00));
        assert_eq!(lighting.right(), Rgb::new(0x00, 0xff, 0x64));
    }

    #[test]
    fn lighting_changes_match_official_writes() {
        // Captures 14–16: fully lit, left red (f4 00 00), right #123456.
        let mut lighting = Lighting::from_bytes(LIGHTING_START);
        lighting.set_effect(Effect::Static).expect("known effect");
        lighting.set_left(Rgb::new(0xf4, 0x00, 0x00));
        lighting.set_right(Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(
            lighting.bytes(),
            [
                0x01, 0x64, 0x64, 0x46, 0x04, 0xf4, 0x00, 0x00, 0x12, 0x34, 0x56
            ]
        );
    }

    #[test]
    fn lighting_validation() {
        let mut lighting = Lighting::from_bytes(LIGHTING_START);
        assert!(lighting.set_brightness(101).is_err());
        assert!(lighting.set_speed(0).is_err());
        assert!(lighting.set_speed(12).is_err());
        assert!(lighting.set_effect(Effect::Other(0x03)).is_err());
        assert_eq!(lighting.bytes(), LIGHTING_START);
        lighting.set_speed(11).expect("valid speed");
        lighting.set_brightness(0).expect("valid brightness");
        assert_eq!(lighting.bytes()[3..5], [0, 11]);
    }

    #[test]
    fn battery_effect_sets_full_brightness() {
        // Capture 28.
        let mut lighting = Lighting::from_bytes(LIGHTING_START);
        lighting.set_effect(Effect::Battery).expect("known effect");
        assert_eq!(lighting.bytes()[..4], [0x0a, 0x64, 0x64, 0x64]);
    }

    #[test]
    fn effect_names_round_trip() {
        for effect in Effect::KNOWN {
            let name: String = effect.to_string();
            assert_eq!(name.parse::<Effect>().expect("known name"), effect);
            assert_eq!(Effect::from_code(effect.code()), effect);
        }
        assert!("rainbow".parse::<Effect>().is_err());
        assert_eq!(Effect::from_code(0x03).to_string(), "unknown (0x03)");
    }

    #[test]
    fn firmware_display_and_parse() {
        let firmware = Firmware {
            major: 4,
            minor: 19,
        };
        assert_eq!(firmware.to_string(), "4.19");
        assert_eq!("4.19".parse::<Firmware>().expect("valid"), firmware);
        assert!("4".parse::<Firmware>().is_err());
    }
}
