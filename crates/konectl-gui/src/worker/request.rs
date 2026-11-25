//! Messages between the user interface and the worker.

use std::path::PathBuf;

use konectl::pairing::PairingStep;
use konectl::{Backup, ButtonAction, ConnectionMode, Layer, MacroMode, Rgb};

use super::device::{Firmwares, Power};
use crate::command_line::CommandLines;
use crate::edit::Edit;

/// What the user interface asks the worker to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Read everything from the mouse again.
    Reload,
    /// Look for the mouse, or wake it, now instead of at the next poll.
    Retry,
    /// Install the udev rule through pkexec, then connect.
    AllowAccess,
    /// Change a setting of a profile. Edits that follow quickly are written
    /// together.
    Edit {
        profile: u8,
        edit: Edit,
    },
    ActivateProfile(u8),
    AddProfile {
        copy_from: u8,
    },
    RemoveProfile(u8),
    ProfileColour {
        profile: u8,
        colour: Option<Rgb>,
    },
    /// Give a button a function (not a macro).
    SetButton {
        profile: u8,
        button: u8,
        layer: Layer,
        action: ButtonAction,
    },
    /// Give a button a macro, with its keys as typed (`Macro` syntax).
    SetMacro {
        profile: u8,
        button: u8,
        layer: Layer,
        keys: String,
        mode: MacroMode,
    },
    /// Go back to the state before the last change.
    Undo,
    /// Energy saving on or off (one setting for all profiles).
    EnergySaving(bool),
    /// The debounce time in every profile.
    Debounce(u8),
    /// Measure the lift-off distance (the user moves the mouse meanwhile),
    /// then use it in every profile.
    CalibrateLiftOff,
    /// Back to the factory settings.
    Reset,
    /// Pair the mouse with the dongle; the user follows the steps the worker
    /// reports. Cancelled with [`super::Requests::cancel`].
    Pair,
    /// Save all profiles to a file.
    SaveBackup(PathBuf),
    /// Write the profiles from a file back to the mouse.
    RestoreBackup(PathBuf),
    /// Write back the state before konectl's first change (`original.txt`).
    RestoreOriginal,
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
    /// A change was written.
    Changed(Box<Change>),
    /// A change failed; the mouse is as in this snapshot (as far as known).
    Reverted(Box<Snapshot>),
    /// Whether there is a change to undo.
    UndoAvailable(bool),
    /// New battery and signal values.
    Status(Status),
    /// A request is being handled (what is done), or `None` when it is over.
    Busy(Option<&'static str>),
    /// A request failed.
    Error(String),
    /// What a request did when there is no change to show with it (e.g.
    /// pairing was cancelled).
    Message(String),
    /// Another program changed the mouse (it saved a safety backup).
    ChangedElsewhere,
    /// The lift-off calibration runs: seconds left, or `None` once it is
    /// over.
    Calibration(Option<u64>),
    /// Pairing waits for this step, or `None` once it is over.
    Pairing(Option<PairingStep>),
}

/// A written change.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub snapshot: Snapshot,
    /// The same change with the CLI.
    pub commands: CommandLines,
    /// What else happened, e.g. profiles that moved.
    pub note: Option<String>,
    /// The profile to show next, e.g. one that was just added.
    pub select: Option<u8>,
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
