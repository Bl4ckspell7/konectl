//! Text backups of the settings konectl changes.
//!
//! A backup holds, for every profile in use, the profile table entry and the
//! raw settings blocks 0 and 1 as read from the mouse, plus the active profile.
//! Restoring writes exactly these bytes back. Example:
//!
//! ```text
//! konectl-backup 1
//! firmware 4.19
//! active-profile 1
//! profile 1 entry 01 64 00 00 00
//! profile 1 sensor 01 00 00 01 00 00 12 00 00 00 00 00 00 00 07 00 06 0f 00 02
//! profile 1 lighting 06 64 64 46 04 00 ff 00 00 ff 64
//! ```

use std::fmt::Write as _;

use crate::error::{Error, Result};
use crate::settings::{
    Firmware, LIGHTING_LEN, Lighting, PROFILE_ENTRY_LEN, ProfileEntry, ProfileSettings, SENSOR_LEN,
    Sensor, profile_in_use, profile_index,
};

const HEADER: &str = "konectl-backup 1";
const UNKNOWN_FIRMWARE: &str = "unknown";

/// Settings of all profiles in use, as read from the mouse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// Firmware the backup was made with, if the mouse reported it (it does
    /// not over the USB cable). Restoring needs the same firmware.
    pub firmware: Option<Firmware>,
    pub active_profile: u8,
    pub profiles: Vec<ProfileBackup>,
}

/// One profile of a [`Backup`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileBackup {
    /// Profile table entry, written back unchanged.
    pub entry: ProfileEntry,
    pub settings: ProfileSettings,
}

impl Backup {
    /// The backup as text, one field per line.
    pub fn to_text(&self) -> String {
        let mut text: String = format!(
            "{HEADER}\nfirmware {}\nactive-profile {}\n",
            self.firmware.map_or_else(
                || UNKNOWN_FIRMWARE.to_owned(),
                |firmware| firmware.to_string()
            ),
            self.active_profile
        );
        for profile in &self.profiles {
            let number: u8 = profile.settings.profile();
            // Writing to a String cannot fail.
            let _ = writeln!(text, "profile {number} entry {}", hex(&profile.entry));
            let _ = writeln!(
                text,
                "profile {number} sensor {}",
                hex(&profile.settings.sensor.bytes())
            );
            let _ = writeln!(
                text,
                "profile {number} lighting {}",
                hex(&profile.settings.lighting.bytes())
            );
        }
        text
    }

    /// Reads a backup written by [`Backup::to_text`]. Empty lines and lines
    /// starting with `#` are ignored.
    pub fn from_text(text: &str) -> Result<Self> {
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'));
        if lines.next() != Some(HEADER) {
            return Err(invalid(format!("first line must be {HEADER:?}")));
        }

        let mut firmware: Option<Option<Firmware>> = None;
        let mut active_profile: Option<u8> = None;
        let mut parts: Vec<ProfileParts> = Vec::new();
        for line in lines {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["firmware", _] if firmware.is_some() => {
                    return Err(invalid("more than one firmware line"));
                }
                ["active-profile", _] if active_profile.is_some() => {
                    return Err(invalid("more than one active-profile line"));
                }
                ["firmware", version] if *version == UNKNOWN_FIRMWARE => firmware = Some(None),
                ["firmware", version] => {
                    let parsed: Firmware = version
                        .parse()
                        .map_err(|_| invalid(format!("invalid firmware version {version:?}")))?;
                    firmware = Some(Some(parsed));
                }
                ["active-profile", number] => active_profile = Some(parse_profile(number)?),
                ["profile", number, kind, bytes @ ..] => {
                    let number: u8 = parse_profile(number)?;
                    let index: usize = match parts.iter().position(|part| part.number == number) {
                        Some(index) => index,
                        None => {
                            parts.push(ProfileParts::new(number));
                            parts.len() - 1
                        }
                    };
                    parts[index].set(kind, bytes)?;
                }
                _ => return Err(invalid(format!("unexpected line {line:?}"))),
            }
        }

        let firmware: Option<Firmware> =
            firmware.ok_or_else(|| invalid("missing firmware line"))?;
        let active_profile: u8 =
            active_profile.ok_or_else(|| invalid("missing active-profile line"))?;
        let profiles: Vec<ProfileBackup> = parts
            .into_iter()
            .map(ProfileParts::finish)
            .collect::<Result<_>>()?;
        if !profiles
            .iter()
            .any(|profile| profile.settings.profile() == active_profile)
        {
            return Err(invalid(format!(
                "active profile {active_profile} is not in the backup"
            )));
        }
        Ok(Self {
            firmware,
            active_profile,
            profiles,
        })
    }
}

/// The lines of one profile, collected while parsing.
struct ProfileParts {
    number: u8,
    entry: Option<ProfileEntry>,
    sensor: Option<[u8; SENSOR_LEN]>,
    lighting: Option<[u8; LIGHTING_LEN]>,
}

impl ProfileParts {
    fn new(number: u8) -> Self {
        Self {
            number,
            entry: None,
            sensor: None,
            lighting: None,
        }
    }

