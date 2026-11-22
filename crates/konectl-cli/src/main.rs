//! `konectl` — command-line front end of the konectl library.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use konectl::protocol::{REPORT_LEN, Report, VENDOR_ID};
use konectl::{
    Backup, Effect, HidTransport, KoneProAir, LiftOff, Manager, PollingRate, ProfileSettings, Rgb,
};

/// Configure a ROCCAT Kone Pro Air mouse (unofficial).
#[derive(Debug, Parser)]
#[command(name = "konectl", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List connected Kone Pro Air devices.
    List,
    /// Show the battery level.
    Battery,
    /// Show the wireless signal strength.
    Signal,
    /// Show the settings saved on the mouse.
    Settings {
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Show the profiles in use, or make PROFILE the active one.
    Profile {
        /// Profile slot to activate (1–5).
        #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
        profile: Option<u8>,
    },
    /// Set the DPI of a stage, or disable the stage.
    Dpi {
        /// DPI stage (1–5).
        #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
        stage: u8,
        /// DPI (a multiple of 50 from 50 to 19000), or "off" to disable the stage.
        dpi: DpiValue,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Choose the active DPI stage.
    Stage {
        /// DPI stage (1–5); it must have a DPI value.
        #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
        stage: u8,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Set the polling rate.
    Rate {
        /// 125, 250, 500 or 1000 (Hz).
        rate: PollingRate,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Set the lift-off distance.
    Liftoff {
        /// very-low or low.
        distance: LiftOff,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Set the lighting saved on the mouse. Unset options keep their values.
    Lighting {
        /// off, static, breathing, heartbeat, wave, blinking, aimo or battery.
        effect: Effect,
        /// Colour of both zones, e.g. ff8800.
        #[arg(short, long, conflicts_with_all = ["left", "right"])]
        color: Option<Rgb>,
        /// Colour of the left zone.
        #[arg(long)]
        left: Option<Rgb>,
        /// Colour of the right zone.
        #[arg(long)]
        right: Option<Rgb>,
        /// Brightness in percent (0–100).
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(0..=100))]
        brightness: Option<u8>,
        /// Effect speed, 1 (slow) to 11 (fast).
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=11))]
        speed: Option<u8>,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Save the settings of all profiles to FILE.
    Backup { file: PathBuf },
    /// Write the settings from a backup FILE back to the mouse.
    Restore { file: PathBuf },
    /// Set the LED colours until the mouse goes to sleep (not saved on the mouse).
    Led {
        /// Left LED colour, e.g. ff8800 or '#ff8800'.
        left: Rgb,
        /// Right LED colour; defaults to the left colour.
        right: Option<Rgb>,
        /// Brightness in percent.
        #[arg(short, long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(0..=100))]
        brightness: u8,
    },
    /// Send a raw report and print the replies (debugging).
    ///
    /// Meant for protocol work. Unknown commands can change or break the mouse
    /// configuration — only send what the official software was seen sending.
    Raw {
        /// Hex bytes, e.g. "90 0a" or 900a; zero-padded to 64 bytes.
        #[arg(required = true)]
        bytes: Vec<String>,
    },
}

#[derive(Debug, Args)]
struct ProfileArg {
    /// Profile slot (1–5); defaults to the active profile.
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=5))]
    profile: Option<u8>,
}

/// A DPI value or `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DpiValue(Option<u16>);

impl FromStr for DpiValue {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "off" {
            return Ok(Self(None));
        }
        s.parse::<u16>()
            .map(|dpi| Self(Some(dpi)))
            .map_err(|_| format!("invalid DPI {s:?}: use a number like 800, or off"))
    }
}

