//! The watchdog: it ends the process when a task stops its heartbeat.
//!
//! Railway runs the healthcheck only at the start of a deployment, so a hung
//! `ingest` would stay up while the child keeps acking (ADR 0001,
//! "Watchdog"). The watchdog runs on its own operating-system thread,
//! outside the Tokio runtime, so a stalled runtime cannot stop it.
//!
//! - A task registers with [`Watchdog::register`] and gets a [`Heartbeat`].
//!   It calls [`Heartbeat::beat`] at least once each [`BEAT_INTERVAL`], also
//!   when it has no work.
//! - The watchdog thread checks the heartbeats each [`CHECK_PERIOD`]. When
//!   the last beat of a task is older than its limit, the thread calls the
//!   late action once with the task name and stops checking. The production
//!   action is [`expire`]: it logs the task name and ends the process with
//!   `std::process::exit`. PID 1 then ends, the kernel kills the child, and
//!   Railway restarts the container.
//! - A task that ends on purpose calls [`Heartbeat::retire`]. A dropped
//!   heartbeat that was not retired stays registered, so a task that panics
//!   or is cancelled still makes the watchdog act.
//!
//! The limits of ADR 0001: 30 s for the gate acceptor and the ZMQ reader
//! ([`GATE_ACCEPTOR`], [`READER`]), 120 s for the database writer
//! ([`WRITER`]).
//!
//! The heartbeats are atomic values. A beat takes no lock. The registry
//! lock is held only to add a task and to copy the list for one check.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::error;

/// How often a registered task must beat at least.
pub const BEAT_INTERVAL: Duration = Duration::from_secs(1);

/// How often the watchdog thread checks the heartbeats.
pub const CHECK_PERIOD: Duration = Duration::from_secs(1);

/// The name and the limit of a watched task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Task {
    /// Static name for the log.
    pub name: &'static str,
    /// The longest time between two beats.
    pub limit: Duration,
}

/// The gate acceptor.
pub const GATE_ACCEPTOR: Task = Task {
    name: "gate_acceptor",
    limit: Duration::from_secs(30),
};

/// The ZMQ reader ([`crate::pipeline::reader`]).
pub const READER: Task = Task {
    name: "reader",
    limit: Duration::from_secs(30),
};

/// The database writer ([`crate::pipeline::writer`]). Every database call has a
/// deadline (connect 15 s, statement 30 s), so a live writer beats within
/// 120 s (ADR 0001).
pub const WRITER: Task = Task {
    name: "writer",
    limit: Duration::from_secs(120),
};

/// Exit code of the process when the watchdog ends it.
pub const EXIT_CODE: i32 = 1;

/// The longest wait for the log line before [`expire`] ends the process.
const EXPIRE_LOG_WAIT: Duration = Duration::from_secs(1);

struct Entry {
    task: Task,
    /// Milliseconds from the watchdog start to the last beat.
    last_beat: AtomicU64,
    retired: AtomicBool,
}

/// The registry of the watched tasks. Clones share the registry.
#[derive(Clone)]
pub struct Watchdog {
    started: Instant,
    entries: Arc<Mutex<Vec<Arc<Entry>>>>,
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl Watchdog {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            entries: Arc::default(),
        }
    }

    /// Watches a task from now. The registration counts as the first beat.
    #[must_use]
    pub fn register(&self, task: Task) -> Heartbeat {
        let entry = Arc::new(Entry {
            task,
            last_beat: AtomicU64::new(millis(self.started.elapsed())),
            retired: AtomicBool::new(false),
        });
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&entry));
        Heartbeat {
            started: self.started,
            entry,
        }
    }

    /// Starts the watchdog thread. It checks every `period` and calls `late`
    /// once with the name of the first late task.
    ///
    /// # Errors
    ///
    /// Returns the error of the operating system when the thread cannot
    /// start.
    pub fn start(
        self,
        period: Duration,
        late: impl FnOnce(&'static str) + Send + 'static,
    ) -> std::io::Result<JoinHandle<()>> {
        std::thread::Builder::new()
            .name("watchdog".to_owned())
            .spawn(move || {
                loop {
                    std::thread::sleep(period);
                    let now = millis(self.started.elapsed());
                    if let Some(name) = self.late_task(now) {
                        late(name);
                        return;
                    }
                }
            })
    }

    fn late_task(&self, now: u64) -> Option<&'static str> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let beats: Vec<Beat> = entries
            .iter()
            .filter(|entry| !entry.retired.load(Ordering::Relaxed))
            .map(|entry| Beat {
                task: entry.task,
                last: entry.last_beat.load(Ordering::Relaxed),
            })
            .collect();
        first_late(&beats, now)
    }
}

/// The handle of one watched task.
pub struct Heartbeat {
    started: Instant,
    entry: Arc<Entry>,
}

impl Heartbeat {
    /// Records a beat now.
    pub fn beat(&self) {
        self.entry
            .last_beat
            .store(millis(self.started.elapsed()), Ordering::Relaxed);
    }

