//! What the pages show, made from what the worker read. Pure, so the wording
//! is tested without a window.

use konectl::settings::{DPI_STAGE_COUNT, profile_color};
use konectl::{
    Backup, ConnectionMode, Effect, LedTimeout, LiftOff, PollingRate, ProfileSettings, Rgb, Sensor,
};
use slint::Color;

use crate::generated::{DeviceView, ProfileView, StageView};
use crate::worker::device::{Firmwares, Power};
use crate::worker::{Snapshot, Status};

/// The Device page and the sidebar.
pub fn device(snapshot: &Snapshot) -> DeviceView {
    let mut view = DeviceView {
        connection: connection(snapshot.mode).into(),
        firmware: firmware(&snapshot.firmwares).into(),
        profiles: profile_summary(&snapshot.backup).into(),
        energy_saving: match snapshot.energy_saving {
            Some(true) => "On",
            Some(false) => "Off",
            None => "unknown",
        }
        .into(),
        ..DeviceView::default()
    };
    apply_status(&mut view, &snapshot.status);
    view
}

/// Updates battery and signal.
pub fn apply_status(view: &mut DeviceView, status: &Status) {
    view.battery = battery(status.power).into();
    view.battery_level = status
        .power
        .and_then(|power| power.percent)
        .map_or(-1.0, |percent| f32::from(percent) / 100.0);
    view.signal = status
        .signal
        .map_or_else(|| "–".to_owned(), |percent| format!("{percent} %"))
        .into();
}

/// The effects in the order of `Lighting.effects`.
pub const EFFECTS: [Effect; 8] = [
    Effect::Off,
    Effect::Static,
    Effect::Breathing,
    Effect::Heartbeat,
    Effect::Wave,
    Effect::Blinking,
    Effect::Aimo,
    Effect::Battery,
];
/// The rates in the order of `Performance.rates`.
pub const RATES: [PollingRate; 4] = [
    PollingRate::Hz125,
    PollingRate::Hz250,
    PollingRate::Hz500,
    PollingRate::Hz1000,
];
/// The distances in the order of `Performance.lift-offs`.
pub const LIFT_OFFS: [LiftOff; 3] = [LiftOff::VeryLow, LiftOff::Low, LiftOff::Custom];
/// The LED time-out shown for "never", in case it is switched on.
pub const DEFAULT_TIMEOUT_MINUTES: u8 = 15;

/// The position of `value` in `list`, or -1 (Slint's "nothing selected").
fn index_of<T: PartialEq>(list: &[T], value: &T) -> i32 {
    list.iter()
        .position(|candidate| candidate == value)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

/// The entry of `list` at a Slint index, if there is one.
pub fn at<T: Copy>(list: &[T], index: i32) -> Option<T> {
    usize::try_from(index)
        .ok()
        .and_then(|index| list.get(index).copied())
}

pub fn colour(rgb: Rgb) -> Color {
    let [red, green, blue] = rgb.to_bytes();
    Color::from_rgb_u8(red, green, blue)
}

pub fn rgb(colour: Color) -> Rgb {
    Rgb::new(colour.red(), colour.green(), colour.blue())
}

/// The profile bar.
pub fn profiles(backup: &Backup) -> Vec<ProfileView> {
    backup
        .profiles
        .iter()
        .map(|profile| {
            let number: u8 = profile.settings.profile();
            let shown: Option<Rgb> = profile_color(&profile.entry);
            ProfileView {
                number: i32::from(number),
                color: shown.map_or_else(Color::default, colour),
                has_color: shown.is_some(),
                active: number == backup.active_profile,
            }
        })
        .collect()
}

/// The DPI stages of the Performance page.
pub fn stages(sensor: &Sensor) -> Vec<StageView> {
    (1..=DPI_STAGE_COUNT)
        .map(|stage| {
            let dpi: Option<u16> = sensor.dpi(stage).ok().flatten();
            StageView {
                enabled: dpi.is_some(),
                dpi: dpi.map_or(0, i32::from),
                auto: sensor.dpi_auto(stage).unwrap_or(false),
                active: sensor.active_stage() == stage,
                edits: 0,
            }
        })
        .collect()
}

/// The rest of the Performance page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorView {
    pub rate: i32,
    pub lift_off: i32,
    pub angle_snapping: bool,
    pub led_timeout: bool,
    pub led_timeout_minutes: i32,
    pub led_timeout_effect: i32,
}

pub fn sensor(sensor: &Sensor) -> SensorView {
    let (led_timeout, minutes, effect) = match sensor.led_timeout() {
        LedTimeout::Never => (false, DEFAULT_TIMEOUT_MINUTES, 0),
        LedTimeout::After { minutes, effect } => {
            (true, minutes, index_of(&LedTimeout::EFFECTS, &effect))
        }
    };
    SensorView {
        rate: sensor
            .polling_rate()
            .map_or(-1, |rate| index_of(&RATES, &rate)),
        lift_off: index_of(&LIFT_OFFS, &sensor.lift_off()),
        angle_snapping: sensor.angle_snapping(),
        led_timeout,
        led_timeout_minutes: i32::from(minutes),
        led_timeout_effect: effect,
    }
}

/// The Lighting page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightingView {
    pub effect: i32,
    pub brightness: f32,
    pub speed: f32,
    pub left: Color,
    pub right: Color,
    pub left_intensity: f32,
    pub right_intensity: f32,
}

