//! Process lifecycle: the start order, the shutdown signals, and the
//! shutdown sequence of ADR 0001.
//!
//! Start of the `run` command:
//!
//! 1. Install the SIGTERM and SIGINT handlers. A failure stops the start.
//!    PID 1 of a container gets a signal from outside the container only
//!    when it has a handler for it, so these handlers are also what makes
//!    SIGTERM reach `ingest` as PID 1. Other signals, except SIGKILL, have
//!    no effect on PID 1.
//! 2. Make the run ID (random bytes of the operating system) and the
//!    connection settings of the database. A failure stops the start. No
//!    connection opens yet.
//! 3. Bind the port of `/health` and the gate port. A failure stops the
//!    start. The gate is closed.
//! 4. Record the `started` event.
//! 5. Start the watchdog thread and the gate acceptor with its heartbeat.
//! 6. Start the child supervisor ([`crate::child`]).
//! 7. Start the pipeline ([`crate::pipeline`]): the reader connects the SUB
//!    socket, the writer runs the start-up check, and the shortfall check
//!    starts its period. Each watched task registers its heartbeat when it
//!    starts, because the limit of a heartbeat counts from its
//!    registration. The pipeline opens the gate through
//!    [`crate::readiness`] when its conditions are met.
//! 8. Serve `/health`.
//!
//! Shutdown starts at the first SIGTERM or SIGINT, or when a task that must
//! run until the shutdown ends by itself (then the exit code is 1). The
//! health server stops at once, in parallel with the steps below; its own
//! deadline is 3 s ([`http::LIMITS`]). The steps of ADR 0001 run in order:
//!
//! 1. Shut the gate. The acceptor closes every forwarded connection. Bound:
//!    [`ShutdownBudget::gate_close`]. The sequence waits until the gate task
//!    has ended, also after an abort, so that no forwarded byte reaches
//!    `fleet-telemetry` during step 2. A copy task that an abort leaves can
//!    run for one more scheduler step; the drain of step 2 waits for 1 s
//!    without a message, which covers that step.
//! 2. Drain the SUB socket: read until no message arrives for 1 s, at most
//!    3 s. Bound: [`ShutdownBudget::zmq_drain`].
//! 3. The final shortfall read. Bound: [`ShutdownBudget::shortfall_read`].
//! 4. Stop the child with SIGKILL. Bound: [`ShutdownBudget::child_stop`].
//! 5. Record the `stopped` event, then flush the queue and every event, the
//!    `stopped` event too. Bound: [`ShutdownBudget::flush`].
//! 6. If the flush did not store everything, log the counts of what is
//!    lost (`flush_incomplete`), and exit with code 1.
//!
//! Then the sequence writes the last log lines of the events, waits for the
//! health server, and returns. The binary stops the runtime without a wait.
//!
//! Each step gives its work the bound minus [`STEP_MARGIN`], and aborts the
//! work at the bound, so that a step that does not end in time cannot
//! delay the next one. Railway sends SIGKILL `drainingSeconds` (30 s) after
//! SIGTERM. The sum of the bounds, [`BUDGET`], is 26 s, so the sequence ends
//! before the SIGKILL with 4 s of margin.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{Notify, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::RunConfig;
use crate::events::{EventBuffer, EventKind, EventSink, IngestEvent};
use crate::pipeline::cadence::random_jitter;
use crate::pipeline::conditions::Conditions;
use crate::pipeline::queue::Queue;
use crate::pipeline::reader::Reader;
use crate::pipeline::shortfall::{Received, Shortfall};
use crate::pipeline::writer::{DatabaseStore, Report, ShutdownRequest, Writer};
use crate::readiness::Readiness;
use crate::storage::{self, RUN_STATEMENT_TIMEOUT, StorageError};
use crate::watchdog::{self, Watchdog};
use crate::{child, gate, http};

/// The bound of each shutdown step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownBudget {
    /// Step 1: the gate closes every connection.
    pub gate_close: Duration,
    /// Step 2: the SUB drain. ADR 0001: until no message arrives for 1 s,
    /// at most 3 s.
    pub zmq_drain: Duration,
    /// Step 3: one HTTP read of the metrics of `fleet-telemetry` on the
    /// loopback interface.
    pub shortfall_read: Duration,
    /// Step 4: SIGKILL, the wait for the exit, and the last output lines.
    pub child_stop: Duration,
    /// Step 5: the flush of the queue. ADR 0001: 20 s.
    pub flush: Duration,
}

impl ShutdownBudget {
    /// The sum of the bounds.
    #[must_use]
    pub const fn total(&self) -> Duration {
        self.gate_close
            .saturating_add(self.zmq_drain)
            .saturating_add(self.shortfall_read)
            .saturating_add(self.child_stop)
            .saturating_add(self.flush)
    }
}

