//! The Buttons page.

use std::rc::Rc;

use konectl::buttons::{BUTTON_COUNT, HostFunction};
use konectl::{Backup, ButtonAction, Buttons, Layer, Macro, MacroMode};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use super::{Model, SharedModel, lock, selected_profile};
use crate::functions::{self, BUTTON_NAMES, Group, Kind};
use crate::generated::{AppWindow, ButtonPage, ButtonView};
use crate::worker::{Request, Requests};

/// Easy-Aim DPI shown when a button had none before.
const EASY_AIM_DPI: i32 = 400;
/// Index of "several times" in `ButtonPage.macro-modes`.
const MODE_REPEAT: i32 = 1;

fn layer(ui: &AppWindow) -> Layer {
    if ui.global::<ButtonPage>().get_easy_shift() {
        Layer::EasyShift
    } else {
        Layer::Standard
    }
}

fn button(ui: &AppWindow) -> u8 {
    u8::try_from(ui.global::<ButtonPage>().get_button())
        .unwrap_or(1)
        .clamp(1, BUTTON_COUNT)
}

fn profiles(backup: &Backup) -> Vec<u8> {
    backup
        .profiles
        .iter()
        .map(|profile| profile.settings.profile())
        .collect()
}

/// The selected profile's number and buttons.
fn selected_buttons(ui: &AppWindow, model: &Model) -> Option<(u8, Buttons, Vec<u8>)> {
    let backup: &Backup = &model.snapshot.as_ref()?.backup;
    let profile: u8 = selected_profile(ui)?;
    let buttons: Buttons = backup
        .profiles
        .iter()
        .find(|candidate| candidate.settings.profile() == profile)?
        .buttons?;
    Some((profile, buttons, profiles(backup)))
}

/// The group shown: the one the user chose, else the current function's.
fn shown_group(groups: &[Group], chosen: Option<&str>, action: ButtonAction) -> usize {
    chosen
        .and_then(|name| groups.iter().position(|group| group.name == name))
        .or_else(|| functions::position(groups, action).map(|(group, _)| group))
        .unwrap_or(0)
}

/// Shown first in a list that does not have the button's function.
const CHOOSE: &str = "Choose …";

/// The labels of a list group and the one to show: the button's function,
/// or a first "Choose …" entry when the group does not have it.
fn list(group: &Group, action: ButtonAction) -> (Vec<SharedString>, i32) {
    let labels = group
        .functions
        .iter()
        .map(|(label, _)| SharedString::from(*label));
    match group
        .functions
        .iter()
        .position(|(_, candidate)| *candidate == action)
    {
        Some(index) => (labels.collect(), i32::try_from(index).unwrap_or(-1)),
        None => (std::iter::once(CHOOSE.into()).chain(labels).collect(), 0),
    }
}

/// The function at a shown list position (see [`list`]).
fn chosen(group: &Group, action: ButtonAction, index: i32) -> Option<ButtonAction> {
    let has_current: bool = group
        .functions
        .iter()
        .any(|(_, candidate)| *candidate == action);
    let index: usize = usize::try_from(index).ok()?;
    let index: usize = if has_current {
        index
    } else {
        index.checked_sub(1)?
    };
    group.functions.get(index).map(|(_, function)| *function)
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::List => "list",
        Kind::Key => "key",
        Kind::EasyAim => "easy-aim",
        Kind::Macro => "macro",
    }
}

/// Why a button takes no function, or what to know about its function.
fn note(
    buttons: &Buttons,
    button: u8,
    layer: Layer,
    action: ButtonAction,
    groups: &[Group],
) -> String {
    if let ButtonAction::Host(HostFunction::Swarm(_)) = action {
        return "The official software carries this function out; konectl keeps it.".to_owned();
    }
    let only_current: bool = groups.len() == 1
        && matches!(groups[0].functions.as_slice(), [(_, only)] if *only == action);
    if only_current {
        return if button == 1 && layer == Layer::Standard {
            "Button 1 stays a click, so the mouse can always be used.".to_owned()
        } else {
            "Right click has to stay on at least one button: give it to another button first."
                .to_owned()
        };
    }
    if !groups.is_empty() {
        return String::new();
    }
    if layer == Layer::EasyShift && button == BUTTON_COUNT {
        "The profile button has no function while Easy-Shift is held.".to_owned()
    } else if layer == Layer::EasyShift
        && buttons
            .has_easy_shift_function(button)
            .is_ok_and(|has| !has)
    {
        "This is an Easy-Shift button: while it is held, it has no function of its own.".to_owned()
    } else {
        "This button takes no other function here.".to_owned()
    }
}

