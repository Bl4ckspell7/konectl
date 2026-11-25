//! One change the pages make to the settings of a profile. Applied to a copy
//! of the settings, so the library's checks decide what is allowed.

use std::time::Duration;

use konectl::{Effect, LedTimeout, LiftOff, PollingRate, ProfileSettings, Rgb};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Sets the DPI of a stage (1–5), or disables it with `None`.
    Dpi {
        stage: u8,
        dpi: Option<u16>,
    },
    DpiAuto {
        stage: u8,
        auto: bool,
    },
    ActiveStage(u8),
    PollingRate(PollingRate),
    LiftOff(LiftOff),
    AngleSnapping(bool),
    LedTimeout(LedTimeout),
    /// The minutes of the LED time-out; the effect after it stays (off if
    /// there was no time-out).
    LedTimeoutMinutes(u8),
    Effect(Effect),
    Brightness(u8),
    Speed(u8),
    Left(Rgb),
    Right(Rgb),
    LeftIntensity(u8),
    RightIntensity(u8),
}

/// How long the worker waits for more edits after a click before it writes.
const CLICK_SETTLE: Duration = Duration::from_millis(120);
/// The same for a number being typed: every key press that gives a valid
/// number is an edit, and the digits before the last are not meant.
const TYPING_SETTLE: Duration = Duration::from_millis(1500);

impl Edit {
    pub fn apply(self, settings: &mut ProfileSettings) -> konectl::Result<()> {
        let sensor = &mut settings.sensor;
        let lighting = &mut settings.lighting;
        match self {
            Self::Dpi { stage, dpi } => sensor.set_dpi(stage, dpi),
            Self::DpiAuto { stage, auto } => sensor.set_dpi_auto(stage, auto),
            Self::ActiveStage(stage) => sensor.set_active_stage(stage),
            Self::PollingRate(rate) => {
                sensor.set_polling_rate(rate);
                Ok(())
            }
            Self::LiftOff(distance) => sensor.set_lift_off(distance),
            Self::AngleSnapping(enabled) => {
                sensor.set_angle_snapping(enabled);
                Ok(())
            }
            Self::LedTimeout(timeout) => sensor.set_led_timeout(timeout),
            Self::LedTimeoutMinutes(minutes) => {
                let effect: Effect = match sensor.led_timeout() {
                    LedTimeout::Never => Effect::Off,
                    LedTimeout::After { effect, .. } => effect,
                };
                sensor.set_led_timeout(LedTimeout::After { minutes, effect })
            }
            Self::Effect(effect) => lighting.set_effect(effect),
            Self::Brightness(percent) => lighting.set_brightness(percent),
            Self::Speed(speed) => lighting.set_speed(speed),
            Self::Left(colour) => {
                lighting.set_left(colour);
                Ok(())
            }
            Self::Right(colour) => {
                lighting.set_right(colour);
                Ok(())
            }
            Self::LeftIntensity(percent) => lighting.set_left_intensity(percent),
            Self::RightIntensity(percent) => lighting.set_right_intensity(percent),
        }
    }

    /// How long to wait for further edits before writing this one.
    pub fn settle(self) -> Duration {
        match self {
            Self::Dpi { dpi: Some(_), .. } | Self::LedTimeoutMinutes(_) => TYPING_SETTLE,
            _ => CLICK_SETTLE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::demo::DemoState;

    fn settings() -> ProfileSettings {
        DemoState::new().backup.profiles[0].settings
    }

    #[test]
    fn edits_go_through_the_library_checks() {
        let mut settings: ProfileSettings = settings();
        Edit::Dpi {
            stage: 5,
            dpi: Some(6400),
        }
        .apply(&mut settings)
        .expect("valid DPI");
        assert_eq!(settings.sensor.dpi(5).expect("stage"), Some(6400));
        assert!(
            Edit::Dpi {
                stage: 5,
                dpi: Some(6420)
            }
            .apply(&mut settings)
            .is_err()
        );
        // The active stage cannot be disabled.
        let active: u8 = settings.sensor.active_stage();
        assert!(
            Edit::Dpi {
                stage: active,
                dpi: None
            }
            .apply(&mut settings)
            .is_err()
        );
        Edit::Left(Rgb::new(1, 2, 3))
            .apply(&mut settings)
            .expect("colour");
        assert_eq!(settings.lighting.left(), Rgb::new(1, 2, 3));
    }

    #[test]
    fn typed_numbers_wait_longer_than_clicks() {
        assert!(
            Edit::Dpi {
                stage: 1,
                dpi: Some(800)
            }
            .settle()
                > Edit::AngleSnapping(true).settle()
        );
        assert_eq!(
            Edit::Dpi {
                stage: 1,
                dpi: None
            }
            .settle(),
            CLICK_SETTLE
        );
    }

    #[test]
    fn minutes_keep_the_effect_after_the_time_out() {
        let mut settings: ProfileSettings = settings();
        Edit::LedTimeout(LedTimeout::After {
            minutes: 5,
            effect: Effect::Breathing,
        })
        .apply(&mut settings)
        .expect("time-out");
        Edit::LedTimeoutMinutes(20)
            .apply(&mut settings)
            .expect("minutes");
        assert_eq!(
            settings.sensor.led_timeout(),
            LedTimeout::After {
                minutes: 20,
                effect: Effect::Breathing
            }
        );
    }
}
