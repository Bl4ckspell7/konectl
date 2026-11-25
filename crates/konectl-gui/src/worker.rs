//! The worker thread: the only place that talks to the mouse.
//!
//! Every library call blocks (a write waits for its read-back, reading all
//! profiles takes up to two seconds), so they run here and never on the UI
//! thread. The UI sends [`Request`]s; the worker answers with [`Event`]s
//! through a callback and polls the mouse in between (`schedule.rs`).

mod change;
pub mod demo;
pub mod device;
mod maintenance;
mod request;
pub mod schedule;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime};

use konectl::safety::SafetyBackups;
use konectl::{ConnectionMode, Sensor};

use crate::access::Outcome;
use crate::command_line::CommandLines;
use crate::undo::UndoStack;
use change::{Following, collect_edits, latest_of_kind, skip_repeats};
use device::{ConnectError, Connector, Device};
pub use request::{Change, Event, Request, Snapshot, Status};
use schedule::{Phase, Schedule, Task};

/// Receives the worker's events, on the worker thread.
pub type Sink = Box<dyn FnMut(Event) + Send>;

/// The error code the dongle answers battery and signal queries with while
/// the mouse sleeps (docs/protocol.md).
const ASLEEP_CODE: u8 = 0x0d;

/// Why there is no usable mouse.
enum Lost {
    /// Neither dongle nor cable is connected.
    NotFound,
    NoAccess,
    /// Found, but opening or reading it failed.
    Failed(String),
}

/// The worker's state; [`Worker::run`] is its thread's loop.
pub struct Worker {
    connector: Box<dyn Connector>,
    device: Option<Box<dyn Device>>,
    /// What was read from the mouse, kept up to date by every change.
    snapshot: Option<Snapshot>,
    undo: UndoStack,
    schedule: Schedule,
    /// Other programs that change the mouse save `previous.txt` first (the
    /// CLI does), so its modification time tells that the snapshot is old.
    backups: Option<SafetyBackups>,
    /// The modification time of `previous.txt` when the snapshot was read.
    backups_seen: Option<SystemTime>,
    changed_elsewhere: bool,
    /// The last state reported while there is no usable mouse, so polling
    /// does not repeat it.
    reported: Option<Event>,
    /// The udev rule was just installed: if there is still no access, say so.
    access_allowed: bool,
    /// Set by the user interface to stop waiting (pairing); shared with
    /// [`Requests`], since the worker does not read requests meanwhile.
    cancel: Arc<AtomicBool>,
    sink: Sink,
}

impl Worker {
    pub fn new(
        connector: Box<dyn Connector>,
        backups: Option<SafetyBackups>,
        sink: Sink,
        now: Instant,
    ) -> Self {
        Self {
            connector,
            device: None,
            snapshot: None,
            undo: UndoStack::default(),
            schedule: Schedule::new(Phase::Searching, now),
            backups,
            backups_seen: None,
            changed_elsewhere: false,
            reported: None,
            access_allowed: false,
            cancel: Arc::default(),
            sink,
        }
    }

    /// Handles requests and polls until [`Request::Shutdown`], or until the
    /// user interface is gone.
    pub fn run(mut self, requests: &Receiver<Request>) {
        self.start(Instant::now());
        let mut next: Option<Request> = None;
        loop {
            let received = match (next.take(), self.schedule.deadline()) {
                (Some(request), _) => Ok(request),
                (None, Some(deadline)) => {
                    requests.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                }
                (None, None) => requests.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            let following: Following = match received {
                Ok(Request::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Request::Edit { profile, edit }) => {
                    let (edits, following) = collect_edits((profile, edit), requests);
                    self.apply_edits(&edits, Instant::now());
                    following
                }
                Ok(request @ (Request::SetButton { .. } | Request::Debounce(_))) => {
                    let (latest, following) = latest_of_kind(request, requests);
                    self.handle(latest, Instant::now());
                    following
                }
                Ok(request) => {
                    self.handle(request.clone(), Instant::now());
                    skip_repeats(&request, requests)
                }
                Err(RecvTimeoutError::Timeout) => Following::Nothing,
            };
            match following {
                Following::Nothing => {}
                Following::Next(request) => next = Some(request),
                Following::End => return,
            }
            self.tick(Instant::now());
        }
    }

    /// Looks for the mouse right away.
    pub fn start(&mut self, now: Instant) {
        self.connect(now);
    }

    /// Runs the polling tasks that are due.
    pub fn tick(&mut self, now: Instant) {
        while let Some(task) = self.schedule.next_due(now) {
            log::trace!("poll: {task:?}");
            self.schedule.done(task, now);
            match task {
                Task::Connect => self.connect(now),
                Task::Wake => self.wake(now),
                Task::Watch => self.watch(now),
                Task::Active => self.read_active(now),
                Task::Status => self.read_status(now),
            }
        }
    }

    pub fn handle(&mut self, request: Request, now: Instant) {
        log_request(&request);
        match request {
            Request::Reload => {
                // Undo compares with the snapshot; after reading everything
                // again it would also undo what other programs changed.
                if !self.undo.is_empty() {
                    self.undo.clear();
                    self.send(Event::UndoAvailable(false));
                }
                self.read_all(now);
            }
            Request::Retry => match self.schedule.phase() {
                Phase::Searching | Phase::Failed | Phase::NoAccess => self.connect(now),
                Phase::Asleep => self.wake(now),
                Phase::Connected => {}
            },
            Request::AllowAccess => self.allow_access(now),
            Request::Edit { profile, edit } => self.apply_edits(&[(profile, edit)], now),
            Request::ActivateProfile(profile) => self.activate(profile, now),
            Request::AddProfile { copy_from } => self.add_profile(copy_from, now),
            Request::RemoveProfile(profile) => self.remove_profile(profile, now),
            Request::ProfileColour { profile, colour } => self.colour(profile, colour, now),
            Request::SetButton {
                profile,
                button,
                layer,
                action,
            } => self.set_button(profile, button, layer, action, now),
            Request::SetMacro {
                profile,
                button,
                layer,
                keys,
                mode,
            } => self.set_macro(profile, button, layer, &keys, mode, now),
            Request::Undo => self.undo(now),
            Request::EnergySaving(enabled) => self.set_energy_saving(enabled, now),
            Request::Debounce(ms) => self.set_debounce(ms, now),
            Request::CalibrateLiftOff => self.calibrate_lift_off(now),
            Request::Reset => self.reset(now),
            Request::Pair => self.pair(now),
            Request::SaveBackup(path) => self.save_backup(&path, now),
            Request::RestoreBackup(path) => self.restore_file(&path, now),
            Request::RestoreOriginal => self.restore_original(now),
            Request::Shutdown => {}
        }
    }

    fn send(&mut self, event: Event) {
        log_event(&event);
        (self.sink)(event);
    }

    /// Sends a state that polling finds again and again only once.
    fn report(&mut self, event: Event) {
        if self.reported.as_ref() != Some(&event) {
            self.reported = Some(event.clone());
            self.send(event);
        }
    }

    fn connect(&mut self, now: Instant) {
        // Close the old handle first; a replugged device has a new one.
        self.device = None;
        // Undo belongs to the mouse as it was read; it may be another now.
        if !self.undo.is_empty() {
            self.undo.clear();
            self.send(Event::UndoAvailable(false));
        }
        match self.connector.connect() {
            Ok(device) => {
                self.device = Some(device);
                self.read_all(now);
            }
            Err(ConnectError::NotFound) => self.lose(Lost::NotFound, now),
            Err(ConnectError::NoAccess) => self.lose(Lost::NoAccess, now),
            Err(ConnectError::Failed(error)) => self.lose(Lost::Failed(error.to_string()), now),
        }
    }

    fn lose(&mut self, lost: Lost, now: Instant) {
        self.device = None;
        self.snapshot = None;
        if matches!(lost, Lost::NoAccess) && std::mem::take(&mut self.access_allowed) {
            self.send(Event::Error(
                "The udev rule is installed, but there is still no access. Unplug the dongle \
                 or cable and plug it in again, or log out and in: the rule only applies to \
                 the active local session."
                    .to_owned(),
            ));
        }
        let (phase, event) = match lost {
            Lost::NotFound => (Phase::Searching, Event::Searching { problem: None }),
            Lost::NoAccess => (Phase::NoAccess, Event::NoAccess),
            Lost::Failed(problem) => (
                Phase::Failed,
                Event::Searching {
                    problem: Some(problem),
                },
            ),
        };
        self.schedule.enter(phase, now);
        self.report(event);
    }

    /// Reads everything into a new snapshot.
    fn read_all(&mut self, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.connect(now);
            return;
        };
        (self.sink)(Event::Busy(Some("Reading the mouse …")));
        // Before reading: a change saved during the read is then noticed.
        let backups_before: Option<SystemTime> = backups_modified(self.backups.as_ref());
        let read = read_snapshot(device.as_mut());
        self.send(Event::Busy(None));
        match read {
            Ok(snapshot) => {
                self.backups_seen = backups_before;
                self.changed_elsewhere = false;
                self.reported = None;
                self.access_allowed = false;
                self.schedule.enter(Phase::Connected, now);
                self.snapshot = Some(snapshot.clone());
                self.send(Event::Connected(Box::new(snapshot)));
            }
            Err(error) => self.failed(error, now),
        }
    }