/// The shutdown budget of ADR 0001.
pub const BUDGET: ShutdownBudget = ShutdownBudget {
    gate_close: Duration::from_secs(1),
    zmq_drain: Duration::from_secs(3),
    shortfall_read: Duration::from_secs(1),
    child_stop: Duration::from_secs(1),
    flush: Duration::from_secs(20),
};

/// The part of a step bound that the work of the step does not get: the
/// time to end and to join the task of the step before the abort.
pub const STEP_MARGIN: Duration = Duration::from_millis(250);

/// Railway `drainingSeconds` (ADR 0001): the time from SIGTERM to SIGKILL.
pub const DRAINING: Duration = Duration::from_secs(30);

/// A failure of the `run` command.
///
/// The text holds static words, a SQLSTATE, and operating system error
/// text only.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A signal handler could not be installed.
    #[error("cannot install the {signal} handler: {source}")]
    Signal {
        /// Name of the signal.
        signal: &'static str,
        /// Error from the operating system.
        source: io::Error,
    },
    /// The operating system gave no random bytes for the run ID.
    #[error("cannot make the run ID: no random bytes")]
    RunId,
    /// The database URL does not give valid connection settings.
    #[error("cannot use DATABASE_URL: {0}")]
    Database(StorageError),
    /// The server could not listen on the configured port.
    #[error("cannot listen on the configured port: {0}")]
    Bind(#[source] io::Error),
    /// The gate could not listen on its port.
    #[error("cannot listen on the gate port: {0}")]
    GateBind(#[source] io::Error),
    /// The watchdog thread could not start.
    #[error("cannot start the watchdog thread: {0}")]
    Watchdog(#[source] io::Error),
    /// A task that must run until the shutdown ended by itself.
    #[error("the {0} task ended by itself")]
    TaskEnded(&'static str),
    /// The final flush did not store every record and event.
    #[error("the final flush did not store everything")]
    FlushIncomplete,
}

/// The handlers of the signals that stop the process.
pub struct ShutdownSignals {
    terminate: Signal,
    interrupt: Signal,
}

impl ShutdownSignals {
    /// Installs the SIGTERM and SIGINT handlers. Call it inside a Tokio
    /// runtime.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::Signal`] when a handler cannot be installed. The
    /// rule is the same for both signals: the process does not start.
    pub fn install() -> Result<Self, RunError> {
        Ok(Self {
            terminate: install(SignalKind::terminate(), "SIGTERM")?,
            interrupt: install(SignalKind::interrupt(), "SIGINT")?,
        })
    }

    /// Waits for the first SIGTERM or SIGINT and returns its name.
    pub async fn recv(mut self) -> &'static str {
        // `recv` returns `None` only when the signal driver stops. Treat
        // that as a shutdown request too, so that the process cannot run
        // without a working stop path.
        tokio::select! {
            _ = self.terminate.recv() => "SIGTERM",
            _ = self.interrupt.recv() => "SIGINT",
        }
    }
}

fn install(kind: SignalKind, name: &'static str) -> Result<Signal, RunError> {
    signal(kind).map_err(|source| RunError::Signal {
        signal: name,
        source,
    })
}

/// A random run ID (ADR 0002, `raw.ingest_event.run_id`).
fn run_id() -> Result<Uuid, RunError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| RunError::RunId)?;
    Ok(uuid::Builder::from_random_bytes(bytes).into_uuid())
}

/// The tasks that run until the shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Task {
    Gate,
    Supervisor,
    Reader,
    Writer,
    Shortfall,
}

impl Task {
    const fn name(self) -> &'static str {
        match self {
            Self::Gate => "gate",
            Self::Supervisor => "child_supervisor",
            Self::Reader => "reader",
            Self::Writer => "writer",
            Self::Shortfall => "shortfall",
        }
    }
}

