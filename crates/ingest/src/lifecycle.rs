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
//! 2. Bind the port of `/health` and the gate port. A failure stops the
//!    start. The gate is closed.
//! 3. Record the `started` event.
//! 4. Start the watchdog thread and the gate acceptor with its heartbeat.
//! 5. Start the child supervisor ([`crate::child`]).
//! 6. Serve `/health`.
//!
//! The pipeline change adds the SUB connection and the start-up check after
//! step 5. They open the gate through [`crate::readiness`].
//!
//! Shutdown starts at the first SIGTERM or SIGINT, or when the gate task or
//! the supervisor task ends by itself (then the exit code is 1). The
//! health server stops at once, in parallel with the steps below; its own
//! deadline is 3 s ([`http::LIMITS`]). The steps of ADR 0001 run in order:
//!
//! 1. Shut the gate. The acceptor closes every forwarded connection.
//!    Bound: [`ShutdownBudget::gate_close`].
//! 2. Drain the SUB socket (pipeline change). Bound:
//!    [`ShutdownBudget::zmq_drain`].
//! 3. The final shortfall read (pipeline change). Bound:
//!    [`ShutdownBudget::shortfall_read`].
//! 4. Stop the child with SIGKILL. Bound: [`ShutdownBudget::child_stop`].
//! 5. Flush the queue (pipeline change). Bound: [`ShutdownBudget::flush`].
//! 6. The exit code of a failed flush (pipeline change).
//!
//! Then the sequence records the `stopped` event, waits for the health
//! server, and returns. The binary stops the runtime without a wait.
//!
//! Railway sends SIGKILL `drainingSeconds` (30 s) after SIGTERM. The sum of
//! the bounds, [`BUDGET`], is 26 s, so the sequence ends before the SIGKILL
//! with 4 s of margin. A step that does not end within its bound is
//! aborted, and the sequence goes on.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{error, info, warn};

use crate::config::RunConfig;
use crate::events::{EventKind, EventSink, IngestEvent, LogSink};
use crate::readiness::Readiness;
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

/// The shutdown budget. Steps 2, 3, and 5 belong to the pipeline change.
pub const BUDGET: ShutdownBudget = ShutdownBudget {
    gate_close: Duration::from_secs(1),
    zmq_drain: Duration::from_secs(3),
    shortfall_read: Duration::from_secs(1),
    child_stop: Duration::from_secs(1),
    flush: Duration::from_secs(20),
};

/// Railway `drainingSeconds` (ADR 0001): the time from SIGTERM to SIGKILL.
pub const DRAINING: Duration = Duration::from_secs(30);

/// A failure of the `run` command.
///
/// The text holds static words and operating system error text only.
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

/// Runs the `run` command until a shutdown signal arrives and the shutdown
/// sequence has ended.
///
/// # Errors
///
/// Returns [`RunError`] when the start fails, or when the gate task or the
/// supervisor task ended by itself.
pub async fn run(config: RunConfig) -> Result<(), RunError> {
    let signals = ShutdownSignals::install()?;
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

    let sink: Arc<dyn EventSink> = Arc::new(LogSink);
    sink.record(IngestEvent::now(EventKind::Started));
    let readiness = Arc::new(Readiness::new(Arc::clone(&sink)));

    let watchdog = Watchdog::new();
    let gate_heartbeat = watchdog.register(watchdog::GATE_ACCEPTOR);
    watchdog
        .start(watchdog::CHECK_PERIOD, watchdog::expire)
        .map_err(RunError::Watchdog)?;
    let mut gate_task = tokio::spawn(gate::run(
        gate_listener,
        SocketAddr::from((Ipv4Addr::LOCALHOST, config.upstream_port)),
        readiness.subscribe(),
        Arc::clone(&sink),
        gate_heartbeat,
        gate::LIMITS,
    ));
    let (stop_child, child_stop) = oneshot::channel();
    let mut supervisor = tokio::spawn(child::supervise(
        config.child,
        Arc::clone(&readiness),
        Arc::clone(&sink),
        child::LIMITS,
        child_stop,
    ));
    let (stop_health, health_stop) = oneshot::channel::<()>();
    let health = tokio::spawn(http::serve(
        health_listener,
        http::router(readiness.subscribe()),
        async move {
            health_stop.await.ok();
        },
        http::LIMITS,
    ));

    // `reason`: the signal name, or `task_ended`. `ended`: the task that
    // ended by itself.
    let (reason, ended) = tokio::select! {
        signal = signals.recv() => (signal, None),
        _ = &mut gate_task => ("task_ended", Some("gate")),
        _ = &mut supervisor => ("task_ended", Some("child_supervisor")),
    };
    if let Some(task) = ended {
        error!(task, "task_ended");
    }
    info!(reason, "shutdown_started");

    stop_health.send(()).ok();
    // Step 1.
    readiness.shut();
    if ended != Some("gate") {
        stop_task(gate_task, BUDGET.gate_close, "gate").await;
    }
    // Steps 2 and 3 belong to the pipeline change.
    // Step 4.
    stop_child.send(()).ok();
    if ended != Some("child_supervisor") {
        stop_task(supervisor, BUDGET.child_stop, "child_supervisor").await;
    }
    // Steps 5 and 6 belong to the pipeline change.
    sink.record(IngestEvent::now(EventKind::Stopped { reason }));
    if let Err(error) = health.await
        && error.is_panic()
    {
        warn!(task = "health_server", "task_panicked");
    }
    info!("shutdown_complete");
    ended.map_or(Ok(()), |task| Err(RunError::TaskEnded(task)))
}

/// Waits for a task that was asked to stop, at most `bound`. Aborts it
/// after the bound.
async fn stop_task(task: JoinHandle<()>, bound: Duration, name: &'static str) {
    let abort = task.abort_handle();
    match timeout(bound, task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if error.is_panic() {
                warn!(task = name, "task_panicked");
            }
        }
        Err(_elapsed) => {
            warn!(task = name, "task_stop_timeout");
            abort.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shutdown_budget_fits_the_draining_time() {
        assert_eq!(BUDGET.total(), Duration::from_secs(26));
        assert!(BUDGET.total() < DRAINING);
        // ADR 0001 gives these two bounds.
        assert_eq!(BUDGET.zmq_drain, Duration::from_secs(3));
        assert_eq!(BUDGET.flush, Duration::from_secs(20));
        // The health server stops in parallel with the steps.
        assert!(http::LIMITS.shutdown_deadline < DRAINING);
    }

    #[test]
    fn the_inner_bounds_fit_their_steps() {
        assert!(gate::LIMITS.close_deadline < BUDGET.gate_close);
        assert!(child::LIMITS.kill_wait + child::LIMITS.output_drain < BUDGET.child_stop);
    }
}
