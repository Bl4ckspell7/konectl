//! konectl-gui: a desktop app to configure the ROCCAT Kone Pro Air on Linux
//! (unofficial).
//!
//! The window runs on the main thread; all mouse access runs on a worker
//! thread (`worker`). `KONECTL_GUI_DEMO=1` starts it on a simulated mouse.

mod access;
mod backend;
mod command_line;
mod edit;
mod functions;
#[cfg(feature = "screenshots")]
mod screenshots;
mod ui;
mod undo;
mod view;
mod worker;

/// The components generated from `ui/*.slint`.
#[allow(unsafe_code)]
mod generated {
    slint::include_modules!();
}

use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitCode};

use generated::AppWindow;
use slint::ComponentHandle as _;

/// The application ID: Wayland app ID, desktop file, icon and Flatpak.
const APP_ID: &str = "io.github.bl4ckspell7.konectl";

/// The environment variable that selects Slint's backend and renderer, and the
/// value for the renderer that needs no GPU.
const BACKEND_VAR: &str = "SLINT_BACKEND";
const SOFTWARE_RENDERER: &str = "winit-software";

fn main() -> ExitCode {
    #[cfg(feature = "screenshots")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--screenshots")
    {
        let Some(directory) = std::env::args_os().nth(2) else {
            eprintln!("usage: konectl-gui --screenshots DIR");
            return ExitCode::from(2);
        };
        return match screenshots::run(std::path::Path::new(&directory)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("konectl-gui: {error}");
                ExitCode::FAILURE
            }
        };
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        // Without a usable OpenGL driver (a VM without 3D, a remote session)
        // the window fails on its first frame, and the renderer cannot be
        // changed afterwards: start again with the software renderer.
        Err(error) if no_gpu(&error) && std::env::var_os(BACKEND_VAR).is_none() => {
            eprintln!("konectl-gui: {error}; starting again with the software renderer");
            restart_with_software_renderer()
        }
        Err(error) => {
            eprintln!("konectl-gui: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;
    // Matches the desktop file (data/), so the desktop shows its name and icon.
    slint::set_xdg_app_id(APP_ID)?;
    let worker = backend::start(&window)
        .map_err(|error| slint::PlatformError::Other(format!("could not start: {error}")))?;
    let result = window.run();
    // Waits for the current request: closing in the middle of reading a
    // profile would leave that profile selected on the mouse.
    worker.stop();
    result
}

/// Whether a platform error is about drawing rather than about the app.
fn no_gpu(error: &slint::PlatformError) -> bool {
    let message: String = error.to_string().to_ascii_lowercase();
    ["opengl", "egl", "glx", "renderer", "surface"]
        .iter()
        .any(|hint| message.contains(hint))
}

/// Replaces this process with a new run that uses the software renderer.
fn restart_with_software_renderer() -> ExitCode {
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => {
            eprintln!("konectl-gui: cannot start again: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Returns only if starting failed.
    let error = Command::new(program)
        .args(std::env::args_os().skip(1))
        .env(BACKEND_VAR, SOFTWARE_RENDERER)
        .exec();
    eprintln!("konectl-gui: cannot start again: {error}");
    ExitCode::FAILURE
}
