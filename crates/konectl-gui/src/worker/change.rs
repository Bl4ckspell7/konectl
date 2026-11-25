//! Changes to the mouse: settings edits (written together when they come
//! quickly), profile operations and undo.
//!
//! Every change saves the safety backups first and remembers the state for
//! undo. Settings edits re-read the profile before writing, so a change made
//! meanwhile (the mouse's own DPI or profile button, another program) is kept.
//! After a failed write the mouse is read again: part of it may have arrived.

use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use konectl::buttons::Special;
use konectl::settings::set_profile_color;
use konectl::{
    Backup, ButtonAction, Buttons, Layer, Macro, MacroMode, ProfileSettings, RemovedProfile, Rgb,
};

use super::device::Device;
use super::request::{Change, Event, Request};
use super::{ASLEEP_CODE, Worker, backups_modified};
use crate::command_line::{self, CommandLines};
use crate::edit::Edit;
use crate::undo::{self, Plan, Step};

/// What follows a handled request in the queue.
#[derive(Debug, PartialEq)]
pub(super) enum Following {
    Nothing,
    Next(Request),
    /// The worker is to end.
    End,
}

/// Collects the edits that follow `first` until none came for their settle
/// time (a dragged slider or a typed number sends many).
pub(super) fn collect_edits(
    first: (u8, Edit),
    requests: &Receiver<Request>,
) -> (Vec<(u8, Edit)>, Following) {
    let mut edits: Vec<(u8, Edit)> = vec![first];
    let mut quiet_until: Instant = Instant::now() + first.1.settle();
    loop {
        match requests.recv_timeout(quiet_until.saturating_duration_since(Instant::now())) {
            Ok(Request::Edit { profile, edit }) => {
                edits.push((profile, edit));
                quiet_until = quiet_until.max(Instant::now() + edit.settle());
            }
            // Written before the worker ends.
            Ok(Request::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                return (edits, Following::End);
            }
            Ok(other) => return (edits, Following::Next(other)),
            Err(RecvTimeoutError::Timeout) => return (edits, Following::Nothing),
        }
    }
}

/// How long a button function waits for another one for the same button
/// (stepping through a list with the arrow keys chooses each function).
const BUTTON_SETTLE: Duration = Duration::from_millis(400);

/// The last of the button functions chosen in quick succession for the same
/// button and layer, starting with `first`.
pub(super) fn latest_button(first: Request, requests: &Receiver<Request>) -> (Request, Following) {
    let target = |request: &Request| match request {
        Request::SetButton {
            profile,
            button,
            layer,
            ..
        } => Some((*profile, *button, *layer)),
        _ => None,
    };
    let mut latest: Request = first;
    loop {
        match requests.recv_timeout(BUTTON_SETTLE) {
            Ok(next) if target(&next).is_some() && target(&next) == target(&latest) => {
                latest = next
            }
            Ok(Request::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                return (latest, Following::End);
            }
            Ok(other) => return (latest, Following::Next(other)),
            Err(RecvTimeoutError::Timeout) => return (latest, Following::Nothing),
        }
    }
}

/// Drops repeats of `handled` that queued up meanwhile (impatient clicks).
/// Undo is not dropped: each click undoes one more change.
pub(super) fn skip_repeats(handled: &Request, requests: &Receiver<Request>) -> Following {
    if *handled == Request::Undo {
        return Following::Nothing;
    }
    loop {
        match requests.try_recv() {
            Ok(repeat) if &repeat == handled => {}
            Ok(Request::Shutdown) | Err(TryRecvError::Disconnected) => return Following::End,
            Ok(other) => return Following::Next(other),
            Err(TryRecvError::Empty) => return Following::Nothing,
        }
    }
}

/// What a change reports.
struct Done {
    commands: CommandLines,
    note: Option<String>,
    select: Option<u8>,
}

impl Done {
    fn commands(commands: CommandLines) -> Self {
        Self {
            commands,
            note: None,
            select: None,
        }
    }
}

/// Why a change did not happen.
enum Failure {
    /// Refused before anything was written (e.g. an invalid value).
    Refused(konectl::Error),
    /// Reading or writing the mouse failed; `pushed` when the state was
    /// already saved for undo (then part of the change may have arrived).
    Failed { error: konectl::Error, pushed: bool },
}

fn set_settings(backup: &mut Backup, settings: ProfileSettings) {
    if let Some(profile) = backup
        .profiles
        .iter_mut()
        .find(|profile| profile.settings.profile() == settings.profile())
    {
        profile.settings = settings;
    }
}