    fn set(&mut self, kind: &str, bytes: &[&str]) -> Result<()> {
        let duplicate = || invalid(format!("profile {} has two {kind} lines", self.number));
        match kind {
            "entry" if self.entry.is_none() => {
                self.entry = Some(parse_hex::<PROFILE_ENTRY_LEN>(bytes)?)
            }
            "sensor" if self.sensor.is_none() => {
                self.sensor = Some(parse_hex::<SENSOR_LEN>(bytes)?)
            }
            "lighting" if self.lighting.is_none() => {
                self.lighting = Some(parse_hex::<LIGHTING_LEN>(bytes)?);
            }
            "entry" | "sensor" | "lighting" => return Err(duplicate()),
            _ => return Err(invalid(format!("unknown field {kind:?}"))),
        }
        Ok(())
    }

    fn finish(self) -> Result<ProfileBackup> {
        let missing = |kind: &str| invalid(format!("profile {} has no {kind} line", self.number));
        let entry: ProfileEntry = self.entry.ok_or_else(|| missing("entry"))?;
        if !profile_in_use(&entry) {
            return Err(invalid(format!(
                "the table entry of profile {} marks it as unused",
                self.number
            )));
        }
        let sensor = Sensor::from_bytes(self.sensor.ok_or_else(|| missing("sensor"))?);
        let lighting = Lighting::from_bytes(self.lighting.ok_or_else(|| missing("lighting"))?);
        if sensor.profile() != self.number {
            return Err(invalid(format!(
                "the sensor block of profile {} belongs to profile {}",
                self.number,
                sensor.profile()
            )));
        }
        Ok(ProfileBackup {
            entry,
            settings: ProfileSettings { sensor, lighting },
        })
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidBackup(message.into())
}

fn parse_profile(number: &str) -> Result<u8> {
    let profile: u8 = number
        .parse()
        .map_err(|_| invalid(format!("invalid profile number {number:?}")))?;
    profile_index(profile).map_err(|_| invalid(format!("invalid profile number {number:?}")))?;
    Ok(profile)
}

fn parse_hex<const N: usize>(tokens: &[&str]) -> Result<[u8; N]> {
    if tokens.len() != N {
        return Err(invalid(format!(
            "expected {N} bytes, found {}",
            tokens.len()
        )));
    }
    let mut bytes: [u8; N] = [0; N];
    for (byte, token) in bytes.iter_mut().zip(tokens) {
        if token.len() != 2 || !token.bytes().all(|digit| digit.is_ascii_hexdigit()) {
            return Err(invalid(format!("invalid byte {token:?}")));
        }
        *byte = u8::from_str_radix(token, 16)
            .map_err(|_| invalid(format!("invalid byte {token:?}")))?;
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "konectl-backup 1
firmware 4.19
active-profile 1
profile 1 entry 01 64 00 00 00
profile 1 sensor 01 00 00 01 00 00 12 00 00 00 00 00 00 00 07 00 06 0f 00 02
profile 1 lighting 06 64 64 46 04 00 ff 00 00 ff 64
profile 2 entry 01 64 00 00 00
profile 2 sensor 02 00 00 01 00 00 0c 00 00 00 00 00 00 00 06 00 00 0f 00 02
profile 2 lighting 01 64 64 3c 06 f4 00 00 32 a6 20
";

    #[test]
    fn round_trip() {
        let backup = Backup::from_text(TEXT).expect("valid backup");
        assert_eq!(backup.firmware.expect("firmware").to_string(), "4.19");
        assert_eq!(backup.active_profile, 1);
        assert_eq!(backup.profiles.len(), 2);
        assert_eq!(backup.profiles[1].settings.profile(), 2);
        assert_eq!(
            backup.profiles[1].settings.sensor.dpi(2).expect("stage"),
            Some(600)
        );
        assert_eq!(backup.to_text(), TEXT);
    }

    #[test]
    fn unknown_firmware_round_trip() {
        let text: String = TEXT.replace("firmware 4.19", "firmware unknown");
        let backup = Backup::from_text(&text).expect("valid backup");
        assert_eq!(backup.firmware, None);
        assert_eq!(backup.to_text(), text);
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let text: String = format!("# saved by hand\n\n{TEXT}\n");
        assert!(Backup::from_text(&text).is_ok());
    }

    #[test]
    fn rejects_broken_backups() {
        let broken: [String; 12] = [
            TEXT.replace("konectl-backup 1", "konectl-backup 2"),
            TEXT.replace("firmware 4.19\n", ""),
            TEXT.replace("active-profile 1", "active-profile 3"),
            TEXT.replace("profile 2 lighting 01 64 64 3c 06 f4 00 00 32 a6 20\n", ""),
            TEXT.replace("profile 2 sensor 02", "profile 2 sensor 03"),
            TEXT.replace("06 64 64 46 04", "06 64 64 46"),
            TEXT.replace("ff 64\n", "zz 64\n"),
            format!("{TEXT}profile 1 entry 01 64 00 00 00\n"),
            format!("{TEXT}active-profile 2\n"),
            format!("{TEXT}firmware 4.19\n"),
            TEXT.replace("ff 64\n", "+f 64\n"),
            TEXT.replace("profile 2 entry 01 64", "profile 2 entry 00 00"),
        ];
        for text in &broken {
            assert!(
                matches!(Backup::from_text(text), Err(Error::InvalidBackup(_))),
                "accepted:\n{text}"
            );
        }
    }
}
