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
//! mouse.set_leds(Rgb::new(255, 0, 0), Rgb::new(0, 0, 255))?;
//! # Ok::<(), konectl::Error>(())
//! ```

mod color;
mod device;
mod error;
pub mod protocol;
mod transport;

pub use color::{ParseRgbError, Rgb};
pub use device::{ConnectionMode, DeviceInfo, KoneProAir, Manager};
pub use error::{Error, Result};
pub use protocol::{Battery, Report, Signal};
pub use transport::{HidTransport, Transport};
