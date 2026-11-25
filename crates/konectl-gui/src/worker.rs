//! The worker thread: the only place that talks to the mouse.
//!
//! Every library call blocks (a write waits for its read-back, reading all
//! profiles takes up to two seconds), so they run here and never on the UI
//! thread. The UI sends [`Request`]s; the worker answers with [`Event`]s
//! through a callback and polls the mouse in between (`schedule.rs`).

mod change;
pub mod demo;
pub mod device;
mod request;
pub mod schedule;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime};

use konectl::safety::SafetyBackups;
use konectl::{ConnectionMode, Sensor};

use crate::access::Outcome;
use crate::command_line::CommandLines;
use crate::undo::UndoStack;
use change::{Following, collect_edits, skip_repeats};
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
            Request::Undo => self.undo(now),
            Request::Shutdown => {}
        }
    }

    fn send(&mut self, event: Event) {
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
        let mut changed: bool = snapshot.backup.active_profile != active;
        snapshot.backup.active_profile = active;
        if let Some(profile) = snapshot
            .backup
            .profiles
            .iter_mut()
            .find(|profile| profile.settings.profile() == active)
        {
            changed |= profile.settings.sensor != sensor;
            profile.settings.sensor = sensor;
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
pub struct Requests(Sender<Request>);

impl Requests {
    pub fn send(&self, request: Request) {
        // Fails only once the worker has ended, when nothing is to be done.
        let _ = self.0.send(request);
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
        let thread = thread::Builder::new()
            .name("konectl-worker".to_owned())
            .spawn(move || {
                Worker::new(connector(), backups, sink, Instant::now()).run(&receiver);
            })?;
        Ok(Self {
            requests: Requests(sender),
            thread,
        })
    }

    pub fn requests(&self) -> Requests {
        self.requests.clone()
    }

    /// Ends the worker after its current request: an interrupted read of
    /// another profile would leave that profile selected on the mouse.
    pub fn stop(self) {
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

    /// Writes and undoes a DPI change, a lighting change and a profile
    /// switch; the mouse ends as it started. `cargo test -p konectl-gui --
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
        let original: Backup = start
            .iter()
            .find_map(|event| match event {
                Event::Connected(snapshot) => Some(snapshot.backup.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("not connected: {start:?}"));
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
