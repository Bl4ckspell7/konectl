//! The mouse as the worker sees it: the real one through hidraw, or the demo
//! (`demo.rs`). The worker only talks to these traits, so its tests and the
//! demo mode need no hardware.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use konectl::{
    Backup, ConnectionMode, Firmware, HidTransport, KoneProAir, Manager, ProfileSettings,
    RemovedProfile, Rgb, Sensor,
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
}

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
}
