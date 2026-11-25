//! Undo: the states before the last changes, and what going back to one of
//! them needs. Only what the undone changes touched is written back, onto
//! what the mouse has now, so changes made meanwhile (the mouse's DPI button,
//! another program) stay. When the profiles themselves differ (after adding or
//! removing one), the whole state is restored.

use konectl::settings::profile_color;
use konectl::{Backup, Effect, Lighting, ProfileSettings, Rgb, Sensor};

/// How many changes can be undone.
const DEPTH: usize = 20;
/// Lighting bytes before the colours (effect, intensities, brightness, speed).
const LIGHTING_COLOURS: usize = 5;

/// The states before the last changes, newest last.
#[derive(Debug, Default)]
pub struct UndoStack {
    states: Vec<Backup>,
}

impl UndoStack {
    pub fn push(&mut self, state: Backup) {
        if self.states.len() == DEPTH {
            self.states.remove(0);
        }
        self.states.push(state);
    }

    pub fn pop(&mut self) -> Option<Backup> {
        self.states.pop()
    }

    pub fn clear(&mut self) {
        self.states.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

/// One write that brings a profile back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The settings that differ between `now` and `before` go back to
    /// `before` (see [`merge`]).
    Settings {
        now: ProfileSettings,
        before: ProfileSettings,
    },
    Colour {
        profile: u8,
        colour: Option<Rgb>,
    },
    Activate(u8),
}

/// What going back from `current` to `previous` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Profiles were added or removed (or buttons changed): write it all back.
    Restore,
    /// Writes in this order (activating last, when everything is in place).
    Steps(Vec<Step>),
}

/// With AIMO the mouse reports the colours it shows, not the saved ones, so
/// they are no difference.
fn lighting_differs(one: &Lighting, other: &Lighting) -> bool {
    if one.effect() == Effect::Aimo && other.effect() == Effect::Aimo {
        one.bytes()[..LIGHTING_COLOURS] != other.bytes()[..LIGHTING_COLOURS]
    } else {
        one != other
    }
}

pub fn plan(current: &Backup, previous: &Backup) -> Plan {
    let numbers = |backup: &Backup| -> Vec<u8> {
        backup
            .profiles
            .iter()
            .map(|profile| profile.settings.profile())
            .collect()
    };
    if numbers(current) != numbers(previous) {
        return Plan::Restore;
    }
    let mut steps: Vec<Step> = Vec::new();
    for (now, before) in current.profiles.iter().zip(&previous.profiles) {
        if now.buttons != before.buttons {
            return Plan::Restore;
        }
        if now.settings.sensor != before.settings.sensor
            || lighting_differs(&now.settings.lighting, &before.settings.lighting)
        {
            steps.push(Step::Settings {
                now: now.settings,
                before: before.settings,
            });
        }
        let colour: Option<Rgb> = profile_color(&before.entry);
        if profile_color(&now.entry) != colour {
            steps.push(Step::Colour {
                profile: before.settings.profile(),
                colour,
            });
        }
    }
    if current.active_profile != previous.active_profile {
        steps.push(Step::Activate(previous.active_profile));
    }
    Plan::Steps(steps)
}

/// `fresh` with the bytes that differ between `now` and `before` set back
/// to `before`; the first `limit` bytes only.
fn merge_bytes<const N: usize>(
    fresh: [u8; N],
    now: [u8; N],
    before: [u8; N],
    limit: usize,
) -> [u8; N] {
    let mut merged: [u8; N] = fresh;
    for (index, byte) in merged.iter_mut().enumerate().take(limit) {
        if now[index] != before[index] {
            *byte = before[index];
        }
    }
    merged
}

/// What the mouse has (`fresh`) with an undone change (`now` back to
/// `before`) applied.
pub fn merge(
    fresh: &ProfileSettings,
    now: &ProfileSettings,
    before: &ProfileSettings,
) -> ProfileSettings {
    let sensor = Sensor::from_bytes(merge_bytes(
        fresh.sensor.bytes(),
        now.sensor.bytes(),
        before.sensor.bytes(),
        usize::MAX,
    ));
    let lighting: Lighting = if lighting_differs(&now.lighting, &before.lighting) {
        let aimo: bool =
            now.lighting.effect() == Effect::Aimo && before.lighting.effect() == Effect::Aimo;
        Lighting::from_bytes(merge_bytes(
            fresh.lighting.bytes(),
            now.lighting.bytes(),
            before.lighting.bytes(),
            if aimo { LIGHTING_COLOURS } else { usize::MAX },
        ))
    } else {
        fresh.lighting
    };
    ProfileSettings { sensor, lighting }
}

