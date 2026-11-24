//! Messages between the user interface and the worker.

use konectl::{Backup, ConnectionMode};

use super::device::{Firmwares, Power};

/// What the user interface asks the worker to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Read everything from the mouse again.
    Reload,
    /// Look for the mouse, or wake it, now instead of at the next poll.
    Retry,
    /// Install the udev rule through pkexec, then connect.
    AllowAccess,
    /// Finish the current request and end the worker.
    Shutdown,
}

/// What the worker tells the user interface.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// No mouse found; `problem` says why if it is not simply unplugged.
    Searching { problem: Option<String> },
    /// A mouse is connected, but this user may not open it.
    NoAccess,
    /// The dongle is connected, but the mouse does not answer.
    Asleep,
    /// Connected; everything read from the mouse.
    Connected(Box<Snapshot>),
    /// New battery and signal values.
    Status(Status),
    /// A request is being handled (what is done), or `None` when it is over.
    Busy(Option<&'static str>),
    /// A request failed.
    Error(String),
    /// Another program changed the mouse (it saved a safety backup).
    ChangedElsewhere,
}

/// Everything read from the mouse when it connects or is reloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub mode: ConnectionMode,
    pub firmwares: Firmwares,
    pub backup: Backup,
    /// One setting for all profiles, not part of backups; `None` if it
    /// could not be read.
    pub energy_saving: Option<bool>,
    pub status: Status,
}

/// Battery and signal; values that could not be read are `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub power: Option<Power>,
    /// Signal quality in percent; `None` over the USB cable.
    pub signal: Option<u8>,
}
