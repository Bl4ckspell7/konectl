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
}

/// Result type of konectl.
pub type Result<T> = std::result::Result<T, Error>;
