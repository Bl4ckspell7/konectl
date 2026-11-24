//! What the pages show, made from what the worker read. Pure, so the wording
//! is tested without a window.

use konectl::{Backup, ConnectionMode};

use crate::generated::DeviceView;
use crate::worker::device::{Firmwares, Power};
use crate::worker::{Snapshot, Status};

/// The Device page and the sidebar.
pub fn device(snapshot: &Snapshot) -> DeviceView {
    let mut view = DeviceView {
        connection: connection(snapshot.mode).into(),
        firmware: firmware(&snapshot.firmwares).into(),
        profiles: profiles(&snapshot.backup).into(),
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

fn profiles(backup: &Backup) -> String {
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
