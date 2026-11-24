//! Pairing the mouse with the dongle.
//!
//! [`KoneProAir::start_pairing`](crate::KoneProAir::start_pairing) puts the
//! dongle into pairing mode. The user then switches the mouse on (without the
//! cable), holds its profile button for three seconds and plugs the dongle in
//! again; [`finish_pairing`] waits for that and for the mouse to answer.

use std::ops::ControlFlow;
use std::thread;
use std::time::{Duration, Instant};

use crate::device::{ConnectionMode, KoneProAir, Manager};
use crate::error::{Error, Result};
use crate::transport::HidTransport;

/// How long [`finish_pairing`] waits for the dongle to be unplugged, and
/// again for it to be plugged in.
pub const REPLUG_WAIT: Duration = Duration::from_secs(180);
/// How long it then waits for the mouse to answer through the dongle.
pub const CONNECT_WAIT: Duration = Duration::from_secs(15);

/// What [`finish_pairing`] is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingStep {
    /// The dongle to be unplugged (after the mouse's pairing light effect).
    Unplug,
    /// The dongle to be plugged in again.
    Replug,
    /// The mouse to answer through the dongle.
    Connect,
}

/// Waits until `dongle`, in pairing mode, was unplugged and plugged in again,
/// then until the mouse answers through it, and returns the mouse.
///
/// `on_step` is called before every check with the current step: about
/// every 200 ms while waiting for the dongle, and before each attempt to
/// reach the mouse (an attempt can take a few seconds). Returning
/// [`ControlFlow::Break`] stops waiting with [`Error::Cancelled`]. Fails with
/// [`Error::InvalidValue`] when `dongle` is connected by cable, with
/// [`Error::NotReplugged`] when the dongle is not unplugged or plugged in
/// again within [`REPLUG_WAIT`] each, or with the last error when the mouse
/// does not answer within [`CONNECT_WAIT`].
pub fn finish_pairing(
    manager: &mut Manager,
    mut dongle: KoneProAir<HidTransport>,
    mut on_step: impl FnMut(PairingStep) -> ControlFlow<()>,
) -> Result<KoneProAir<HidTransport>> {
    let mut step = |current: PairingStep| -> Result<()> {
        match on_step(current) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(()) => Err(Error::Cancelled),
        }
    };

    if dongle.mode() != ConnectionMode::Wireless {
        return Err(Error::InvalidValue(
            "pairing works through the dongle, not over the USB cable".to_owned(),
        ));
    }

    // The opened dongle stops working once it is unplugged, however quickly
    // it comes back.
    let deadline: Instant = Instant::now() + REPLUG_WAIT;
    while dongle.is_present() {
        step(PairingStep::Unplug)?;
        if Instant::now() >= deadline {
            return Err(Error::NotReplugged);
        }
        thread::sleep(Duration::from_millis(200));
    }
    drop(dongle);

    let deadline: Instant = Instant::now() + REPLUG_WAIT;
    loop {
        step(PairingStep::Replug)?;
        manager.refresh()?;
        if manager
            .devices()
            .iter()
            .any(|device| device.mode == ConnectionMode::Wireless)
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(Error::NotReplugged);
        }
        thread::sleep(Duration::from_millis(200));
    }

    // The mouse needs a moment to connect, and the new device node to get its
    // permissions.
    let deadline: Instant = Instant::now() + CONNECT_WAIT;
    loop {
        step(PairingStep::Connect)?;
        let answered = manager
            .open_mode(ConnectionMode::Wireless)
            .and_then(|mut mouse| mouse.info().map(|_| mouse));
        match answered {
            Ok(mouse) => return Ok(mouse),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => thread::sleep(Duration::from_millis(500)),
        }
    }
}
