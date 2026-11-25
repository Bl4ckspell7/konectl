//! The Device page's requests: settings for the whole mouse, the lift-off
//! calibration, pairing, the factory reset and backup files.

use std::fs;
use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use konectl::pairing::{self, PairingStep};
use konectl::{Backup, LIFT_OFF_CALIBRATION_SECONDS, LiftOff};

use super::change::Done;
use super::{Event, Worker};
use crate::command_line::{self, CommandLines};

/// One second of the calibration countdown (none in tests).
const SECOND: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_secs(1)
};

const NO_MOUSE: &str = "No mouse is connected.";

/// The profile numbers in `backup`.
fn numbers(backup: &Backup) -> Vec<u8> {
    backup
        .profiles
        .iter()
        .map(|profile| profile.settings.profile())
        .collect()
}

/// What pairing tells when it did not finish, after waiting for `last`.
fn pairing_failure(error: &konectl::Error, last: Option<PairingStep>) -> String {
    // Unplugging and plugging in the dongle ends its pairing mode.
    let what_now: &str = match last {
        None | Some(PairingStep::Unplug) => {
            " The dongle stays in pairing mode until it is unplugged and plugged in again."
        }
        Some(PairingStep::Replug) => " Plug the dongle in again.",
        Some(PairingStep::Connect) => "",
    };
    match error {
        konectl::Error::Cancelled => format!("Pairing cancelled.{what_now}"),
        konectl::Error::NotReplugged => {
            let missed: &str = if last == Some(PairingStep::Unplug) {
                "unplugged"
            } else {
                "plugged in again"
            };
            format!(
                "The dongle was not {missed} within {} seconds.{what_now}",
                pairing::REPLUG_WAIT.as_secs()
            )
        }
        error if last == Some(PairingStep::Connect) => format!(
            "The mouse does not answer through the dongle ({error}): move it, or pair again."
        ),
        error => format!("Pairing failed: {error}.{what_now}"),
    }
}

