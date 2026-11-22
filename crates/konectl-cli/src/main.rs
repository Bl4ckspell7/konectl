//! `konectl` — command-line front end of the konectl library.

#[cfg(target_os = "linux")]
mod udev;

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use konectl::buttons::{BUTTON_COUNT, HostFunction, Special};
use konectl::protocol::{REPORT_LEN, Report, VENDOR_ID};
use konectl::settings::DEBOUNCE_MAX_MS;
use konectl::{
    Backup, ButtonAction, Buttons, Effect, HidTransport, KoneProAir, Layer, LiftOff, Manager,
    PollingRate, ProfileSettings, Rgb,
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
    /// Install or check the udev rule that lets konectl run without sudo.
    #[cfg(target_os = "linux")]
    Udev {
        #[command(subcommand)]
        action: UdevAction,
    },
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
        /// very-low, low, custom (the distance measured by the last calibration;
        /// calibrate first) or calibrate (measure your surface, then use custom
        /// in all profiles).
        distance: LiftOffArg,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Set the "AUTO" flag of a DPI stage.
    DpiAuto {
        /// DPI stage (1–5); it must have a DPI value.
        #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
        stage: u8,
        state: Switch,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Set what the LEDs do when the mouse is not used for a while.
    LedTimeout {
        /// Minutes without use (1–255), or "never".
        time: LedTimeoutArg,
        /// Effect after the time-out: off, static, breathing, heartbeat, blinking
        /// or aimo; defaults to the current one (or off).
        #[arg(short, long, value_parser = parse_sleep_effect)]
        effect: Option<Effect>,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Show energy saving, or turn it on or off (one setting for all profiles).
    EnergySaving { state: Option<Switch> },
    /// Turn angle snapping on or off.
    AngleSnapping {
        state: Switch,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Set the debounce time in all profiles.
    Debounce {
        /// Debounce time in milliseconds (0–10).
        #[arg(value_parser = clap::value_parser!(u8).range(0..=i64::from(DEBOUNCE_MAX_MS)))]
        ms: u8,
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
        /// Intensity of both colours in percent (0–100).
        #[arg(long, conflicts_with_all = ["left_intensity", "right_intensity"], value_parser = clap::value_parser!(u8).range(0..=100))]
        intensity: Option<u8>,
        /// Intensity of the left colour in percent (0–100).
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
        left_intensity: Option<u8>,
        /// Intensity of the right colour in percent (0–100).
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
        right_intensity: Option<u8>,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Show the button assignments (standard and Easy-Shift layer).
    Buttons {
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Assign a function to a button.
    #[command(after_long_help = BUTTON_FUNCTIONS_HELP)]
    Button {
        /// Button (1–8): 1 left, 2 right, 3 wheel click, 4 wheel up, 5 wheel
        /// down, 6 front side button, 7 rear side button, 8 profile button.
        #[arg(value_parser = clap::value_parser!(u8).range(1..=i64::from(BUTTON_COUNT)))]
        button: u8,
        /// Function, e.g. back, dpi-up, key:ctrl+c or volume-up (see --help for the list).
        action: ButtonAction,
        /// Set the function used while an Easy-Shift button is held.
        #[arg(short, long)]
        easy_shift: bool,
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

const BUTTON_FUNCTIONS_HELP: &str = "\
Functions:
  disabled
  click, menu (right click), universal-scroll (middle click), double-click,
  back, forward, scroll-up, scroll-down
  key:KEY        a key with modifiers: key:ctrl+c, key:shift+alt+f5, key:home,
                 or modifiers alone: key:ctrl. Modifiers: ctrl, shift, alt,
                 super. Keys: a-z, 0-9, f1-f24, enter, esc, tab, space,
                 backspace, insert, delete, home, end, page-up, page-down, up,
                 down, left, right, print-screen, kp-0 to kp-9, ..., or a HID
                 usage such as 0x32
  mute, volume-up, volume-down, play-pause, stop, next-track, previous-track,
  browser-search, browser-stop, browser-refresh,
  consumer:0x0223 (any HID consumer usage)
  dpi-cycle, dpi-up, dpi-down, profile-cycle, profile-next, profile-previous,
  profile:N      switch to profile N (1-3)
  easy-shift     hold for the Easy-Shift layer
  easy-aim:DPI   use DPI (a multiple of 50 from 100 to 19000) while held;
                 also easy-aim-200, easy-aim-900
  easy-wheel-dpi, easy-wheel-volume, easy-wheel-alt-tab, easy-wheel-flip-3d
                 hold and turn the wheel to change the DPI, the volume, or the
                 window

Limits, as in the official software: button 1 stays click; button 8 takes only
the dpi-* and profile-* functions and has no Easy-Shift function; the wheel
(4, 5) takes no easy-* functions; an Easy-Shift button has no Easy-Shift
function.";

#[derive(Debug, Args)]
struct ProfileArg {
    /// Profile slot (1–5); defaults to the active profile.
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=5))]
    profile: Option<u8>,
}

/// `on` or `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Switch {
    On,
    Off,
}

impl Switch {
    fn enabled(self) -> bool {
        self == Self::On
    }
}

/// A lift-off distance, or `calibrate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiftOffArg {
    Set(LiftOff),
    Calibrate,
}

impl FromStr for LiftOffArg {
    type Err = konectl::Error;

    fn from_str(s: &str) -> konectl::Result<Self> {
        if s == "calibrate" {
            return Ok(Self::Calibrate);
        }
        s.parse().map(Self::Set)
    }
}

/// An effect the LEDs can switch to after the time-out.
fn parse_sleep_effect(s: &str) -> std::result::Result<Effect, String> {
    let effect: Effect = s
        .parse()
        .map_err(|error: konectl::Error| error.to_string())?;
    if konectl::LedTimeout::EFFECTS.contains(&effect) {
        Ok(effect)
    } else {
        Err(format!(
            "{effect} cannot be used after the LED time-out: use off, static, breathing, \
             heartbeat, blinking or aimo"
        ))
    }
}

/// Minutes (1–255) or `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LedTimeoutArg {
    Never,
    Minutes(u8),
}

impl FromStr for LedTimeoutArg {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "never" {
            return Ok(Self::Never);
        }
        match s.parse::<u8>() {
            Ok(minutes) if minutes > 0 => Ok(Self::Minutes(minutes)),
            _ => Err(format!(
                "invalid time-out {s:?}: use minutes from 1 to 255, or never"
            )),
        }
    }
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

#[cfg(target_os = "linux")]
#[derive(Debug, Subcommand)]
enum UdevAction {
    /// Install the rule (runs sudo; shows each command first) and apply it.
    Install,
    /// Remove the rule (runs sudo) and apply the change.
    Uninstall,
    /// Show whether the rule is installed and the mouse can be opened.
    Status,
    /// Print the rule, e.g. for a distribution package.
    Rule,
}

fn main() -> Result<()> {
    let cli: Cli = Cli::parse();
    // The udev commands work without HID access; that is what they set up.
    #[cfg(target_os = "linux")]
    if let Command::Udev { action } = &cli.command {
        return match action {
            UdevAction::Install => udev::install(),
            UdevAction::Uninstall => udev::uninstall(),
            UdevAction::Status => udev::status(),
            UdevAction::Rule => {
                print!("{}", udev::RULE);
                Ok(())
            }
        };
    }
    let manager: Manager = Manager::new().context("failed to initialise HID access")?;

    match cli.command {
        #[cfg(target_os = "linux")]
        Command::Udev { .. } => unreachable!("handled above"),
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
            let firmware: String = match info.dongle_firmware {
                Some(dongle) => format!("{} (dongle {dongle})", info.firmware),
                None => info.firmware.to_string(),
            };
            println!("{:<16}{firmware}", "firmware");
            // Best effort: the settings above are complete without it.
            let energy_saving: &str = match mouse.energy_saving() {
                Ok(true) => "on",
                Ok(false) => "off",
                Err(_) => "unknown",
            };
            println!("{:<16}{energy_saving}", "energy saving");
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
        Command::Liftoff {
            distance: LiftOffArg::Set(distance),
            target,
        } => {
            change(&manager, target.profile, |settings| {
                settings.sensor.set_lift_off(distance)
            })?;
        }
        Command::Liftoff {
            distance: LiftOffArg::Calibrate,
            target,
        } => {
            if target.profile.is_some() {
                bail!("the calibration applies to all profiles; leave out -p");
            }
            calibrate_lift_off(&manager)?;
        }
        Command::DpiAuto {
            stage,
            state,
            target,
        } => {
            change(&manager, target.profile, |settings| {
                settings.sensor.set_dpi_auto(stage, state.enabled())
            })?;
        }
        Command::LedTimeout {
            time,
            effect,
            target,
        } => {
            if time == LedTimeoutArg::Never && effect.is_some() {
                bail!("--effect needs a time-out in minutes, not never");
            }
            change(&manager, target.profile, |settings| {
                let timeout = match time {
                    LedTimeoutArg::Never => konectl::LedTimeout::Never,
                    LedTimeoutArg::Minutes(minutes) => {
                        // Keep the current effect if it is one that can be set.
                        let current: Option<Effect> = match settings.sensor.led_timeout() {
                            konectl::LedTimeout::After { effect, .. }
                                if konectl::LedTimeout::EFFECTS.contains(&effect) =>
                            {
                                Some(effect)
                            }
                            _ => None,
                        };
                        konectl::LedTimeout::After {
                            minutes,
                            effect: effect.or(current).unwrap_or(Effect::Off),
                        }
                    }
                };
                settings.sensor.set_led_timeout(timeout)
            })?;
        }
        Command::EnergySaving { state } => {
            let mut mouse = open(&manager)?;
            // Reports a sleeping or switched-off mouse clearly.
            mouse.info()?;
            if let Some(state) = state
                && mouse.energy_saving()? != state.enabled()
            {
                mouse.set_energy_saving(state.enabled())?;
            }
            let enabled: bool = mouse.energy_saving()?;
            println!(
                "{:<16}{}",
                "energy saving",
                if enabled { "on" } else { "off" }
            );
        }
        Command::AngleSnapping { state, target } => {
            change(&manager, target.profile, |settings| {
                settings.sensor.set_angle_snapping(state.enabled());
                Ok(())
            })?;
        }
        Command::Debounce { ms } => {
            let mut mouse = open(&manager)?;
            // The safety backup reads every profile anyway; it also tells
            // whether anything changes.
            let backup: Backup = mouse.backup()?;
            let unchanged: bool = backup
                .profiles
                .iter()
                .all(|profile| profile.settings.sensor.debounce_ms() == ms);
            if unchanged {
                println!("unchanged");
            } else {
                write_safety_backups(&backup)?;
                mouse.set_debounce(ms)?;
            }
            let active_profile: u8 = mouse.active_profile()?;
            print_settings(&mouse.settings(None)?, active_profile);
        }
        Command::Lighting {
            effect,
            color,
            left,
            right,
            brightness,
            speed,
            intensity,
            left_intensity,
            right_intensity,
            target,
        } => {
            change(&manager, target.profile, |settings| {
                let lighting = &mut settings.lighting;
                if let Some(percent) = intensity.or(left_intensity) {
                    lighting.set_left_intensity(percent)?;
                }
                if let Some(percent) = intensity.or(right_intensity) {
                    lighting.set_right_intensity(percent)?;
                }
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
        Command::Buttons { target } => {
            let mut mouse = open(&manager)?;
            let active_profile: u8 = mouse.active_profile()?;
            print_buttons(&mouse.buttons(target.profile)?, active_profile)?;
        }
        Command::Button {
            button,
            action,
            easy_shift,
            target,
        } => {
            let layer: Layer = if easy_shift {
                Layer::EasyShift
            } else {
                Layer::Standard
            };
            let mut mouse = open(&manager)?;
            let current: Buttons = mouse.buttons(target.profile)?;
            let mut buttons: Buttons = current;
            buttons.set_action(button, layer, action)?;
            if let ButtonAction::Special(Special::Profile(profile)) = action
                && !mouse.profiles()?.contains(&profile)
            {
                return Err(konectl::Error::UnusedProfile(profile).into());
            }
            let active_profile: u8 = mouse.active_profile()?;
            if buttons == current {
                println!("unchanged");
            } else {
                save_safety_backups(&mut mouse)?;
                mouse.write_buttons(&buttons)?;
            }
            print_buttons(&buttons, active_profile)?;
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
            let without_buttons: usize = backup
                .profiles
                .iter()
                .filter(|profile| profile.buttons.is_none())
                .count();
            if without_buttons > 0 {
                println!(
                    "{without_buttons} of them have no button assignments in the backup \
                     (made before konectl 0.3); their buttons were not changed"
                );
            }
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
            "could not open the mouse (no access to /dev/hidraw*?): run `konectl udev install`, or use sudo",
        )),
        Err(error) => Err(error.into()),
    }
}

/// Seconds the mouse has to be moved during the lift-off calibration (the
/// official software asks for 10).
const CALIBRATION_SECONDS: u64 = 12;

/// Runs the lift-off calibration, then uses the measured distance in every
/// profile, as the official software does.
fn calibrate_lift_off(manager: &Manager) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("the calibration needs a terminal: it waits for Enter before it starts");
    }
    let mut mouse = open(manager)?;
    save_safety_backups(&mut mouse)?;
    println!("Put the mouse on the surface you use. After you press Enter, move it");
    println!("continuously for {CALIBRATION_SECONDS} seconds. If the calibration is interrupted,");
    print!("run it again, or go back with `konectl liftoff low`. ");
    io::stdout().flush()?;
    let read: usize = io::stdin()
        .read_line(&mut String::new())
        .context("could not read from the terminal")?;
    if read == 0 {
        bail!("no input; the calibration was not started");
    }

    mouse.start_lift_off_calibration()?;
    for remaining in (1..=CALIBRATION_SECONDS).rev() {
        print!("\rkeep moving the mouse … {remaining:2} s ");
        io::stdout().flush()?;
        thread::sleep(Duration::from_secs(1));
    }
    println!("\rcalibration done                ");
    mouse.finish_lift_off_calibration()?;

    for profile in mouse.profiles()? {
        let mut sensor = mouse.settings(Some(profile))?.sensor;
        if sensor.lift_off() != LiftOff::Custom {
            sensor.set_lift_off(LiftOff::Custom)?;
            mouse.write_sensor(&sensor)?;
        }
    }
    println!("lift-off is now custom (calibrated) in all profiles");
    Ok(())
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
    write_safety_backups(&mouse.backup()?)
}

/// Writes a backup as `previous.txt`, and as `original.txt` if that does not
/// exist yet. An `original.txt` without button assignments gets the current ones.
fn write_safety_backups(backup: &Backup) -> Result<()> {
    let text: &str = &backup.to_text();
    let directory: PathBuf =
        state_directory(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))?;
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;

    // Both files are written to a temporary file first, so an interrupted
    // write never leaves a truncated backup behind.
    let original: PathBuf = directory.join("original.txt");
    let original_temporary: PathBuf = directory.join("original.txt.tmp");
    write_file(&original_temporary, text)?;
    // The hard link only succeeds while original.txt does not exist yet.
    let linked = fs::hard_link(&original_temporary, &original);
    fs::remove_file(&original_temporary)
        .with_context(|| format!("could not remove {}", original_temporary.display()))?;
    match linked {
        Ok(()) => eprintln!("saved the original settings to {}", original.display()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            add_buttons_to_original(&original, backup)?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not write {}", original.display()));
        }
    }

    let previous: PathBuf = directory.join("previous.txt");
    let previous_temporary: PathBuf = directory.join("previous.txt.tmp");
    write_file(&previous_temporary, text)?;
    fs::rename(&previous_temporary, &previous)
        .with_context(|| format!("could not write {}", previous.display()))
}