fn main() -> Result<()> {
    let cli: Cli = Cli::parse();
    let manager: Manager = Manager::new().context("failed to initialise HID access")?;

    match cli.command {
        Command::List => list(&manager)?,
        Command::Battery => match open(&manager)?.battery() {
            Ok(battery) => match battery.percent() {
                Some(percent) => println!("{percent}%"),
                None => println!("unknown (raw {})", battery.raw),
            },
            Err(konectl::Error::Rejected { .. }) => {
                println!("unknown (the mouse is asleep, switched off or out of range)");
            }
            Err(error) => return Err(error.into()),
        },
        Command::Signal => {
            let signal = open(&manager)?.signal()?;
            if signal.radio_enabled() {
                println!("{}% (raw {})", signal.percent(), signal.raw);
            } else {
                println!("no radio link (cable mode, or the mouse is asleep or switched off)");
            }
        }
        Command::Settings { target } => {
            let mut mouse = open(&manager)?;
            let info = mouse.info()?;
            let settings: ProfileSettings = mouse.settings(target.profile)?;
            print_settings(&settings, info.sensor.profile());
            let firmware: String = info.firmware.map_or_else(
                || "unknown (only reported over the dongle)".to_owned(),
                |firmware| firmware.to_string(),
            );
            println!("{:<16}{firmware}", "firmware");
        }
        Command::Profile { profile: None } => {
            let mut mouse = open(&manager)?;
            let active: u8 = mouse.active_profile()?;
            for profile in mouse.profiles()? {
                let note: &str = if profile == active { "  (active)" } else { "" };
                println!("profile {profile}{note}");
            }
        }
        Command::Profile {
            profile: Some(profile),
        } => {
            let mut mouse = open(&manager)?;
            if !mouse.profiles()?.contains(&profile) {
                return Err(konectl::Error::UnusedProfile(profile).into());
            }
            save_safety_backups(&mut mouse)?;
            mouse.set_active_profile(profile)?;
            println!("profile {profile} is active");
        }
        Command::Dpi { stage, dpi, target } => {
            change(&manager, target.profile, |settings| {
                settings.sensor.set_dpi(stage, dpi.0)
            })?;
        }
        Command::Stage { stage, target } => {
            change(&manager, target.profile, |settings| {
                settings.sensor.set_active_stage(stage)
            })?;
        }
        Command::Rate { rate, target } => {
            change(&manager, target.profile, |settings| {
                settings.sensor.set_polling_rate(rate);
                Ok(())
            })?;
        }
        Command::Liftoff { distance, target } => {
            change(&manager, target.profile, |settings| {
                settings.sensor.set_lift_off(distance)
            })?;
        }
        Command::Lighting {
            effect,
            color,
            left,
            right,
            brightness,
            speed,
            target,
        } => {
            change(&manager, target.profile, |settings| {
                let lighting = &mut settings.lighting;
                lighting.set_effect(effect)?;
                if let Some(colour) = color.or(left) {
                    lighting.set_left(colour);
                }
                if let Some(colour) = color.or(right) {
                    lighting.set_right(colour);
                }
                if let Some(percent) = brightness {
                    lighting.set_brightness(percent)?;
                }
                if let Some(speed) = speed {
                    lighting.set_speed(speed)?;
                }
                Ok(())
            })?;
        }
        Command::Backup { file } => {
            let backup: Backup = open(&manager)?.backup()?;
            fs::write(&file, backup.to_text())
                .with_context(|| format!("could not write {}", file.display()))?;
            println!(
                "saved {} profiles to {}",
                backup.profiles.len(),
                file.display()
            );
        }
        Command::Restore { file } => {
            let text: String = fs::read_to_string(&file)
                .with_context(|| format!("could not read {}", file.display()))?;
            let backup: Backup = Backup::from_text(&text)
                .with_context(|| format!("could not use {}", file.display()))?;
            let mut mouse = open(&manager)?;
            mouse
                .check_backup(&backup)
                .with_context(|| format!("could not use {}", file.display()))?;
            save_safety_backups(&mut mouse)?;
            mouse.restore(&backup)?;
            println!(
                "restored {} profiles from {}",
                backup.profiles.len(),
                file.display()
            );
        }
        Command::Led {
            left,
            right,
            brightness,
        } => {
            let right: Rgb = right.unwrap_or(left);
            open(&manager)?.set_leds(left.scaled(brightness), right.scaled(brightness))?;
        }
        Command::Raw { bytes } => {
            let report: Report = parse_hex_report(&bytes)?;
            let replies: Vec<Report> = open(&manager)?.raw(&report)?;
            if replies.is_empty() {
                println!("no reply");
            }
            for reply in &replies {
                println!("{}", format_hex(reply));
            }
        }
    }
    Ok(())
}