/// "profile 2" or "profiles 2 and 3": the other profiles that play the macro
/// of `button` in `layer`.
fn macro_shared(backup: &Backup, profile: u8, button: u8, layer: Layer) -> String {
    let numbers: Vec<String> = backup
        .profiles
        .iter()
        .filter(|candidate| candidate.settings.profile() != profile)
        .filter(|candidate| {
            candidate.buttons.is_some_and(|buttons| {
                matches!(buttons.action(button, layer), Ok(ButtonAction::Macro(_)))
            })
        })
        .map(|candidate| candidate.settings.profile().to_string())
        .collect();
    match numbers.as_slice() {
        [] => String::new(),
        [one] => format!("profile {one}"),
        [first @ .., last] => format!("profiles {} and {last}", first.join(", ")),
    }
}

/// Shows the buttons of the selected profile.
pub fn show(ui: &AppWindow, model: &mut Model) {
    let page = ui.global::<ButtonPage>();
    let Some((profile, buttons, in_use)) = selected_buttons(ui, model) else {
        page.set_buttons(ModelRc::default());
        page.set_groups(ModelRc::default());
        page.set_kind(SharedString::new());
        page.set_title(SharedString::new());
        page.set_current(SharedString::new());
        page.set_note("The button assignments of this profile could not be read.".into());
        model.button_shown = None;
        model.button_group = None;
        return;
    };
    let layer: Layer = layer(ui);
    let button: u8 = button(ui);
    let action: ButtonAction = buttons
        .action(button, layer)
        .unwrap_or(ButtonAction::Disabled);

    let rows: Vec<ButtonView> = (1..=BUTTON_COUNT)
        .map(|number| ButtonView {
            number: i32::from(number),
            name: BUTTON_NAMES[usize::from(number - 1)].into(),
            function: buttons
                .action(number, layer)
                .map_or_else(|_| String::new(), functions::describe)
                .into(),
        })
        .collect();
    page.set_buttons(ModelRc::from(Rc::new(VecModel::from(rows))));
    let name: &str = BUTTON_NAMES[usize::from(button - 1)];
    page.set_title(
        match layer {
            Layer::Standard => format!("Button {button}: {name}"),
            Layer::EasyShift => format!("Button {button}: {name}, with Easy-Shift"),
        }
        .into(),
    );
    page.set_current(functions::describe(action).into());

    let groups: Vec<Group> = functions::offered(&buttons, button, layer, &in_use);
    page.set_note(note(&buttons, button, layer, action, &groups).into());
    let names: Vec<SharedString> = groups.iter().map(|group| group.name.into()).collect();
    page.set_groups(ModelRc::from(Rc::new(VecModel::from(names))));
    // Another profile, button or layer: start from the button's own kind.
    let shown_now = (profile, button, layer == Layer::EasyShift, action);
    let other: bool = model.button_shown != Some(shown_now);
    if other {
        model.button_group = None;
    }
    let shown: usize = shown_group(&groups, model.button_group, action);
    page.set_group(i32::try_from(shown).unwrap_or(-1));
    let group: Option<&Group> = groups.get(shown);
    page.set_kind(group.map_or("", |group| kind_name(group.kind)).into());
    let (labels, function) = group.map_or((Vec::new(), -1), |group| list(group, action));
    page.set_functions(ModelRc::from(Rc::new(VecModel::from(labels))));
    page.set_function(function);

    // The editors start from the button's function when another button,
    // layer or profile is shown; otherwise they keep what is being typed.
    if other {
        page.set_key(match action {
            ButtonAction::Key(key) => key.to_string().into(),
            _ => SharedString::new(),
        });
        page.set_key_problem(SharedString::new());
        page.set_easy_aim_dpi(match action {
            ButtonAction::EasyAim(dpi) => i32::from(dpi),
            _ => EASY_AIM_DPI,
        });
        // Typed macro keys stay for the same button: the mouse cannot report
        // them, so after writing they are the only record.
        let same_button: bool =
            model
                .button_shown
                .is_some_and(|(shown_profile, shown_button, easy_shift, _)| {
                    (shown_profile, shown_button, easy_shift)
                        == (profile, button, layer == Layer::EasyShift)
                });
        if !(same_button && matches!(action, ButtonAction::Macro(_))) {
            page.set_macro_keys(SharedString::new());
        }
        page.set_macro_problem(SharedString::new());
        let (mode, repeat): (i32, i32) = match action {
            ButtonAction::Macro(MacroMode::OnPress { repeat }) if repeat > 1 => {
                (MODE_REPEAT, i32::from(repeat))
            }
            ButtonAction::Macro(MacroMode::WhilePressed) => (2, 2),
            ButtonAction::Macro(MacroMode::Toggle) => (3, 2),
            _ => (0, 2),
        };
        page.set_macro_mode(mode);
        page.set_macro_repeat(repeat);
        model.button_shown = Some(shown_now);
    }
    if let Some(snapshot) = &model.snapshot {
        page.set_macro_shared(macro_shared(&snapshot.backup, profile, button, layer).into());
    }
}

