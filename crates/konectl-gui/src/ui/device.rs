//! The Device page: settings for the whole mouse, calibration, pairing,
//! backups and reset. File dialogs go through the XDG desktop portal on a
//! thread of their own, so the window keeps drawing.

use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

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
                .set_file_name(backup_name(SystemTime::now()))
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

/// `konectl-backup-YYYY-MM-DD.txt`, with the date (UTC) of `now`.
fn backup_name(now: SystemTime) -> String {
    let days: i64 = now.duration_since(UNIX_EPOCH).map_or(0, |since| {
        i64::try_from(since.as_secs() / 86_400).unwrap_or(0)
    });
    let (year, month, day) = civil_date(days);
    format!("konectl-backup-{year:04}-{month:02}-{day:02}.txt")
}

/// The calendar date `days` after 1970-01-01 (Howard Hinnant's algorithm).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let shifted: i64 = days + 719_468;
    let era: i64 = shifted.div_euclid(146_097);
    let day_of_era: i64 = shifted.rem_euclid(146_097);
    let year_of_era: i64 =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year: i64 = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index: i64 = (5 * day_of_year + 2) / 153;
    let day: i64 = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month: i64 = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year: i64 = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
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
    use std::time::Duration;

    use super::*;

    #[test]
    fn dates_of_days_since_1970() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(59), (1970, 3, 1));
        // 2000 is a leap year, 1900 and 2100 are not.
        assert_eq!(civil_date(11_016), (2000, 2, 29));
        assert_eq!(civil_date(20_454), (2026, 1, 1));
        assert_eq!(civil_date(47_540), (2100, 2, 28));
        assert_eq!(civil_date(47_541), (2100, 3, 1));
    }

    #[test]
    fn backup_names_carry_the_date() {
        let now: SystemTime = UNIX_EPOCH + Duration::from_secs(20_454 * 86_400 + 3600);
        assert_eq!(backup_name(now), "konectl-backup-2026-01-01.txt");
    }
}