fn set_buttons(backup: &mut Backup, buttons: Buttons) {
    if let Some(profile) = backup
        .profiles
        .iter_mut()
        .find(|profile| profile.settings.profile() == buttons.profile())
    {
        profile.buttons = Some(buttons);
    }
}

fn set_colour(backup: &mut Backup, number: u8, colour: Option<Rgb>) {
    if let Some(profile) = backup
        .profiles
        .iter_mut()
        .find(|profile| profile.settings.profile() == number)
    {
        set_profile_color(&mut profile.entry, colour);
    }
}

fn removed_note(profile: u8, removed: &RemovedProfile) -> String {
    let mut note: String = format!("Removed profile {profile}.");
    let moves: Vec<String> = removed
        .moved
        .iter()
        .map(|(old, new)| format!("profile {old} is now profile {new}"))
        .collect();
    if !moves.is_empty() {
        note.push_str(&format!(" The others moved down: {}.", moves.join(", ")));
    }
    match removed.disabled_buttons {
        0 => {}
        1 => note.push_str(" A button that switched to it is now disabled."),
        count => note.push_str(&format!(
            " {count} buttons that switched to it are now disabled."
        )),
    }
    note
}

impl Worker {
    /// Writes edits; edits of the same profile are applied together, and the
    /// whole batch is one step to undo.
    pub(super) fn apply_edits(&mut self, edits: &[(u8, Edit)], now: Instant) {
        match self.write_edits(edits) {
            Ok(commands) => self.changed(Done::commands(commands)),
            Err(Failure::Refused(error)) => self.change_refused(error),
            Err(Failure::Failed { error, pushed }) => self.change_failed(error, pushed, now),
        }
    }

    fn write_edits(&mut self, edits: &[(u8, Edit)]) -> Result<CommandLines, Failure> {
        let mut profiles: Vec<u8> = Vec::new();
        for (profile, _) in edits {
            if !profiles.contains(profile) {
                profiles.push(*profile);
            }
        }
        // Read and check everything before anything is written.
        let mut writes: Vec<(ProfileSettings, ProfileSettings)> = Vec::new();
        let unread = |error: konectl::Error| Failure::Failed {
            error,
            pushed: false,
        };
        for profile in profiles {
            let device = self
                .device
                .as_mut()
                .ok_or(konectl::Error::NotFound)
                .map_err(unread)?;
            let current: ProfileSettings = device.settings(profile).map_err(unread)?;
            let mut after: ProfileSettings = current;
            for (_, edit) in edits.iter().filter(|(number, _)| *number == profile) {
                edit.apply(&mut after).map_err(Failure::Refused)?;
            }
            if let Some(snapshot) = self.snapshot.as_mut() {
                set_settings(&mut snapshot.backup, current);
            }
            if after != current {
                writes.push((current, after));
            }
        }
        let mut commands = CommandLines::new();
        if writes.is_empty() {
            return Ok(commands);
        }
        self.save_state(true).map_err(unread)?;
        let written = |error: konectl::Error| Failure::Failed {
            error,
            pushed: true,
        };
        for (current, after) in writes {
            let device = self
                .device
                .as_mut()
                .ok_or(konectl::Error::NotFound)
                .map_err(written)?;
            device.write_changes(&current, &after).map_err(written)?;
            if let Some(snapshot) = self.snapshot.as_mut() {
                set_settings(&mut snapshot.backup, after);
            }
            commands.extend(command_line::settings(&current, &after));
        }
        Ok(commands)
    }

    /// Gives a button a function (not a macro).
    pub(super) fn set_button(
        &mut self,
        profile: u8,
        button: u8,
        layer: Layer,
        action: ButtonAction,
        now: Instant,
    ) {
        if let ButtonAction::Special(Special::Profile(target)) = action
            && self.snapshot.as_ref().is_some_and(|snapshot| {
                !snapshot
                    .backup
                    .profiles
                    .iter()
                    .any(|candidate| candidate.settings.profile() == target)
            })
        {
            self.change_refused(konectl::Error::UnusedProfile(target));
            return;
        }
        let result = self.write_button(profile, |current| {
            let mut after: Buttons = *current;
            after.set_action(button, layer, action)?;
            Ok(after)
        });
        match result {
            Ok(Some(())) => {
                self.changed(Done::commands(command_line::button(
                    profile, button, layer, action,
                )));
            }
            Ok(None) => self.changed(Done::commands(CommandLines::new())),
            Err(Failure::Refused(error)) => self.change_refused(error),
            Err(Failure::Failed { error, pushed }) => self.change_failed(error, pushed, now),
        }
    }

