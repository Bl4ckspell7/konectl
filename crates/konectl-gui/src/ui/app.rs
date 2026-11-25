//! The window frame: connection state, banners, status and command bar, and
//! showing snapshots on the pages.

use slint::{ComponentHandle as _, Weak};

use super::{Model, SharedModel, lock};
use crate::access;
use crate::generated::{AppState, AppWindow, MouseState, Performance};
use crate::view;
use crate::worker::{Change, Event, Request, Requests, Sink, Snapshot};

/// Wires the global callbacks to the worker.
pub fn install(
    ui: &AppWindow,
    requests: &Requests,
    model: &SharedModel,
    demo: bool,
    backups_folder: Option<String>,
) {
    let state = ui.global::<AppState>();
    state.set_demo(demo);
    state.set_in_flatpak(access::in_flatpak());
    state.set_manual_commands(access::manual_commands().into());

    let sender: Requests = requests.clone();
    state.on_reload(move || sender.send(Request::Reload));
    let sender: Requests = requests.clone();
    state.on_retry(move || sender.send(Request::Retry));
    let sender: Requests = requests.clone();
    state.on_allow_access(move || sender.send(Request::AllowAccess));
    let sender: Requests = requests.clone();
    state.on_undo(move || sender.send(Request::Undo));

    super::profiles::install(ui, requests, model);
    super::performance::install(ui, requests, model);
    super::buttons::install(ui, requests, model);
    super::lighting::install(ui, requests);
    super::device::install(ui, requests, backups_folder);
}

/// Passes the worker's events to the UI thread.
pub fn sink(window: Weak<AppWindow>, model: SharedModel) -> Sink {
    Box::new(move |event: Event| {
        let window: Weak<AppWindow> = window.clone();
        let model: SharedModel = model.clone();
        // Fails only once the event loop has ended, when nothing is shown.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = window.upgrade() {
                apply(&ui, &mut lock(&model), event);
            }
        });
    })
}

/// Shows an event of the worker.
pub fn apply(ui: &AppWindow, model: &mut Model, event: Event) {
    let state = ui.global::<AppState>();
    match event {
        Event::Searching { problem } => {
            state.set_state(MouseState::Searching);
            state.set_problem(problem.unwrap_or_default().into());
            forget(ui, model);
        }
        Event::NoAccess => {
            state.set_state(MouseState::NoAccess);
            state.set_problem("".into());
            forget(ui, model);
        }
        Event::Asleep => {
            // The pages keep showing what was read before.
            state.set_state(MouseState::Asleep);
            state.set_problem("".into());
        }
        Event::Connected(snapshot) => {
            state.set_state(MouseState::Connected);
            state.set_problem("".into());
            state.set_changed_elsewhere(false);
            show(ui, model, *snapshot, None);
        }
        Event::Changed(change) => {
            let Change {
                snapshot,
                commands,
                note,
                select,
            } = *change;
            if !commands.lines.is_empty() || !commands.complete {
                let mut lines: Vec<String> = commands.lines;
                if !commands.complete {
                    lines.push("# part of this change has no konectl command".to_owned());
                }
                state.set_commands(lines.join("\n").into());
            }
            state.set_message(note.unwrap_or_default().into());
            show(ui, model, snapshot, select);
        }
        Event::Reverted(snapshot) => {
            model.resets += 1;
            ui.global::<Performance>().set_resets(model.resets);
            show(ui, model, *snapshot, None);
        }
        Event::UndoAvailable(available) => state.set_can_undo(available),
        Event::Status(status) => {
            let mut device = state.get_device();
            view::apply_status(&mut device, &status);
            state.set_device(device);
        }
        Event::Busy(what) => {
            state.set_busy(what.is_some());
            state.set_busy_text(what.unwrap_or_default().into());
        }
        Event::Error(message) => state.set_error(message.into()),
        Event::Message(message) => state.set_message(message.into()),
        Event::ChangedElsewhere => state.set_changed_elsewhere(true),
        Event::Calibration(left) => super::device::show_calibration(ui, left),
        Event::Pairing(step) => super::device::show_pairing(ui, step),
    }
}

/// No mouse: nothing to show.
fn forget(ui: &AppWindow, model: &mut Model) {
    let state = ui.global::<AppState>();
    state.set_has_snapshot(false);
    state.set_changed_elsewhere(false);
    state.set_commands("".into());
    state.set_message("".into());
    model.snapshot = None;
    model.button_shown = None;
    model.button_group = None;
}

/// Shows a snapshot; `select` is the profile to show, else the shown one if
/// it still exists, else the active one.
fn show(ui: &AppWindow, model: &mut Model, snapshot: Snapshot, select: Option<u8>) {
    let state = ui.global::<AppState>();
    let numbers: Vec<u8> = snapshot
        .backup
        .profiles
        .iter()
        .map(|profile| profile.settings.profile())
        .collect();
    let shown: Option<u8> = super::selected_profile(ui).filter(|number| numbers.contains(number));
    let profile: u8 = select
        .filter(|number| numbers.contains(number))
        .or(shown)
        .unwrap_or(snapshot.backup.active_profile);
    state.set_profile(i32::from(profile));
    state.set_device(view::device(&snapshot));
    super::device::show(ui, &snapshot);
    state.set_has_snapshot(true);
    model.snapshot = Some(snapshot);
    show_profile(ui, model);
}

/// Shows the settings of the selected profile.
pub fn show_profile(ui: &AppWindow, model: &mut Model) {
    let Some(snapshot) = &model.snapshot else {
        return;
    };
    super::profiles::show(ui, &snapshot.backup);
    let Some(profile) = super::selected_profile(ui).and_then(|number| {
        snapshot
            .backup
            .profiles
            .iter()
            .find(|profile| profile.settings.profile() == number)
    }) else {
        return;
    };
    let (sensor, settings) = (profile.settings.sensor, profile.settings);
    super::performance::show(ui, &sensor, &model.stage_edits);
    super::lighting::show(ui, &settings);
    super::buttons::show(ui, model);
}