fn list(manager: &Manager) -> Result<()> {
    let devices = manager.devices();
    if devices.is_empty() {
        bail!(konectl::Error::NotFound);
    }
    for device in &devices {
        println!(
            "{:<18}  {:04x}:{:04x}  iface {}  {}  serial {}",
            device.mode,
            VENDOR_ID,
            device.mode.product_id(),
            device.mode.interface(),
            device.path.to_string_lossy(),
            device.serial.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

fn open(manager: &Manager) -> Result<KoneProAir<HidTransport>> {
    match manager.open_first() {
        Ok(mouse) => Ok(mouse),
        Err(error @ konectl::Error::Hid(_)) => Err(anyhow::Error::new(error).context(
            "could not open the mouse (no access to /dev/hidraw*? install udev/70-konectl.rules or use sudo)",
        )),
        Err(error) => Err(error.into()),
    }
}

/// Reads the settings of `profile` (default: active), applies `edit` and
/// writes them back, after saving the safety backups. Prints the result.
fn change(
    manager: &Manager,
    profile: Option<u8>,
    edit: impl FnOnce(&mut ProfileSettings) -> konectl::Result<()>,
) -> Result<()> {
    let mut mouse = open(manager)?;
    let current: ProfileSettings = mouse.settings(profile)?;
    let mut settings: ProfileSettings = current;
    edit(&mut settings)?;
    let active_profile: u8 = mouse.active_profile()?;
    if settings == current {
        println!("unchanged");
    } else {
        save_safety_backups(&mut mouse)?;
        if settings.lighting == current.lighting {
            // Leaves the saved lighting alone; with AIMO it could not be read.
            mouse.write_sensor(&settings.sensor)?;
        } else {
            mouse.write_settings(&settings)?;
        }
    }
    print_settings(&settings, active_profile);
    Ok(())
}

/// Saves the settings before konectl changes them: `original.txt` once (the
/// state before konectl's first change) and `previous.txt` before every change.
fn save_safety_backups(mouse: &mut KoneProAir<HidTransport>) -> Result<()> {
    let directory: PathBuf =
        state_directory(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))?;
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let text: String = mouse.backup()?.to_text();

    // Both files are written to a temporary file first, so an interrupted
    // write never leaves a truncated backup behind.
    let original: PathBuf = directory.join("original.txt");
    let original_temporary: PathBuf = directory.join("original.txt.tmp");
    write_file(&original_temporary, &text)?;
    // The hard link only succeeds while original.txt does not exist yet.
    let linked = fs::hard_link(&original_temporary, &original);
    fs::remove_file(&original_temporary)
        .with_context(|| format!("could not remove {}", original_temporary.display()))?;
    match linked {
        Ok(()) => eprintln!("saved the original settings to {}", original.display()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("could not write {}", original.display()));
        }
    }

    let previous: PathBuf = directory.join("previous.txt");
    let previous_temporary: PathBuf = directory.join("previous.txt.tmp");
    write_file(&previous_temporary, &text)?;
    fs::rename(&previous_temporary, &previous)
        .with_context(|| format!("could not write {}", previous.display()))
}

/// `$XDG_STATE_HOME/konectl`, or `$HOME/.local/state/konectl`.
fn state_directory(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    let base: PathBuf = match xdg_state_home.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => PathBuf::from(home.context("HOME is not set")?).join(".local/state"),
    };
    Ok(base.join("konectl"))
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}

fn print_settings(settings: &ProfileSettings, active_profile: u8) {
    let row = |label: &str, value: &dyn fmt::Display| println!("{label:<16}{value}");
    let sensor = &settings.sensor;
    let lighting = &settings.lighting;

    let profile: u8 = settings.profile();
    let active_note: &str = if profile == active_profile {
        " (active)"
    } else {
        ""
    };
    row("profile", &format!("{profile}{active_note}"));
    match sensor.polling_rate() {
        Some(rate) => row("polling rate", &rate),
        None => row("polling rate", &"unknown"),
    }
    row("lift-off", &sensor.lift_off());
    row(
        "angle snapping",
        &if sensor.angle_snapping() { "on" } else { "off" },
    );
    row("debounce", &format!("{} ms", sensor.debounce_ms()));
    for (stage, dpi) in (1u8..).zip(sensor.dpi_stages()) {
        let value: String = dpi.map_or_else(|| "off".to_owned(), |dpi| dpi.to_string());
        let marker: &str = if stage == sensor.active_stage() {
            " (active)"
        } else {
            ""
        };
        row(&format!("dpi stage {stage}"), &format!("{value}{marker}"));
    }
    row("lighting", &lighting.effect());
    row("brightness", &format!("{}%", lighting.brightness()));
    row("speed", &lighting.speed());
    row(
        "colours",
        &format!("left {}, right {}", lighting.left(), lighting.right()),
    );
    if lighting.effect() == Effect::Aimo {
        println!("(with AIMO the mouse reports the colours it shows, not saved ones)");
    }
}

