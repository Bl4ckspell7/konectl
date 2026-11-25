//! The Lighting page.

use konectl::ProfileSettings;
use slint::{Color, ComponentHandle as _};

use super::send_for_profile;
use crate::edit::Edit;
use crate::generated::{AppWindow, Lighting};
use crate::view::{self, EFFECTS};
use crate::worker::{Request, Requests};

/// A slider value as the byte the mouse takes.
fn percent(value: i32) -> Option<u8> {
    u8::try_from(value).ok()
}

pub fn install(ui: &AppWindow, requests: &Requests) {
    let page = ui.global::<Lighting>();
    let edit = |make: fn(i32) -> Option<Edit>| {
        let (window, sender) = (ui.as_weak(), requests.clone());
        move |value: i32| {
            send_for_profile(&window, &sender, |profile| {
                make(value).map(|edit| Request::Edit { profile, edit })
            });
        }
    };
    let colour = |make: fn(Color) -> Edit| {
        let (window, sender) = (ui.as_weak(), requests.clone());
        move |chosen: Color| {
            send_for_profile(&window, &sender, |profile| {
                Some(Request::Edit {
                    profile,
                    edit: make(chosen),
                })
            });
        }
    };

    page.on_set_effect(edit(|index| Some(Edit::Effect(view::at(&EFFECTS, index)?))));
    page.on_set_brightness(edit(|value| Some(Edit::Brightness(percent(value)?))));
    page.on_set_speed(edit(|value| Some(Edit::Speed(percent(value)?))));
    page.on_set_left_intensity(edit(|value| Some(Edit::LeftIntensity(percent(value)?))));
    page.on_set_right_intensity(edit(|value| Some(Edit::RightIntensity(percent(value)?))));
    page.on_set_left(colour(|chosen| Edit::Left(view::rgb(chosen))));
    page.on_set_right(colour(|chosen| Edit::Right(view::rgb(chosen))));
}

/// Shows the lighting of the selected profile.
pub fn show(ui: &AppWindow, settings: &ProfileSettings) {
    let page = ui.global::<Lighting>();
    let shown = view::lighting(settings);
    page.set_effect(shown.effect);
    page.set_brightness(shown.brightness);
    page.set_speed(shown.speed);
    page.set_left(shown.left);
    page.set_right(shown.right);
    page.set_left_intensity(shown.left_intensity);
    page.set_right_intensity(shown.right_intensity);
}