    /// Stops the watch of this task. Call it when the task ends on purpose.
    pub fn retire(self) {
        self.entry.retired.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy)]
struct Beat {
    task: Task,
    /// Milliseconds from the watchdog start.
    last: u64,
}

/// The name of the first task whose last beat is older than its limit at
/// `now` (milliseconds from the watchdog start).
fn first_late(beats: &[Beat], now: u64) -> Option<&'static str> {
    beats
        .iter()
        .find(|beat| now.saturating_sub(beat.last) > millis(beat.task.limit))
        .map(|beat| beat.task.name)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The production late action: logs `watchdog_expired` with the task name
/// and ends the process with [`EXIT_CODE`].
///
/// A blocked standard output can be the cause of the hang. Another thread
/// therefore writes the log line, and this function ends the process after
/// [`EXPIRE_LOG_WAIT`] also when the write does not finish.
pub fn expire(task: &'static str) {
    let (done, written) = mpsc::channel();
    let logger = std::thread::Builder::new().spawn(move || {
        error!(task, "watchdog_expired");
        done.send(()).ok();
    });
    if logger.is_ok() {
        written.recv_timeout(EXPIRE_LOG_WAIT).ok();
    }
    std::process::exit(EXIT_CODE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beat(name: &'static str, limit_ms: u64, last: u64) -> Beat {
        Beat {
            task: Task {
                name,
                limit: Duration::from_millis(limit_ms),
            },
            last,
        }
    }

    #[test]
    fn a_beat_within_the_limit_is_not_late() {
        let beats = [beat("reader", 30_000, 1_000), beat("writer", 120_000, 0)];
        assert_eq!(first_late(&beats, 31_000), None);
        let beats = [beat("reader", 30_000, 100_000), beat("writer", 120_000, 0)];
        assert_eq!(first_late(&beats, 120_000), None);
    }

    #[test]
    fn a_beat_older_than_the_limit_is_late() {
        let beats = [beat("reader", 30_000, 1_000), beat("writer", 120_000, 0)];
        assert_eq!(first_late(&beats, 31_001), Some("reader"));
        let beats = [beat("reader", 30_000, 100_000), beat("writer", 120_000, 0)];
        assert_eq!(first_late(&beats, 120_001), Some("writer"));
    }

    #[test]
    fn a_beat_after_the_check_time_is_not_late() {
        // A task can beat between the clock read and the load.
        assert_eq!(first_late(&[beat("reader", 10, 50)], 40), None);
    }

    #[test]
    fn the_limits_are_those_of_adr_0001() {
        assert_eq!(GATE_ACCEPTOR.limit, Duration::from_secs(30));
        assert_eq!(READER.limit, Duration::from_secs(30));
        assert_eq!(WRITER.limit, Duration::from_secs(120));
        assert!(BEAT_INTERVAL < GATE_ACCEPTOR.limit);
    }

    fn start_with_channel(watchdog: Watchdog) -> mpsc::Receiver<&'static str> {
        let (sender, receiver) = mpsc::channel();
        watchdog
            .start(Duration::from_millis(10), move |name| {
                sender.send(name).unwrap();
            })
            .unwrap();
        receiver
    }

    const SHORT: Task = Task {
        name: "stalled",
        limit: Duration::from_millis(100),
    };

    #[test]
    fn the_thread_acts_on_a_stalled_task() {
        let watchdog = Watchdog::new();
        let _live = watchdog.register(Task {
            name: "live",
            limit: Duration::from_secs(60),
        });
        let stalled = watchdog.register(SHORT);
        let started = Instant::now();
        let late = start_with_channel(watchdog);
        assert_eq!(late.recv_timeout(Duration::from_secs(5)), Ok("stalled"));
        assert!(started.elapsed() >= SHORT.limit);
        drop(stalled);
    }

    #[test]
    fn a_dropped_heartbeat_stays_watched() {
        let watchdog = Watchdog::new();
        drop(watchdog.register(SHORT));
        let late = start_with_channel(watchdog);
        assert_eq!(late.recv_timeout(Duration::from_secs(5)), Ok("stalled"));
    }

    #[test]
    fn a_beating_task_and_a_retired_task_are_not_late() {
        // The limit is 100 beat intervals, so that a busy test machine
        // cannot delay a beat past it. `SHORT` would act after 100 ms if
        // the retirement did not work.
        const BEATING: Task = Task {
            name: "stalled",
            limit: Duration::from_secs(1),
        };
        let watchdog = Watchdog::new();
        let beating = watchdog.register(BEATING);
        watchdog.register(SHORT).retire();
        let late = start_with_channel(watchdog);
        // Beat for two limits. The watchdog must stay quiet.
        let until = Instant::now() + BEATING.limit * 2;
        while Instant::now() < until {
            beating.beat();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            late.try_recv(),
            Err(mpsc::TryRecvError::Empty),
            "the watchdog acted on a live task"
        );
        // Without beats, the same task becomes late.
        assert_eq!(late.recv_timeout(Duration::from_secs(5)), Ok("stalled"));
    }
}
