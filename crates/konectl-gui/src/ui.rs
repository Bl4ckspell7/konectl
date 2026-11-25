//! Connects the Slint window with the worker. Runs on the UI thread.

pub mod app;
pub mod buttons;
pub mod device;
pub mod lighting;
pub mod performance;
pub mod profiles;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use konectl::ButtonAction;
use slint::{ComponentHandle as _, Weak};

use crate::generated::{AppState, AppWindow};
use crate::worker::{Request, Requests, Snapshot};

/// What the window keeps of the worker's events: the last snapshot, to show
/// another profile without asking the mouse.
#[derive(Debug, Default)]
pub struct Model {
    pub snapshot: Option<Snapshot>,
    /// Failed changes so far; the stage rows reset when it changes.
    pub resets: i32,
    /// Edits per DPI stage row (see `StageView.edits`).
    pub stage_edits: [i32; 5],
    /// The function group chosen on the Buttons page, until another button
    /// or layer is shown.
    pub button_group: Option<&'static str>,
    /// Profile, button, layer (Easy-Shift) and function the Buttons page
    /// editors were filled for.
    pub button_shown: Option<(u8, u8, bool, ButtonAction)>,
}

/// Shared with the event callback, which the worker thread creates; it is
/// only used on the UI thread.
pub type SharedModel = Arc<Mutex<Model>>;

pub fn lock(model: &SharedModel) -> MutexGuard<'_, Model> {
    model.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The profile the pages show.
pub fn selected_profile(ui: &AppWindow) -> Option<u8> {
    u8::try_from(ui.global::<AppState>().get_profile())
        .ok()
        .filter(|profile| *profile > 0)
}

/// Sends a request about the selected profile, built by `request`.
pub fn send_for_profile(
    window: &Weak<AppWindow>,
    requests: &Requests,
    request: impl FnOnce(u8) -> Option<Request>,
) {
    let Some(ui) = window.upgrade() else {
        return;
    };
    if let Some(request) = selected_profile(&ui).and_then(request) {
        requests.send(request);
    }
}