impl Worker {
    pub(super) fn set_energy_saving(&mut self, enabled: bool, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.send(Event::Error(NO_MOUSE.to_owned()));
            return;
        };
        let written = device.set_energy_saving(enabled);
        // Not part of the backups, so it is read again either way.
        let read: Option<bool> = device.energy_saving().ok();
        if let Some(snapshot) = self.snapshot.as_mut() {
            snapshot.energy_saving = read;
        }
        match written {
            // Undo would undo an earlier change instead: say so.
            Ok(()) => self.changed(Done {
                commands: command_line::energy_saving(enabled),
                note: Some(format!(
                    "Energy saving is {}. Undo does not switch it back.",
                    if enabled { "on" } else { "off" }
                )),
                select: None,
            }),
            Err(error) => self.change_failed(error, false, now),
        }
    }

    pub(super) fn set_debounce(&mut self, ms: u8, now: Instant) {
        let unchanged: bool = self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .backup
                .profiles
                .iter()
                .all(|profile| profile.settings.sensor.debounce_ms() == ms)
        });
        if unchanged {
            return;
        }
        self.profile_change(now, |device, backup| {
            device.set_debounce(ms)?;
            for profile in &mut backup.profiles {
                profile.settings.sensor.set_debounce_ms(ms)?;
            }
            Ok(Done::commands(command_line::debounce(ms)))
        });
    }

    /// Like the official software: measures, then uses the measured distance
    /// in every profile. Undo brings back the lift-off setting before, not an
    /// older measurement. Ends with `Calibration(None)` whatever happens, so
    /// the window can close its dialog.
    pub(super) fn calibrate_lift_off(&mut self, now: Instant) {
        if self.device.is_none() || self.snapshot.is_none() {
            self.send(Event::Error(NO_MOUSE.to_owned()));
            self.send(Event::Calibration(None));
            return;
        }
        if let Err(error) = self.save_state(true) {
            self.send(Event::Calibration(None));
            self.change_failed(error, false, now);
            return;
        }
        self.send(Event::Busy(Some("Calibrating the lift-off distance …")));
        let result: konectl::Result<()> = self.run_calibration();
        self.send(Event::Calibration(None));
        self.send(Event::Busy(None));
        match result {
            Ok(()) => self.changed(Done {
                commands: command_line::calibrate_lift_off(),
                note: Some(
                    "Calibrated: every profile uses the measured lift-off distance.".to_owned(),
                ),
                select: None,
            }),
            Err(error) => self.change_failed(error, true, now),
        }
    }

    fn run_calibration(&mut self) -> konectl::Result<()> {
        let (Some(device), Some(snapshot)) = (self.device.as_mut(), self.snapshot.as_mut()) else {
            return Err(konectl::Error::NotFound);
        };
        device.start_lift_off_calibration()?;
        for remaining in (1..=LIFT_OFF_CALIBRATION_SECONDS).rev() {
            (self.sink)(Event::Calibration(Some(remaining)));
            thread::sleep(SECOND);
        }
        device.finish_lift_off_calibration()?;
        device.set_lift_off_all(LiftOff::Custom)?;
        for profile in &mut snapshot.backup.profiles {
            profile.settings.sensor.set_lift_off(LiftOff::Custom)?;
        }
        Ok(())
    }

    /// Undo writes the profiles back; energy saving (switched on by the
    /// reset) and the lift-off calibration are not part of them.
    pub(super) fn reset(&mut self, now: Instant) {
        if self.device.is_none() || self.snapshot.is_none() {
            self.send(Event::Error(NO_MOUSE.to_owned()));
            return;
        }
        if let Err(error) = self.save_state(true) {
            self.change_failed(error, false, now);
            return;
        }
        self.send(Event::Busy(Some("Resetting the mouse …")));
        let written: konectl::Result<()> = match self.device.as_mut() {
            Some(device) => device.reset(),
            None => Err(konectl::Error::NotFound),
        };
        let reread: Result<(), Reread> = written
            .map_err(Reread::Write)
            .and_then(|()| self.reread().map_err(Reread::Read));
        self.send(Event::Busy(None));
        match reread {
            Ok(()) => self.changed(Done {
                commands: command_line::reset(),
                note: Some(
                    "The mouse is back at its factory settings, with energy saving on. Undo \
                     brings the profiles back."
                        .to_owned(),
                ),
                select: None,
            }),
            Err(Reread::Write(error)) => self.change_failed(error, true, now),
            // The reset happened: keep its undo entry.
            Err(Reread::Read(error)) => self.change_failed(error, false, now),
        }
    }

    /// Reads the profiles and energy saving into the snapshot, after a
    /// change of everything.
    fn reread(&mut self) -> konectl::Result<()> {
        let (Some(device), Some(snapshot)) = (self.device.as_mut(), self.snapshot.as_mut()) else {
            return Err(konectl::Error::NotFound);
        };
        snapshot.backup = device.backup()?;
        snapshot.energy_saving = device.energy_saving().ok();
        Ok(())
    }

    /// Puts the dongle into pairing mode and waits while the user pairs the
    /// mouse and plugs the dongle in again; reports each step. Works while
    /// the mouse does not answer (it may not be paired yet). The window
    /// cancels with [`super::Requests::cancel`], also before this starts.
    /// Ends with `Pairing(None)` whatever happens.
    pub(super) fn pair(&mut self, now: Instant) {
        if self.cancel.load(Ordering::SeqCst) {
            self.send(Event::Message("Pairing cancelled.".to_owned()));
            self.send(Event::Pairing(None));
            return;
        }
        let Some(mut dongle) = self.device.take() else {
            self.send(Event::Error(NO_MOUSE.to_owned()));
            self.send(Event::Pairing(None));
            return;
        };
        if let Err(error) = dongle.start_pairing() {
            self.device = Some(dongle);
            self.send(Event::Error(format!("Pairing did not start: {error}")));
            self.send(Event::Pairing(None));
            return;
        }
        // Undo belongs to the mouse as it was read; another one may answer
        // after pairing.
        if !self.undo.is_empty() {
            self.undo.clear();
            self.send(Event::UndoAvailable(false));
        }
        self.send(Event::Busy(Some("Pairing …")));
        let cancel: Arc<AtomicBool> = Arc::clone(&self.cancel);
        let sink = &mut self.sink;
        let mut last: Option<PairingStep> = None;
        let paired = self.connector.finish_pairing(dongle, &mut |step| {
            if last != Some(step) {
                last = Some(step);
                sink(Event::Pairing(Some(step)));
            }
            if cancel.load(Ordering::SeqCst) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        self.send(Event::Pairing(None));
        self.send(Event::Busy(None));
        match paired {
            Ok(mouse) => {
                self.device = Some(mouse);
                self.read_all(now);
                let note: String = "Paired: the mouse answers through the dongle.".to_owned();
                if self.snapshot.is_some() {
                    self.changed(Done {
                        commands: command_line::pair(),
                        note: Some(note),
                        select: None,
                    });
                } else {
                    self.send(Event::Message(note));
                }
            }
            Err(error) => {
                let message: String = pairing_failure(&error, last);
                if matches!(error, konectl::Error::Cancelled) {
                    self.send(Event::Message(message));
                } else {
                    self.send(Event::Error(message));
                }
                self.connect(now);
            }
        }
    }

    /// Reads all profiles and writes them to `path`. The snapshot stays as
    /// it is: undo compares with it (see `Request::Reload`).
    pub(super) fn save_backup(&mut self, path: &Path, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.send(Event::Error(format!("{NO_MOUSE} Nothing was saved.")));
            return;
        };
        (self.sink)(Event::Busy(Some("Reading the mouse …")));
        let read = device.backup();
        self.send(Event::Busy(None));
        let backup: Backup = match read {
            Ok(backup) => backup,
            Err(error) => {
                self.send(Event::Error(format!("Could not read the mouse: {error}")));
                self.failed(error, now);
                return;
            }
        };
        match fs::write(path, backup.to_text()) {
            Ok(()) => self.changed(Done {
                commands: command_line::save_backup(path),
                note: Some(format!(
                    "Saved {} profiles to {}.",
                    backup.profiles.len(),
                    path.display()
                )),
                select: None,
            }),
            Err(error) => self.send(Event::Error(format!(
                "Could not write {}: {error}",
                path.display()
            ))),
        }
    }

    pub(super) fn restore_file(&mut self, path: &Path, now: Instant) {
        let read = fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| Backup::from_text(&text).map_err(|error| error.to_string()));
        match read {
            Ok(backup) => {
                let note: String = format!(
                    "Restored {} profiles from {}.",
                    backup.profiles.len(),
                    path.display()
                );
                self.restore(&backup, command_line::restore(path), note, now);
            }
            Err(problem) => self.send(Event::Error(format!(
                "Could not use {}: {problem}",
                path.display()
            ))),
        }
    }

    /// The state before konectl's first change, saved by the CLI or this
    /// app (`original.txt`).
    pub(super) fn restore_original(&mut self, now: Instant) {
        let Some(backups) = &self.backups else {
            self.send(Event::Error(
                "There are no safety backups here to restore from.".to_owned(),
            ));
            return;
        };
        let path = backups.original_path();
        match backups.load_original() {
            Ok(backup) => {
                let note: String =
                    "Restored the settings from before konectl's first change.".to_owned();
                self.restore(&backup, command_line::restore(&path), note, now);
            }
            Err(konectl::Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
                self.send(Event::Error(
                    "There are no original settings yet: konectl saves them before its first \
                     change."
                        .to_owned(),
                ));
            }
            Err(error) => self.send(Event::Error(format!(
                "Could not read the original settings: {error}"
            ))),
        }
    }

    /// Writes `backup` to the mouse after checking that it fits; one step to
    /// undo.
    fn restore(&mut self, backup: &Backup, commands: CommandLines, note: String, now: Instant) {
        let (Some(device), Some(snapshot)) = (self.device.as_mut(), self.snapshot.as_ref()) else {
            self.send(Event::Error(format!("{NO_MOUSE} Nothing was restored.")));
            return;
        };
        let before: Vec<u8> = numbers(&snapshot.backup);
        match device.check_backup(backup) {
            Ok(()) => {}
            Err(error @ konectl::Error::InvalidBackup(_)) => {
                self.change_refused(error);
                return;
            }
            // The mouse could not be asked (asleep, unplugged).
            Err(error) => {
                self.change_failed(error, false, now);
                return;
            }
        }
        if let Err(error) = self.save_state(true) {
            self.change_failed(error, false, now);
            return;
        }
        self.send(Event::Busy(Some("Restoring …")));
        let written: konectl::Result<()> = match self.device.as_mut() {
            Some(device) => device.restore(backup),
            None => Err(konectl::Error::NotFound),
        };
        let reread: Result<(), Reread> = written
            .map_err(Reread::Write)
            .and_then(|()| self.reread().map_err(Reread::Read));
        self.send(Event::Busy(None));
        match reread {
            Ok(()) => {
                let now_in_use: Vec<u8> = self
                    .snapshot
                    .as_ref()
                    .map(|snapshot| numbers(&snapshot.backup))
                    .unwrap_or_default();
                let gone: Vec<String> = before
                    .iter()
                    .filter(|number| !now_in_use.contains(number))
                    .map(u8::to_string)
                    .collect();
                let note: String = if gone.is_empty() {
                    note
                } else {
                    format!(
                        "{note} Not in it, so no longer in use: profile {}.",
                        gone.join(", ")
                    )
                };
                self.changed(Done {
                    commands,
                    note: Some(note),
                    select: None,
                });
            }
            Err(Reread::Write(error)) => self.change_failed(error, true, now),
            // Written: keep the undo entry.
            Err(Reread::Read(error)) => self.change_failed(error, false, now),
        }
    }
}

/// Which part of a change of everything failed.
enum Reread {
    /// Writing: perhaps nothing changed, so the undo entry goes.
    Write(konectl::Error),
    /// Reading back after the write: the change happened.
    Read(konectl::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_failures_say_what_to_do() {
        assert_eq!(
            pairing_failure(&konectl::Error::Cancelled, Some(PairingStep::Unplug)),
            "Pairing cancelled. The dongle stays in pairing mode until it is unplugged and \
             plugged in again."
        );
        assert_eq!(
            pairing_failure(&konectl::Error::Cancelled, Some(PairingStep::Replug)),
            "Pairing cancelled. Plug the dongle in again."
        );
        assert_eq!(
            pairing_failure(&konectl::Error::Cancelled, Some(PairingStep::Connect)),
            "Pairing cancelled."
        );
        assert!(
            pairing_failure(&konectl::Error::NotReplugged, Some(PairingStep::Unplug))
                .contains("was not unplugged")
        );
        assert!(
            pairing_failure(&konectl::Error::NotReplugged, Some(PairingStep::Replug))
                .contains("was not plugged in again")
        );
        assert!(
            pairing_failure(&konectl::Error::NotReachable, Some(PairingStep::Connect))
                .contains("does not answer")
        );
    }
}
