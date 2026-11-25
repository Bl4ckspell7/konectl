//! When the worker polls the mouse. Pure: the worker passes the time in.
//!
//! hidapi has no hotplug events, so unplugging and plugging in is noticed by
//! polling. Every input report reaches every program that has the mouse open
//! (the CLI, the official software in a VM), so polling stays sparse and only
//! sends harmless queries.

use std::time::{Duration, Instant};

/// What the worker knows about the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No mouse found.
    Searching,
    /// The mouse was found, but reading it failed: try again, less often.
    Failed,
    /// A mouse is connected, but this user may not open it.
    NoAccess,
    /// The dongle answers, the mouse does not (asleep or switched off).
    Asleep,
    Connected,
}

/// A periodic job of the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Look for the mouse and open it.
    Connect,
    /// Ask the sleeping mouse whether it is awake again.
    Wake,
    /// Check that the dongle or cable is still there, and whether another
    /// program changed the mouse. Sends nothing to the mouse.
    Watch,
    /// Ask which profile and DPI stage are active: the mouse's own buttons
    /// change them, and it does not report that by itself. One small query;
    /// while the mouse sleeps, the dongle answers it alone.
    Active,
    /// Read battery and signal.
    Status,
}

pub const SEARCH_INTERVAL: Duration = Duration::from_millis(1500);
pub const FAILED_INTERVAL: Duration = Duration::from_secs(10);
pub const NO_ACCESS_INTERVAL: Duration = Duration::from_secs(3);
pub const WAKE_INTERVAL: Duration = Duration::from_secs(3);
pub const WATCH_INTERVAL: Duration = Duration::from_secs(2);
pub const ACTIVE_INTERVAL: Duration = Duration::from_secs(5);
pub const STATUS_INTERVAL: Duration = Duration::from_secs(30);

/// The tasks of a phase and how often they run.
fn tasks(phase: Phase) -> &'static [(Task, Duration)] {
    match phase {
        Phase::Searching => &[(Task::Connect, SEARCH_INTERVAL)],
        Phase::Failed => &[(Task::Connect, FAILED_INTERVAL)],
        Phase::NoAccess => &[(Task::Connect, NO_ACCESS_INTERVAL)],
        Phase::Asleep => &[(Task::Wake, WAKE_INTERVAL)],
        Phase::Connected => &[
            (Task::Watch, WATCH_INTERVAL),
            (Task::Active, ACTIVE_INTERVAL),
            (Task::Status, STATUS_INTERVAL),
        ],
    }
}

fn interval(phase: Phase, task: Task) -> Option<Duration> {
    tasks(phase)
        .iter()
        .find(|(candidate, _)| *candidate == task)
        .map(|(_, interval)| *interval)
}

/// The tasks of the current phase and when each is due next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    phase: Phase,
    due: Vec<(Task, Instant)>,
}

impl Schedule {
    pub fn new(phase: Phase, now: Instant) -> Self {
        let mut schedule = Self {
            phase,
            due: Vec::new(),
        };
        schedule.enter(phase, now);
        schedule
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Switches to `phase`; each of its tasks is due one interval from now.
    pub fn enter(&mut self, phase: Phase, now: Instant) {
        self.phase = phase;
        self.due = tasks(phase)
            .iter()
            .map(|(task, interval)| (*task, now + *interval))
            .collect();
    }

    /// The task that is due at `now` (the most overdue one), if any.
    pub fn next_due(&self, now: Instant) -> Option<Task> {
        self.due
            .iter()
            .filter(|(_, due)| *due <= now)
            .min_by_key(|(_, due)| *due)
            .map(|(task, _)| *task)
    }

    /// `task` ran at `now`: it is due again one interval later.
    pub fn done(&mut self, task: Task, now: Instant) {
        let Some(interval) = interval(self.phase, task) else {
            return;
        };
        for (candidate, due) in &mut self.due {
            if *candidate == task {
                *due = now + interval;
            }
        }
    }

    /// Makes `task` due now, if the phase has it.
    pub fn expedite(&mut self, task: Task, now: Instant) {
        for (candidate, due) in &mut self.due {
            if *candidate == task {
                *due = now;
            }
        }
    }

    /// When the next task is due.
    pub fn deadline(&self) -> Option<Instant> {
        self.due.iter().map(|(_, due)| *due).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_watches_often_and_reads_the_battery_rarely() {
        let start: Instant = Instant::now();
        let mut schedule = Schedule::new(Phase::Connected, start);
        assert_eq!(schedule.next_due(start), None);
        assert_eq!(schedule.deadline(), Some(start + WATCH_INTERVAL));

        let mut watches: u32 = 0;
        let mut actives: u32 = 0;
        let mut statuses: u32 = 0;
        let mut now: Instant = start;
        while now < start + STATUS_INTERVAL {
            now += Duration::from_millis(500);
            while let Some(task) = schedule.next_due(now) {
                schedule.done(task, now);
                match task {
                    Task::Watch => watches += 1,
                    Task::Active => actives += 1,
                    Task::Status => statuses += 1,
                    other => panic!("{other:?} while connected"),
                }
            }
        }
        assert_eq!(watches, 15);
        assert_eq!(actives, 6);
        assert_eq!(statuses, 1);
    }

    #[test]
    fn entering_a_phase_replaces_its_tasks() {
        let start: Instant = Instant::now();
        let mut schedule = Schedule::new(Phase::Searching, start);
        assert_eq!(
            schedule.next_due(start + SEARCH_INTERVAL),
            Some(Task::Connect)
        );

        schedule.enter(Phase::Asleep, start);
        assert_eq!(schedule.phase(), Phase::Asleep);
        assert_eq!(schedule.next_due(start + SEARCH_INTERVAL), None);
        assert_eq!(schedule.next_due(start + WAKE_INTERVAL), Some(Task::Wake));

        // A task of another phase does not come back.
        schedule.done(Task::Connect, start);
        assert_eq!(schedule.deadline(), Some(start + WAKE_INTERVAL));
    }

    #[test]
    fn the_most_overdue_task_comes_first() {
        let start: Instant = Instant::now();
        let schedule = Schedule::new(Phase::Connected, start);
        assert_eq!(
            schedule.next_due(start + STATUS_INTERVAL),
            Some(Task::Watch)
        );
    }
}
