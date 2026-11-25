//! The profile bar: selecting, activating, adding, removing and colouring
//! profiles, and the colour field of the colour picker.

use std::rc::Rc;

use konectl::{Backup, Rgb};
use slint::{ComponentHandle as _, ModelRc, VecModel};

use super::{SharedModel, lock, send_for_profile};
use crate::generated::{AppState, AppWindow, ParsedColor};
use crate::view;
use crate::worker::{Request, Requests};

pub fn install(ui: &AppWindow, requests: &Requests, model: &SharedModel) {
    let state = ui.global::<AppState>();

    let window = ui.as_weak();
    let shared: SharedModel = model.clone();
    state.on_profile_selected(move |_| {
        if let Some(ui) = window.upgrade() {
            super::app::show_profile(&ui, &mut lock(&shared));
        }
    });

    let sender: Requests = requests.clone();
    state.on_activate_profile(move |profile| {
        if let Ok(profile) = u8::try_from(profile) {
            sender.send(Request::ActivateProfile(profile));
        }
    });
    let sender: Requests = requests.clone();
    state.on_add_profile(move |copy_from| {
        if let Ok(copy_from) = u8::try_from(copy_from) {
            sender.send(Request::AddProfile { copy_from });
        }
    });
    let sender: Requests = requests.clone();
    state.on_remove_profile(move |profile| {
        if let Ok(profile) = u8::try_from(profile) {
            sender.send(Request::RemoveProfile(profile));
        }
    });

    let (window, sender) = (ui.as_weak(), requests.clone());
    state.on_set_profile_color(move |_, colour| {
        send_for_profile(&window, &sender, |profile| {
            Some(Request::ProfileColour {
                profile,
                colour: Some(view::rgb(colour)),
            })
        });
    });
    let (window, sender) = (ui.as_weak(), requests.clone());
    state.on_clear_profile_color(move |_| {
        send_for_profile(&window, &sender, |profile| {
            Some(Request::ProfileColour {
                profile,
                colour: None,
            })
        });
    });

    state.on_parse_color(|text| match text.trim().parse::<Rgb>() {
        Ok(rgb) => ParsedColor {
            valid: true,
            color: view::colour(rgb),
        },
        Err(_) => ParsedColor::default(),
    });
}

/// Shows the profiles in use and the selected one.
pub fn show(ui: &AppWindow, backup: &Backup) {
    let state = ui.global::<AppState>();
    let profiles = view::profiles(backup);
    let selected = profiles
        .iter()
        .find(|profile| profile.number == state.get_profile())
        .cloned()
        .unwrap_or_default();
    state.set_selected_profile(selected);
    state.set_profiles(ModelRc::from(Rc::new(VecModel::from(profiles))));
}
