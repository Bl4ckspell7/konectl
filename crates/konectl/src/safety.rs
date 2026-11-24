//! Safety backups: the settings saved before konectl changes the mouse.
//!
//! Two files in `$XDG_STATE_HOME/konectl` (or `~/.local/state/konectl`):
//! `original.txt`, the state before konectl's first change ever, and
//! `previous.txt`, the state before the most recent change. Both are written
//! through a temporary file, so an interrupted write never leaves a truncated
//! backup behind; the temporary names are unique to the process and the call,
//! so two programs (or threads) saving at the same moment do not collide.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backup::Backup;
use crate::error::{Error, Result};

const ORIGINAL: &str = "original.txt";
const PREVIOUS: &str = "previous.txt";

/// Makes the temporary file names of one process unique.
static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// What [`SafetyBackups::save`] did with `original.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginalBackup {
    /// It did not exist and was created from this backup.
    Created,
    /// It was written by konectl 0.2 or older and got the current button
    /// assignments (konectl could not change buttons before 0.3).
    Completed,
    /// It was written by konectl 0.2 or older and was rewritten in the current
    /// format without new content.
    Updated,
    /// It exists and was left alone.
    Kept,
}

/// The safety backup files of one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyBackups {
    directory: PathBuf,
}

impl SafetyBackups {
    /// The directory from the environment: `$XDG_STATE_HOME/konectl`, or
    /// `$HOME/.local/state/konectl`.
    pub fn from_env() -> Result<Self> {
        let directory: PathBuf =
            state_directory(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
                .ok_or_else(|| {
                    Error::InvalidValue(
                        "neither XDG_STATE_HOME nor HOME is set to an absolute path, so there \
                         is nowhere to keep safety backups"
                            .to_owned(),
                    )
                })?;
        Ok(Self::in_dir(directory))
    }

    /// The safety backups in `directory`.
    pub fn in_dir(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// `original.txt`: the state before konectl's first change.
    pub fn original_path(&self) -> PathBuf {
        self.directory.join(ORIGINAL)
    }

    /// `previous.txt`: the state before the most recent change.
    pub fn previous_path(&self) -> PathBuf {
        self.directory.join(PREVIOUS)
    }

    /// Saves `backup` as `previous.txt`, and as `original.txt` if that does
    /// not exist yet. An `original.txt` from konectl 0.2 or older gets the
    /// button assignments of `backup` once.
    pub fn save(&self, backup: &Backup) -> Result<OriginalBackup> {
        fs::create_dir_all(&self.directory).map_err(|error| {
            Error::io(
                format!("could not create {}", self.directory.display()),
                error,
            )
        })?;
        let text: String = backup.to_text();

        let original: PathBuf = self.original_path();
        let temporary: PathBuf = self.temporary_path(ORIGINAL);
        write_file(&temporary, &text)?;
        // The hard link only succeeds while original.txt does not exist yet.
        let linked = fs::hard_link(&temporary, &original);
        fs::remove_file(&temporary).map_err(|error| {
            Error::io(format!("could not remove {}", temporary.display()), error)
        })?;
        let outcome: OriginalBackup = match linked {
            Ok(()) => OriginalBackup::Created,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                self.complete_original(backup)?
            }
            Err(error) => {
                return Err(Error::io(
                    format!("could not write {}", original.display()),
                    error,
                ));
            }
        };

        let previous: PathBuf = self.previous_path();
        let temporary: PathBuf = self.temporary_path(PREVIOUS);
        write_file(&temporary, &text)?;
        fs::rename(&temporary, &previous)
            .map_err(|error| Error::io(format!("could not write {}", previous.display()), error))?;
        Ok(outcome)
    }

    /// Reads `previous.txt`.
    pub fn load_previous(&self) -> Result<Backup> {
        read_backup(&self.previous_path())
    }

    /// Reads `original.txt`.
    pub fn load_original(&self) -> Result<Backup> {
        read_backup(&self.original_path())
    }

    /// A temporary file next to `name`, unique to this process and call.
    fn temporary_path(&self, name: &str) -> PathBuf {
        let call: u64 = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.directory
            .join(format!(".{name}.{}-{call}.tmp", std::process::id()))
    }

    /// Adds the button assignments of `current` to an `original.txt` written
    /// by konectl 0.2 or older, once: the file is then rewritten in the
    /// current format and never completed again. A file that cannot be read
    /// is left alone.
    fn complete_original(&self, current: &Backup) -> Result<OriginalBackup> {
        let path: PathBuf = self.original_path();
        let Ok(text) = fs::read_to_string(&path) else {
            return Ok(OriginalBackup::Kept);
        };
        if Backup::format_version(&text).is_none_or(|version| version >= 3) {
            return Ok(OriginalBackup::Kept);
        }
        let Ok(original) = Backup::from_text(&text) else {
            return Ok(OriginalBackup::Kept);
        };
        let completed: Backup = with_original_buttons(&original, current);
        let temporary: PathBuf = self.temporary_path(ORIGINAL);
        write_file(&temporary, &completed.to_text())?;
        fs::rename(&temporary, &path)
            .map_err(|error| Error::io(format!("could not write {}", path.display()), error))?;
        Ok(if completed == original {
            OriginalBackup::Updated
        } else {
            OriginalBackup::Completed
        })
    }
}

/// `$XDG_STATE_HOME/konectl`, or `$HOME/.local/state/konectl`; relative
/// paths do not count (they would depend on the working directory). `None`
/// without either.
pub fn state_directory(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let absolute = |value: Option<OsString>| {
        value
            .map(PathBuf::from)
            .filter(|path: &PathBuf| path.is_absolute())
    };
    let base: PathBuf = match absolute(xdg_state_home) {
        Some(path) => path,
        None => absolute(home)?.join(".local/state"),
    };
    Some(base.join("konectl"))
}

/// `original` with the button assignments of `current` added where it has
/// none. konectl 0.2 and older could not change the buttons, so the current
/// ones are the original ones.
pub fn with_original_buttons(original: &Backup, current: &Backup) -> Backup {
    let mut completed: Backup = original.clone();
    for profile in &mut completed.profiles {
        if profile.buttons.is_some() {
            continue;
        }
        let number: u8 = profile.settings.profile();
        if let Some(buttons) = current
            .profiles
            .iter()
            .find(|candidate| candidate.settings.profile() == number)
            .and_then(|candidate| candidate.buttons)
        {
            profile.buttons = Some(buttons);
        }
    }
    completed
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text)
        .map_err(|error| Error::io(format!("could not write {}", path.display()), error))
}