    /// After a failed request: unplugged, asleep, or a real error.
    fn failed(&mut self, error: konectl::Error, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.lose(Lost::Failed(error.to_string()), now);
            return;
        };
        if !device.is_present() {
            self.lose(Lost::NotFound, now);
            return;
        }
        let wireless: bool = device.mode() == ConnectionMode::Wireless;
        let asleep: bool = match error {
            konectl::Error::NotReachable => true,
            konectl::Error::Rejected {
                code: ASLEEP_CODE, ..
            } => wireless,
            // A mouse that falls asleep can stop answering in the middle of a
            // request.
            konectl::Error::Timeout(_) => wireless,
            _ => false,
        };
        if asleep {
            self.schedule.enter(Phase::Asleep, now);
            self.report(Event::Asleep);
        } else {
            self.lose(Lost::Failed(error.to_string()), now);
        }
    }

    fn wake(&mut self, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.connect(now);
            return;
        };
        match device.firmwares() {
            Ok(_) => self.read_all(now),
            // The cable may have been plugged in to charge: use it.
            Err(_) if self.connector.wired_connected() => self.connect(now),
            Err(error) => self.failed(error, now),
        }
    }

    fn watch(&mut self, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.connect(now);
            return;
        };
        let Ok(events) = device.function_events() else {
            // Unplugged; the other connection may still be there (cable and
            // dongle).
            self.connect(now);
            return;
        };
        if !self.changed_elsewhere && backups_modified(self.backups.as_ref()) != self.backups_seen {
            self.changed_elsewhere = true;
            self.send(Event::ChangedElsewhere);
        }
        // E.g. the profile button: show the new profile right away.
        if events > 0 {
            self.read_active(now);
        }
    }

    fn read_status(&mut self, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.connect(now);
            return;
        };
        match read_status(device.as_mut()) {
            Ok(status) => {
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.status = status;
                }
                self.send(Event::Status(status));
            }
            Err(error) => self.failed(error, now),
        }
    }

    fn read_active(&mut self, now: Instant) {
        let Some(device) = self.device.as_mut() else {
            self.connect(now);
            return;
        };
        match device.active_sensor() {
            Ok(sensor) => self.refresh_active(sensor),
            Err(error) => self.failed(error, now),
        }
    }

    /// Keeps the snapshot up to date with the active profile and DPI stage,
    /// which the mouse's buttons change.
    fn refresh_active(&mut self, sensor: Sensor) {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        let active: u8 = sensor.profile();
        let mut switched: bool = snapshot.backup.active_profile != active;
        let mut changed: bool = switched;
        snapshot.backup.active_profile = active;
        if let Some(profile) = snapshot
            .backup
            .profiles
            .iter_mut()
            .find(|profile| profile.settings.profile() == active)
        {
            switched |= profile.settings.sensor.active_stage() != sensor.active_stage();
            changed |= profile.settings.sensor != sensor;
            profile.settings.sensor = sensor;
        }
        if switched {
            log::info!(
                "the mouse switched to profile {active}, DPI stage {}",
                sensor.active_stage()
            );
        } else if changed {
            log::debug!("the sensor settings of profile {active} changed outside this app");
        }
        if changed {
            let snapshot: Snapshot = snapshot.clone();
            self.send(Event::Changed(Box::new(Change {
                snapshot,
                commands: CommandLines::new(),
                note: None,
                select: None,
            })));
        }
    }

    fn allow_access(&mut self, now: Instant) {
        self.send(Event::Busy(Some("Waiting for permission …")));
        let outcome = self.connector.allow_access();
        self.send(Event::Busy(None));
        match outcome {
            // Connect with the next tick, so a window closed meanwhile does
            // not wait for a full read first.
            Ok(Outcome::Allowed) => {
                self.access_allowed = true;
                self.schedule.enter(Phase::NoAccess, now);
                self.schedule.expedite(Task::Connect, now);
            }
            Ok(Outcome::Dismissed) => {}
            Err(message) => self.send(Event::Error(message)),
        }
    }
}