#[cfg(test)]
mod tests {
    use konectl::settings::set_profile_color;

    use super::*;
    use crate::edit::Edit;
    use crate::worker::demo::DemoState;

    #[test]
    fn keeps_the_last_twenty_states() {
        let state: Backup = DemoState::new().backup;
        let mut stack = UndoStack::default();
        for active in 0..25u8 {
            let mut state: Backup = state.clone();
            state.active_profile = active;
            stack.push(state);
        }
        let mut popped: Vec<u8> = Vec::new();
        while let Some(state) = stack.pop() {
            popped.push(state.active_profile);
        }
        assert_eq!(popped.len(), DEPTH);
        assert_eq!(popped[0], 24);
        assert_eq!(popped[DEPTH - 1], 5);
        assert!(stack.is_empty());
    }

    #[test]
    fn plans_only_the_differences() {
        let previous: Backup = DemoState::new().backup;
        let mut current: Backup = previous.clone();
        Edit::Brightness(10)
            .apply(&mut current.profiles[1].settings)
            .expect("brightness");
        set_profile_color(&mut current.profiles[2].entry, None);
        current.active_profile = 3;
        assert_eq!(
            plan(&current, &previous),
            Plan::Steps(vec![
                Step::Settings {
                    now: current.profiles[1].settings,
                    before: previous.profiles[1].settings,
                },
                Step::Colour {
                    profile: 3,
                    colour: profile_color(&previous.profiles[2].entry),
                },
                Step::Activate(1),
            ])
        );
        assert_eq!(plan(&previous, &previous), Plan::Steps(Vec::new()));
    }

    #[test]
    fn other_profiles_need_a_restore() {
        let previous: Backup = DemoState::new().backup;
        let mut current: Backup = previous.clone();
        current.profiles.pop();
        assert_eq!(plan(&current, &previous), Plan::Restore);
    }

    #[test]
    fn undo_keeps_what_changed_meanwhile() {
        let before: ProfileSettings = DemoState::new().backup.profiles[0].settings;
        let mut now: ProfileSettings = before;
        Edit::Dpi {
            stage: 1,
            dpi: Some(400),
        }
        .apply(&mut now)
        .expect("dpi");
        Edit::Brightness(20).apply(&mut now).expect("brightness");
        // Meanwhile the DPI button switched stages and the speed changed.
        let mut fresh: ProfileSettings = now;
        Edit::ActiveStage(3).apply(&mut fresh).expect("stage");
        Edit::Speed(9).apply(&mut fresh).expect("speed");

        let merged: ProfileSettings = merge(&fresh, &now, &before);
        assert_eq!(
            merged.sensor.dpi(1).expect("stage"),
            before.sensor.dpi(1).expect("stage")
        );
        assert_eq!(merged.sensor.active_stage(), 3);
        assert_eq!(merged.lighting.brightness(), before.lighting.brightness());
        assert_eq!(merged.lighting.speed(), 9);
    }

    #[test]
    fn aimo_colours_are_no_change() {
        let mut before: ProfileSettings = DemoState::new().backup.profiles[0].settings;
        Edit::Effect(Effect::Aimo)
            .apply(&mut before)
            .expect("effect");
        let mut now: ProfileSettings = before;
        Edit::PollingRate(konectl::PollingRate::Hz500)
            .apply(&mut now)
            .expect("rate");
        // The mouse showed other colours when it was read again.
        Edit::Left(Rgb::new(1, 2, 3))
            .apply(&mut now)
            .expect("colour");
        let mut previous: Backup = DemoState::new().backup;
        previous.profiles[0].settings = before;
        let mut current: Backup = previous.clone();
        current.profiles[0].settings = now;

        let Plan::Steps(steps) = plan(&current, &previous) else {
            panic!("no restore");
        };
        assert_eq!(steps.len(), 1);
        let mut fresh: ProfileSettings = now;
        Edit::Left(Rgb::new(9, 9, 9))
            .apply(&mut fresh)
            .expect("colour");
        let merged: ProfileSettings = merge(&fresh, &now, &before);
        assert_eq!(merged.lighting, fresh.lighting);
        assert_eq!(merged.sensor, before.sensor);
    }
}
