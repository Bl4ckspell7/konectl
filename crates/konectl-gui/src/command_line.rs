//! The `konectl` command lines that make the same change as the GUI, shown in
//! the command bar. Every line names the profile (`-p N`), so it does the same
//! whichever profile is active.

use konectl::buttons::BUTTON_COUNT;
use konectl::settings::{DPI_STAGE_COUNT, profile_color};
use konectl::{
    Backup, ButtonAction, Buttons, Effect, Layer, LedTimeout, LiftOff, Lighting, MacroMode,
    ProfileSettings, Rgb, Sensor,
};

/// Command lines for one change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLines {
    pub lines: Vec<String>,
    /// `false` when part of the change has no konectl syntax.
    pub complete: bool,
}

impl CommandLines {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            complete: true,
        }
    }

    fn push(&mut self, line: Option<String>) {
        match line {
            Some(line) => self.lines.push(line),
            None => self.complete = false,
        }
    }

    pub fn extend(&mut self, other: Self) {
        self.lines.extend(other.lines);
        self.complete &= other.complete;
    }
}

/// `rrggbb`, as the CLI takes it (without `#`, which would start a shell comment).
fn hex(colour: Rgb) -> String {
    colour.to_string().trim_start_matches('#').to_owned()
}

fn switch(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

/// The lines that turn the settings `before` into `after` (one profile).
pub fn settings(before: &ProfileSettings, after: &ProfileSettings) -> CommandLines {
    let mut commands = CommandLines::new();
    let profile: u8 = after.profile();
    sensor(&mut commands, &before.sensor, &after.sensor, profile);
    if before.lighting != after.lighting {
        commands.push(lighting(&before.lighting, &after.lighting, profile));
    }
    commands
}

fn sensor(commands: &mut CommandLines, before: &Sensor, after: &Sensor, profile: u8) {
    let stages = 1..=DPI_STAGE_COUNT;
    let dpi = |sensor: &Sensor, stage: u8| sensor.dpi(stage).ok().flatten();
    let auto = |sensor: &Sensor, stage: u8| sensor.dpi_auto(stage).unwrap_or(false);
    // A stage gets its DPI before it becomes active, and is disabled only
    // after another one is active.
    for stage in stages.clone() {
        if let Some(value) = dpi(after, stage)
            && dpi(before, stage) != Some(value)
        {
            commands.push(Some(format!("konectl dpi {stage} {value} -p {profile}")));
        }
    }
    for stage in stages.clone() {
        if dpi(after, stage).is_some() && auto(before, stage) != auto(after, stage) {
            commands.push(Some(format!(
                "konectl dpi-auto {stage} {} -p {profile}",
                switch(auto(after, stage))
            )));
        }
    }
    if before.active_stage() != after.active_stage() {
        commands.push(Some(format!(
            "konectl stage {} -p {profile}",
            after.active_stage()
        )));
    }
    for stage in stages {
        if dpi(after, stage).is_none() && dpi(before, stage).is_some() {
            commands.push(Some(format!("konectl dpi {stage} off -p {profile}")));
        }
    }
    if before.polling_rate() != after.polling_rate() {
        commands.push(
            after
                .polling_rate()
                .map(|rate| format!("konectl rate {} -p {profile}", rate.hz())),
        );
    }
    if before.lift_off() != after.lift_off() {
        let distance: Option<&str> = match after.lift_off() {
            LiftOff::VeryLow => Some("very-low"),
            LiftOff::Low => Some("low"),
            LiftOff::Custom => Some("custom"),
            LiftOff::Other(_) => None,
        };
        commands.push(distance.map(|distance| format!("konectl liftoff {distance} -p {profile}")));
    }
    if before.angle_snapping() != after.angle_snapping() {
        commands.push(Some(format!(
            "konectl angle-snapping {} -p {profile}",
            switch(after.angle_snapping())
        )));
    }
    if before.led_timeout() != after.led_timeout() {
        commands.push(match after.led_timeout() {
            LedTimeout::Never => Some(format!("konectl led-timeout never -p {profile}")),
            LedTimeout::After { minutes, effect } => effect_name(effect)
                .map(|effect| format!("konectl led-timeout {minutes} -e {effect} -p {profile}")),
        });
    }
}

fn effect_name(effect: Effect) -> Option<String> {
    match effect {
        Effect::Other(_) => None,
        known => Some(known.to_string()),
    }
}

/// `konectl lighting` always names the effect; the options keep the values
/// that are not given.
fn lighting(before: &Lighting, after: &Lighting, profile: u8) -> Option<String> {
    let mut line: String = format!("konectl lighting {}", effect_name(after.effect())?);
    let left_changed: bool = before.left() != after.left();
    let right_changed: bool = before.right() != after.right();
    if left_changed && right_changed && after.left() == after.right() {
        line.push_str(&format!(" -c {}", hex(after.left())));
    } else {
        if left_changed {
            line.push_str(&format!(" --left {}", hex(after.left())));
        }
        if right_changed {
            line.push_str(&format!(" --right {}", hex(after.right())));
        }
    }
    if before.brightness() != after.brightness() {
        line.push_str(&format!(" -b {}", after.brightness()));
    }
    if before.speed() != after.speed() {
        line.push_str(&format!(" -s {}", after.speed()));
    }
    let left_changed: bool = before.left_intensity() != after.left_intensity();
    let right_changed: bool = before.right_intensity() != after.right_intensity();
    if left_changed && right_changed && after.left_intensity() == after.right_intensity() {
        line.push_str(&format!(" --intensity {}", after.left_intensity()));
    } else {
        if left_changed {
            line.push_str(&format!(" --left-intensity {}", after.left_intensity()));
        }
        if right_changed {
            line.push_str(&format!(" --right-intensity {}", after.right_intensity()));
        }
    }
    line.push_str(&format!(" -p {profile}"));
    Some(line)
}

/// `text` as one shell word.
fn shell_quote(text: &str) -> String {
    let plain: bool = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ":+,._-/=".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn layer_flag(layer: Layer) -> &'static str {
    match layer {
        Layer::Standard => "",
        Layer::EasyShift => " -e",
    }
}

/// `konectl button`; `None` for functions the CLI cannot set (macros need
/// their keys, see [`macro_button`]).
fn button_line(profile: u8, button: u8, layer: Layer, action: ButtonAction) -> Option<String> {
    match action {
        ButtonAction::Macro(_)
        | ButtonAction::Host(_)
        | ButtonAction::FactoryDefault
        | ButtonAction::Other { .. } => None,
        action => Some(format!(
            "konectl button {button} {}{} -p {profile}",
            shell_quote(&action.to_string()),
            layer_flag(layer)
        )),
    }
}

pub fn button(profile: u8, button: u8, layer: Layer, action: ButtonAction) -> CommandLines {
    let mut commands = CommandLines::new();
    commands.push(button_line(profile, button, layer, action));
    commands
}

/// `konectl button N 'macro:KEYS'` with the keys as typed.
pub fn macro_button(
    profile: u8,
    button: u8,
    layer: Layer,
    keys: &str,
    mode: MacroMode,
) -> CommandLines {
    let mode: Option<String> = match mode {
        MacroMode::OnPress { repeat: 1 } => Some(String::new()),
        MacroMode::OnPress { repeat } => Some(format!(" --repeat {repeat}")),
        MacroMode::WhilePressed => Some(" --while-pressed".to_owned()),
        MacroMode::Toggle => Some(" --toggle".to_owned()),
        _ => None,
    };
    let mut commands = CommandLines::new();
    commands.push(mode.map(|mode| {
        format!(
            "konectl button {button} {}{mode}{} -p {profile}",
            shell_quote(&format!("macro:{keys}")),
            layer_flag(layer)
        )
    }));
    commands
}

/// The lines that turn the assignments `before` into `after`; a macro has no
/// line, its keys are not known.
pub fn buttons(before: &Buttons, after: &Buttons) -> CommandLines {
    let mut commands = CommandLines::new();
    for layer in [Layer::Standard, Layer::EasyShift] {
        for number in 1..=BUTTON_COUNT {
            let (Ok(old), Ok(new)) = (before.action(number, layer), after.action(number, layer))
            else {
                continue;
            };
            if old != new {
                // Only what the CLI accepts (not e.g. keys with right-hand
                // modifiers, which the official software sets).
                let mut copy: Buttons = *before;
                let accepted: bool = copy.set_action(number, layer, new).is_ok();
                commands.push(
                    accepted
                        .then(|| button_line(after.profile(), number, layer, new))
                        .flatten(),
                );
            }
        }
    }
    commands
}

pub fn activate(profile: u8) -> CommandLines {
    one(format!("konectl profile {profile}"))
}

pub fn add_profile(copy_from: u8) -> CommandLines {
    one(format!("konectl profile add --copy-from {copy_from}"))
}

pub fn remove_profile(profile: u8) -> CommandLines {
    one(format!("konectl profile remove {profile}"))
}

pub fn profile_colour(profile: u8, colour: Option<Rgb>) -> CommandLines {
    let colour: String = colour.map_or_else(|| "none".to_owned(), hex);
    one(format!("konectl profile color {profile} {colour}"))
}

fn one(line: String) -> CommandLines {
    CommandLines {
        lines: vec![line],
        complete: true,
    }
}

/// The lines that turn the backup `before` into `after` when both have the
/// same profiles (as an undo does); otherwise only a full restore can.
pub fn backup(before: &Backup, after: &Backup) -> CommandLines {
    let mut commands = CommandLines::new();
    let numbers = |backup: &Backup| -> Vec<u8> {
        backup
            .profiles
            .iter()
            .map(|profile| profile.settings.profile())
            .collect()
    };
    if numbers(before) != numbers(after) {
        commands.complete = false;
        return commands;
    }
    for (old, new) in before.profiles.iter().zip(&after.profiles) {
        commands.extend(settings(&old.settings, &new.settings));
        if profile_color(&old.entry) != profile_color(&new.entry) {
            commands.extend(profile_colour(
                new.settings.profile(),
                profile_color(&new.entry),
            ));
        }
        match (&old.buttons, &new.buttons) {
            (Some(old), Some(new)) => commands.extend(buttons(old, new)),
            (None, None) => {}
            _ => commands.complete = false,
        }
    }
    if before.active_profile != after.active_profile {
        commands.extend(activate(after.active_profile));
    }
    commands
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use konectl::PollingRate;

    use super::*;
    use crate::edit::Edit;
    use crate::worker::demo::DemoState;

    fn profile_1() -> ProfileSettings {
        DemoState::new().backup.profiles[0].settings
    }

    fn after(edits: &[Edit]) -> (ProfileSettings, ProfileSettings) {
        let before: ProfileSettings = profile_1();
        let mut after: ProfileSettings = before;
        for edit in edits {
            edit.apply(&mut after).expect("valid edit");
        }
        (before, after)
    }

    fn lines(edits: &[Edit]) -> Vec<String> {
        let (before, after) = after(edits);
        let commands: CommandLines = settings(&before, &after);
        assert!(commands.complete);
        commands.lines
    }

    /// The values in a generated line parse with the types the CLI uses.
    fn parses_back(line: &str) {
        let words: Vec<&str> = line.split(' ').collect();
        assert_eq!(words[0], "konectl");
        assert_eq!(words[words.len() - 2], "-p");
        assert!(u8::from_str(words[words.len() - 1]).is_ok());
        let value: &str = words[2];
        match words[1] {
            "rate" => assert!(PollingRate::from_str(value).is_ok()),
            "liftoff" => assert!(LiftOff::from_str(value).is_ok()),
            "lighting" => {
                assert!(Effect::from_str(value).is_ok());
                for pair in words.windows(2) {
                    if matches!(pair[0], "-c" | "--left" | "--right") {
                        assert!(Rgb::from_str(pair[1]).is_ok(), "{line}");
                    }
                }
            }
            "dpi" => assert!(value == "off" || u16::from_str(value).is_ok()),
            "dpi-auto" | "angle-snapping" | "stage" | "led-timeout" => {}
            other => panic!("unexpected command {other}"),
        }
    }

    #[test]
    fn sensor_changes() {
        let cases: Vec<(Vec<Edit>, Vec<&str>)> = vec![
            (
                vec![Edit::Dpi {
                    stage: 2,
                    dpi: Some(900),
                }],
                vec!["konectl dpi 2 900 -p 1"],
            ),
            (
                vec![
                    Edit::Dpi {
                        stage: 5,
                        dpi: Some(6400),
                    },
                    Edit::ActiveStage(5),
                    Edit::Dpi {
                        stage: 2,
                        dpi: None,
                    },
                ],
                vec![
                    "konectl dpi 5 6400 -p 1",
                    "konectl stage 5 -p 1",
                    "konectl dpi 2 off -p 1",
                ],
            ),
            (
                vec![Edit::DpiAuto {
                    stage: 1,
                    auto: true,
                }],
                vec!["konectl dpi-auto 1 on -p 1"],
            ),
            (
                vec![Edit::PollingRate(PollingRate::Hz250)],
                vec!["konectl rate 250 -p 1"],
            ),
            (
                vec![Edit::LiftOff(LiftOff::VeryLow)],
                vec!["konectl liftoff very-low -p 1"],
            ),
            (
                vec![Edit::AngleSnapping(true)],
                vec!["konectl angle-snapping on -p 1"],
            ),
            (
                vec![Edit::LedTimeout(LedTimeout::After {
                    minutes: 5,
                    effect: Effect::Breathing,
                })],
                vec!["konectl led-timeout 5 -e breathing -p 1"],
            ),
            (
                vec![Edit::LedTimeout(LedTimeout::Never)],
                vec!["konectl led-timeout never -p 1"],
            ),
        ];
        for (edits, expected) in cases {
            let generated: Vec<String> = lines(&edits);
            assert_eq!(generated, expected, "{edits:?}");
            generated.iter().for_each(|line| parses_back(line));
        }
    }

    #[test]
    fn lighting_changes_name_the_effect_and_only_what_changed() {
        let cases: Vec<(Vec<Edit>, &str)> = vec![
            (
                vec![Edit::Brightness(40)],
                "konectl lighting wave -b 40 -p 1",
            ),
            (
                vec![
                    Edit::Left(Rgb::new(0xff, 0x88, 0)),
                    Edit::Right(Rgb::new(0xff, 0x88, 0)),
                ],
                "konectl lighting wave -c ff8800 -p 1",
            ),
            (
                vec![
                    Edit::Effect(Effect::Static),
                    Edit::Right(Rgb::new(0, 0, 0xff)),
                ],
                "konectl lighting static --right 0000ff -p 1",
            ),
            (
                vec![Edit::Speed(9), Edit::LeftIntensity(50)],
                "konectl lighting wave -s 9 --left-intensity 50 -p 1",
            ),
            (
                vec![Edit::LeftIntensity(30), Edit::RightIntensity(30)],
                "konectl lighting wave --intensity 30 -p 1",
            ),
        ];
        for (edits, expected) in cases {
            let generated: Vec<String> = lines(&edits);
            assert_eq!(generated, [expected], "{edits:?}");
            parses_back(expected);
        }
    }

    #[test]
    fn no_change_no_line() {
        assert_eq!(lines(&[]), Vec::<String>::new());
        assert_eq!(lines(&[Edit::Brightness(70)]), Vec::<String>::new());
    }

    #[test]
    fn profile_commands() {
        assert_eq!(activate(2).lines, ["konectl profile 2"]);
        assert_eq!(add_profile(1).lines, ["konectl profile add --copy-from 1"]);
        assert_eq!(remove_profile(3).lines, ["konectl profile remove 3"]);
        assert_eq!(
            profile_colour(2, Some(Rgb::new(0xf4, 0, 0))).lines,
            ["konectl profile color 2 f40000"]
        );
        assert_eq!(
            profile_colour(2, None).lines,
            ["konectl profile color 2 none"]
        );
    }

    fn demo_buttons() -> Buttons {
        DemoState::new().backup.profiles[0]
            .buttons
            .expect("the demo has buttons")
    }

    #[test]
    fn button_commands() {
        let back: ButtonAction = "back".parse().expect("back");
        assert_eq!(
            button(1, 7, Layer::Standard, back).lines,
            ["konectl button 7 back -p 1"]
        );
        let key: ButtonAction = "key:ctrl+c".parse().expect("key");
        assert_eq!(
            button(2, 6, Layer::EasyShift, key).lines,
            ["konectl button 6 key:ctrl+c -e -p 2"]
        );
        assert!(!button(1, 7, Layer::Standard, ButtonAction::FactoryDefault).complete);

        let line: Vec<String> = macro_button(
            1,
            7,
            Layer::Standard,
            "ctrl+c, 50ms, ctrl+v",
            MacroMode::OnPress { repeat: 3 },
        )
        .lines;
        assert_eq!(
            line,
            ["konectl button 7 'macro:ctrl+c, 50ms, ctrl+v' --repeat 3 -p 1"]
        );
        let toggle: Vec<String> =
            macro_button(1, 3, Layer::EasyShift, "a", MacroMode::Toggle).lines;
        assert_eq!(toggle, ["konectl button 3 macro:a --toggle -e -p 1"]);
    }

    #[test]
    fn quotes_what_the_shell_would_split() {
        assert_eq!(shell_quote("key:ctrl+c"), "key:ctrl+c");
        assert_eq!(shell_quote("macro:a, b"), "'macro:a, b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn changed_buttons_give_one_line_each() {
        let before: Buttons = demo_buttons();
        let mut after: Buttons = before;
        after
            .set_action(7, Layer::Standard, "volume-up".parse().expect("named"))
            .expect("allowed");
        after
            .set_action(6, Layer::Standard, ButtonAction::Macro(MacroMode::Toggle))
            .expect("allowed");
        let commands: CommandLines = buttons(&before, &after);
        assert_eq!(commands.lines, ["konectl button 7 volume-up -p 1"]);
        assert!(!commands.complete);
    }

    #[test]
    fn backups_with_the_same_profiles_differ_by_commands() {
        let before: Backup = DemoState::new().backup;
        let mut after: Backup = before.clone();
        after.active_profile = 2;
        Edit::Speed(2)
            .apply(&mut after.profiles[2].settings)
            .expect("speed");
        let commands: CommandLines = backup(&before, &after);
        assert!(commands.complete);
        assert_eq!(
            commands.lines,
            ["konectl lighting breathing -s 2 -p 3", "konectl profile 2"]
        );

        after.profiles.pop();
        assert!(!backup(&before, &after).complete);
    }
}
