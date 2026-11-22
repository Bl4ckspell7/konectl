//! Unofficial library to configure the ROCCAT Kone Pro Air mouse on Linux.
//!
//! Not affiliated with ROCCAT or Turtle Beach.
//!
//! ```no_run
//! use konectl::{Manager, Rgb};
//!
//! let manager = Manager::new()?;
//! let mut mouse = manager.open_first()?;
//! println!("battery: {:?}%", mouse.battery()?.percent());
//!
//! // Saved settings: read, change, write back (checked by reading again).
//! let mut settings = mouse.settings(None)?; // active profile
//! settings.sensor.set_dpi(2, Some(900))?;
//! settings.lighting.set_left(Rgb::new(255, 0, 0));
//! mouse.write_settings(&settings)?;
//! # Ok::<(), konectl::Error>(())
//! ```

mod backup;
mod color;
mod device;
mod error;
pub mod protocol;
pub mod settings;
mod transport;

pub use backup::{Backup, ProfileBackup};
pub use color::{ParseRgbError, Rgb};
pub use device::{ConnectionMode, DeviceInfo, KoneProAir, Manager};
pub use error::{Error, Result};
pub use protocol::{Battery, Report, Signal};
pub use settings::{
    Effect, Firmware, Info, LedTimeout, LiftOff, Lighting, PollingRate, ProfileSettings, Sensor,
};
pub use transport::{HidTransport, Transport};