/// Runs the `run` command until a shutdown signal arrives and the shutdown
/// sequence has ended.
///
/// # Errors
///
/// Returns [`RunError`] when the start fails, when a task ended by itself,
/// or when the final flush did not store everything.
#[expect(
    clippy::too_many_lines,
    reason = "the start order and the shutdown steps read best as one sequence"
)]
pub async fn run(config: RunConfig) -> Result<(), RunError> {
    let signals = ShutdownSignals::install()?;
    let run_id = run_id()?;
    let options =
        storage::connect_options(&config.database_url, &config.schema, RUN_STATEMENT_TIMEOUT)
            .map_err(RunError::Database)?;

    let health_listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.port))
        .await
        .map_err(RunError::Bind)?;
    let port = health_listener.local_addr().map_err(RunError::Bind)?.port();
    info!(port, "listening");
    let gate_listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.gate_port))
        .await
        .map_err(RunError::GateBind)?;
    let port = gate_listener
        .local_addr()
        .map_err(RunError::GateBind)?
        .port();
    info!(port, "gate_listening");

    let limits = config.limits;
    let budget = config.budget;
    let wake = Arc::new(Notify::new());
    let events = Arc::new(EventBuffer::new(limits.max_events, Arc::clone(&wake)));
    let sink: Arc<dyn EventSink> = Arc::clone(&events) as Arc<dyn EventSink>;
    sink.record(IngestEvent::now(EventKind::Started));
    let log_task = tokio::spawn(Arc::clone(&events).run_log());
    let readiness = Arc::new(Readiness::new(Arc::clone(&sink)));
    let conditions = Arc::new(Conditions::new(Arc::clone(&readiness), Arc::clone(&sink)));
    let queue = Arc::new(Queue::new(
        &limits,
        Arc::clone(&conditions),
        Arc::clone(&wake),
    ));
    let received = Arc::new(Received::default());

    let watchdog = Watchdog::new();
    watchdog
        .clone()
        .start(watchdog::CHECK_PERIOD, watchdog::expire)
        .map_err(RunError::Watchdog)?;
    let gate_task = tokio::spawn(gate::run(
        gate_listener,
        SocketAddr::from((Ipv4Addr::LOCALHOST, config.upstream_port)),
        readiness.subscribe(),
        Arc::clone(&sink),
        watchdog.register(watchdog::GATE_ACCEPTOR),
        gate::LIMITS,
    ));
    let (stop_child, child_stop) = oneshot::channel();
    let supervisor = tokio::spawn(child::supervise(
        config.child,
        Arc::clone(&readiness),
        Arc::clone(&sink),
        child::LIMITS,
        child_stop,
    ));
    let (start_drain, drain) = oneshot::channel();
    let (stalled, stall_requests) = watch::channel(None);
    let reader = Reader {
        endpoint: format!("tcp://127.0.0.1:{}", config.zmq_port),
        limits,
        queue: Arc::clone(&queue),
        conditions: Arc::clone(&conditions),
        received: Arc::clone(&received),
        sink: Arc::clone(&sink),
        gate: readiness.subscribe(),
        stalled: stall_requests,
    };
    let reader_task = tokio::spawn(reader.run(watchdog.register(watchdog::READER), drain));
    let (request_flush, flush_requests) = oneshot::channel();
    let writer = Writer {
        store: DatabaseStore::new(options, run_id),
        queue: Arc::clone(&queue),
        events: Arc::clone(&events),
        conditions: Arc::clone(&conditions),
        wake,
        limits,
        flush_interval: config.flush_interval,
        jitter: random_jitter,
    };
    let writer_task = tokio::spawn(writer.run(watchdog.register(watchdog::WRITER), flush_requests));
    let (request_final_read, final_read) = oneshot::channel();
    let shortfall = Shortfall {
        port: config.metrics_port,
        period: limits.shortfall_period,
        max_body: limits.metrics_body_bytes,
        deadline: budget.shortfall_read.saturating_sub(STEP_MARGIN),
        grace: limits.stall_grace,
        unreadable_after: limits.unreadable_after,
        retry: limits.metrics_retry,
        received,
        sink: Arc::clone(&sink),
        stalled,
        conditions: Arc::clone(&conditions),
    };
    let shortfall_task = tokio::spawn(shortfall.run(final_read));
    let (stop_health, health_stop) = oneshot::channel::<()>();
    let health = tokio::spawn(http::serve(
        health_listener,
        http::router(readiness.subscribe()),
        async move {
            health_stop.await.ok();
        },
        http::LIMITS,
    ));

    let mut tasks = [
        (Task::Gate, gate_task),
        (Task::Supervisor, supervisor),
        (Task::Reader, reader_task),
        (Task::Writer, writer_task),
        (Task::Shortfall, shortfall_task),
    ];
    // `reason`: the signal name, or `task_ended`. `ended`: the task that
    // ended by itself.
    let (reason, ended) = tokio::select! {
        signal = signals.recv() => (signal, None),
        task = first_ended(&mut tasks) => ("task_ended", Some(task)),
    };
    if let Some(task) = ended {
        error!(task = task.name(), "task_ended");
    }
    info!(reason, "shutdown_started");
    let [
        gate_task,
        supervisor,
        reader_task,
        writer_task,
        shortfall_task,
    ] = tasks.map(|(task, handle)| {
        // A task that ended has nothing to stop.
        (ended != Some(task)).then_some(handle)
    });

    stop_health.send(()).ok();
    // Step 1.
    readiness.shut();
    stop_task(gate_task, budget.gate_close, Task::Gate).await;
    // Step 2.
    start_drain
        .send(budget.zmq_drain.saturating_sub(STEP_MARGIN))
        .ok();
    stop_task(reader_task, budget.zmq_drain, Task::Reader).await;
    // Step 3.
    request_final_read.send(()).ok();
    stop_task(shortfall_task, budget.shortfall_read, Task::Shortfall).await;
    // Step 4.
    stop_child.send(()).ok();
    stop_task(supervisor, budget.child_stop, Task::Supervisor).await;
    // Step 5. The `stopped` event goes into the final flush.
    sink.record(IngestEvent::now(EventKind::Stopped { reason }));
    let (done, report) = oneshot::channel();
    let requested = request_flush
        .send(ShutdownRequest {
            deadline: budget.flush.saturating_sub(STEP_MARGIN),
            done,
        })
        .is_ok();
    let report = if requested {
        timeout(budget.flush, report)
            .await
            .ok()
            .and_then(Result::ok)
    } else {
        None
    };
    stop_task(writer_task, STEP_MARGIN, Task::Writer).await;
    // Step 6.
    let report = report.unwrap_or_else(|| {
        let stats = queue.stats();
        Report {
            records: stats.items,
            signal_rows: stats.signal_rows,
            events: events.len(),
            events_not_kept: events.not_kept(),
        }
    });
    if !report.is_complete() {
        error!(
            records = report.records,
            signal_rows = report.signal_rows,
            events = report.events,
            count = report.events_not_kept,
            "flush_incomplete"
        );
    }
    events.log_pending();
    log_task.abort();
    if let Err(error) = health.await
        && error.is_panic()
    {
        warn!(task = "health_server", "task_panicked");
    }
    info!("shutdown_complete");
    match ended {
        Some(task) => Err(RunError::TaskEnded(task.name())),
        None if !report.is_complete() => Err(RunError::FlushIncomplete),
        None => Ok(()),
    }
}