    /// Gives a button a macro; `keys` as typed. The macro is always written:
    /// what the mouse has cannot be read.
    pub(super) fn set_macro(
        &mut self,
        profile: u8,
        button: u8,
        layer: Layer,
        keys: &str,
        mode: MacroMode,
        now: Instant,
    ) {
        let macro_: Macro = match keys.parse() {
            Ok(macro_) => macro_,
            Err(error) => {
                self.change_refused(error);
                return;
            }
        };
        self.send(Event::Busy(Some("Writing the macro …")));
        let result: Result<(), Failure> = (|| {
            let unread = |error: konectl::Error| Failure::Failed {
                error,
                pushed: false,
            };
            let device = self
                .device
                .as_mut()
                .ok_or(konectl::Error::NotFound)
                .map_err(unread)?;
            let current: Buttons = device.buttons(profile).map_err(unread)?;
            let mut after: Buttons = current;
            after
                .set_action(button, layer, ButtonAction::Macro(mode))
                .map_err(Failure::Refused)?;
            if let Some(snapshot) = self.snapshot.as_mut() {
                set_buttons(&mut snapshot.backup, current);
            }
            // Only the keys change: there is nothing Undo could write back.
            let undoable: bool = after != current;
            self.save_state(undoable).map_err(unread)?;
            let device = self
                .device
                .as_mut()
                .ok_or(konectl::Error::NotFound)
                .map_err(unread)?;
            device
                .write_macro(&after, button, layer, &macro_)
                .map_err(|error| Failure::Failed {
                    error,
                    pushed: undoable,
                })?;
            if let Some(snapshot) = self.snapshot.as_mut() {
                set_buttons(&mut snapshot.backup, after);
            }
            Ok(())
        })();
        self.send(Event::Busy(None));
        match result {
            Ok(()) => self.changed(Done {
                commands: command_line::macro_button(profile, button, layer, keys, mode),
                note: Some("Macro written; its old keys cannot be brought back.".to_owned()),
                select: None,
            }),
            Err(Failure::Refused(error)) => self.change_refused(error),
            Err(Failure::Failed { error, pushed }) => self.change_failed(error, pushed, now),
        }
    }

    /// Reads the buttons of `profile`, changes them with `change` and writes
    /// them if they differ; `Some(())` when written.
    fn write_button(
        &mut self,
        profile: u8,
        change: impl FnOnce(&Buttons) -> konectl::Result<Buttons>,
    ) -> Result<Option<()>, Failure> {
        let unread = |error: konectl::Error| Failure::Failed {
            error,
            pushed: false,
        };
        let device = self
            .device
            .as_mut()
            .ok_or(konectl::Error::NotFound)
            .map_err(unread)?;
        let current: Buttons = device.buttons(profile).map_err(unread)?;
        let after: Buttons = change(&current).map_err(Failure::Refused)?;
        if let Some(snapshot) = self.snapshot.as_mut() {
            set_buttons(&mut snapshot.backup, current);
        }
        if after == current {
            return Ok(None);
        }
        self.save_state(true).map_err(unread)?;
        let device = self
            .device
            .as_mut()
            .ok_or(konectl::Error::NotFound)
            .map_err(unread)?;
        device
            .write_buttons(&after)
            .map_err(|error| Failure::Failed {
                error,
                pushed: true,
            })?;
        if let Some(snapshot) = self.snapshot.as_mut() {
            set_buttons(&mut snapshot.backup, after);
        }
        Ok(Some(()))
    }

    pub(super) fn activate(&mut self, profile: u8, now: Instant) {
        // Also when the snapshot says it is active: the profile button of the
        // mouse may have switched meanwhile.
        self.profile_change(now, |device, backup| {
            device.set_active_profile(profile)?;
            backup.active_profile = profile;
            Ok(Done::commands(command_line::activate(profile)))
        });
    }

    pub(super) fn colour(&mut self, profile: u8, colour: Option<Rgb>, now: Instant) {
        self.profile_change(now, |device, backup| {
            device.set_profile_color(profile, colour)?;
            set_colour(backup, profile, colour);
            Ok(Done::commands(command_line::profile_colour(
                profile, colour,
            )))
        });
    }