/// Builds the request for the selected profile from what the page shows.
type MakeRequest = Box<dyn Fn(&AppWindow, &Model, u8) -> Option<Request>>;

/// The macro mode the controls show.
fn macro_mode(mode: i32, repeat: i32) -> MacroMode {
    match mode {
        MODE_REPEAT => MacroMode::OnPress {
            repeat: u8::try_from(repeat.clamp(2, 99)).unwrap_or(2),
        },
        2 => MacroMode::WhilePressed,
        3 => MacroMode::Toggle,
        _ => MacroMode::OnPress { repeat: 1 },
    }
}

/// What is wrong with a typed key, or nothing.
fn key_problem(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    format!("key:{}", text.trim())
        .parse::<ButtonAction>()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default()
}

/// What is wrong with typed macro keys, or nothing.
fn macro_problem(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    text.parse::<Macro>()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default()
}

pub fn install(ui: &AppWindow, requests: &Requests, model: &SharedModel) {
    let page = ui.global::<ButtonPage>();

    // Selection: shows another button or layer.
    let refresh = |reset_group: bool| {
        let (window, shared) = (ui.as_weak(), model.clone());
        move || {
            if let Some(ui) = window.upgrade() {
                let mut model = lock(&shared);
                if reset_group {
                    model.button_group = None;
                }
                show(&ui, &mut model);
            }
        }
    };
    let (window, shown) = (ui.as_weak(), refresh(true));
    page.on_select(move |number| {
        if let Some(ui) = window.upgrade() {
            ui.global::<ButtonPage>().set_button(number);
        }
        shown();
    });
    let (window, shown) = (ui.as_weak(), refresh(true));
    page.on_show_layer(move |easy_shift| {
        if let Some(ui) = window.upgrade() {
            ui.global::<ButtonPage>().set_easy_shift(easy_shift);
        }
        shown();
    });
    let (window, shared) = (ui.as_weak(), model.clone());
    page.on_choose_group(move |index| {
        let Some(ui) = window.upgrade() else {
            return;
        };
        let mut model = lock(&shared);
        if let Some((_, buttons, in_use)) = selected_buttons(&ui, &model) {
            let groups: Vec<Group> = functions::offered(&buttons, button(&ui), layer(&ui), &in_use);
            model.button_group = usize::try_from(index)
                .ok()
                .and_then(|index| groups.get(index))
                .map(|group| group.name);
        }
        show(&ui, &mut model);
    });

    // Changes: sent to the worker for the selected profile.
    let send = |make: MakeRequest| {
        let (window, shared, sender) = (ui.as_weak(), model.clone(), requests.clone());
        move || {
            let Some(ui) = window.upgrade() else {
                return;
            };
            let model = lock(&shared);
            let Some(profile) = selected_profile(&ui) else {
                return;
            };
            if let Some(request) = make(&ui, &model, profile) {
                sender.send(request);
            }
        }
    };
    let set = |action: ButtonAction, ui: &AppWindow, profile: u8| Request::SetButton {
        profile,
        button: button(ui),
        layer: layer(ui),
        action,
    };

    let (window, shared, sender) = (ui.as_weak(), model.clone(), requests.clone());
    page.on_choose_function(move |index| {
        let Some(ui) = window.upgrade() else {
            return;
        };
        let model = lock(&shared);
        let Some((profile, buttons, in_use)) = selected_buttons(&ui, &model) else {
            return;
        };
        let groups: Vec<Group> = functions::offered(&buttons, button(&ui), layer(&ui), &in_use);
        let current: ButtonAction = buttons
            .action(button(&ui), layer(&ui))
            .unwrap_or(ButtonAction::Disabled);
        let group: i32 = ui.global::<ButtonPage>().get_group();
        let picked: Option<ButtonAction> = usize::try_from(group)
            .ok()
            .and_then(|group| groups.get(group))
            .and_then(|group| chosen(group, current, index));
        if let Some(action) = picked {
            sender.send(set(action, &ui, profile));
        }
    });

    let window = ui.as_weak();
    page.on_check_key(move |text| {
        if let Some(ui) = window.upgrade() {
            ui.global::<ButtonPage>()
                .set_key_problem(key_problem(&text).into());
        }
    });
    let window = ui.as_weak();
    let set_key = send(Box::new(move |ui, _, profile| {
        let text: SharedString = ui.global::<ButtonPage>().get_key();
        match format!("key:{}", text.trim()).parse::<ButtonAction>() {
            Ok(action) => Some(set(action, ui, profile)),
            Err(error) => {
                ui.global::<ButtonPage>()
                    .set_key_problem(error.to_string().into());
                None
            }
        }
    }));
    page.on_set_key(move |text| {
        if let Some(ui) = window.upgrade() {
            ui.global::<ButtonPage>().set_key(text);
        }
        set_key();
    });

    let set_easy_aim = send(Box::new(move |ui, _, profile| {
        let dpi: u16 = u16::try_from(ui.global::<ButtonPage>().get_easy_aim_dpi()).ok()?;
        Some(set(ButtonAction::EasyAim(dpi), ui, profile))
    }));
    page.on_set_easy_aim(move |_| set_easy_aim());

    let window = ui.as_weak();
    page.on_check_macro(move |text| {
        if let Some(ui) = window.upgrade() {
            ui.global::<ButtonPage>()
                .set_macro_problem(macro_problem(&text).into());
        }
    });
    let write_macro = send(Box::new(move |ui, _, profile| {
        let page = ui.global::<ButtonPage>();
        Some(Request::SetMacro {
            profile,
            button: button(ui),
            layer: layer(ui),
            keys: page.get_macro_keys().trim().to_owned(),
            mode: macro_mode(page.get_macro_mode(), page.get_macro_repeat()),
        })
    }));
    page.on_write_macro(write_macro);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::demo::DemoState;

    #[test]
    fn typed_keys_and_macros_are_checked() {
        assert_eq!(key_problem("ctrl+c"), "");
        assert_eq!(key_problem(""), "");
        assert!(!key_problem("ctrl+nothing").is_empty());
        assert_eq!(macro_problem("ctrl+c, 50ms, ctrl+v"), "");
        assert!(!macro_problem("down:a").is_empty());
    }

    #[test]
    fn a_list_without_the_function_starts_with_choose() {
        let named = |name: &str| -> ButtonAction { name.parse().expect("named") };
        let group = Group {
            name: "Media and browser",
            kind: Kind::List,
            functions: vec![("Mute", named("mute")), ("Volume up", named("volume-up"))],
        };
        assert_eq!(list(&group, named("mute")).1, 0);
        assert_eq!(chosen(&group, named("mute"), 1), Some(named("volume-up")));
        let (labels, shown) = list(&group, named("back"));
        assert_eq!((labels[0].as_str(), shown), (CHOOSE, 0));
        assert_eq!(chosen(&group, named("back"), 0), None);
        assert_eq!(chosen(&group, named("back"), 2), Some(named("volume-up")));
    }

    #[test]
    fn macro_modes_from_the_controls() {
        assert_eq!(macro_mode(0, 5), MacroMode::OnPress { repeat: 1 });
        assert_eq!(macro_mode(1, 5), MacroMode::OnPress { repeat: 5 });
        assert_eq!(macro_mode(1, 500), MacroMode::OnPress { repeat: 99 });
        assert_eq!(macro_mode(2, 5), MacroMode::WhilePressed);
        assert_eq!(macro_mode(3, 5), MacroMode::Toggle);
    }

    #[test]
    fn names_the_profiles_that_share_a_macro() {
        let mut backup: Backup = DemoState::new().backup;
        assert_eq!(macro_shared(&backup, 1, 7, Layer::Standard), "");
        for profile in &mut backup.profiles {
            if let Some(buttons) = profile.buttons.as_mut() {
                buttons
                    .set_action(7, Layer::Standard, ButtonAction::Macro(MacroMode::Toggle))
                    .expect("allowed");
            }
        }
        assert_eq!(
            macro_shared(&backup, 1, 7, Layer::Standard),
            "profiles 2 and 3"
        );
        assert_eq!(
            macro_shared(&backup, 2, 7, Layer::Standard),
            "profiles 1 and 3"
        );
    }
}