/// Parses whitespace-separated hex tokens into a zero-padded report.
///
/// Each token holds whole bytes (`0a`, `900a`), so `1 2` is rejected instead
/// of being read as `12`.
fn parse_hex_report(parts: &[String]) -> Result<Report> {
    let mut bytes: Vec<u8> = Vec::new();
    for token in parts.iter().flat_map(|part| part.split_whitespace()) {
        if !token.len().is_multiple_of(2) || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid hex token {token:?}: write whole bytes like 0a or 900a");
        }
        for start in (0..token.len()).step_by(2) {
            bytes.push(u8::from_str_radix(&token[start..start + 2], 16)?);
        }
    }
    if bytes.is_empty() {
        bail!("no bytes given");
    }
    if bytes.len() > REPORT_LEN {
        bail!(
            "report has {} bytes, the maximum is {REPORT_LEN}",
            bytes.len()
        );
    }
    let mut report: Report = [0; REPORT_LEN];
    report[..bytes.len()].copy_from_slice(&bytes);
    Ok(report)
}

fn format_hex(report: &Report) -> String {
    report
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(parts: &[&str]) -> Result<Report> {
        let parts: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
        parse_hex_report(&parts)
    }

    #[test]
    fn parses_spaced_and_joined_hex() {
        let report = parse(&["90 0a", "FF"]).expect("valid hex");
        assert_eq!(report[..3], [0x90, 0x0a, 0xff]);
        assert!(report[3..].iter().all(|&byte| byte == 0));
        assert_eq!(parse(&["900aff"]).expect("valid hex"), report);
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(parse(&["90 0"]).is_err());
        assert!(parse(&["zz"]).is_err());
        assert!(parse(&["+1"]).is_err());
        assert!(parse(&[&"00".repeat(REPORT_LEN + 1)]).is_err());
    }

    #[test]
    fn single_digits_are_not_merged() {
        assert!(parse(&["1", "2"]).is_err());
        assert!(parse(&["9 0 0 a"]).is_err());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse(&[""]).is_err());
        assert!(parse(&["  "]).is_err());
    }

    #[test]
    fn accepts_full_length_report() {
        assert!(parse(&[&"ab".repeat(REPORT_LEN)]).is_ok());
    }

    #[test]
    fn formats_hex() {
        let mut report: Report = [0; REPORT_LEN];
        report[0] = 0x90;
        report[1] = 0x0a;
        assert!(format_hex(&report).starts_with("90 0a 00"));
    }

    #[test]
    fn parses_dpi_values() {
        assert_eq!("off".parse::<DpiValue>(), Ok(DpiValue(None)));
        assert_eq!("900".parse::<DpiValue>(), Ok(DpiValue(Some(900))));
        assert!("fast".parse::<DpiValue>().is_err());
    }

    #[test]
    fn state_directory_prefers_xdg_state_home() {
        let home = Some(OsString::from("/home/user"));
        assert_eq!(
            state_directory(Some(OsString::from("/state")), home.clone()).expect("directory"),
            PathBuf::from("/state/konectl")
        );
        assert_eq!(
            state_directory(Some(OsString::from("relative")), home.clone()).expect("directory"),
            PathBuf::from("/home/user/.local/state/konectl")
        );
        assert_eq!(
            state_directory(None, home).expect("directory"),
            PathBuf::from("/home/user/.local/state/konectl")
        );
        assert!(state_directory(None, None).is_err());
    }

    #[test]
    fn lighting_colour_options_conflict() {
        let parsed = Cli::try_parse_from(["konectl", "lighting", "static", "-c", "ff0000"]);
        assert!(parsed.is_ok());
        let conflicting = Cli::try_parse_from([
            "konectl", "lighting", "static", "-c", "ff0000", "--left", "00ff00",
        ]);
        assert!(conflicting.is_err());
    }

    #[test]
    fn cli_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
