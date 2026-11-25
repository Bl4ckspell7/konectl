//! Demo mode (`KONECTL_GUI_DEMO=1`): a mouse that exists only in memory, made
//! from `fixtures/demo-backup.txt`. Nothing is sent to a real mouse. The tests
//! of the worker use it too, and change its state to unplug the dongle or put
//! the mouse to sleep.
//!
//! `KONECTL_GUI_DEMO=no-access`, `asleep` or `unplugged` start the demo in
//! that state, to show those screens.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use konectl::settings::{PROFILE_COUNT, set_profile_color};
use konectl::{
    Backup, ButtonAction, Buttons, ConnectionMode, Layer, Macro, ProfileBackup, ProfileSettings,
    RemovedProfile, Rgb,
};

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
    /// Functions the mouse reported by itself and nobody read yet.
    pub function_events: usize,
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
            function_events: 0,
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

fn find(backup: &mut Backup, profile: u8) -> konectl::Result<&mut ProfileBackup> {
    backup
        .profiles
        .iter_mut()
        .find(|candidate| candidate.settings.profile() == profile)
        .ok_or(konectl::Error::UnusedProfile(profile))
}

fn numbers(backup: &Backup) -> Vec<u8> {
    backup
        .profiles
        .iter()
        .map(|profile| profile.settings.profile())
        .collect()
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

    fn function_events(&mut self) -> konectl::Result<usize> {
        let mut state = lock(&self.state);
        if !self.present(&state) {
            return Err(konectl::Error::NotFound);
        }
        Ok(std::mem::take(&mut state.function_events))
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

    fn active_sensor(&mut self) -> konectl::Result<konectl::Sensor> {
        let mut state = self.answering("info query")?;
        let active: u8 = state.backup.active_profile;
        Ok(find(&mut state.backup, active)?.settings.sensor)
    }

    fn settings(&mut self, profile: u8) -> konectl::Result<ProfileSettings> {
        let mut state = self.answering("info query")?;
        Ok(find(&mut state.backup, profile)?.settings)
    }

    fn write_changes(
        &mut self,
        before: &ProfileSettings,
        after: &ProfileSettings,
    ) -> konectl::Result<bool> {
        if before.profile() != after.profile() {
            return Err(konectl::Error::InvalidValue(
                "the settings belong to another profile".to_owned(),
            ));
        }
        let mut state = self.answering("settings write")?;
        find(&mut state.backup, after.profile())?.settings = *after;
        Ok(before != after)
    }

    fn set_active_profile(&mut self, profile: u8) -> konectl::Result<()> {
        let mut state = self.answering("profile switch")?;
        find(&mut state.backup, profile)?;
        state.backup.active_profile = profile;
        Ok(())
    }

    /// Like the mouse: a copy without the colour, in the first free slot.
    fn add_profile(&mut self, copy_from: u8) -> konectl::Result<u8> {
        let mut state = self.answering("profile table write")?;
        let source: ProfileBackup = *find(&mut state.backup, copy_from)?;
        let used: Vec<u8> = numbers(&state.backup);
        let number: u8 = (1..=PROFILE_COUNT)
            .find(|number| !used.contains(number))
            .ok_or_else(|| {
                konectl::Error::InvalidValue(format!(
                    "all {PROFILE_COUNT} profile slots are in use"
                ))
            })?;
        let mut entry = source.entry;
        set_profile_color(&mut entry, None);
        state.backup.profiles.push(ProfileBackup {
            entry,
            settings: ProfileSettings {
                sensor: source.settings.sensor.with_profile(number),
                lighting: source.settings.lighting,
            },
            buttons: source.buttons.map(|buttons| buttons.with_profile(number)),
        });
        state
            .backup
            .profiles
            .sort_by_key(|profile| profile.settings.profile());
        Ok(number)
    }

    /// Like the mouse, but buttons that switch to a profile keep their number.
    fn remove_profile(&mut self, profile: u8) -> konectl::Result<RemovedProfile> {
        let mut state = self.answering("profile table write")?;
        let used: Vec<u8> = numbers(&state.backup);
        if !used.contains(&profile) {
            return Err(konectl::Error::UnusedProfile(profile));
        }
        if used.len() == 1 || profile == state.backup.active_profile {
            return Err(konectl::Error::InvalidValue(format!(
                "profile {profile} cannot be removed"
            )));
        }
        state
            .backup
            .profiles
            .retain(|candidate| candidate.settings.profile() != profile);
        let mut moved: Vec<(u8, u8)> = Vec::new();
        for (target, candidate) in (1u8..).zip(state.backup.profiles.iter_mut()) {
            let source: u8 = candidate.settings.profile();
            if source != target {
                moved.push((source, target));
                candidate.settings.sensor = candidate.settings.sensor.with_profile(target);
                candidate.buttons = candidate
                    .buttons
                    .map(|buttons| buttons.with_profile(target));
            }
        }
        let active: u8 = state.backup.active_profile;
        if let Some((_, target)) = moved.iter().find(|(source, _)| *source == active) {
            state.backup.active_profile = *target;
        }
        Ok(RemovedProfile {
            moved,
            disabled_buttons: 0,
        })
    }

    fn set_profile_color(&mut self, profile: u8, colour: Option<Rgb>) -> konectl::Result<()> {
        let mut state = self.answering("profile table write")?;
        set_profile_color(&mut find(&mut state.backup, profile)?.entry, colour);
        Ok(())
    }

    fn restore(&mut self, backup: &Backup) -> konectl::Result<()> {
        self.answering("restore")?.backup = backup.clone();
        Ok(())
    }

    fn buttons(&mut self, profile: u8) -> konectl::Result<Buttons> {
        let mut state = self.answering("button query")?;
        find(&mut state.backup, profile)?
            .buttons
            .ok_or_else(|| konectl::Error::InvalidValue("no buttons in the demo".to_owned()))
    }

    fn write_buttons(&mut self, buttons: &Buttons) -> konectl::Result<()> {
        let mut state = self.answering("button write")?;
        find(&mut state.backup, buttons.profile())?.buttons = Some(*buttons);
        Ok(())
    }

    /// The demo keeps no macro keys, like the mouse cannot report them.
    fn write_macro(
        &mut self,
        buttons: &Buttons,
        button: u8,
        layer: Layer,
        _macro: &Macro,
    ) -> konectl::Result<()> {
        if !matches!(buttons.action(button, layer)?, ButtonAction::Macro(_)) {
            return Err(konectl::Error::InvalidValue(format!(
                "button {button} does not play a macro"
            )));
        }
        self.write_buttons(buttons)
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
    fn adds_and_removes_profiles_like_the_mouse() {
        let state: SharedDemo = shared(DemoState::new());
        let mut device = DemoConnector::new(Arc::clone(&state))
            .connect()
            .expect("plugged in");
        assert_eq!(device.add_profile(1).expect("add"), 4);
        let added = device.settings(4).expect("profile 4");
        assert_eq!(
            added.lighting,
            device.settings(1).expect("profile 1").lighting
        );

        device.set_active_profile(3).expect("activate");
        let removed = device.remove_profile(2).expect("remove");
        assert_eq!(removed.moved, [(3, 2), (4, 3)]);
        let backup = device.backup().expect("backup");
        assert_eq!(numbers(&backup), [1, 2, 3]);
        assert_eq!(backup.active_profile, 2);
        assert!(device.remove_profile(2).is_err());
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