/// Waits until one of the tasks ends and returns it.
async fn first_ended(tasks: &mut [(Task, JoinHandle<()>)]) -> Task {
    std::future::poll_fn(|context| {
        for (task, handle) in tasks.iter_mut() {
            if std::pin::Pin::new(handle).poll(context).is_ready() {
                return std::task::Poll::Ready(*task);
            }
        }
        std::task::Poll::Pending
    })
    .await
}

/// Waits for a task that was asked to stop, at most `bound`. After the
/// bound, aborts the task and waits until it has ended: an aborted task
/// ends at its next `await`.
async fn stop_task(task: Option<JoinHandle<()>>, bound: Duration, name: Task) {
    let Some(mut task) = task else {
        return;
    };
    match timeout(bound, &mut task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if error.is_panic() {
                warn!(task = name.name(), "task_panicked");
            }
        }
        Err(_elapsed) => {
            warn!(task = name.name(), "task_stop_timeout");
            task.abort();
            task.await.ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Limits;

    #[test]
    fn the_shutdown_budget_fits_the_draining_time() {
        assert_eq!(BUDGET.total(), Duration::from_secs(26));
        assert!(BUDGET.total() < DRAINING);
        // ADR 0001 gives these two bounds.
        assert_eq!(BUDGET.zmq_drain, Duration::from_secs(3));
        assert_eq!(BUDGET.flush, Duration::from_secs(20));
        // The health server stops in parallel with the steps.
        assert!(http::LIMITS.shutdown_deadline < DRAINING);
        // The writer join after the flush is the only time outside the
        // budget of the steps.
        assert!(BUDGET.total() + STEP_MARGIN < DRAINING);
    }

    #[test]
    fn the_inner_bounds_fit_their_steps() {
        assert!(gate::LIMITS.close_deadline < BUDGET.gate_close);
        assert!(child::LIMITS.kill_wait + child::LIMITS.output_drain < BUDGET.child_stop);
        // The drain quiet time fits the drain bound that the reader gets.
        assert!(Some(Limits::PRODUCTION.drain_quiet) < BUDGET.zmq_drain.checked_sub(STEP_MARGIN));
        assert!(STEP_MARGIN < BUDGET.shortfall_read);
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_that_does_not_end_is_aborted_and_joined_at_its_bound() {
        let started = tokio::time::Instant::now();
        let (dropped, mut gone) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _keep = dropped;
            std::future::pending::<()>().await;
        });
        stop_task(Some(task), Duration::from_secs(3), Task::Reader).await;
        assert_eq!(started.elapsed(), Duration::from_secs(3));
        // The aborted task has ended: its state is dropped.
        assert!(gone.try_recv().is_err());
        assert!(matches!(
            gone.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_that_ends_in_time_is_not_aborted() {
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(tokio::time::sleep(Duration::from_secs(1)));
        stop_task(Some(task), Duration::from_secs(3), Task::Gate).await;
        assert_eq!(started.elapsed(), Duration::from_secs(1));
        stop_task(None, Duration::from_secs(3), Task::Gate).await;
    }
}