pub fn lighting(settings: &ProfileSettings) -> LightingView {
    let lighting = &settings.lighting;
    LightingView {
        effect: index_of(&EFFECTS, &lighting.effect()),
        brightness: f32::from(lighting.brightness()),
        speed: f32::from(lighting.speed()),
        left: colour(lighting.left()),
        right: colour(lighting.right()),
        left_intensity: f32::from(lighting.left_intensity()),
        right_intensity: f32::from(lighting.right_intensity()),
    }
}

fn connection(mode: ConnectionMode) -> &'static str {
    match mode {
        ConnectionMode::Wireless => "Wireless (dongle)",
        ConnectionMode::Wired => "USB cable",
    }
}

fn firmware(firmwares: &Firmwares) -> String {
    match firmwares.dongle {
        Some(dongle) => format!("{}, dongle {dongle}", firmwares.mouse),
        None => firmwares.mouse.to_string(),
    }
}

fn battery(power: Option<Power>) -> String {
    let Some(power) = power else {
        return "unknown".to_owned();
    };
    let level: String = power
        .percent
        .map_or_else(|| "unknown".to_owned(), |percent| format!("{percent} %"));
    if power.charging {
        format!("{level}, charging")
    } else if power.cable_power {
        format!("{level}, on the cable")
    } else {
        level
    }
}

fn profile_summary(backup: &Backup) -> String {
    let count: usize = backup.profiles.len();
    let noun: &str = if count == 1 { "profile" } else { "profiles" };
    format!("{count} {noun}, profile {} active", backup.active_profile)
}

#[cfg(test)]
mod tests {
    use konectl::Firmware;

    use super::*;
    use crate::worker::demo::DemoState;

    fn snapshot() -> Snapshot {
        let demo = DemoState::new();
        Snapshot {
            mode: ConnectionMode::Wireless,
            firmwares: Firmwares {
                mouse: Firmware {
                    major: 4,
                    minor: 32,
                },
                dongle: Some(Firmware {
                    major: 4,
                    minor: 19,
                }),
            },
            backup: demo.backup,
            energy_saving: Some(true),
            status: Status {
                power: Some(demo.power),
                signal: Some(69),
            },
        }
    }

    #[test]
    fn describes_the_mouse() {
        let view: DeviceView = device(&snapshot());
        assert_eq!(view.connection, "Wireless (dongle)");
        assert_eq!(view.firmware, "4.32, dongle 4.19");
        assert_eq!(view.battery, "87 %");
        assert!((view.battery_level - 0.87).abs() < f32::EPSILON);
        assert_eq!(view.signal, "69 %");
        assert_eq!(view.profiles, "3 profiles, profile 1 active");
        assert_eq!(view.energy_saving, "On");
    }

    #[test]
    fn describes_the_profiles_and_their_settings() {
        let backup: Backup = DemoState::new().backup;
        let profiles: Vec<ProfileView> = profiles(&backup);
        assert_eq!(profiles.len(), 3);
        assert!(profiles[0].active && profiles[0].has_color);
        assert_eq!(profiles[1].color, Color::from_rgb_u8(0xe0, 0x1b, 0x24));

        let settings: ProfileSettings = backup.profiles[0].settings;
        let stages: Vec<StageView> = stages(&settings.sensor);
        assert_eq!(
            stages.iter().map(|stage| stage.dpi).collect::<Vec<i32>>(),
            [800, 1200, 1600, 3200, 0]
        );
        assert!(stages[1].active && !stages[4].enabled);

        let shown: SensorView = sensor(&settings.sensor);
        assert_eq!(at(&RATES, shown.rate), Some(PollingRate::Hz1000));
        assert_eq!(at(&LIFT_OFFS, shown.lift_off), Some(LiftOff::Low));
        assert!(shown.led_timeout);
        assert_eq!(shown.led_timeout_minutes, 15);

        let lit: LightingView = lighting(&settings);
        assert_eq!(at(&EFFECTS, lit.effect), Some(Effect::Wave));
        assert_eq!(rgb(lit.left), Rgb::new(0x35, 0x84, 0xe4));
        assert_eq!(at(&EFFECTS, -1), None);
    }

    #[test]
    fn describes_charging_and_the_cable() {
        let mut snapshot: Snapshot = snapshot();
        snapshot.mode = ConnectionMode::Wired;
        snapshot.firmwares.dongle = None;
        snapshot.status = Status {
            power: Some(Power {
                percent: Some(40),
                charging: true,
                cable_power: true,
            }),
            signal: None,
        };
        let view: DeviceView = device(&snapshot);
        assert_eq!(view.connection, "USB cable");
        assert_eq!(view.firmware, "4.32");
        assert_eq!(view.battery, "40 %, charging");
        assert_eq!(view.signal, "–");

        let mut status: Status = snapshot.status;
        status.power = Some(Power {
            percent: Some(100),
            charging: false,
            cable_power: true,
        });
        let mut view: DeviceView = view;
        apply_status(&mut view, &status);
        assert_eq!(view.battery, "100 %, on the cable");

        apply_status(
            &mut view,
            &Status {
                power: None,
                signal: None,
            },
        );
        assert_eq!(view.battery, "unknown");
        assert!(view.battery_level < 0.0);
    }
}
