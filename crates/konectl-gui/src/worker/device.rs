//! The mouse as the worker sees it: the real one through hidraw, or the demo
//! (`demo.rs`). The worker only talks to these traits, so its tests and the
//! demo mode need no hardware.

use std::any::Any;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use konectl::pairing::{self, PairingStep};
use konectl::{
    Backup, Buttons, ConnectionMode, Firmware, HidTransport, KoneProAir, Layer, LiftOff, Macro,
    Manager, ProfileSettings, RemovedProfile, Rgb, Sensor,
};

use crate::access::{self, Outcome};

/// Firmware versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Firmwares {
    pub mouse: Firmware,
    /// `None` over the USB cable.
    pub dongle: Option<Firmware>,
}

/// Battery state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Power {
    /// `None` if the mouse reports a value that is not a percentage.
    pub percent: Option<u8>,
    pub charging: bool,
    /// Powered over the USB cable.
    pub cable_power: bool,
}

/// One opened mouse.
pub trait Device {
    fn mode(&self) -> ConnectionMode;
    /// `false` once the dongle or cable was unplugged. Sends nothing.
    fn is_present(&mut self) -> bool;
    /// How many functions the mouse reported by itself since the last call
    /// (e.g. its profile button switched profiles). Sends nothing; fails
    /// once the dongle or cable was unplugged.
    fn function_events(&mut self) -> konectl::Result<usize>;
    /// Fails with [`konectl::Error::NotReachable`] while the mouse sleeps.
    fn firmwares(&mut self) -> konectl::Result<Firmwares>;
    fn power(&mut self) -> konectl::Result<Power>;
    /// Signal quality in percent; `None` over the USB cable.
    fn signal(&mut self) -> konectl::Result<Option<u8>>;
    /// All profiles in use.
    fn backup(&mut self) -> konectl::Result<Backup>;
    fn energy_saving(&mut self) -> konectl::Result<bool>;

    /// Block 0 of the active profile (one query): which profile is active,
    /// and its active DPI stage.
    fn active_sensor(&mut self) -> konectl::Result<Sensor>;
    /// The settings of `profile`, read from the mouse.
    fn settings(&mut self, profile: u8) -> konectl::Result<ProfileSettings>;
    /// Writes what changed from `before` to `after` (one profile); `false`
    /// when nothing did.
    fn write_changes(
        &mut self,
        before: &ProfileSettings,
        after: &ProfileSettings,
    ) -> konectl::Result<bool>;
    fn set_active_profile(&mut self, profile: u8) -> konectl::Result<()>;
    /// Adds a copy of `copy_from` and returns its number.
    fn add_profile(&mut self, copy_from: u8) -> konectl::Result<u8>;
    fn remove_profile(&mut self, profile: u8) -> konectl::Result<RemovedProfile>;
    fn set_profile_color(&mut self, profile: u8, colour: Option<Rgb>) -> konectl::Result<()>;
    /// Writes a whole state back (profiles, settings, buttons, active profile).
    fn restore(&mut self, backup: &Backup) -> konectl::Result<()>;
    /// The button assignments of `profile`, read from the mouse.
    fn buttons(&mut self, profile: u8) -> konectl::Result<Buttons>;
    fn write_buttons(&mut self, buttons: &Buttons) -> konectl::Result<()>;
    /// Writes `buttons` and the macro that `button` plays in `layer`.
    fn write_macro(
        &mut self,
        buttons: &Buttons,
        button: u8,
        layer: Layer,
        macro_: &Macro,
    ) -> konectl::Result<()>;

    /// One setting for all profiles; checked by reading it back.
    fn set_energy_saving(&mut self, enabled: bool) -> konectl::Result<()>;
    /// Sets the debounce time in every profile.
    fn set_debounce(&mut self, ms: u8) -> konectl::Result<()>;
    /// Starts measuring the lift-off distance; the mouse is then moved for
    /// [`konectl::LIFT_OFF_CALIBRATION_SECONDS`].
    fn start_lift_off_calibration(&mut self) -> konectl::Result<()>;
    fn finish_lift_off_calibration(&mut self) -> konectl::Result<()>;
    /// Sets the lift-off distance in every profile.
    fn set_lift_off_all(&mut self, lift_off: LiftOff) -> konectl::Result<()>;
    /// Back to the factory settings (energy saving on).
    fn reset(&mut self) -> konectl::Result<()>;
    /// Puts the dongle into pairing mode (see [`Connector::finish_pairing`]).
    fn start_pairing(&mut self) -> konectl::Result<()>;
    /// Whether `backup` fits this mouse (firmware, profile numbers).
    fn check_backup(&mut self, backup: &Backup) -> konectl::Result<()>;