/// Logs what the user interface is told: connection states, changes with
/// their commands, notes and errors (the rest only at `trace`).
fn log_event(event: &Event) {
    match event {
        Event::Searching { problem: None } => log::info!("no mouse found; looking for it"),
        Event::Searching {
            problem: Some(problem),
        } => log::warn!("the mouse cannot be used: {problem}"),
        Event::NoAccess => log::info!("a mouse is connected, but this user may not open it"),
        Event::Asleep => log::info!("the mouse does not answer (asleep or switched off)"),
        Event::Connected(snapshot) => {
            let active = snapshot.backup.active_profile;
            log::info!(
                "connected over the {}: firmware {}{}, {} profiles, profile {active} active",
                snapshot.mode,
                snapshot.firmwares.mouse,
                snapshot
                    .firmwares
                    .dongle
                    .map_or_else(String::new, |dongle| format!(", dongle {dongle}")),
                snapshot.backup.profiles.len()
            );
        }
        Event::Changed(change) => {
            for line in &change.commands.lines {
                log::info!("changed: {}", hide_macro_keys(line));
            }
            if !change.commands.complete {
                log::info!("changed: (partly without a konectl command)");
            }
            if let Some(note) = &change.note {
                log::info!("{note}");
            }
        }
        Event::Reverted(_) => log::debug!("the controls show the mouse's settings again"),
        Event::Error(message) => log::warn!("{message}"),
        Event::Message(message) => log::info!("{message}"),
        Event::ChangedElsewhere => log::info!("another program changed the mouse settings"),
        Event::Pairing(Some(step)) => log::info!("pairing: waiting for {step:?}"),
        Event::Pairing(None) => log::debug!("pairing is over"),
        Event::Calibration(None) => log::debug!("the calibration is over"),
        Event::UndoAvailable(_) | Event::Status(_) | Event::Busy(_) | Event::Calibration(_) => {
            log::trace!("{event:?}");
        }
    }
}

/// Logs a request at `debug`, without the keys of a macro.
fn log_request(request: &Request) {
    match request {
        Request::SetMacro {
            profile,
            button,
            layer,
            mode,
            ..
        } => log::debug!(
            "request SetMacro {{ profile: {profile}, button: {button}, layer: {layer:?}, \
             mode: {mode:?}, keys not logged }}"
        ),
        request => log::debug!("request {request:?}"),
    }
}

/// A command line without the keys of a macro (`macro:…`): they may be
/// anything the user types, and logs end up in bug reports.
fn hide_macro_keys(line: &str) -> String {
    let Some(start) = line.find("macro:") else {
        return line.to_owned();
    };
    let keys: usize = start + "macro:".len();
    // Quoted (`'macro:a, b'`) up to the closing quote, else up to the space.
    let end: usize = if line[..start].ends_with('\'') {
        line[keys..]
            .find('\'')
            .map_or(line.len(), |quote| keys + quote)
    } else {
        line[keys..]
            .find(' ')
            .map_or(line.len(), |space| keys + space)
    };
    format!("{}…{}", &line[..keys], &line[end..])
}