/// Adds the current button assignments to an `original.txt` written by
/// konectl 0.2 or older, once. A file konectl cannot read is left alone.
fn add_buttons_to_original(path: &Path, current: &Backup) -> Result<()> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };
    let Ok(original) = Backup::from_text(&text) else {
        return Ok(());
    };
    let Some(completed) = with_original_buttons(&original, current) else {
        return Ok(());
    };
    let temporary: PathBuf = path.with_extension("txt.tmp");
    write_file(&temporary, &completed.to_text())?;
    fs::rename(&temporary, path).with_context(|| format!("could not write {}", path.display()))?;
    eprintln!("added the current button assignments to {}", path.display());
    Ok(())
}

/// `original` with the button assignments of `current` added where it has
/// none, or `None` if none were missing. konectl 0.2 and older could not
/// change the buttons, so the current ones are the original ones.
fn with_original_buttons(original: &Backup, current: &Backup) -> Option<Backup> {
    let mut completed: Backup = original.clone();
    let mut added: bool = false;
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
            added = true;
        }
    }
    added.then_some(completed)
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
    row("led time-out", &sensor.led_timeout());
    for (stage, dpi) in (1u8..).zip(sensor.dpi_stages()) {
        let value: String = dpi.map_or_else(|| "off".to_owned(), |dpi| dpi.to_string());
        let mut notes: Vec<&str> = Vec::new();
        if stage == sensor.active_stage() {
            notes.push("active");
        }
        if sensor.dpi_auto(stage).unwrap_or(false) {
            notes.push("auto");
        }
        let marker: String = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        };
        row(&format!("dpi stage {stage}"), &format!("{value}{marker}"));
    }
    row("lighting", &lighting.effect());
    row("brightness", &format!("{}%", lighting.brightness()));
    row("speed", &lighting.speed());
    row(
        "colours",
        &format!(
            "left {} ({}%), right {} ({}%)",
            lighting.left(),
            lighting.left_intensity(),
            lighting.right(),
            lighting.right_intensity()
        ),
    );
    if lighting.effect() == Effect::Aimo {
        println!("(with AIMO the mouse reports the colours it shows, not saved ones)");
    }
}