    /// For [`Connector::finish_pairing`], which needs the device itself.
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

/// Called with the step pairing waits for; `Break` cancels it.
pub type OnPairingStep<'a> = dyn FnMut(PairingStep) -> ControlFlow<()> + 'a;

/// Why no mouse could be opened.
#[derive(Debug)]
pub enum ConnectError {
    /// Neither the dongle nor the cable is connected.
    NotFound,
    /// A mouse is connected, but this user may not open it (no udev rule).
    NoAccess,
    Failed(konectl::Error),
}

/// Finds and opens the mouse.
pub trait Connector {
    fn connect(&mut self) -> Result<Box<dyn Device>, ConnectError>;

    /// Whether the mouse is connected by USB cable (it is used before the
    /// dongle). Sends nothing.
    fn wired_connected(&mut self) -> bool;

    /// Gives this user access to the mouse (see [`access::allow`]).
    fn allow_access(&mut self) -> Result<Outcome, String>;

    /// After [`Device::start_pairing`]: waits until the dongle was unplugged
    /// and plugged in again and the mouse answers through it (see
    /// [`konectl::pairing::finish_pairing`]), and returns the new device.
    /// `dongle` is closed either way.
    fn finish_pairing(
        &mut self,
        dongle: Box<dyn Device>,
        on_step: &mut OnPairingStep<'_>,
    ) -> konectl::Result<Box<dyn Device>>;
}

impl Device for KoneProAir<HidTransport> {
    fn mode(&self) -> ConnectionMode {
        KoneProAir::mode(self)
    }

    fn is_present(&mut self) -> bool {
        KoneProAir::is_present(self)
    }

    fn function_events(&mut self) -> konectl::Result<usize> {
        Ok(self.take_function_events()?.len())
    }

    fn firmwares(&mut self) -> konectl::Result<Firmwares> {
        let info = self.info()?;
        Ok(Firmwares {
            mouse: info.firmware,
            dongle: info.dongle_firmware,
        })
    }

    fn power(&mut self) -> konectl::Result<Power> {
        let battery = self.battery()?;
        Ok(Power {
            percent: battery.percent(),
            charging: battery.charging,
            cable_power: battery.cable_power,
        })
    }

    fn signal(&mut self) -> konectl::Result<Option<u8>> {
        match KoneProAir::mode(self) {
            ConnectionMode::Wireless => {
                let signal = KoneProAir::signal(self)?;
                Ok(signal.radio_enabled().then(|| signal.percent()))
            }
            ConnectionMode::Wired => Ok(None),
        }
    }

    fn backup(&mut self) -> konectl::Result<Backup> {
        KoneProAir::backup(self)
    }

    fn energy_saving(&mut self) -> konectl::Result<bool> {
        KoneProAir::energy_saving(self)
    }

    fn active_sensor(&mut self) -> konectl::Result<Sensor> {
        Ok(self.info()?.sensor)
    }

    fn settings(&mut self, profile: u8) -> konectl::Result<ProfileSettings> {
        KoneProAir::settings(self, Some(profile))
    }

    fn write_changes(
        &mut self,
        before: &ProfileSettings,
        after: &ProfileSettings,
    ) -> konectl::Result<bool> {
        KoneProAir::write_changes(self, before, after)
    }

    fn set_active_profile(&mut self, profile: u8) -> konectl::Result<()> {
        KoneProAir::set_active_profile(self, profile)
    }

    fn add_profile(&mut self, copy_from: u8) -> konectl::Result<u8> {
        KoneProAir::add_profile(self, copy_from)
    }

    fn remove_profile(&mut self, profile: u8) -> konectl::Result<RemovedProfile> {
        KoneProAir::remove_profile(self, profile)
    }

    fn set_profile_color(&mut self, profile: u8, colour: Option<Rgb>) -> konectl::Result<()> {
        KoneProAir::set_profile_color(self, profile, colour)
    }

