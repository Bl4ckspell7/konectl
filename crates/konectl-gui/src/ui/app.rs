//! The window frame: connection state, banners, status bar.

use slint::{ComponentHandle as _, Weak};

use crate::access;
use crate::generated::{AppState, AppWindow, MouseState};
use crate::view;
use crate::worker::{Event, Request, Requests, Sink};

/// Wires the global callbacks to the worker.
pub fn install(ui: &AppWindow, requests: &Requests, demo: bool) {
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
}

/// Passes the worker's events to the UI thread.
pub fn sink(window: Weak<AppWindow>) -> Sink {
    Box::new(move |event: Event| {
        let window: Weak<AppWindow> = window.clone();
        // Fails only once the event loop has ended, when nothing is shown.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = window.upgrade() {
                apply(&ui, event);
            }
        });
    })
}

/// Shows an event of the worker.
pub fn apply(ui: &AppWindow, event: Event) {
    let state = ui.global::<AppState>();
    match event {
        Event::Searching { problem } => {
            state.set_state(MouseState::Searching);
            state.set_problem(problem.unwrap_or_default().into());
            state.set_has_snapshot(false);
            state.set_changed_elsewhere(false);
        }
        Event::NoAccess => {
            state.set_state(MouseState::NoAccess);
            state.set_problem("".into());
            state.set_has_snapshot(false);
            state.set_changed_elsewhere(false);
        }
        Event::Asleep => {
            // The pages keep showing what was read before.
            state.set_state(MouseState::Asleep);
            state.set_problem("".into());
        }
        Event::Connected(snapshot) => {
            state.set_device(view::device(&snapshot));
            state.set_state(MouseState::Connected);
            state.set_problem("".into());
            state.set_has_snapshot(true);
            state.set_changed_elsewhere(false);
        }
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
        Event::ChangedElsewhere => state.set_changed_elsewhere(true),
    }
}
