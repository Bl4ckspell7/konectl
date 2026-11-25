//! The button functions the Buttons page offers, grouped like the CLI help,
//! with names for people. Which ones a button takes decides the library:
//! every entry is tried with `Buttons::set_action` on a copy.

use konectl::buttons::HostFunction;
use konectl::{ButtonAction, Buttons, Layer, MacroMode};

/// Buttons, as the CLI numbers them.
pub const BUTTON_NAMES: [&str; 8] = [
    "Left button",
    "Right button",
    "Wheel click",
    "Wheel up",
    "Wheel down",
    "Side button, front",
    "Side button, rear",
    "Profile button",
];

/// How a group lets the user choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A list of fixed functions.
    List,
    /// A key with modifiers, typed.
    Key,
    /// Easy-Aim with a DPI.
    EasyAim,
    /// A macro, typed.
    Macro,
}

/// A group of functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: &'static str,
    pub kind: Kind,
    /// Label and function, for [`Kind::List`].
    pub functions: Vec<(&'static str, ButtonAction)>,
}

/// A key and a DPI that stand for "any key" and "any DPI" when checking
/// whether a button takes keys or Easy-Aim at all.
const SAMPLE_KEY: &str = "key:a";
const SAMPLE_EASY_AIM_DPI: u16 = 400;

fn parsed(name: &str) -> ButtonAction {
    name.parse().expect("the catalogue uses valid names")
}

/// All groups with all functions, before checking what a button takes.
/// `profiles` are the profiles in use (switching to another makes no sense).
fn catalogue(profiles: &[u8]) -> Vec<Group> {
    let list = |name: &'static str, functions: Vec<(&'static str, &str)>| Group {
        name,
        kind: Kind::List,
        functions: functions
            .into_iter()
            .map(|(label, action)| (label, parsed(action)))
            .collect(),
    };
    let presets: Vec<(&'static str, &str)> = [
        (1, "Profile 1", "profile:1"),
        (2, "Profile 2", "profile:2"),
        (3, "Profile 3", "profile:3"),
    ]
    .into_iter()
    .filter(|(number, _, _)| profiles.contains(number))
    .map(|(_, label, action)| (label, action))
    .collect();
    let mut profile_functions: Vec<(&'static str, &str)> = vec![
        ("Next profile, from the last to the first", "profile-cycle"),
        ("Next profile", "profile-next"),
        ("Previous profile", "profile-previous"),
    ];
    profile_functions.extend(presets);
    vec![
        list(
            "Mouse",
            vec![
                ("Click", "click"),
                ("Menu (right click)", "menu"),
                ("Universal scroll (middle click)", "universal-scroll"),
                ("Double click", "double-click"),
                ("Back", "back"),
                ("Forward", "forward"),
                ("Scroll up", "scroll-up"),
                ("Scroll down", "scroll-down"),
            ],
        ),
        Group {
            name: "Key",
            kind: Kind::Key,
            functions: Vec::new(),
        },
        list(
            "Media and browser",
            vec![
                ("Mute", "mute"),
                ("Volume up", "volume-up"),
                ("Volume down", "volume-down"),
                ("Play / pause", "play-pause"),
                ("Stop", "stop"),
                ("Next track", "next-track"),
                ("Previous track", "previous-track"),
                ("Browser search", "browser-search"),
                ("Browser stop", "browser-stop"),
                ("Browser refresh", "browser-refresh"),
            ],
        ),
        list(
            "Computer",
            vec![
                ("Sleep", "system:sleep"),
                ("Wake up", "system:wake"),
                ("Shut down", "system:shut-down"),
            ],
        ),
        list(
            "DPI",
            vec![
                ("Next DPI stage, from the last to the first", "dpi-cycle"),
                ("DPI up", "dpi-up"),
                ("DPI down", "dpi-down"),
                ("Easy-Aim: 200 DPI while held", "easy-aim-200"),
                ("Easy-Aim: 900 DPI while held", "easy-aim-900"),
            ],
        ),
        Group {
            name: "Easy-Aim with another DPI",
            kind: Kind::EasyAim,
            functions: Vec::new(),
        },
        list("Profiles", profile_functions),
        list(
            "Easy-Shift and Easy-Wheel",
            vec![
                ("Easy-Shift: hold for the second layer", "easy-shift"),
                ("Easy-Wheel: hold and turn for the DPI", "easy-wheel-dpi"),
                (
                    "Easy-Wheel: hold and turn for the volume",
                    "easy-wheel-volume",
                ),
                (
                    "Easy-Wheel: hold and turn to switch windows",
                    "easy-wheel-alt-tab",
                ),
                (
                    "Easy-Wheel: hold and turn for Flip 3D",
                    "easy-wheel-flip-3d",
                ),
            ],
        ),
        Group {
            name: "Macro",
            kind: Kind::Macro,
            functions: Vec::new(),
        },
        list("Off", vec![("Disabled", "disabled")]),
    ]
}

fn takes(buttons: &Buttons, button: u8, layer: Layer, action: ButtonAction) -> bool {
    let mut copy: Buttons = *buttons;
    copy.set_action(button, layer, action).is_ok()
}

