//! Error type of the library.

/// Errors returned by konectl.
#[derive(Debug, thiserror::Error)]
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
}

/// Result type of konectl.
pub type Result<T> = std::result::Result<T, Error>;
