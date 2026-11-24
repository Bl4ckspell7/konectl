//! `konectl` — command-line front end of the konectl library.

// `println!` and `print!` for this crate: when stdout is closed (e.g.
// `konectl buttons | head -n 1`), the program ends quietly instead of
// panicking with "Broken pipe". Defined before the modules that use them.
macro_rules! println {
    () => {
        $crate::write_stdout(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::write_stdout(format_args!("{}\n", format_args!($($arg)*)))
    };
}
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::write_stdout(format_args!($($arg)*))
    };
}

#[cfg(target_os = "linux")]
mod udev;

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::io::{self, IsTerminal as _, Write as _};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use konectl::buttons::{BUTTON_COUNT, HostFunction, MACRO_REPEAT_MAX, Special};
use konectl::pairing::{self, PairingStep};
use konectl::protocol::{REPORT_LEN, Report, VENDOR_ID};
use konectl::safety::{OriginalBackup, SafetyBackups};
use konectl::settings::{DEBOUNCE_MAX_MS, PROFILE_COUNT, profile_color, profile_in_use};
use konectl::{
    Backup, ButtonAction, Buttons, ConnectionMode, Effect, HidTransport, KoneProAir,
    LIFT_OFF_CALIBRATION_SECONDS, Layer, LiftOff, Macro, MacroMode, Manager, PollingRate,
    ProfileSettings, Rgb,
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
    /// Show the profiles in use, make PROFILE the active one, or add, remove
    /// or colour profiles.
    #[command(args_conflicts_with_subcommands = true)]
    Profile {
        /// Profile slot to activate (1–5).
        #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
        profile: Option<u8>,
        #[command(subcommand)]
        action: Option<ProfileAction>,
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
        /// Function, e.g. back, dpi-up, key:ctrl+c, volume-up, or a macro such as
        /// 'macro:ctrl+c, 50ms, ctrl+v' (see --help for the list).
        action: ButtonArg,
        /// Set the function used while an Easy-Shift button is held.
        #[arg(short, long)]
        easy_shift: bool,
        /// Macros: play it this many times per press (1–99, default 1).
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=i64::from(MACRO_REPEAT_MAX)),
              conflicts_with_all = ["while_pressed", "toggle"])]
        repeat: Option<u8>,
        /// Macros: play it over and over while the button is held.
        #[arg(long, conflicts_with = "toggle")]
        while_pressed: bool,
        /// Macros: start it with a press and stop it with the next one.
        #[arg(long)]
        toggle: bool,
        /// Macros: do not ask before replacing a macro that other profiles play.
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        target: ProfileArg,
    },
    /// Reset the mouse to its factory settings (asks first).
    Reset {
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Pair the mouse with the dongle (asks first, then shows the steps).
    Pair {
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
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
  system:sleep, system:wake, system:shut-down   power the computer down,
                 put it to sleep or wake it
  dpi-cycle, dpi-up, dpi-down, profile-cycle, profile-next, profile-previous,
  profile:N      switch to profile N (1-3)
  easy-shift     hold for the Easy-Shift layer
  easy-aim:DPI   use DPI (a multiple of 50 from 100 to 19000) while held;
                 also easy-aim-200, easy-aim-900
  easy-wheel-dpi, easy-wheel-volume, easy-wheel-alt-tab, easy-wheel-flip-3d
                 hold and turn the wheel to change the DPI, the volume, or the
                 window

  macro:KEYS     a macro the mouse plays by itself: comma-separated keys with
                 modifiers (ctrl+c, a, f5), down:KEY / up:KEY for a single
                 press or release, and delays such as 50ms (default 20 ms
                 between key events; at most 100 events). Example:
                 'macro:ctrl+c, 50ms, ctrl+v'. With --repeat N, --while-pressed
                 or --toggle. The mouse cannot report a macro back.

Limits, as in the official software: button 1 stays click, and right click
(menu) stays on at least one button; button 8 takes only
the dpi-* and profile-* functions and has no Easy-Shift function; the wheel
(4, 5) takes no easy-* functions; an Easy-Shift button has no Easy-Shift
function; only buttons 1, 2, 3, 6 and 7 play macros (button 1 only in the
Easy-Shift layer).";

/// A button function, or a macro.
#[derive(Debug, Clone)]
enum ButtonArg {
    Action(ButtonAction),
    Macro(Macro),
}

impl FromStr for ButtonArg {
    type Err = konectl::Error;

    fn from_str(s: &str) -> konectl::Result<Self> {
        match s.strip_prefix("macro:") {
            Some(keys) => keys.parse().map(Self::Macro),
            None => s.parse().map(Self::Action),
        }
    }
}

#[derive(Debug, Subcommand)]
enum ProfileAction {
    /// Add a profile in the first free slot, as a copy of another one.
    Add {
        /// Profile to copy (1–5); defaults to the active profile.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
        copy_from: Option<u8>,
    },
    /// Remove a profile (not the active one); the profiles after it move down one slot.
    Remove {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
        profile: u8,
    },
    /// Set the colour the DPI LED flashes when the mouse switches to a profile.
    Color {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
        profile: u8,
        /// A colour such as f40000, or none.
        color: ProfileColor,
    },
}

/// A profile colour, or `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProfileColor(Option<Rgb>);

impl FromStr for ProfileColor {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "none" {
            return Ok(Self(None));
        }
        s.parse()
            .map(|color| Self(Some(color)))
            .map_err(|error: konectl::ParseRgbError| format!("{error}, or none"))
    }
}

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
                print!("{}", konectl::udev::RULE);
                Ok(())
            }
        };
    }
    let mut manager: Manager = Manager::new().context("failed to initialise HID access")?;

    match cli.command {
        #[cfg(target_os = "linux")]
        Command::Udev { .. } => unreachable!("handled above"),
        Command::List => list(&manager)?,
        Command::Battery => match open(&manager)?.battery() {
            Ok(battery) => {
                let level: String = battery.percent().map_or_else(
                    || format!("unknown (raw {})", battery.raw),
                    |p| format!("{p}%"),
                );
                let state: &str = match (battery.charging, battery.cable_power) {
                    (true, _) => " (charging)",
                    (false, true) if battery.percent() == Some(100) => {
                        " (on the cable, fully charged)"
                    }
                    (false, true) => " (on the cable)",
                    (false, false) => "",
                };
                println!("{level}{state}");
            }
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
        Command::Profile {
            profile: None,
            action: None,
        } => list_profiles(&mut open(&manager)?)?,
        Command::Profile {
            profile: None,
            action: Some(action),
        } => {
            let mut mouse = open(&manager)?;
            // Checked before the safety backup, so a refused change keeps the
            // previous backup.
            let profiles: Vec<u8> = mouse.profiles()?;
            let in_use = |profile: u8| -> Result<()> {
                if profiles.contains(&profile) {
                    Ok(())
                } else {
                    Err(konectl::Error::UnusedProfile(profile).into())
                }
            };
            match action {
                ProfileAction::Add { copy_from } => {
                    let source: u8 = match copy_from {
                        Some(profile) => profile,
                        None => mouse.active_profile()?,
                    };
                    in_use(source)?;
                    if profiles.len() == usize::from(PROFILE_COUNT) {
                        bail!("all {PROFILE_COUNT} profile slots are in use");
                    }
                    save_safety_backups(&mut mouse)?;
                    let added: u8 = mouse.add_profile(source).context(undo_hint()?)?;
                    println!("added profile {added}, a copy of profile {source}");
                }
                ProfileAction::Remove { profile } => {
                    in_use(profile)?;
                    if profiles.len() == 1 {
                        bail!("profile {profile} is the only profile");
                    }
                    if mouse.active_profile()? == profile {
                        bail!("profile {profile} is active: activate another profile first");
                    }
                    save_safety_backups(&mut mouse)?;
                    let removed = mouse.remove_profile(profile).context(undo_hint()?)?;
                    println!("removed profile {profile}");
                    for (old, new) in &removed.moved {
                        println!("profile {old} is now profile {new}");
                    }
                    if removed.disabled_buttons > 0 {
                        println!(
                            "{} button functions switched to profile {profile} and are now \
                             disabled",
                            removed.disabled_buttons
                        );
                    }
                }
                ProfileAction::Color { profile, color } => {
                    in_use(profile)?;
                    let entry = mouse.profile_table()?[usize::from(profile - 1)];
                    if profile_color(&entry) == color.0 {
                        println!("unchanged");
                    } else {
                        save_safety_backups(&mut mouse)?;
                        mouse.set_profile_color(profile, color.0)?;
                    }
                }
            }
            list_profiles(&mut mouse)?;
        }
        Command::Profile {
            profile: Some(profile),
            ..
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
            action: ButtonArg::Macro(macro_),
            easy_shift,
            repeat,
            while_pressed,
            toggle,
            yes,
            target,
        } => {
            let layer: Layer = if easy_shift {
                Layer::EasyShift
            } else {
                Layer::Standard
            };
            let mode: MacroMode = if while_pressed {
                MacroMode::WhilePressed
            } else if toggle {
                MacroMode::Toggle
            } else {
                MacroMode::OnPress {
                    repeat: repeat.unwrap_or(1),
                }
            };
            let mut mouse = open(&manager)?;
            let mut buttons: Buttons = mouse.buttons(target.profile)?;
            buttons.set_action(button, layer, ButtonAction::Macro(mode))?;
            let active_profile: u8 = mouse.active_profile()?;
            // The macro on the mouse cannot be read, so it is always written.
            let backup: Backup = mouse.backup()?;
            let shared: Vec<String> = backup
                .profiles
                .iter()
                .filter(|profile| profile.settings.profile() != buttons.profile())
                .filter(|profile| {
                    profile.buttons.is_some_and(|other| {
                        matches!(other.action(button, layer), Ok(ButtonAction::Macro(_)))
                    })
                })
                .map(|profile| profile.settings.profile().to_string())
                .collect();
            if !shared.is_empty() {
                println!(
                    "The mouse keeps one macro per button for all profiles: profile {} plays \
                     the macro on this button too, and its keys would be replaced (they cannot \
                     be read back).",
                    shared.join(", ")
                );
                if !yes && !confirm("Replace it?")? {
                    println!("cancelled");
                    return Ok(());
                }
            }
            write_safety_backups(&backup)?;
            mouse
                .write_macro(&buttons, button, layer, &macro_)
                .with_context(|| {
                    format!(
                        "the macro may be incomplete: write it again, or give button {button} \
                         another function"
                    )
                })?;
            println!("wrote a macro of {} key events", macro_.events().len());
            print_buttons(&buttons, active_profile)?;
        }
        Command::Button {
            button,
            action: ButtonArg::Action(action),
            easy_shift,
            repeat,
            while_pressed,
            toggle,
            yes,
            target,
        } => {
            if repeat.is_some() || while_pressed || toggle || yes {
                bail!("--repeat, --while-pressed, --toggle and --yes are for macros (macro:…)");
            }
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
        Command::Reset { yes } => {
            let mut mouse = open(&manager)?;
            // Reports a sleeping or switched-off mouse before asking.
            mouse.info()?;
            if !yes
                && !confirm(
                    "Reset the mouse to its factory settings? Profiles, buttons and lighting \
                     are replaced.",
                )?
            {
                println!("cancelled");
                return Ok(());
            }
            save_safety_backups(&mut mouse)?;
            mouse.reset()?;
            println!(
                "the mouse is back at its factory settings (the same profile slots, energy saving on)"
            );
            println!(
                "to undo: konectl restore {} (energy saving and the lift-off calibration are \
                 not in it)",
                SafetyBackups::from_env()?.previous_path().display()
            );
        }
        Command::Pair { yes } => pair(&mut manager, yes)?,
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
            let before: Vec<u8> = mouse.profiles()?;
            mouse.restore(&backup)?;
            println!(
                "restored {} profiles from {}",
                backup.profiles.len(),
                file.display()
            );
            let removed: Vec<String> = before
                .iter()
                .filter(|number| {
                    !backup
                        .profiles
                        .iter()
                        .any(|profile| profile.settings.profile() == **number)
                })
                .map(u8::to_string)
                .collect();
            if !removed.is_empty() {
                println!(
                    "removed profile {}, which the backup does not have",
                    removed.join(", ")
                );
            }
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

/// Exit status after stdout was closed, as for a process ended by SIGPIPE.
const EXIT_BROKEN_PIPE: i32 = 128 + 13;

/// Writes to stdout. If stdout was closed, the program ends without a
/// message (status 141, as after SIGPIPE); other errors end it with one.
fn write_stdout(args: fmt::Arguments<'_>) {
    let mut stdout = io::stdout().lock();
    match stdout.write_fmt(args) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::BrokenPipe => std::process::exit(EXIT_BROKEN_PIPE),
        Err(error) => {
            eprintln!("Error: could not write the output: {error}");
            std::process::exit(1);
        }
    }
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
    manager.open_first().map_err(open_error)
}

/// Adds a hint to errors from opening the mouse.
fn open_error(error: konectl::Error) -> anyhow::Error {
    match error {
        konectl::Error::Hid(_) => anyhow::Error::new(error).context(
            "could not open the mouse (no access to /dev/hidraw*?): run `konectl udev install`, or use sudo",
        ),
        error => error.into(),
    }
}

/// Puts the dongle into pairing mode, shows the steps, waits until the dongle
/// was plugged in again and checks that the mouse answers through it.
fn pair(manager: &mut Manager, yes: bool) -> Result<()> {
    let mut dongle = match manager.open_mode(ConnectionMode::Wireless) {
        Err(konectl::Error::NotFound) => bail!("pairing needs the dongle: plug it in"),
        opened => opened.map_err(open_error)?,
    };
    println!("Pairing connects the mouse to this dongle.");
    if !yes && !confirm("Start pairing?")? {
        println!("cancelled");
        return Ok(());
    }
    dongle.start_pairing()?;
    println!("The dongle is waiting for the mouse. Now:");
    println!("  1. Unplug the USB cable from the mouse and switch the mouse on.");
    println!("  2. Hold the profile button (button 8) for 3 seconds.");
    println!("  3. When the lighting effect has finished, unplug the dongle and plug it in again.");
    println!("Waiting for the dongle to be plugged in again …");
    let mut last_step: PairingStep = PairingStep::Unplug;
    let paired = pairing::finish_pairing(manager, dongle, |step| {
        last_step = step;
        ControlFlow::Continue(())
    });
    let not_replugged = "the dongle stays in pairing mode until it is plugged in again";
    match paired {
        Ok(_) => {
            println!("paired: the mouse answers through the dongle");
            Ok(())
        }
        Err(konectl::Error::NotReplugged) => {
            let missed: &str = if last_step == PairingStep::Unplug {
                "unplugged"
            } else {
                "plugged in again"
            };
            Err(anyhow::anyhow!(
                "the dongle was not {missed} within {} seconds",
                pairing::REPLUG_WAIT.as_secs()
            ))
            .context(not_replugged)
        }
        Err(error) if last_step == PairingStep::Connect => Err(open_error(error))
            .context("the mouse does not answer through the dongle: move it, or pair again"),
        Err(error) => Err(error).context(not_replugged),
    }
}

/// How to undo a change that failed halfway.
fn undo_hint() -> Result<String> {
    Ok(format!(
        "to go back to the settings before this command: konectl restore {}",
        SafetyBackups::from_env()?.previous_path().display()
    ))
}

/// Lists the profiles in use with their colours.
fn list_profiles(mouse: &mut KoneProAir<HidTransport>) -> Result<()> {
    let active: u8 = mouse.active_profile()?;
    for (profile, entry) in (1u8..).zip(mouse.profile_table()?) {
        if !profile_in_use(&entry) {
            continue;
        }
        let color: String =
            profile_color(&entry).map_or_else(String::new, |color| format!("  colour {color}"));
        let note: &str = if profile == active { "  (active)" } else { "" };
        println!("profile {profile}{color}{note}");
    }
    Ok(())
}

/// Asks a yes/no question on the terminal; only `y` or `yes` count as yes.
fn confirm(question: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("this needs a confirmation: run it in a terminal, or pass --yes");
    }
    // On stderr, so it is seen even when the output is redirected.
    eprint!("{question} [y/N] ");
    io::stderr().flush()?;
    let mut answer: String = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("could not read from the terminal")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Runs the lift-off calibration, then uses the measured distance in every
/// profile, as the official software does.
fn calibrate_lift_off(manager: &Manager) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("the calibration needs a terminal: it waits for Enter before it starts");
    }
    let mut mouse = open(manager)?;
    save_safety_backups(&mut mouse)?;
    println!("Put the mouse on the surface you use. After you press Enter, move it");
    println!(
        "continuously for {LIFT_OFF_CALIBRATION_SECONDS} seconds. If the calibration is interrupted,"
    );
    print!("run it again, or go back with `konectl liftoff low`. ");
    io::stdout().flush()?;
    let read: usize = io::stdin()
        .read_line(&mut String::new())
        .context("could not read from the terminal")?;
    if read == 0 {
        bail!("no input; the calibration was not started");
    }

    mouse.start_lift_off_calibration()?;
    for remaining in (1..=LIFT_OFF_CALIBRATION_SECONDS).rev() {
        print!("\rkeep moving the mouse … {remaining:2} s ");
        io::stdout().flush()?;
        thread::sleep(Duration::from_secs(1));
    }
    println!("\rcalibration done                ");
    mouse.finish_lift_off_calibration()?;
    mouse.set_lift_off_all(LiftOff::Custom)?;
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
        mouse.write_changes(&current, &settings)?;
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
    let backups: SafetyBackups = SafetyBackups::from_env()?;
    let original: PathBuf = backups.original_path();
    match backups.save(backup)? {
        OriginalBackup::Created => {
            eprintln!("saved the original settings to {}", original.display());
        }
        OriginalBackup::Completed => {
            eprintln!(
                "added the current button assignments to {}",
                original.display()
            );
        }
        OriginalBackup::Updated => {
            eprintln!(
                "updated {} to the current backup format",
                original.display()
            );
        }
        OriginalBackup::Kept => {}
    }
    Ok(())
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
    let mut macros: bool = false;
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
                ButtonAction::Macro(_) => macros = true,
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
    if macros {
        println!("(the mouse cannot report which keys a macro plays)");
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
    fn parses_profile_reset_and_pair_commands() {
        let parse = |args: &[&str]| Cli::try_parse_from(args).map(|_| ());
        assert!(parse(&["konectl", "profile"]).is_ok());
        assert!(parse(&["konectl", "profile", "2"]).is_ok());
        assert!(parse(&["konectl", "profile", "6"]).is_err());
        assert!(parse(&["konectl", "profile", "add"]).is_ok());
        assert!(parse(&["konectl", "profile", "add", "--copy-from", "2"]).is_ok());
        assert!(parse(&["konectl", "profile", "remove", "3"]).is_ok());
        assert!(parse(&["konectl", "profile", "remove"]).is_err());
        assert!(parse(&["konectl", "profile", "color", "2", "f40000"]).is_ok());
        assert!(parse(&["konectl", "profile", "color", "2", "none"]).is_ok());
        assert!(parse(&["konectl", "profile", "color", "2", "reddish"]).is_err());
        assert!(parse(&["konectl", "profile", "2", "add"]).is_err());
        assert!(parse(&["konectl", "reset"]).is_ok());
        assert!(parse(&["konectl", "reset", "--yes"]).is_ok());
        assert!(parse(&["konectl", "pair", "--yes"]).is_ok());
        assert_eq!("none".parse::<ProfileColor>(), Ok(ProfileColor(None)));
    }

    #[test]
    fn parses_macro_buttons() {
        let parse = |args: &[&str]| Cli::try_parse_from(args).map(|_| ());
        assert!(parse(&["konectl", "button", "7", "macro:ctrl+c, 50ms, ctrl+v"]).is_ok());
        assert!(parse(&["konectl", "button", "7", "macro:a", "--repeat", "3"]).is_ok());
        assert!(parse(&["konectl", "button", "7", "macro:a", "--while-pressed"]).is_ok());
        assert!(parse(&["konectl", "button", "7", "macro:a", "--toggle", "-e"]).is_ok());
        assert!(parse(&["konectl", "button", "7", "macro:a", "--repeat", "100"]).is_err());
        assert!(
            parse(&[
                "konectl",
                "button",
                "7",
                "macro:a",
                "--toggle",
                "--while-pressed"
            ])
            .is_err()
        );
        assert!(parse(&["konectl", "button", "7", "macro:"]).is_err());
        assert!(parse(&["konectl", "button", "7", "macro:50ms"]).is_err());
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
