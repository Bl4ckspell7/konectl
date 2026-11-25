//! The Device page: settings for the whole mouse, calibration, pairing,
//! backups and reset. File dialogs go through the XDG desktop portal on a
//! thread of their own, so the window keeps drawing.

use std::path::PathBuf;
use std::thread;

use jiff::ZonedDateTime;
use konectl::ConnectionMode;
use konectl::pairing::PairingStep;
use slint::ComponentHandle as _;

use crate::generated::{AppWindow, DeviceSettings};
use crate::worker::{Request, Requests, Snapshot};

pub fn install(ui: &AppWindow, requests: &Requests, backups_folder: Option<String>) {
    let page = ui.global::<DeviceSettings>();
    page.set_backups_folder(backups_folder.unwrap_or_default().into());

    let sender: Requests = requests.clone();
    page.on_set_energy_saving(move |enabled| sender.send(Request::EnergySaving(enabled)));
    let sender: Requests = requests.clone();
    page.on_set_debounce(move |ms| {
        if let Ok(ms) = u8::try_from(ms) {
            sender.send(Request::Debounce(ms));
        }
    });
    let sender: Requests = requests.clone();
    page.on_calibrate(move || sender.send(Request::CalibrateLiftOff));
    let sender: Requests = requests.clone();
    page.on_pair(move || sender.send_cancellable(Request::Pair));
    let sender: Requests = requests.clone();
    page.on_cancel_pairing(move || sender.cancel());
    let sender: Requests = requests.clone();
    page.on_reset(move || sender.send(Request::Reset));
    let sender: Requests = requests.clone();
    page.on_restore_original(move || sender.send(Request::RestoreOriginal));

    let sender: Requests = requests.clone();
    page.on_save_backup(move || {
        let sender: Requests = sender.clone();
        dialog(move || {
            let path: PathBuf = rfd::FileDialog::new()
                .set_title("Save the mouse settings")
                .set_directory(home())
                .set_file_name(backup_name(&ZonedDateTime::now()))
                .add_filter("konectl backup", &["txt"])
                .save_file()
                .or_else(cancelled)?;
            log::info!("saving the settings to {}", path.display());
            sender.send(Request::SaveBackup(path));
            Some(())
        });
    });
    let sender: Requests = requests.clone();
    page.on_restore_backup(move || {
        let sender: Requests = sender.clone();
        dialog(move || {
            let path: PathBuf = rfd::FileDialog::new()
                .set_title("Restore the mouse settings")
                .set_directory(home())
                .add_filter("konectl backup", &["txt"])
                .pick_file()
                .or_else(cancelled)?;
            log::info!("restoring the settings from {}", path.display());
            sender.send(Request::RestoreBackup(path));
            Some(())
        });
    });
}

/// Runs a file dialog on its own thread; `None` from it means cancelled.
fn dialog(run: impl FnOnce() -> Option<()> + Send + 'static) {
    let spawned = thread::Builder::new()
        .name("konectl-dialog".to_owned())
        .spawn(move || {
            run();
        });
    if let Err(error) = spawned {
        log::error!("cannot open the file dialog: {error}");
    }
}

/// A file dialog without a choice (cancelled, or no portal answered).
fn cancelled() -> Option<PathBuf> {
    log::debug!("file dialog: no file chosen");
    None
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `konectl-backup-YYYY-MM-DD-HHMM.txt`, local time: the time keeps a second
/// backup of the same day from replacing the first.
fn backup_name(now: &ZonedDateTime) -> String {
    format!("konectl-backup-{}.txt", now.strftime("%Y-%m-%d-%H%M"))
}

/// Shows what the page needs from a snapshot.
pub fn show(ui: &AppWindow, snapshot: &Snapshot) {
    let page = ui.global::<DeviceSettings>();
    page.set_energy_saving_known(snapshot.energy_saving.is_some());
    page.set_energy_saving(snapshot.energy_saving.unwrap_or(false));
    page.set_wireless(snapshot.mode == ConnectionMode::Wireless);
    // The same in all profiles when set here; the active profile's otherwise.
    let active = snapshot
        .backup
        .profiles
        .iter()
        .find(|profile| profile.settings.profile() == snapshot.backup.active_profile);
    if let Some(profile) = active.or_else(|| snapshot.backup.profiles.first()) {
        page.set_debounce(i32::from(profile.settings.sensor.debounce_ms()));
    }
}

pub fn show_calibration(ui: &AppWindow, left: Option<u64>) {
    let page = ui.global::<DeviceSettings>();
    if left.is_none() {
        page.set_calibration_ended(page.get_calibration_ended().wrapping_add(1));
    }
    page.set_calibrating(left.is_some());
    page.set_calibration_left(left.map_or(0, |left| i32::try_from(left).unwrap_or(0)));
}

pub fn show_pairing(ui: &AppWindow, step: Option<PairingStep>) {
    let page = ui.global::<DeviceSettings>();
    if step.is_none() {
        page.set_pairing_ended(page.get_pairing_ended().wrapping_add(1));
    }
    page.set_pairing(step.is_some());
    page.set_pairing_step(match step {
        Some(PairingStep::Unplug) | None => 0,
        Some(PairingStep::Replug) => 1,
        Some(PairingStep::Connect) => 2,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_carry_date_and_time() {
        let now: ZonedDateTime = "2026-01-01T09:05:42+01:00[Europe/Berlin]"
            .parse()
            .expect("time");
        assert_eq!(backup_name(&now), "konectl-backup-2026-01-01-0905.txt");
    }
}
