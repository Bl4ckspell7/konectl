//! Demo mode (`KONECTL_GUI_DEMO=1`): a mouse that exists only in memory, made
//! from `fixtures/demo-backup.txt`. Nothing is sent to a real mouse. The tests
//! of the worker use it too, and change its state to unplug the dongle or put
//! the mouse to sleep.
//!
//! `KONECTL_GUI_DEMO=no-access`, `asleep` or `unplugged` start the demo in
//! that state, to show those screens.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use konectl::{Backup, ConnectionMode};

use super::device::{ConnectError, Connector, Device, Firmwares, Power};
use crate::access::Outcome;

/// Set to `1` (or a start state) to start in demo mode.
pub const DEMO_VAR: &str = "KONECTL_GUI_DEMO";

const FIXTURE: &str = include_str!("../../fixtures/demo-backup.txt");

/// The demo state the environment asks for, if any.
pub fn requested() -> Option<DemoState> {
    let value = std::env::var(DEMO_VAR).ok()?;
    let mut state = DemoState::new();
    match value.as_str() {
        "1" => {}
        "no-access" => state.accessible = false,
        "asleep" => state.awake = false,
        "unplugged" => state.plugged_in = false,
        _ => return None,
    }
    Some(state)
}

/// The demo mouse and how it is connected.
#[derive(Debug, Clone)]
pub struct DemoState {
    /// The dongle is plugged in.
    pub plugged_in: bool,
    /// Counts unplugs of the dongle: a device opened before stays gone.
    pub plugs: u32,
    /// The USB cable is plugged in; it is used before the dongle.
    pub cable: bool,
    /// This user may open the mouse.
    pub accessible: bool,
    /// "Allow access" gives access (otherwise the rule does not help).
    pub allow_works: bool,
    /// The mouse answers (over the dongle it may sleep).
    pub awake: bool,
    pub backup: Backup,
    pub energy_saving: bool,
    pub power: Power,
    pub signal: u8,
}

impl DemoState {
    pub fn new() -> Self {
        let backup: Backup = Backup::from_text(FIXTURE).expect("the demo backup is valid");
        Self {
            plugged_in: true,
            plugs: 0,
            cable: false,
            accessible: true,
            allow_works: true,
            awake: true,
            backup,
            energy_saving: true,
            power: Power {
                percent: Some(87),
                charging: false,
                cable_power: false,
            },
            signal: 92,
        }
    }

    /// Unplugs (`false`) or plugs in (`true`) the dongle.
    #[cfg(test)]
    pub fn set_plugged_in(&mut self, plugged_in: bool) {
        if self.plugged_in && !plugged_in {
            self.plugs += 1;
        }
        self.plugged_in = plugged_in;
    }
}

/// The state, shared by the connector, its devices and the tests.
pub type SharedDemo = Arc<Mutex<DemoState>>;

pub fn shared(state: DemoState) -> SharedDemo {
    Arc::new(Mutex::new(state))
}

fn lock(state: &SharedDemo) -> MutexGuard<'_, DemoState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct DemoConnector {
    state: SharedDemo,
}

impl DemoConnector {
    pub fn new(state: SharedDemo) -> Self {
        Self { state }
    }
}

impl Connector for DemoConnector {
    fn connect(&mut self) -> Result<Box<dyn Device>, ConnectError> {
        let state = lock(&self.state);
        let mode: ConnectionMode = if state.cable {
            ConnectionMode::Wired
        } else if state.plugged_in {
            ConnectionMode::Wireless
        } else {
            return Err(ConnectError::NotFound);
        };
        if !state.accessible {
            return Err(ConnectError::NoAccess);
        }
        Ok(Box::new(DemoDevice {
            state: Arc::clone(&self.state),
            mode,
            plugs: state.plugs,
        }))
    }

    fn wired_connected(&mut self) -> bool {
        lock(&self.state).cable
    }

