//! Error type of the library.

/// Errors returned by konectl.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No Kone Pro Air (dongle or cable) is connected.
    #[error("no ROCCAT Kone Pro Air found (is the dongle or the cable connected?)")]
    NotFound,

    /// The HID layer failed, e.g. missing permissions on `/dev/hidraw*`.
    #[error(transparent)]
    Hid(#[from] hidapi::HidError),

    /// The HID layer accepted only part of a report.
    #[error("short write to the mouse: {written} of {expected} bytes")]
    ShortWrite { written: usize, expected: usize },

    /// The mouse did not answer a request in time.
    #[error("the mouse did not answer the {0} in time")]
    Timeout(&'static str),

    /// The mouse answered a request with an error status, e.g. because the
    /// mouse is switched off while the dongle is connected.
    #[error("the mouse rejected the {command} (error code {code:#04x})")]
    Rejected { command: &'static str, code: u8 },

    /// A setting read back differently from what was written.
    #[error("the {0} did not read back as written")]
    VerifyFailed(&'static str),

    /// A setting value is out of range or not supported.
    #[error("{0}")]
    InvalidValue(String),

    /// The profile slot is not in use on the mouse.
    #[error("profile {0} is not in use")]
    UnusedProfile(u8),

    /// A settings backup could not be read or does not fit the mouse.
    #[error("invalid backup: {0}")]
    InvalidBackup(String),

    /// The dongle answered, but the mouse did not: it is asleep, switched
    /// off or out of range.
    #[error(
        "the mouse does not answer (asleep, switched off or out of range?): move it and try again"
    )]
    NotReachable,

    /// Reading or writing a file failed, or a program could not be started.
    #[error("{context}")]
    Io {
        /// What konectl was doing, e.g. "could not write /path/to/file".
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// A program konectl started (e.g. to install the udev rule) failed.
    #[error("{command} failed ({status})")]
    CommandFailed {
        command: String,
        status: std::process::ExitStatus,
    },

    /// The dongle was not unplugged and plugged in again in time during
    /// pairing; it stays in pairing mode until that happens.
    #[error("the dongle was not plugged in again in time; it stays in pairing mode until it is")]
    NotReplugged,

    /// The caller stopped a long-running operation.
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// An [`Error::Io`] with a description of what failed.
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Result type of konectl.
pub type Result<T> = std::result::Result<T, Error>;