    fn restore(&mut self, backup: &Backup) -> konectl::Result<()> {
        KoneProAir::restore(self, backup)
    }

    fn buttons(&mut self, profile: u8) -> konectl::Result<Buttons> {
        KoneProAir::buttons(self, Some(profile))
    }

    fn write_buttons(&mut self, buttons: &Buttons) -> konectl::Result<()> {
        KoneProAir::write_buttons(self, buttons)
    }

    fn write_macro(
        &mut self,
        buttons: &Buttons,
        button: u8,
        layer: Layer,
        macro_: &Macro,
    ) -> konectl::Result<()> {
        KoneProAir::write_macro(self, buttons, button, layer, macro_)
    }

    fn set_energy_saving(&mut self, enabled: bool) -> konectl::Result<()> {
        KoneProAir::set_energy_saving(self, enabled)
    }

    fn set_debounce(&mut self, ms: u8) -> konectl::Result<()> {
        KoneProAir::set_debounce(self, ms)
    }

    fn start_lift_off_calibration(&mut self) -> konectl::Result<()> {
        KoneProAir::start_lift_off_calibration(self)
    }

    fn finish_lift_off_calibration(&mut self) -> konectl::Result<()> {
        KoneProAir::finish_lift_off_calibration(self)
    }

    fn set_lift_off_all(&mut self, lift_off: LiftOff) -> konectl::Result<()> {
        KoneProAir::set_lift_off_all(self, lift_off).map(|_| ())
    }

    fn reset(&mut self) -> konectl::Result<()> {
        KoneProAir::reset(self)
    }

    fn start_pairing(&mut self) -> konectl::Result<()> {
        KoneProAir::start_pairing(self)
    }

    fn check_backup(&mut self, backup: &Backup) -> konectl::Result<()> {
        KoneProAir::check_backup(self, backup)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// The real mouse, through hidapi.
#[derive(Default)]
pub struct HidConnector {
    /// Created on the first attempt; hidapi keeps its device list in it.
    manager: Option<Manager>,
}

impl HidConnector {
    /// The manager with a fresh device list.
    fn refreshed(&mut self) -> konectl::Result<&mut Manager> {
        let manager: Manager = match self.manager.take() {
            Some(mut manager) => {
                manager.refresh()?;
                manager
            }
            None => Manager::new()?,
        };
        Ok(self.manager.insert(manager))
    }
}

impl Connector for HidConnector {
    fn connect(&mut self) -> Result<Box<dyn Device>, ConnectError> {
        let manager: &mut Manager = self.refreshed().map_err(ConnectError::Failed)?;
        match manager.open_first() {
            Ok(mouse) => Ok(Box::new(mouse)),
            Err(konectl::Error::NotFound) => Err(ConnectError::NotFound),
            // Opening fails the same way for missing permissions and for other
            // HID problems; look at the permissions of the device nodes.
            Err(error @ konectl::Error::Hid(_)) => {
                let denied: bool = manager.devices().iter().any(|device| {
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(Path::new(OsStr::from_bytes(device.path.to_bytes())))
                        .is_err_and(|error| error.kind() == ErrorKind::PermissionDenied)
                });
                Err(if denied {
                    ConnectError::NoAccess
                } else {
                    ConnectError::Failed(error)
                })
            }
            Err(error) => Err(ConnectError::Failed(error)),
        }
    }

    fn wired_connected(&mut self) -> bool {
        self.refreshed().is_ok_and(|manager| {
            manager
                .devices()
                .iter()
                .any(|device| device.mode == ConnectionMode::Wired)
        })
    }

    fn allow_access(&mut self) -> Result<Outcome, String> {
        access::allow()
    }

    fn finish_pairing(
        &mut self,
        dongle: Box<dyn Device>,
        on_step: &mut OnPairingStep<'_>,
    ) -> konectl::Result<Box<dyn Device>> {
        let dongle: Box<KoneProAir<HidTransport>> = dongle
            .into_any()
            .downcast()
            .map_err(|_| konectl::Error::InvalidValue("not a mouse opened here".to_owned()))?;
        let manager: &mut Manager = self.refreshed()?;
        let mouse = pairing::finish_pairing(manager, *dongle, on_step)?;
        Ok(Box::new(mouse))
    }
}