/// When `previous.txt` was last written.
fn backups_modified(backups: Option<&SafetyBackups>) -> Option<SystemTime> {
    std::fs::metadata(backups?.previous_path())
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn read_snapshot(device: &mut dyn Device) -> konectl::Result<Snapshot> {
    let firmwares = device.firmwares()?;
    let backup = device.backup()?;
    // The settings are complete without these; they may fail on their own.
    let energy_saving: Option<bool> = device.energy_saving().ok();
    let status = Status {
        power: device.power().ok(),
        signal: device.signal().ok().flatten(),
    };
    Ok(Snapshot {
        mode: device.mode(),
        firmwares,
        backup,
        energy_saving,
        status,
    })
}

fn read_status(device: &mut dyn Device) -> konectl::Result<Status> {
    let power = device.power()?;
    let signal: Option<u8> = device.signal()?;
    Ok(Status {
        power: Some(power),
        signal,
    })
}

/// Sends requests to the worker; cheap to clone into UI callbacks.
#[derive(Clone)]
pub struct Requests {
    sender: Sender<Request>,
    cancel: Arc<AtomicBool>,
}

impl Requests {
    pub fn send(&self, request: Request) {
        // Fails only once the worker has ended, when nothing is to be done.
        let _ = self.sender.send(request);
    }

    /// Stops the request that waits for the user (pairing), also before
    /// the worker starts it.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Sends a request that [`Self::cancel`] can stop, without an earlier
    /// cancel.
    pub fn send_cancellable(&self, request: Request) {
        self.cancel.store(false, Ordering::SeqCst);
        self.send(request);
    }
}

/// The running worker thread.
pub struct WorkerHandle {
    requests: Requests,
    thread: JoinHandle<()>,
}

impl WorkerHandle {
    /// Starts the worker. `connector` is made on the worker thread, which
    /// then owns the mouse; changes save `backups` first.
    pub fn spawn(
        connector: impl FnOnce() -> Box<dyn Connector> + Send + 'static,
        backups: Option<SafetyBackups>,
        sink: Sink,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let cancel: Arc<AtomicBool> = Arc::default();
        let worker_cancel: Arc<AtomicBool> = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("konectl-worker".to_owned())
            .spawn(move || {
                let mut worker = Worker::new(connector(), backups, sink, Instant::now());
                worker.cancel = worker_cancel;
                worker.run(&receiver);
            })?;
        Ok(Self {
            requests: Requests { sender, cancel },
            thread,
        })
    }

    pub fn requests(&self) -> Requests {
        self.requests.clone()
    }

    /// Ends the worker after its current request: an interrupted read of
    /// another profile would leave that profile selected on the mouse. A
    /// pairing that waits for the user is cancelled.
    pub fn stop(self) {
        self.requests.cancel();
        self.requests.send(Request::Shutdown);
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use konectl::pairing::PairingStep;
    use konectl::{Backup, ProfileSettings};

    use super::demo::{DemoConnector, SharedDemo};
    use super::*;
    use crate::edit::Edit;

    /// A worker on the demo mouse, collecting its events.
    struct Harness {
        worker: Worker,
        demo: SharedDemo,
        events: Arc<Mutex<Vec<Event>>>,
        start: Instant,
    }

    impl Harness {
        fn new(backups: Option<SafetyBackups>) -> Self {
            let demo: SharedDemo = demo::shared(demo::DemoState::new());
            let events: Arc<Mutex<Vec<Event>>> = Arc::default();
            let sink_events = Arc::clone(&events);
            let start: Instant = Instant::now();
            let worker = Worker::new(
                Box::new(DemoConnector::new(Arc::clone(&demo))),
                backups,
                Box::new(move |event| {
                    sink_events
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(event);
                }),
                start,
            );
            Self {
                worker,
                demo,
                events,
                start,
            }
        }

        /// The events since the last call, without the busy notes.
        fn take(&self) -> Vec<Event> {
            std::mem::take(&mut *self.events.lock().unwrap_or_else(PoisonError::into_inner))
                .into_iter()
                .filter(|event| !matches!(event, Event::Busy(_)))
                .collect()
        }

        fn demo(&self) -> std::sync::MutexGuard<'_, demo::DemoState> {
            self.demo.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn at(&self, millis: u64) -> Instant {
            self.start + Duration::from_millis(millis)
        }
    }

    fn connected(events: &[Event]) -> Option<&Snapshot> {
        match events {
            [Event::Connected(snapshot)] => Some(snapshot),
            _ => None,
        }
    }

    #[test]
    fn connects_and_reads_everything() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        let events: Vec<Event> = harness.take();
        let snapshot: &Snapshot = connected(&events).expect("connected");
        assert_eq!(snapshot.mode, ConnectionMode::Wireless);
        assert_eq!(snapshot.backup.profiles.len(), 3);
        assert_eq!(snapshot.status.signal, Some(92));
        assert_eq!(snapshot.energy_saving, Some(true));
    }

    #[test]
    fn notices_unplug_and_replug() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();

        harness.demo().set_plugged_in(false);
        harness.worker.tick(harness.at(2_000));
        assert_eq!(harness.take(), [Event::Searching { problem: None }]);
        // Searching again is not reported again.
        harness.worker.tick(harness.at(3_500));
        assert_eq!(harness.take(), []);

        harness.demo().set_plugged_in(true);
        harness.worker.tick(harness.at(5_000));
        assert!(connected(&harness.take()).is_some());
    }

    #[test]
    fn a_sleeping_mouse_is_woken_by_polling() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();

        harness.demo().awake = false;
        // Watching sends nothing to the mouse; the next query notices.
        harness.worker.tick(harness.at(4_000));
        assert_eq!(harness.take(), []);
        harness.worker.tick(harness.at(5_000));
        assert_eq!(harness.take(), [Event::Asleep]);
        harness.worker.tick(harness.at(8_000));
        assert_eq!(harness.take(), []);

        harness.demo().awake = true;
        harness.worker.tick(harness.at(11_000));
        assert!(connected(&harness.take()).is_some());
    }

    #[test]
    fn reports_missing_access_until_it_is_allowed() {
        let mut harness = Harness::new(None);
        harness.demo().accessible = false;
        harness.worker.start(harness.start);
        assert_eq!(harness.take(), [Event::NoAccess]);
        harness.worker.tick(harness.at(3_000));
        assert_eq!(harness.take(), []);

        harness
            .worker
            .handle(Request::AllowAccess, harness.at(4_000));
        assert_eq!(harness.take(), []);
        harness.worker.tick(harness.at(4_000));
        assert!(connected(&harness.take()).is_some());
    }

    #[test]
    fn says_why_access_is_still_missing_after_allowing_it() {
        let mut harness = Harness::new(None);
        harness.demo().accessible = false;
        harness.demo().allow_works = false;
        harness.worker.start(harness.start);
        assert_eq!(harness.take(), [Event::NoAccess]);
        harness
            .worker
            .handle(Request::AllowAccess, harness.at(1_000));
        harness.worker.tick(harness.at(1_000));
        assert!(matches!(
            harness.take().as_slice(),
            [Event::Error(message)] if message.contains("still no access")
        ));
    }

    #[test]
    fn uses_the_cable_when_it_is_plugged_in_while_the_mouse_sleeps() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        harness.demo().awake = false;
        harness.worker.tick(harness.at(5_000));
        assert_eq!(harness.take(), [Event::Asleep]);

        harness.demo().cable = true;
        harness.worker.tick(harness.at(8_000));
        let events: Vec<Event> = harness.take();
        assert_eq!(
            connected(&events).expect("connected").mode,
            ConnectionMode::Wired
        );
    }

    #[test]
    fn notices_changes_by_other_programs_once() {
        let directory: PathBuf =
            std::env::temp_dir().join(format!("konectl-gui-worker-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let backups = SafetyBackups::in_dir(&directory);
        let previous: PathBuf = backups.previous_path();
        let mut harness = Harness::new(Some(backups));
        harness.worker.start(harness.start);
        harness.take();

        // The CLI saves previous.txt before it changes the mouse.
        fs::create_dir_all(&directory).expect("directory");
        fs::write(&previous, "changed").expect("previous.txt");
        harness.worker.tick(harness.at(2_000));
        assert_eq!(harness.take(), [Event::ChangedElsewhere]);
        harness.worker.tick(harness.at(4_000));
        assert_eq!(harness.take(), []);

        harness.worker.handle(Request::Reload, harness.at(5_000));
        assert!(connected(&harness.take()).is_some());
        harness.worker.tick(harness.at(7_000));
        assert_eq!(harness.take(), []);
        fs::remove_dir_all(&directory).expect("clean up");
    }

    fn dpi(stage: u8, dpi: Option<u16>) -> Request {
        Request::Edit {
            profile: 1,
            edit: Edit::Dpi { stage, dpi },
        }
    }

    fn changed(events: &[Event]) -> &Change {
        events
            .iter()
            .find_map(|event| match event {
                Event::Changed(change) => Some(change.as_ref()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no change in {events:?}"))
    }

    fn profile_1(harness: &Harness) -> ProfileSettings {
        harness.demo().backup.profiles[0].settings
    }

    #[test]
    fn writes_an_edit_and_shows_its_command() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();

        harness.worker.handle(dpi(2, Some(900)), harness.at(100));
        let events: Vec<Event> = harness.take();
        let change: &Change = changed(&events);
        assert_eq!(change.commands.lines, ["konectl dpi 2 900 -p 1"]);
        assert_eq!(events.last(), Some(&Event::UndoAvailable(true)));
        assert_eq!(profile_1(&harness).sensor.dpi(2).expect("stage"), Some(900));
        assert_eq!(change.snapshot.backup, harness.demo().backup);
    }

    #[test]
    fn keeps_what_the_mouse_changed_meanwhile() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        // The DPI button of the mouse switched to stage 3.
        Edit::ActiveStage(3)
            .apply(&mut harness.demo().backup.profiles[0].settings)
            .expect("stage");

        harness.worker.handle(dpi(1, Some(400)), harness.at(100));
        let events: Vec<Event> = harness.take();
        let sensor = changed(&events).snapshot.backup.profiles[0].settings.sensor;
        assert_eq!(sensor.active_stage(), 3);
        assert_eq!(sensor.dpi(1).expect("stage"), Some(400));
    }

    #[test]
    fn a_refused_edit_puts_the_controls_back() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let before: ProfileSettings = profile_1(&harness);

        // Stage 2 is active and cannot be switched off.
        harness.worker.handle(dpi(2, None), harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(matches!(&events[0], Event::Error(message) if message.contains("active stage")));
        assert!(matches!(&events[1], Event::Reverted(_)));
        assert_eq!(events.len(), 2);
        assert_eq!(profile_1(&harness), before);
    }

    #[test]
    fn a_failed_change_leaves_nothing_to_undo() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        // The snapshot says profile 1 is active, the mouse switched to 2.
        harness.demo().backup.active_profile = 2;

        harness
            .worker
            .handle(Request::RemoveProfile(2), harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(matches!(&events[0], Event::Error(message) if message.contains("failed")));
        let Event::Reverted(snapshot) = &events[1] else {
            panic!("{events:?}");
        };
        // Read again: the active profile is the mouse's.
        assert_eq!(snapshot.backup.active_profile, 2);
        assert_eq!(events[2], Event::UndoAvailable(false));
    }

    #[test]
    fn edits_written_together_are_one_undo() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let original: Backup = harness.demo().backup.clone();
        harness.worker.apply_edits(
            &[
                (1, Edit::Brightness(10)),
                (2, Edit::Speed(9)),
                (1, Edit::AngleSnapping(true)),
            ],
            harness.at(100),
        );
        let events: Vec<Event> = harness.take();
        assert_eq!(changed(&events).commands.lines.len(), 3);
        harness.worker.handle(Request::Undo, harness.at(200));
        let events: Vec<Event> = harness.take();
        assert_eq!(events.last(), Some(&Event::UndoAvailable(false)));
        assert_eq!(harness.demo().backup, original);
    }

    #[test]
    fn undo_keeps_what_the_mouse_changed_meanwhile() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let original_dpi = profile_1(&harness).sensor.dpi(1).expect("stage");
        harness.worker.handle(dpi(1, Some(400)), harness.at(100));
        // The DPI button of the mouse switched to stage 3.
        Edit::ActiveStage(3)
            .apply(&mut harness.demo().backup.profiles[0].settings)
            .expect("stage");

        harness.worker.handle(Request::Undo, harness.at(200));
        let sensor = profile_1(&harness).sensor;
        assert_eq!(sensor.dpi(1).expect("stage"), original_dpi);
        assert_eq!(sensor.active_stage(), 3);
    }

    #[test]
    fn polling_notices_the_buttons_of_the_mouse() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        harness.demo().backup.active_profile = 3;
        harness.demo().power.percent = Some(60);

        // The profile button: noticed within seconds.
        harness.worker.tick(harness.at(5_000));
        let events: Vec<Event> = harness.take();
        assert_eq!(changed(&events).snapshot.backup.active_profile, 3);
        harness.worker.tick(harness.at(10_000));
        assert_eq!(harness.take(), []);

        // The battery, and the snapshot keeps it.
        harness.worker.tick(harness.at(30_000));
        let events: Vec<Event> = harness.take();
        assert!(matches!(
            events.as_slice(),
            [Event::Status(status)] if status.power.and_then(|power| power.percent) == Some(60)
        ));
        harness.worker.handle(dpi(1, Some(400)), harness.at(30_100));
        let events: Vec<Event> = harness.take();
        assert_eq!(
            changed(&events)
                .snapshot
                .status
                .power
                .and_then(|power| power.percent),
            Some(60)
        );
    }

    #[test]
    fn a_profile_switch_by_the_mouse_shows_at_once() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        // The profile button: the mouse switches and reports the function.
        harness.demo().backup.active_profile = 2;
        harness.demo().function_events = 1;

        harness.worker.tick(harness.at(2_000));
        let events: Vec<Event> = harness.take();
        assert_eq!(changed(&events).snapshot.backup.active_profile, 2);
    }

    #[test]
    fn reloading_forgets_what_can_be_undone() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        harness.worker.handle(dpi(1, Some(400)), harness.at(100));
        harness.take();
        harness.worker.handle(Request::Reload, harness.at(200));
        assert_eq!(harness.take()[0], Event::UndoAvailable(false));
    }

    #[test]
    fn undo_goes_back_step_by_step() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let original: Backup = harness.demo().backup.clone();

        harness.worker.handle(dpi(2, Some(900)), harness.at(100));
        harness
            .worker
            .handle(Request::ActivateProfile(3), harness.at(200));
        harness.worker.handle(
            Request::ProfileColour {
                profile: 2,
                colour: None,
            },
            harness.at(300),
        );
        harness.take();
        assert_eq!(harness.demo().backup.active_profile, 3);

        harness.worker.handle(Request::Undo, harness.at(400));
        let events: Vec<Event> = harness.take();
        assert_eq!(
            changed(&events).commands.lines,
            ["konectl profile color 2 e01b24"]
        );
        harness.worker.handle(Request::Undo, harness.at(500));
        harness.worker.handle(Request::Undo, harness.at(600));
        let events: Vec<Event> = harness.take();
        assert_eq!(events.last(), Some(&Event::UndoAvailable(false)));
        assert_eq!(harness.demo().backup, original);
    }

    #[test]
    fn undo_brings_a_removed_profile_back() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let original: Backup = harness.demo().backup.clone();

        harness
            .worker
            .handle(Request::AddProfile { copy_from: 1 }, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert_eq!(changed(&events).select, Some(4));
        harness
            .worker
            .handle(Request::RemoveProfile(2), harness.at(200));
        let events: Vec<Event> = harness.take();
        let change: &Change = changed(&events);
        assert_eq!(change.snapshot.backup.profiles.len(), 3);
        assert!(
            change
                .note
                .as_deref()
                .is_some_and(|note| note.contains("profile 3 is now profile 2"))
        );

        harness.worker.handle(Request::Undo, harness.at(300));
        harness.worker.handle(Request::Undo, harness.at(400));
        harness.take();
        assert_eq!(harness.demo().backup, original);
    }

    #[test]
    fn saves_the_safety_backups_before_a_change() {
        let directory: PathBuf =
            std::env::temp_dir().join(format!("konectl-gui-change-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let backups = SafetyBackups::in_dir(&directory);
        let mut harness = Harness::new(Some(backups.clone()));
        harness.worker.start(harness.start);
        let original: Backup = harness.demo().backup.clone();
        harness.take();

        harness.worker.handle(dpi(2, Some(900)), harness.at(100));
        harness.take();
        assert_eq!(backups.load_previous().expect("previous.txt"), original);
        assert_eq!(backups.load_original().expect("original.txt"), original);
        // Saved by the GUI itself: not a change by another program.
        harness.worker.tick(harness.at(2_100));
        assert_eq!(harness.take(), []);
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn an_edit_to_a_sleeping_mouse_is_not_written() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        harness.demo().awake = false;

        harness.worker.handle(dpi(2, Some(900)), harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(matches!(&events[0], Event::Error(_)));
        assert!(matches!(&events[1], Event::Reverted(_)));
        assert_eq!(events.last(), Some(&Event::Asleep));
    }

    /// Reads only. `cargo test -p konectl-gui -- --ignored --test-threads=1`
    /// with the mouse connected and awake.
    #[test]
    #[ignore = "needs a connected mouse"]
    fn reads_the_real_mouse() {
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let sink_events = Arc::clone(&events);
        let mut worker = Worker::new(
            Box::new(device::HidConnector::default()),
            None,
            Box::new(move |event| {
                sink_events
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(event);
            }),
            Instant::now(),
        );
        worker.start(Instant::now());
        let events = events.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = events
            .iter()
            .find_map(|event| match event {
                Event::Connected(snapshot) => Some(snapshot),
                _ => None,
            })
            .unwrap_or_else(|| panic!("not connected: {events:?}"));
        assert!(!snapshot.backup.profiles.is_empty());
        eprintln!(
            "{:?}, firmware {}, {} profiles, {:?}",
            snapshot.mode,
            snapshot.firmwares.mouse,
            snapshot.backup.profiles.len(),
            snapshot.status
        );
    }

    /// Writes and undoes a DPI change, a lighting change, a button and a
    /// profile switch; the mouse ends as it started. `cargo test -p konectl-gui --
    /// --ignored --test-threads=1` with the mouse connected and awake.
    #[test]
    #[ignore = "needs a connected mouse; writes to it"]
    fn edits_and_undoes_on_the_real_mouse() {
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let sink_events = Arc::clone(&events);
        let mut worker = Worker::new(
            Box::new(device::HidConnector::default()),
            // A failed run leaves previous.txt to restore from.
            SafetyBackups::from_env().ok(),
            Box::new(move |event| {
                sink_events
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(event);
            }),
            Instant::now(),
        );
        let take = || std::mem::take(&mut *events.lock().unwrap_or_else(PoisonError::into_inner));
        worker.start(Instant::now());
        let start: Vec<Event> = take();
        let snapshot: Snapshot = start
            .iter()
            .find_map(|event| match event {
                Event::Connected(snapshot) => Some(snapshot.as_ref().clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("not connected: {start:?}"));
        let original: Backup = snapshot.backup.clone();
        let profile: u8 = original.active_profile;
        let settings = original
            .profiles
            .iter()
            .find(|candidate| candidate.settings.profile() == profile)
            .expect("active profile")
            .settings;
        let sensor = settings.sensor;
        let speed: u8 = if settings.lighting.speed() == 3 { 4 } else { 3 };
        let stage: u8 = sensor.active_stage();
        let dpi: u16 = sensor
            .dpi(stage)
            .ok()
            .flatten()
            .expect("active stage has a DPI");
        let changed_dpi: u16 = if dpi == 19_000 { dpi - 50 } else { dpi + 50 };
        let other: Option<u8> = original
            .profiles
            .iter()
            .map(|candidate| candidate.settings.profile())
            .find(|number| *number != profile);

        let mut requests: Vec<Request> = vec![
            Request::Edit {
                profile,
                edit: Edit::Dpi {
                    stage,
                    dpi: Some(changed_dpi),
                },
            },
            Request::Edit {
                profile,
                edit: Edit::Speed(speed),
            },
        ];
        // A button: a media key other than the one it has (no macro: the
        // keys of one on the mouse could not be restored).
        let button_7 = original
            .profiles
            .iter()
            .find(|candidate| candidate.settings.profile() == profile)
            .and_then(|candidate| candidate.buttons)
            .and_then(|buttons| buttons.action(7, konectl::Layer::Standard).ok());
        let volume_up: konectl::ButtonAction = "volume-up".parse().expect("named");
        requests.push(Request::SetButton {
            profile,
            button: 7,
            layer: konectl::Layer::Standard,
            action: if button_7 == Some(volume_up) {
                "volume-down".parse().expect("named")
            } else {
                volume_up
            },
        });
        // Debounce, in all profiles.
        let debounce: u8 = if sensor.debounce_ms() == 3 { 4 } else { 3 };
        requests.push(Request::Debounce(debounce));
        requests.extend(other.map(Request::ActivateProfile));
        let count: usize = requests.len();
        for request in requests {
            worker.handle(request, Instant::now());
            let events: Vec<Event> = take();
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, Event::Changed(_))),
                "{events:?}"
            );
            eprintln!("{:?}", changed(&events).commands.lines);
        }
        for _ in 0..count {
            worker.handle(Request::Undo, Instant::now());
        }
        let events: Vec<Event> = take();
        assert_eq!(
            events.last(),
            Some(&Event::UndoAvailable(false)),
            "{events:?}"
        );
        // Energy saving has no undo: switch it, and back.
        if let Some(enabled) = snapshot.energy_saving {
            for state in [!enabled, enabled] {
                worker.handle(Request::EnergySaving(state), Instant::now());
                let events: Vec<Event> = take();
                assert_eq!(
                    changed(&events).snapshot.energy_saving,
                    Some(state),
                    "{events:?}"
                );
            }
        }
        worker.handle(Request::Reload, Instant::now());
        let events: Vec<Event> = take();
        let reread: Backup = events
            .iter()
            .find_map(|event| match event {
                Event::Connected(snapshot) => Some(snapshot.backup.clone()),
                _ => None,
            })
            .expect("read again");
        assert_eq!(reread, original);
    }

    /// A fresh temporary directory for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let directory: PathBuf =
            std::env::temp_dir().join(format!("konectl-gui-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("directory");
        directory
    }

    fn debounces(harness: &Harness) -> Vec<u8> {
        harness
            .demo()
            .backup
            .profiles
            .iter()
            .map(|profile| profile.settings.sensor.debounce_ms())
            .collect()
    }

    #[test]
    fn macro_keys_stay_out_of_the_log() {
        assert_eq!(
            hide_macro_keys("konectl button 3 'macro:p, a, s, s' -p 1"),
            "konectl button 3 'macro:…' -p 1"
        );
        assert_eq!(
            hide_macro_keys("konectl button 3 macro:a,b --repeat 3 -p 1"),
            "konectl button 3 macro:… --repeat 3 -p 1"
        );
        assert_eq!(
            hide_macro_keys("konectl dpi 2 900 -p 1"),
            "konectl dpi 2 900 -p 1"
        );
    }

    #[test]
    fn energy_saving_is_written_without_undo() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();

        harness
            .worker
            .handle(Request::EnergySaving(false), harness.at(100));
        let events: Vec<Event> = harness.take();
        let change: &Change = changed(&events);
        assert_eq!(change.commands.lines, ["konectl energy-saving off"]);
        assert_eq!(change.snapshot.energy_saving, Some(false));
        assert!(!harness.demo().energy_saving);
        assert_eq!(events.last(), Some(&Event::UndoAvailable(false)));
    }

    #[test]
    fn debounce_goes_to_every_profile_and_back() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let before: Vec<u8> = debounces(&harness);

        harness.worker.handle(Request::Debounce(7), harness.at(100));
        let events: Vec<Event> = harness.take();
        assert_eq!(changed(&events).commands.lines, ["konectl debounce 7"]);
        assert_eq!(debounces(&harness), [7, 7, 7]);
        assert_eq!(events.last(), Some(&Event::UndoAvailable(true)));

        // Nothing to write the second time.
        harness.worker.handle(Request::Debounce(7), harness.at(200));
        assert_eq!(harness.take(), []);

        harness.worker.handle(Request::Undo, harness.at(300));
        harness.take();
        assert_eq!(debounces(&harness), before);
    }

    #[test]
    fn calibration_counts_down_and_uses_the_distance_everywhere() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let before: Backup = harness.demo().backup.clone();

        harness
            .worker
            .handle(Request::CalibrateLiftOff, harness.at(100));
        let events: Vec<Event> = harness.take();
        let countdown: Vec<u64> = events
            .iter()
            .filter_map(|event| match event {
                Event::Calibration(Some(left)) => Some(*left),
                _ => None,
            })
            .collect();
        assert_eq!(countdown, (1..=12).rev().collect::<Vec<u64>>());
        assert!(events.contains(&Event::Calibration(None)));
        assert_eq!(
            changed(&events).commands.lines,
            ["konectl liftoff calibrate"]
        );
        assert_eq!(harness.demo().calibrations, 1);
        assert!(
            harness
                .demo()
                .backup
                .profiles
                .iter()
                .all(|profile| profile.settings.sensor.lift_off() == konectl::LiftOff::Custom)
        );

        harness.worker.handle(Request::Undo, harness.at(200));
        harness.take();
        assert_eq!(harness.demo().backup, before);
    }

    #[test]
    fn a_reset_can_be_undone() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        harness.demo().energy_saving = false;
        let before: Backup = harness.demo().backup.clone();

        harness.worker.handle(Request::Reset, harness.at(100));
        let events: Vec<Event> = harness.take();
        let change: &Change = changed(&events);
        assert_eq!(change.commands.lines, ["konectl reset"]);
        assert_eq!(change.snapshot.energy_saving, Some(true));
        assert_ne!(harness.demo().backup, before);

        harness.worker.handle(Request::Undo, harness.at(200));
        harness.take();
        assert_eq!(harness.demo().backup, before);
    }

    #[test]
    fn pairing_reports_its_steps_and_reads_the_mouse_again() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();

        harness.worker.handle(Request::Pair, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert_eq!(
            events[..4],
            [
                Event::Pairing(Some(PairingStep::Unplug)),
                Event::Pairing(Some(PairingStep::Replug)),
                Event::Pairing(Some(PairingStep::Connect)),
                Event::Pairing(None),
            ]
        );
        assert!(matches!(events[4], Event::Connected(_)));
        let change: &Change = changed(&events);
        assert_eq!(change.commands.lines, ["konectl pair"]);
        assert!(
            change
                .note
                .as_deref()
                .is_some_and(|note| note.starts_with("Paired"))
        );
        assert_eq!(harness.demo().pairings, 1);
    }

    #[test]
    fn pairing_can_be_cancelled() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        // The user cancels while the dongle should be plugged in again.
        let events = Arc::clone(&harness.events);
        let cancel: Arc<AtomicBool> = Arc::clone(&harness.worker.cancel);
        harness.worker.sink = Box::new(move |event| {
            if event == Event::Pairing(Some(PairingStep::Replug)) {
                cancel.store(true, Ordering::SeqCst);
            }
            events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
        });

        harness.worker.handle(Request::Pair, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert_eq!(events[2], Event::Pairing(None));
        assert!(
            matches!(&events[3], Event::Message(message) if message.starts_with("Pairing cancelled"))
        );
        // The dongle is found again.
        assert!(matches!(events[4], Event::Connected(_)));
        assert_eq!(harness.demo().pairings, 0);
    }

    #[test]
    fn a_cancel_before_pairing_starts_keeps_the_dongle_as_it_is() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        // The window closed right after Start.
        harness.worker.cancel.store(true, Ordering::SeqCst);

        harness.worker.handle(Request::Pair, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert_eq!(
            events,
            [
                Event::Message("Pairing cancelled.".to_owned()),
                Event::Pairing(None)
            ]
        );
        assert!(!harness.demo().pairing);
    }

    #[test]
    fn pairs_a_mouse_that_does_not_answer_yet() {
        let mut harness = Harness::new(None);
        harness.demo().awake = false;
        harness.worker.start(harness.start);
        assert_eq!(harness.take(), [Event::Asleep]);

        harness.worker.handle(Request::Pair, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(events.contains(&Event::Pairing(None)));
        assert_eq!(
            changed(&events).commands.lines,
            ["konectl pair"],
            "{events:?}"
        );
        assert_eq!(harness.demo().pairings, 1);
    }

    #[test]
    fn a_calibration_without_a_mouse_still_ends() {
        let mut harness = Harness::new(None);
        harness.demo().awake = false;
        harness.worker.start(harness.start);
        harness.take();

        harness
            .worker
            .handle(Request::CalibrateLiftOff, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(matches!(&events[0], Event::Error(_)), "{events:?}");
        assert_eq!(events.last(), Some(&Event::Calibration(None)));
    }

    #[test]
    fn a_restore_while_the_mouse_sleeps_is_not_refused_but_waits() {
        let directory: PathBuf = temp_dir("restore-asleep");
        let file: PathBuf = directory.join("mouse.txt");
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let saved: Backup = harness.demo().backup.clone();
        fs::write(&file, saved.to_text()).expect("file");
        harness.demo().awake = false;

        harness
            .worker
            .handle(Request::RestoreBackup(file), harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(events.contains(&Event::Asleep), "{events:?}");
        assert!(!events.iter().any(
            |event| matches!(event, Event::Error(message) if message.starts_with("Not changed"))
        ));
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn saving_a_backup_keeps_the_snapshot_for_undo() {
        let directory: PathBuf = temp_dir("save-keeps-undo");
        let file: PathBuf = directory.join("mouse.txt");
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        harness.worker.handle(dpi(1, Some(400)), harness.at(100));
        let before: Snapshot = changed(&harness.take()).snapshot.clone();
        // Another program changes the mouse meanwhile.
        harness.demo().energy_saving = false;
        Edit::Speed(9)
            .apply(&mut harness.demo().backup.profiles[1].settings)
            .expect("speed");

        harness
            .worker
            .handle(Request::SaveBackup(file), harness.at(200));
        let events: Vec<Event> = harness.take();
        assert_eq!(changed(&events).snapshot, before);
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn pairing_needs_the_dongle() {
        let mut harness = Harness::new(None);
        harness.demo().cable = true;
        harness.worker.start(harness.start);
        harness.take();

        harness.worker.handle(Request::Pair, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(
            matches!(&events[..], [Event::Error(message), Event::Pairing(None)] if message.contains("through the dongle"))
        );
    }

    #[test]
    fn saves_a_backup_and_restores_it() {
        let directory: PathBuf = temp_dir("backup-file");
        let file: PathBuf = directory.join("mouse.txt");
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let saved: Backup = harness.demo().backup.clone();

        harness
            .worker
            .handle(Request::SaveBackup(file.clone()), harness.at(100));
        let events: Vec<Event> = harness.take();
        assert_eq!(
            changed(&events).commands.lines,
            [format!("konectl backup {}", file.display())]
        );
        let text: String = fs::read_to_string(&file).expect("saved");
        assert_eq!(Backup::from_text(&text).expect("valid"), saved);

        harness.worker.handle(dpi(1, Some(400)), harness.at(200));
        harness.take();
        assert_ne!(harness.demo().backup, saved);

        harness
            .worker
            .handle(Request::RestoreBackup(file.clone()), harness.at(300));
        let events: Vec<Event> = harness.take();
        let change: &Change = changed(&events);
        assert_eq!(
            change.note.as_deref(),
            Some(format!("Restored 3 profiles from {}.", file.display()).as_str())
        );
        assert_eq!(harness.demo().backup, saved);
        assert_eq!(events.last(), Some(&Event::UndoAvailable(true)));
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn a_file_that_is_no_backup_changes_nothing() {
        let directory: PathBuf = temp_dir("not-a-backup");
        let file: PathBuf = directory.join("notes.txt");
        fs::write(&file, "shopping list\n").expect("file");
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();
        let before: Backup = harness.demo().backup.clone();

        harness
            .worker
            .handle(Request::RestoreBackup(file), harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(
            matches!(&events[..], [Event::Error(message)] if message.starts_with("Could not use"))
        );
        assert_eq!(harness.demo().backup, before);
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn restores_the_state_before_the_first_change() {
        let directory: PathBuf = temp_dir("original");
        let mut harness = Harness::new(Some(SafetyBackups::in_dir(&directory)));
        harness.worker.start(harness.start);
        harness.take();
        let original: Backup = harness.demo().backup.clone();

        harness.worker.handle(dpi(1, Some(400)), harness.at(100));
        harness.worker.handle(dpi(1, Some(450)), harness.at(200));
        harness.take();

        harness
            .worker
            .handle(Request::RestoreOriginal, harness.at(300));
        let events: Vec<Event> = harness.take();
        assert!(
            changed(&events)
                .note
                .as_deref()
                .is_some_and(|note| note.contains("first change"))
        );
        assert_eq!(harness.demo().backup, original);
        fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn no_original_settings_without_safety_backups() {
        let mut harness = Harness::new(None);
        harness.worker.start(harness.start);
        harness.take();

        harness
            .worker
            .handle(Request::RestoreOriginal, harness.at(100));
        let events: Vec<Event> = harness.take();
        assert!(
            matches!(&events[..], [Event::Error(message)] if message.contains("no safety backups"))
        );
    }

    #[test]
    fn stops_when_asked() {
        let demo: SharedDemo = demo::shared(demo::DemoState::new());
        let (sender, receiver) = mpsc::channel::<Event>();
        let handle = WorkerHandle::spawn(
            move || Box::new(DemoConnector::new(demo)),
            None,
            Box::new(move |event| {
                let _ = sender.send(event);
            }),
        )
        .expect("thread");
        let first_snapshot = receiver
            .iter()
            .find(|event| matches!(event, Event::Connected(_)));
        assert!(first_snapshot.is_some());
        handle.stop();
    }
}
