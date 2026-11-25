//! The Performance page.

use std::rc::Rc;

use konectl::settings::{DPI_MAX, DPI_MIN, DPI_STEP};
use konectl::{Effect, LedTimeout, Sensor};
use slint::{ComponentHandle as _, Model as _, ModelRc, VecModel};

use super::{SharedModel, lock, send_for_profile};
use crate::edit::Edit;
use crate::generated::{AppWindow, Performance, StageView};
use crate::view::{self, DEFAULT_TIMEOUT_MINUTES, LIFT_OFFS, RATES};
use crate::worker::{Request, Requests};

/// The DPI the mouse can take that is nearest to a typed one.
fn dpi(typed: i32) -> u16 {
    let step: i32 = i32::from(DPI_STEP);
    let rounded: i32 = (typed + step / 2) / step * step;
    u16::try_from(rounded.clamp(i32::from(DPI_MIN), i32::from(DPI_MAX))).unwrap_or(DPI_MIN)
}

/// A stage switched on again starts with this DPI.
const ENABLED_DPI: u16 = 800;

/// The time-out from the switch and the effect list (the minutes change
/// with [`Edit::LedTimeoutMinutes`]).
fn led_timeout(on: bool, minutes: i32, effect: i32) -> LedTimeout {
    if !on {
        return LedTimeout::Never;
    }
    LedTimeout::After {
        minutes: u8::try_from(minutes.clamp(1, 255)).unwrap_or(DEFAULT_TIMEOUT_MINUTES),
        effect: view::at(&LedTimeout::EFFECTS, effect).unwrap_or(Effect::Off),
    }
}

fn stage(stage: i32) -> Option<u8> {
    u8::try_from(stage).ok()
}

/// Counts an edit of a stage row (1–5), see `StageView.edits`.
fn count_edit(model: &SharedModel, stage: i32) {
    let Some(index) = usize::try_from(stage)
        .ok()
        .and_then(|stage| stage.checked_sub(1))
    else {
        return;
    };
    if let Some(edits) = lock(model).stage_edits.get_mut(index) {
        *edits += 1;
    }
}

pub fn install(ui: &AppWindow, requests: &Requests, model: &SharedModel) {
    let page = ui.global::<Performance>();
    let edit = |make: fn(i32, i32) -> Option<Edit>| {
        let (window, sender) = (ui.as_weak(), requests.clone());
        move |first: i32, second: i32| {
            send_for_profile(&window, &sender, |profile| {
                make(first, second).map(|edit| Request::Edit { profile, edit })
            });
        }
    };
    // Edits of a stage row are counted, so the row is shown again afterwards.
    let stage_edit = |make: fn(i32, i32) -> Option<Edit>| {
        let send = edit(make);
        let model: SharedModel = model.clone();
        move |number: i32, value: i32| {
            count_edit(&model, number);
            send(number, value);
        }
    };

    page.on_set_dpi(stage_edit(|number, typed| {
        Some(Edit::Dpi {
            stage: stage(number)?,
            dpi: Some(dpi(typed)),
        })
    }));
    let set_enabled = stage_edit(|number, on| {
        Some(Edit::Dpi {
            stage: stage(number)?,
            dpi: (on != 0).then_some(ENABLED_DPI),
        })
    });
    page.on_set_stage_enabled(move |number, on| set_enabled(number, i32::from(on)));
    let set_auto = stage_edit(|number, auto| {
        Some(Edit::DpiAuto {
            stage: stage(number)?,
            auto: auto != 0,
        })
    });
    page.on_set_dpi_auto(move |number, auto| set_auto(number, i32::from(auto)));
    let set_active = edit(|number, _| Some(Edit::ActiveStage(stage(number)?)));
    page.on_set_active_stage(move |number| set_active(number, 0));
    let set_rate = edit(|index, _| Some(Edit::PollingRate(view::at(&RATES, index)?)));
    page.on_set_rate(move |index| set_rate(index, 0));
    let set_lift_off = edit(|index, _| Some(Edit::LiftOff(view::at(&LIFT_OFFS, index)?)));
    page.on_set_lift_off(move |index| set_lift_off(index, 0));
    let set_snapping = edit(|on, _| Some(Edit::AngleSnapping(on != 0)));
    page.on_set_angle_snapping(move |on| set_snapping(i32::from(on), 0));
    let set_minutes = edit(|minutes, _| {
        Some(Edit::LedTimeoutMinutes(
            u8::try_from(minutes.clamp(1, 255)).ok()?,
        ))
    });
    page.on_set_led_timeout_minutes(move |minutes| set_minutes(minutes, 0));

    let (window, sender) = (ui.as_weak(), requests.clone());
    page.on_set_led_timeout(move |on, minutes, effect| {
        send_for_profile(&window, &sender, |profile| {
            Some(Request::Edit {
                profile,
                edit: Edit::LedTimeout(led_timeout(on, minutes, effect)),
            })
        });
    });
}

/// Shows the sensor settings of the selected profile; `edits` counts the
/// edits of each stage row.
pub fn show(ui: &AppWindow, sensor: &Sensor, edits: &[i32; 5]) {
    let page = ui.global::<Performance>();
    let stages: Vec<StageView> = view::stages(sensor)
        .into_iter()
        .zip(edits)
        .map(|(stage, edits)| StageView {
            edits: *edits,
            ..stage
        })
        .collect();
    // Rows that did not change keep their controls, e.g. a number being typed.
    let current = page.get_stages();
    if current.row_count() == stages.len() {
        for (index, stage) in stages.into_iter().enumerate() {
            if current.row_data(index).as_ref() != Some(&stage) {
                current.set_row_data(index, stage);
            }
        }
    } else {
        page.set_stages(ModelRc::from(Rc::new(VecModel::from(stages))));
    }
    let shown = view::sensor(sensor);
    page.set_rate(shown.rate);
    page.set_lift_off(shown.lift_off);
    page.set_angle_snapping(shown.angle_snapping);
    page.set_led_timeout(shown.led_timeout);
    page.set_led_timeout_minutes(shown.led_timeout_minutes);
    page.set_led_timeout_effect(shown.led_timeout_effect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_dpi_snaps_to_what_the_mouse_takes() {
        assert_eq!(dpi(1234), 1250);
        assert_eq!(dpi(1224), 1200);
        assert_eq!(dpi(10), 50);
        assert_eq!(dpi(25_000), 19_000);
    }

    #[test]
    fn led_time_out_from_the_controls() {
        assert_eq!(led_timeout(false, 5, 2), LedTimeout::Never);
        assert_eq!(
            led_timeout(true, 0, 2),
            LedTimeout::After {
                minutes: 1,
                effect: Effect::Breathing
            }
        );
        assert_eq!(
            led_timeout(true, 15, -1),
            LedTimeout::After {
                minutes: 15,
                effect: Effect::Off
            }
        );
    }

    #[test]
    fn stage_edits_are_counted_per_row() {
        let model = SharedModel::default();
        count_edit(&model, 2);
        count_edit(&model, 2);
        count_edit(&model, 6);
        count_edit(&model, 0);
        assert_eq!(lock(&model).stage_edits, [0, 2, 0, 0, 0]);
    }
}
