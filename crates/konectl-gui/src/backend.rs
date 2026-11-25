//! Starts the worker for the window: on the real mouse, or in demo mode.

use konectl::safety::SafetyBackups;
use slint::ComponentHandle as _;

use crate::generated::AppWindow;
use crate::ui;
use crate::worker::WorkerHandle;
use crate::worker::demo::{self, DemoConnector, DemoState};
use crate::worker::device::{Connector, HidConnector};

/// Starts the worker and wires the window to it.
pub fn start(window: &AppWindow) -> std::io::Result<WorkerHandle> {
    let demo: Option<DemoState> = demo::requested();
    let in_demo: bool = demo.is_some();
    let connector = move || -> Box<dyn Connector> {
        match demo {
            Some(state) => Box::new(DemoConnector::new(demo::shared(state))),
            None => Box::new(HidConnector::default()),
        }
    };
    let model: ui::SharedModel = ui::SharedModel::default();
    // The demo never touches the safety backups of the real mouse.
    let backups: Option<SafetyBackups> = if in_demo {
        log::info!(
            "demo mode ({}={}): no mouse is changed",
            demo::DEMO_VAR,
            std::env::var(demo::DEMO_VAR).unwrap_or_default()
        );
        None
    } else {
        match SafetyBackups::from_env() {
            Ok(backups) => {
                log::info!("safety backups in {}", backups.directory().display());
                Some(backups)
            }
            Err(error) => {
                log::warn!("no safety backups: {error}");
                None
            }
        }
    };
    let folder: Option<String> = backups
        .as_ref()
        .map(|backups| backups.directory().display().to_string());
    let worker = WorkerHandle::spawn(
        connector,
        backups,
        ui::app::sink(window.as_weak(), model.clone()),
    )?;
    ui::app::install(window, &worker.requests(), &model, in_demo, folder);
    Ok(worker)
}
