//! Starts the worker for the window: on the real mouse, or in demo mode.

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
    let worker = WorkerHandle::spawn(connector, ui::app::sink(window.as_weak()))?;
    ui::app::install(window, &worker.requests(), in_demo);
    Ok(worker)
}