fn read_backup(path: &Path) -> Result<Backup> {
    let text: String = fs::read_to_string(path)
        .map_err(|error| Error::io(format!("could not read {}", path.display()), error))?;
    Backup::from_text(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One profile with buttons.
    const CURRENT: &str = "konectl-backup 3
firmware 4.32
dongle-firmware unknown
active-profile 1
profile 1 entry 01 64 00 00 00
profile 1 sensor 01 00 00 01 00 00 12 00 00 00 00 00 00 00 08 00 06 0f 00 02
profile 1 lighting 06 64 64 46 04 00 ff 00 00 ff 64
profile 1 button-functions 40 40 40 40 45 46 40 40 40 40 40 00 00 00 40 40 00 00 00 00 00 00 00 00 00 00 00 00
profile 1 buttons 00 01 00 00 02 00 00 03 00 01 00 34 00 00 00 00 00 00 00 07 00 00 08 00
profile 1 easy-shift-buttons 01 00 24 01 00 25 02 e2 00 00 00 00 00 00 00 00 00 00 02 e9 00 02 ea 00
";

    /// As konectl 0.2 wrote it, with other settings.
    const ORIGINAL_V2: &str = "konectl-backup 2
firmware 4.32
dongle-firmware 4.19
active-profile 1
profile 1 entry 01 64 00 00 00
profile 1 sensor 01 00 00 01 00 00 0c 00 00 00 00 00 00 00 07 00 06 0f 00 02
profile 1 lighting 01 64 64 46 04 00 ff 00 00 ff 64
";

    /// A fresh, empty directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let directory: PathBuf =
            std::env::temp_dir().join(format!("konectl-safety-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn state_directory_prefers_xdg_state_home() {
        let home = Some(OsString::from("/home/user"));
        assert_eq!(
            state_directory(Some(OsString::from("/state")), home.clone()),
            Some(PathBuf::from("/state/konectl"))
        );
        assert_eq!(
            state_directory(Some(OsString::from("relative")), home.clone()),
            Some(PathBuf::from("/home/user/.local/state/konectl"))
        );
        assert_eq!(
            state_directory(None, home),
            Some(PathBuf::from("/home/user/.local/state/konectl"))
        );
        assert_eq!(state_directory(None, None), None);
        assert_eq!(state_directory(None, Some(OsString::new())), None);
        assert_eq!(state_directory(None, Some(OsString::from("user"))), None);
    }

    #[test]
    fn saves_original_once_and_previous_every_time() {
        let directory: PathBuf = scratch("save");
        let backups = SafetyBackups::in_dir(&directory);
        let current = Backup::from_text(CURRENT).expect("backup");
        assert_eq!(
            backups.save(&current).expect("save"),
            OriginalBackup::Created
        );
        let mut changed = current.clone();
        changed.profiles[0].buttons = None;
        assert_eq!(backups.save(&changed).expect("save"), OriginalBackup::Kept);
        assert_eq!(backups.load_original().expect("original"), current);
        assert_eq!(backups.load_previous().expect("previous"), changed);
        // No temporary files are left behind.
        let names: Vec<String> = fs::read_dir(&directory)
            .expect("directory")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn original_from_version_2_gets_the_buttons_once() {
        let directory: PathBuf = scratch("original");
        let backups = SafetyBackups::in_dir(&directory);
        fs::create_dir_all(&directory).expect("directory");
        fs::write(backups.original_path(), ORIGINAL_V2).expect("write original");
        let current = Backup::from_text(CURRENT).expect("backup");

        assert_eq!(
            backups.save(&current).expect("save"),
            OriginalBackup::Completed
        );
        let first: String = fs::read_to_string(backups.original_path()).expect("read");
        assert_eq!(Backup::format_version(&first), Some(3));
        let original = backups.load_original().expect("original");
        assert_eq!(original.profiles[0].buttons, current.profiles[0].buttons);
        assert_eq!(
            original.profiles[0].settings,
            Backup::from_text(ORIGINAL_V2).expect("v2").profiles[0].settings
        );

        // Completed once, then left alone.
        let mut changed = current.clone();
        changed.profiles[0].buttons = None;
        assert_eq!(backups.save(&changed).expect("save"), OriginalBackup::Kept);
        assert_eq!(
            fs::read_to_string(backups.original_path()).expect("read"),
            first
        );
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn with_original_buttons_fills_only_missing_buttons() {
        let original = Backup::from_text(ORIGINAL_V2).expect("v2");
        let current = Backup::from_text(CURRENT).expect("current");
        let completed = with_original_buttons(&original, &current);
        assert_eq!(completed.profiles[0].buttons, current.profiles[0].buttons);
        assert_eq!(completed.dongle_firmware, original.dongle_firmware);
        assert_eq!(with_original_buttons(&completed, &current), completed);
    }
}