/// Short names of buttons 1–8.
const BUTTON_NAMES: [&str; BUTTON_COUNT as usize] = [
    "left",
    "right",
    "wheel click",
    "wheel up",
    "wheel down",
    "side front",
    "side rear",
    "profile",
];

fn print_buttons(buttons: &Buttons, active_profile: u8) -> Result<()> {
    let profile: u8 = buttons.profile();
    let active_note: &str = if profile == active_profile {
        " (active)"
    } else {
        ""
    };
    println!("profile {profile}{active_note}");
    println!("{:<16}{:<26}easy-shift", "button", "standard");
    let mut swarm_functions: bool = false;
    let mut system_functions: bool = false;
    for (button, name) in (1u8..).zip(BUTTON_NAMES) {
        let standard: ButtonAction = buttons.action(button, Layer::Standard)?;
        let easy_shift: Option<ButtonAction> = if buttons.has_easy_shift_function(button)? {
            Some(buttons.action(button, Layer::EasyShift)?)
        } else {
            None
        };
        for action in [Some(standard), easy_shift].into_iter().flatten() {
            match action {
                ButtonAction::Host(HostFunction::Swarm(_)) => swarm_functions = true,
                ButtonAction::Host(HostFunction::System(_)) => system_functions = true,
                _ => {}
            }
        }
        let easy_shift: String =
            easy_shift.map_or_else(|| "-".to_owned(), |action| action.to_string());
        println!(
            "{:<16}{standard:<26}{easy_shift}",
            format!("{button} {name}")
        );
    }
    if swarm_functions {
        println!("(swarm:… functions only work while the official software runs)");
    }
    if system_functions {
        println!("(whether system:… functions work without the official software is untested)");
    }
    Ok(())
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
    fn parses_switches_and_debounce() {
        let parsed = Cli::try_parse_from(["konectl", "angle-snapping", "on", "-p", "2"]);
        assert!(parsed.is_ok());
        assert!(Cli::try_parse_from(["konectl", "angle-snapping", "maybe"]).is_err());
        assert!(Cli::try_parse_from(["konectl", "debounce", "10"]).is_ok());
        assert!(Cli::try_parse_from(["konectl", "debounce", "11"]).is_err());
    }

    #[test]
    fn parses_power_and_lighting_options() {
        let parse = |args: &[&str]| Cli::try_parse_from(args).map(|_| ());
        assert!(parse(&["konectl", "liftoff", "custom"]).is_ok());
        assert!(parse(&["konectl", "liftoff", "calibrate"]).is_ok());
        assert!(parse(&["konectl", "liftoff", "high"]).is_err());
        assert!(parse(&["konectl", "dpi-auto", "2", "on"]).is_ok());
        assert!(parse(&["konectl", "led-timeout", "5", "-e", "breathing"]).is_ok());
        assert!(parse(&["konectl", "led-timeout", "never"]).is_ok());
        assert!(parse(&["konectl", "led-timeout", "0"]).is_err());
        assert!(parse(&["konectl", "led-timeout", "5", "-e", "wave"]).is_err());
        assert!(parse(&["konectl", "energy-saving"]).is_ok());
        assert!(parse(&["konectl", "energy-saving", "on"]).is_ok());
        assert!(parse(&["konectl", "lighting", "static", "--intensity", "50"]).is_ok());
        assert!(
            parse(&[
                "konectl",
                "lighting",
                "static",
                "--intensity",
                "50",
                "--left-intensity",
                "20"
            ])
            .is_err()
        );
        assert_eq!("never".parse::<LedTimeoutArg>(), Ok(LedTimeoutArg::Never));
        assert_eq!(
            "15".parse::<LedTimeoutArg>(),
            Ok(LedTimeoutArg::Minutes(15))
        );
    }

    #[test]
    fn parses_button_commands() {
        let parse = |args: &[&str]| Cli::try_parse_from(args).map(|_| ());
        assert!(parse(&["konectl", "buttons"]).is_ok());
        assert!(parse(&["konectl", "buttons", "-p", "2"]).is_ok());
        assert!(parse(&["konectl", "button", "7", "back"]).is_ok());
        assert!(parse(&["konectl", "button", "7", "key:ctrl+c", "--easy-shift"]).is_ok());
        assert!(parse(&["konectl", "button", "3", "easy-aim:800", "-e", "-p", "2"]).is_ok());
        assert!(parse(&["konectl", "button", "9", "back"]).is_err());
        assert!(parse(&["konectl", "button", "0", "back"]).is_err());
        assert!(parse(&["konectl", "button", "7", "teleport"]).is_err());
        assert!(parse(&["konectl", "button", "7", "swarm:lock"]).is_err());
        assert!(parse(&["konectl", "button", "7"]).is_err());
    }

    #[test]
    fn original_backups_get_the_buttons_once() {
        let current = Backup::from_text(
            "konectl-backup 3
firmware 4.32
dongle-firmware unknown
active-profile 1
profile 1 entry 01 64 00 00 00
profile 1 sensor 01 00 00 01 00 00 12 00 00 00 00 00 00 00 08 00 06 0f 00 02
profile 1 lighting 06 64 64 46 04 00 ff 00 00 ff 64
profile 1 button-functions 40 40 40 40 45 46 40 40 40 40 40 00 00 00 40 40 00 00 00 00 00 00 00 00 00 00 00 00
profile 1 buttons 00 01 00 00 02 00 00 03 00 01 00 34 00 00 00 00 00 00 00 07 00 00 08 00
profile 1 easy-shift-buttons 01 00 24 01 00 25 02 e2 00 00 00 00 00 00 00 00 00 00 02 e9 00 02 ea 00
",
        )
        .expect("current backup");
        // An original.txt of konectl 0.2, made with other settings.
        let original = Backup::from_text(
            "konectl-backup 2
firmware 4.32
dongle-firmware 4.19
active-profile 1
profile 1 entry 01 64 00 00 00
profile 1 sensor 01 00 00 01 00 00 0c 00 00 00 00 00 00 00 07 00 06 0f 00 02
profile 1 lighting 01 64 64 46 04 00 ff 00 00 ff 64
",
        )
        .expect("original backup");
        let completed = with_original_buttons(&original, &current).expect("buttons added");
        assert_eq!(completed.profiles[0].buttons, current.profiles[0].buttons);
        assert_eq!(
            completed.profiles[0].settings,
            original.profiles[0].settings
        );
        assert_eq!(completed.dongle_firmware, original.dongle_firmware);
        assert!(with_original_buttons(&completed, &current).is_none());
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