/// The groups `button` takes in `layer`, each with only the functions it
/// takes; empty when the button takes none (e.g. button 8 in Easy-Shift).
pub fn offered(buttons: &Buttons, button: u8, layer: Layer, profiles: &[u8]) -> Vec<Group> {
    catalogue(profiles)
        .into_iter()
        .filter_map(|mut group| {
            let sample: Option<ButtonAction> = match group.kind {
                Kind::List => None,
                Kind::Key => Some(parsed(SAMPLE_KEY)),
                Kind::EasyAim => Some(ButtonAction::EasyAim(SAMPLE_EASY_AIM_DPI)),
                Kind::Macro => Some(ButtonAction::Macro(MacroMode::OnPress { repeat: 1 })),
            };
            match sample {
                Some(action) => takes(buttons, button, layer, action).then_some(group),
                None => {
                    group
                        .functions
                        .retain(|(_, action)| takes(buttons, button, layer, *action));
                    (!group.functions.is_empty()).then_some(group)
                }
            }
        })
        .collect()
}

/// Where `action` is in `groups`: group index, and the function's index in
/// a list group.
pub fn position(groups: &[Group], action: ButtonAction) -> Option<(usize, Option<usize>)> {
    let kind: Option<Kind> = match action {
        ButtonAction::Key(_) => Some(Kind::Key),
        ButtonAction::EasyAim(_) => Some(Kind::EasyAim),
        ButtonAction::Macro(_) => Some(Kind::Macro),
        _ => None,
    };
    groups
        .iter()
        .enumerate()
        .find_map(|(index, group)| match kind {
            Some(kind) => (group.kind == kind).then_some((index, None)),
            None => group
                .functions
                .iter()
                .position(|(_, candidate)| *candidate == action)
                .map(|function| (index, Some(function))),
        })
}

/// The function of a button, for people.
pub fn describe(action: ButtonAction) -> String {
    let named = catalogue(&[1, 2, 3])
        .into_iter()
        .flat_map(|group| group.functions)
        .find(|(_, candidate)| *candidate == action)
        .map(|(label, _)| label.to_owned());
    if let Some(label) = named {
        return label;
    }
    match action {
        ButtonAction::Key(key) => format!("Key {key}"),
        ButtonAction::EasyAim(dpi) => format!("Easy-Aim: {dpi} DPI while held"),
        ButtonAction::Macro(mode) => match mode {
            MacroMode::OnPress { repeat: 1 } => "Macro".to_owned(),
            MacroMode::OnPress { repeat } => format!("Macro, played {repeat} times"),
            MacroMode::WhilePressed => "Macro, while held".to_owned(),
            MacroMode::Toggle => "Macro, on and off".to_owned(),
            MacroMode::Other(_) => "Macro, unknown mode".to_owned(),
        },
        ButtonAction::Host(HostFunction::Swarm(_)) => {
            format!("{action} (needs the official software)")
        }
        ButtonAction::FactoryDefault => "Factory default".to_owned(),
        // Other consumer usages, profiles above 3, unknown functions.
        _ => action.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::demo::DemoState;

    fn buttons() -> Buttons {
        DemoState::new().backup.profiles[0]
            .buttons
            .expect("the demo has buttons")
    }

    fn names(groups: &[Group]) -> Vec<&'static str> {
        groups.iter().map(|group| group.name).collect()
    }

    #[test]
    fn every_catalogue_name_parses() {
        let groups: Vec<Group> = catalogue(&[1, 2, 3]);
        assert_eq!(groups.len(), 10);
        assert!(groups.iter().any(|group| group.functions.len() == 10));
    }

    #[test]
    fn the_profile_button_takes_only_dpi_and_profiles() {
        let groups: Vec<Group> = offered(&buttons(), 8, Layer::Standard, &[1, 2, 3]);
        assert_eq!(names(&groups), ["DPI", "Profiles"]);
        // Easy-Aim is held: not on button 8.
        assert_eq!(groups[0].functions.len(), 3);
        assert!(offered(&buttons(), 8, Layer::EasyShift, &[1, 2, 3]).is_empty());
    }

    #[test]
    fn the_wheel_takes_no_held_functions() {
        let groups: Vec<Group> = offered(&buttons(), 4, Layer::Standard, &[1, 2]);
        assert!(!names(&groups).contains(&"Easy-Aim with another DPI"));
        assert!(!names(&groups).contains(&"Macro"));
        let profiles = groups
            .iter()
            .find(|group| group.name == "Profiles")
            .expect("profiles");
        // Only the profiles in use.
        assert!(
            profiles
                .functions
                .iter()
                .all(|(label, _)| *label != "Profile 3")
        );
    }

    #[test]
    fn a_side_button_takes_keys_and_macros() {
        let groups: Vec<Group> = offered(&buttons(), 7, Layer::Standard, &[1, 2, 3]);
        assert!(names(&groups).contains(&"Key"));
        assert!(names(&groups).contains(&"Macro"));
        let back: ButtonAction = "back".parse().expect("back");
        let (group, function) = position(&groups, back).expect("offered");
        assert_eq!(groups[group].name, "Mouse");
        assert_eq!(groups[group].functions[function.expect("listed")].0, "Back");
        let key: ButtonAction = "key:ctrl+c".parse().expect("key");
        assert_eq!(
            position(&groups, key).map(|(group, _)| groups[group].kind),
            Some(Kind::Key)
        );
    }

    #[test]
    fn functions_are_described_for_people() {
        assert_eq!(describe("volume-up".parse().expect("named")), "Volume up");
        assert_eq!(describe("key:ctrl+c".parse().expect("key")), "Key ctrl+c");
        assert_eq!(
            describe(ButtonAction::Macro(MacroMode::OnPress { repeat: 3 })),
            "Macro, played 3 times"
        );
        assert_eq!(
            describe("easy-aim:450".parse().expect("aim")),
            "Easy-Aim: 450 DPI while held"
        );
    }
}