    fn allow_access(&mut self) -> Result<Outcome, String> {
        let mut state = lock(&self.state);
        if state.allow_works {
            state.accessible = true;
        }
        Ok(Outcome::Allowed)
    }
}

struct DemoDevice {
    state: SharedDemo,
    mode: ConnectionMode,
    /// [`DemoState::plugs`] when it was opened.
    plugs: u32,
}

impl DemoDevice {
    fn present(&self, state: &DemoState) -> bool {
        match self.mode {
            ConnectionMode::Wired => state.cable,
            ConnectionMode::Wireless => state.plugged_in && state.plugs == self.plugs,
        }
    }

    /// The state, if the mouse answers like the real one would. While it
    /// sleeps, the dongle answers the info query with "no mouse"
    /// (`NotReachable`) and other queries with error `0d`.
    fn answering(&self, query: &'static str) -> konectl::Result<MutexGuard<'_, DemoState>> {
        let state = lock(&self.state);
        if !self.present(&state) {
            return Err(konectl::Error::NotFound);
        }
        if self.mode == ConnectionMode::Wireless && !state.awake {
            return Err(match query {
                "info query" => konectl::Error::NotReachable,
                command => konectl::Error::Rejected {
                    command,
                    code: 0x0d,
                },
            });
        }
        Ok(state)
    }
}

impl Device for DemoDevice {
    fn mode(&self) -> ConnectionMode {
        self.mode
    }

    fn is_present(&mut self) -> bool {
        self.present(&lock(&self.state))
    }

    fn firmwares(&mut self) -> konectl::Result<Firmwares> {
        let state = self.answering("info query")?;
        let mouse = state
            .backup
            .firmware
            .ok_or_else(|| konectl::Error::InvalidValue("no firmware in the demo".to_owned()))?;
        Ok(Firmwares {
            mouse,
            dongle: match self.mode {
                ConnectionMode::Wireless => state.backup.dongle_firmware,
                ConnectionMode::Wired => None,
            },
        })
    }

    fn power(&mut self) -> konectl::Result<Power> {
        Ok(self.answering("battery query")?.power)
    }

    fn signal(&mut self) -> konectl::Result<Option<u8>> {
        let state = self.answering("signal query")?;
        Ok(match self.mode {
            ConnectionMode::Wireless => Some(state.signal),
            ConnectionMode::Wired => None,
        })
    }

    fn backup(&mut self) -> konectl::Result<Backup> {
        Ok(self.answering("info query")?.backup.clone())
    }

    fn energy_saving(&mut self) -> konectl::Result<bool> {
        Ok(self.answering("energy saving query")?.energy_saving)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_has_three_profiles_with_buttons() {
        let backup: Backup = DemoState::new().backup;
        assert_eq!(backup.profiles.len(), 3);
        assert!(
            backup
                .profiles
                .iter()
                .all(|profile| profile.buttons.is_some())
        );
        assert_eq!(backup.active_profile, 1);
    }

    #[test]
    fn devices_opened_before_an_unplug_stay_gone() {
        let state: SharedDemo = shared(DemoState::new());
        let mut connector = DemoConnector::new(Arc::clone(&state));
        let mut first = connector.connect().expect("plugged in");
        assert!(first.is_present());
        lock(&state).set_plugged_in(false);
        assert!(matches!(connector.connect(), Err(ConnectError::NotFound)));
        lock(&state).set_plugged_in(true);
        assert!(!first.is_present());
        assert!(connector.connect().expect("plugged in again").is_present());
    }

    #[test]
    fn a_sleeping_mouse_does_not_answer_through_the_dongle() {
        let state: SharedDemo = shared(DemoState::new());
        let mut device = DemoConnector::new(Arc::clone(&state))
            .connect()
            .expect("plugged in");
        lock(&state).awake = false;
        assert!(device.is_present());
        assert!(matches!(
            device.firmwares(),
            Err(konectl::Error::NotReachable)
        ));
        lock(&state).awake = true;
        assert_eq!(device.signal().expect("signal"), Some(92));
    }
}