    pub(super) fn add_profile(&mut self, copy_from: u8, now: Instant) {
        self.send(Event::Busy(Some("Adding a profile …")));
        self.profile_change(now, |device, backup| {
            let number: u8 = device.add_profile(copy_from)?;
            *backup = device.backup()?;
            Ok(Done {
                commands: command_line::add_profile(copy_from),
                note: Some(format!(
                    "Added profile {number} as a copy of profile {copy_from}."
                )),
                select: Some(number),
            })
        });
        self.send(Event::Busy(None));
    }

    pub(super) fn remove_profile(&mut self, profile: u8, now: Instant) {
        self.send(Event::Busy(Some("Removing the profile …")));
        self.profile_change(now, |device, backup| {
            let removed: RemovedProfile = device.remove_profile(profile)?;
            *backup = device.backup()?;
            Ok(Done {
                commands: command_line::remove_profile(profile),
                note: Some(removed_note(profile, &removed)),
                select: None,
            })
        });
        self.send(Event::Busy(None));
    }

    /// Goes back to the state before the last change: only what it changed
    /// is written back, or everything when profiles were added or removed.
    pub(super) fn undo(&mut self, now: Instant) {
        let Some(previous) = self.undo.pop() else {
            return;
        };
        let Some(current) = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.backup.clone())
        else {
            self.undo.push(previous);
            return;
        };
        let plan: Plan = undo::plan(&current, &previous);
        self.send(Event::Busy(Some("Undoing …")));
        let result: konectl::Result<()> = self
            .save_state(false)
            .and_then(|()| self.carry_out(&plan, &previous));
        self.send(Event::Busy(None));
        match result {
            Ok(()) => self.changed(Done {
                commands: command_line::backup(&current, &previous),
                note: Some("Undone.".to_owned()),
                select: None,
            }),
            Err(error) => {
                self.undo.push(previous);
                self.change_failed(error, false, now);
            }
        }
    }

    fn carry_out(&mut self, plan: &Plan, previous: &Backup) -> konectl::Result<()> {
        let (Some(device), Some(snapshot)) = (self.device.as_mut(), self.snapshot.as_mut()) else {
            return Err(konectl::Error::NotFound);
        };
        match plan {
            Plan::Restore => {
                device.restore(previous)?;
                snapshot.backup = device.backup()?;
            }
            Plan::Steps(steps) => {
                for step in steps {
                    match *step {
                        Step::Settings { now, before } => {
                            let fresh: ProfileSettings = device.settings(before.profile())?;
                            let target: ProfileSettings = undo::merge(&fresh, &now, &before);
                            device.write_changes(&fresh, &target)?;
                            set_settings(&mut snapshot.backup, target);
                        }
                        Step::Colour { profile, colour } => {
                            device.set_profile_color(profile, colour)?;
                            set_colour(&mut snapshot.backup, profile, colour);
                        }
                        Step::Buttons { now, before } => {
                            let fresh: Buttons = device.buttons(before.profile())?;
                            let target: Buttons =
                                undo::merge_buttons(&fresh, &now, &before).unwrap_or(before);
                            device.write_buttons(&target)?;
                            set_buttons(&mut snapshot.backup, target);
                        }
                        Step::Activate(profile) => {
                            device.set_active_profile(profile)?;
                            snapshot.backup.active_profile = profile;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Saves the state first, then runs `write` on the mouse and the snapshot.
    fn profile_change(
        &mut self,
        now: Instant,
        write: impl FnOnce(&mut dyn Device, &mut Backup) -> konectl::Result<Done>,
    ) {
        if self.device.is_none() || self.snapshot.is_none() {
            return;
        }
        if let Err(error) = self.save_state(true) {
            self.change_failed(error, false, now);
            return;
        }
        let (Some(device), Some(snapshot)) = (self.device.as_mut(), self.snapshot.as_mut()) else {
            return;
        };
        match write(device.as_mut(), &mut snapshot.backup) {
            Ok(done) => self.changed(done),
            Err(error) => self.change_failed(error, true, now),
        }
    }

    /// Saves the safety backups of the current state, and remembers it for
    /// undo if `undoable`.
    fn save_state(&mut self, undoable: bool) -> konectl::Result<()> {
        let Some(snapshot) = &self.snapshot else {
            return Ok(());
        };
        if let Some(backups) = &self.backups {
            backups.save(&snapshot.backup)?;
        }
        if undoable {
            self.undo.push(snapshot.backup.clone());
        }
        // Saved by this program: not a change by another one.
        self.backups_seen = backups_modified(self.backups.as_ref());
        Ok(())
    }

    fn changed(&mut self, done: Done) {
        if let Some(snapshot) = self.snapshot.clone() {
            self.send(Event::Changed(Box::new(Change {
                snapshot,
                commands: done.commands,
                note: done.note,
                select: done.select,
            })));
        }
        self.send(Event::UndoAvailable(!self.undo.is_empty()));
    }

    /// A change was refused before anything was written.
    fn change_refused(&mut self, error: konectl::Error) {
        self.send(Event::Error(format!("Not changed: {error}")));
        if let Some(snapshot) = self.snapshot.clone() {
            self.send(Event::Reverted(Box::new(snapshot)));
        }
    }

    /// A change failed, perhaps after part of it was written: the mouse is
    /// read again and the controls show what it has. Its undo entry (`pushed`)
    /// is dropped; the safety backup still has the state before. A mouse
    /// that sleeps or is gone is handled like when polling notices it.
    fn change_failed(&mut self, error: konectl::Error, pushed: bool, now: Instant) {
        self.send(Event::Error(format!("The change failed: {error}")));
        if pushed {
            self.undo.pop();
        }
        let connection: bool = match &error {
            konectl::Error::NotReachable | konectl::Error::Timeout(_) => true,
            konectl::Error::Rejected { code, .. } => *code == ASLEEP_CODE,
            _ => false,
        };
        let gone: bool = self
            .device
            .as_mut()
            .is_none_or(|device| !device.is_present());
        if !connection
            && !gone
            && let (Some(device), Some(snapshot)) = (self.device.as_mut(), self.snapshot.as_mut())
            && let Ok(backup) = device.backup()
        {
            snapshot.backup = backup;
        }
        if let Some(snapshot) = self.snapshot.clone() {
            self.send(Event::Reverted(Box::new(snapshot)));
        }
        self.send(Event::UndoAvailable(!self.undo.is_empty()));
        if connection || gone {
            self.failed(error, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn edit(dpi: u16) -> Request {
        Request::Edit {
            profile: 1,
            edit: Edit::Dpi {
                stage: 1,
                dpi: Some(dpi),
            },
        }
    }

    #[test]
    fn edits_that_follow_quickly_are_collected() {
        let (sender, receiver) = mpsc::channel();
        for dpi in [1000, 1050] {
            sender.send(edit(dpi)).expect("send");
        }
        sender.send(Request::Reload).expect("send");
        let first = (
            1,
            Edit::Dpi {
                stage: 1,
                dpi: Some(950),
            },
        );
        let (edits, following) = collect_edits(first, &receiver);
        assert_eq!(edits.len(), 3);
        assert_eq!(following, Following::Next(Request::Reload));

        let started: Instant = Instant::now();
        let (edits, following) = collect_edits(first, &receiver);
        assert_eq!(edits.len(), 1);
        assert_eq!(following, Following::Nothing);
        assert!(started.elapsed() >= Duration::from_millis(1400));
    }

    #[test]
    fn repeated_clicks_are_dropped_but_not_undo() {
        let (sender, receiver) = mpsc::channel();
        sender.send(Request::Reload).expect("send");
        sender.send(Request::Reload).expect("send");
        sender.send(Request::Undo).expect("send");
        assert_eq!(
            skip_repeats(&Request::Reload, &receiver),
            Following::Next(Request::Undo)
        );
        sender.send(Request::Undo).expect("send");
        assert_eq!(skip_repeats(&Request::Undo, &receiver), Following::Nothing);
        assert_eq!(receiver.try_recv(), Ok(Request::Undo));
        drop(sender);
        assert_eq!(skip_repeats(&Request::Reload, &receiver), Following::End);
    }

    #[test]
    fn quick_button_choices_write_the_last() {
        let choose = |action: &str, button: u8| Request::SetButton {
            profile: 1,
            button,
            layer: Layer::Standard,
            action: action.parse().expect("named"),
        };
        let (sender, receiver) = mpsc::channel();
        sender.send(choose("volume-up", 7)).expect("send");
        sender.send(choose("volume-down", 7)).expect("send");
        sender.send(choose("back", 6)).expect("send");
        let (latest, following) = latest_button(choose("mute", 7), &receiver);
        assert_eq!(latest, choose("volume-down", 7));
        assert_eq!(following, Following::Next(choose("back", 6)));
    }

    #[test]
    fn notes_say_where_profiles_moved() {
        let removed = RemovedProfile {
            moved: vec![(3, 2), (4, 3)],
            disabled_buttons: 2,
        };
        assert_eq!(
            removed_note(2, &removed),
            "Removed profile 2. The others moved down: profile 3 is now profile 2, \
             profile 4 is now profile 3. 2 buttons that switched to it are now disabled."
        );
    }
}
