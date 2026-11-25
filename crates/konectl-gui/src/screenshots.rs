//! `konectl-gui --screenshots DIR` (feature `screenshots`): draws the screens
//! of the demo mouse into PPM files with the software renderer, without a
//! window. For the README, and for checking layouts.

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle as _, PhysicalSize, Rgb8Pixel};

use crate::edit::Edit;
use crate::generated::{AppState, AppWindow};
use crate::ui::{self, Model};
use crate::worker::demo::{self, DemoConnector, DemoState};
use crate::worker::{Event, Request, Worker};

const WIDTH: u32 = 980;
const HEIGHT: u32 = 660;
const SCALE: f32 = 1.5;

struct Offscreen {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

/// Draws every page and state into `directory`.
pub fn run(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen {
        window: Rc::clone(&window),
    }))
    .map_err(|error| error.to_string())?;
    let ui = AppWindow::new().map_err(|error| error.to_string())?;
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: SCALE,
    });
    // Physical pixels: the logical window size times the scale.
    window.set_size(PhysicalSize::new(
        (WIDTH as f32 * SCALE) as u32,
        (HEIGHT as f32 * SCALE) as u32,
    ));
    ui.show().map_err(|error| error.to_string())?;
    let state = ui.global::<AppState>();
    state.set_demo(true);
    state.set_manual_commands(crate::access::manual_commands().into());

    let shot = |name: &str| save(&window, &directory.join(format!("{name}.ppm")));
    let mut model = Model::default();

    for (name, start) in [("no-access", no_access()), ("searching", unplugged())] {
        let (_, events) = demo_worker(start);
        apply_all(&ui, &mut model, events.take());
        shot(name)?;
    }
    let (mut worker, events) = demo_worker(DemoState::new());
    apply_all(&ui, &mut model, events.take());
    for (page, name) in ["performance", "lighting", "buttons", "device"]
        .into_iter()
        .enumerate()
    {
        state.set_page(i32::try_from(page).unwrap_or_default());
        shot(name)?;
    }

    state.set_page(0);
    worker.handle(
        Request::Edit {
            profile: 1,
            edit: Edit::Dpi {
                stage: 2,
                dpi: Some(900),
            },
        },
        Instant::now(),
    );
    apply_all(&ui, &mut model, events.take());
    shot("changed")?;

    apply_all(
        &ui,
        &mut model,
        vec![Event::Asleep, Event::ChangedElsewhere],
    );
    state.set_error("Example error: the mouse rejected the sensor write.".into());
    shot("banners")?;

    // The smallest window, with all five profiles: nothing may be cut off.
    state.set_error("".into());
    apply_all(
        &ui,
        &mut model,
        vec![Event::Connected(Box::new(snapshot_with_five_profiles(
            &mut worker,
            &events,
        )))],
    );
    window.set_size(PhysicalSize::new(
        (ui.get_layout_min_width() * SCALE) as u32,
        (ui.get_layout_min_height() * SCALE) as u32,
    ));
    for (page, name) in ["performance", "lighting"].into_iter().enumerate() {
        state.set_page(i32::try_from(page).unwrap_or_default());
        shot(&format!("smallest-{name}"))?;
    }
    Ok(())
}

/// The demo mouse with two profiles added.
fn snapshot_with_five_profiles(worker: &mut Worker, events: &Events) -> crate::worker::Snapshot {
    for _ in 0..2 {
        worker.handle(Request::AddProfile { copy_from: 1 }, Instant::now());
    }
    events
        .take()
        .into_iter()
        .rev()
        .find_map(|event| match event {
            Event::Changed(change) => Some(change.snapshot),
            _ => None,
        })
        .expect("profiles added")
}

/// Collects a worker's events.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<Event>>>);

impl Events {
    fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

fn no_access() -> DemoState {
    let mut state = DemoState::new();
    state.accessible = false;
    state
}

fn unplugged() -> DemoState {
    let mut state = DemoState::new();
    state.plugged_in = false;
    state
}

/// A started worker on this demo mouse (without safety backups), and its
/// events.
fn demo_worker(start: DemoState) -> (Worker, Events) {
    let events = Events::default();
    let sink = events.clone();
    let mut worker = Worker::new(
        Box::new(DemoConnector::new(demo::shared(start))),
        None,
        Box::new(move |event| {
            sink.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
        }),
        Instant::now(),
    );
    worker.start(Instant::now());
    (worker, events)
}

fn apply_all(ui: &AppWindow, model: &mut Model, events: Vec<Event>) {
    for event in events {
        ui::app::apply(ui, model, event);
    }
}

/// Renders the window into a binary PPM file.
fn save(window: &MinimalSoftwareWindow, path: &Path) -> Result<(), String> {
    slint::platform::update_timers_and_animations();
    let size: PhysicalSize = window.window().size();
    let (width, height) = (size.width as usize, size.height as usize);
    let pixels: RefCell<Vec<Rgb8Pixel>> = RefCell::new(vec![Rgb8Pixel::default(); width * height]);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels.borrow_mut(), width);
    });
    let mut file: Vec<u8> = format!("P6\n{width} {height}\n255\n").into_bytes();
    for pixel in pixels.borrow().iter() {
        file.extend_from_slice(&[pixel.r, pixel.g, pixel.b]);
    }
    fs::write(path, file).map_err(|error| format!("{}: {error}", path.display()))
}
