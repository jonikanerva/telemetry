//! Supervision of the child process `fleet-telemetry` (ADR 0001, "Layout").
//!
//! - **Start.** The supervisor starts the program of [`ChildCommand`] with
//!   the arguments `-config <path>`, a cleared environment (ADR 0002,
//!   "Roles and privileges"), standard input from `/dev/null`, and standard
//!   output and standard error through [`crate::child_output`]. After a
//!   successful start it sets gate condition 1 ([`Readiness`]).
//! - **Exit by itself.** The supervisor clears gate condition 1, so the gate
//!   closes. It logs `child_exited` with the exit code or the signal,
//!   records a `child_exited` event (ADR 0002), and starts the child again
//!   after a delay. The delay is 1 s and doubles after each exit up to
//!   30 s. A failed start counts as an exit with the reason
//!   `spawn failed`.
//! - **Provisional rule.** ADR 0001 gives no rule to make the delay short
//!   again. The supervisor uses 1 s again after a run of at least 30 s (the
//!   longest delay), so that a single exit after a long run does not wait
//!   30 s.
//! - **Stop.** The shutdown sequence asks for the stop in step 4 (ADR 0001,
//!   "Shutdown"). The supervisor sends SIGKILL, waits at most
//!   [`SupervisorLimits::kill_wait`] for the exit, and then waits at most
//!   [`SupervisorLimits::output_drain`] for the last output lines. A
//!   process that inherited the output pipes can hold them open; the drain
//!   bound covers that case. The stop is not an exit: it records no event
//!   and does not start the child again. After the gate is shut (step 1),
//!   the supervisor starts no new child, so that a new child cannot reset
//!   the counters of the final shortfall read (step 3).
//!
//! PID 1 duties: `ingest` is PID 1 in the container. Tokio reaps the child
//! that the supervisor waits for. The supervisor also sets kill-on-drop, so
//! a dropped child handle sends SIGKILL, and Tokio reaps such a child in the
//! background. A process that another process leaves behind is adopted by
//! PID 1 and is not reaped: the first-party Go source of `fleet-telemetry`
//! v0.10.1 starts no process (no `os/exec`, `syscall.ForkExec`, or
//! `os.StartProcess`), and `entrypoint.sh` ends with `exec`, so no such
//! process exists in the deployment. Reaping any process with
//! `waitpid(-1)` would need `unsafe` code or another crate, and it would
//! race with the wait of Tokio. The lead decided for no general reaping (PR
//! #18). Evidence limit: the search covered the first-party Go source of
//! the tag (commit `6689adb`), not its Go dependencies, and was not checked
//! on a running container. A process that a dependency starts and leaves
//! behind would stay a zombie; it uses a process table entry and nothing
//! else.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::pin::pin;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, timeout};
use tracing::{info, warn};

use crate::child_output;
use crate::events::{ChildExit, EventKind, EventSink, IngestEvent};
use crate::readiness::Readiness;

/// The program and the configuration file of `fleet-telemetry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildCommand {
    /// Absolute path of the program.
    pub program: PathBuf,
    /// Absolute path of the configuration file, given as `-config <path>`.
    pub config: PathBuf,
}

impl ChildCommand {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .arg("-config")
            .arg(&self.config)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}

/// Delays and bounds of the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorLimits {
    /// Delay before the first restart.
    pub initial_delay: Duration,
    /// The longest delay before a restart.
    pub max_delay: Duration,
    /// A run of at least this time makes the next delay the initial delay.
    pub stable_after: Duration,
    /// The longest wait for the exit after SIGKILL.
    pub kill_wait: Duration,
    /// The longest wait for the last output lines after an exit.
    pub output_drain: Duration,
}

/// The delays of ADR 0001. The kill wait and the output drain are part of
/// the shutdown budget of [`crate::lifecycle`].
pub const LIMITS: SupervisorLimits = SupervisorLimits {
    initial_delay: Duration::from_secs(1),
    max_delay: Duration::from_secs(30),
    stable_after: Duration::from_secs(30),
    kill_wait: Duration::from_millis(500),
    output_drain: Duration::from_millis(250),
};

/// Runs the child and starts it again after each exit, until `stop`
/// completes or its sender is dropped. Then stops the child as the module
/// documentation describes.
pub async fn supervise(
    command: ChildCommand,
    readiness: Arc<Readiness>,
    sink: Arc<dyn EventSink>,
    limits: SupervisorLimits,
    stop: oneshot::Receiver<()>,
) {
    let mut stop = pin!(async move {
        stop.await.ok();
    });
    let mut delays = Backoff::new(limits);
    loop {
        if readiness.state().is_shut() {
            // The shutdown has started: a new child would only reset the
            // counters of the final shortfall read. Wait for step 4.
            info!("child_not_restarted");
            stop.await;
            return;
        }
        let ran_for = match command.command().spawn() {
            Err(error) => {
                warn!(error = %error, "child_spawn_failed");
                sink.record(IngestEvent::now(EventKind::ChildExited {
                    exit: ChildExit::SpawnFailed,
                }));
                Duration::ZERO
            }
            Ok(mut child) => {
                let started = Instant::now();
                info!(pid = child.id(), "child_started");
                let mut output = forward_output(&mut child);
                readiness.set_child_running(true);
                tokio::select! {
                    status = child.wait() => {
                        readiness.set_child_running(false);
                        let exit = match status {
                            Ok(status) => {
                                warn!(
                                    exit_code = status.code(),
                                    exit_signal = status.signal(),
                                    "child_exited"
                                );
                                exit_of(status)
                            }
                            Err(error) => {
                                warn!(error = %error, "child_wait_failed");
                                ChildExit::WaitFailed
                            }
                        };
                        sink.record(IngestEvent::now(EventKind::ChildExited { exit }));
                        drain(&mut output, limits.output_drain).await;
                        started.elapsed()
                    }
                    () = &mut stop => {
                        readiness.set_child_running(false);
                        kill(child, &mut output, limits).await;
                        return;
                    }
                }
            }
        };
        let delay = delays.next(ran_for);
        info!(delay_ms = millis(delay), "child_restart_scheduled");
        tokio::select! {
            () = sleep(delay) => {}
            () = &mut stop => {
                info!("child_not_restarted");
                return;
            }
        }
    }
}

fn forward_output(child: &mut Child) -> JoinSet<()> {
    let mut output = JoinSet::new();
    if let Some(stdout) = child.stdout.take() {
        output.spawn(child_output::forward(stdout, "stdout"));
    }
    if let Some(stderr) = child.stderr.take() {
        output.spawn(child_output::forward(stderr, "stderr"));
    }
    output
}

/// Sends SIGKILL and waits for the exit and the output, each with a bound.
async fn kill(mut child: Child, output: &mut JoinSet<()>, limits: SupervisorLimits) {
    if let Err(error) = child.start_kill() {
        warn!(error = %error, "child_kill_failed");
    }
    match timeout(limits.kill_wait, child.wait()).await {
        Ok(Ok(status)) => info!(
            exit_code = status.code(),
            exit_signal = status.signal(),
            "child_killed"
        ),
        Ok(Err(error)) => warn!(error = %error, "child_wait_failed"),
        Err(_elapsed) => warn!("child_kill_unconfirmed"),
    }
    drain(output, limits.output_drain).await;
}

/// Waits for the output tasks, then aborts the ones that still run.
async fn drain(output: &mut JoinSet<()>, bound: Duration) {
    let drained = timeout(bound, async { while output.join_next().await.is_some() {} }).await;
    if drained.is_err() {
        warn!("child_output_cut");
        output.shutdown().await;
    }
}

fn exit_of(status: ExitStatus) -> ChildExit {
    match (status.code(), status.signal()) {
        (Some(code), _) => ChildExit::Code(code),
        (None, Some(signal)) => ChildExit::Signal(signal),
        (None, None) => ChildExit::Unknown,
    }
}

/// The restart delays: the initial delay, doubled after each exit up to
/// the maximum, and the initial delay again after a stable run.
#[derive(Debug)]
struct Backoff {
    next: Duration,
    limits: SupervisorLimits,
}

impl Backoff {
    const fn new(limits: SupervisorLimits) -> Self {
        Self {
            next: limits.initial_delay,
            limits,
        }
    }

    /// The delay after a run of `ran_for`.
    fn next(&mut self, ran_for: Duration) -> Duration {
        if ran_for >= self.limits.stable_after {
            self.next = self.limits.initial_delay;
        }
        let delay = self.next;
        self.next = delay
            .checked_mul(2)
            .unwrap_or(self.limits.max_delay)
            .min(self.limits.max_delay);
        delay
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::events::recording::RecordingSink;
    use crate::logging::{ALLOWED_FIELDS, capture};
    use crate::readiness::Pipeline;

    const FAKE_CHILD: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake-fleet-telemetry"
    );

    /// Time limit for each wait in a test.
    const WAIT: Duration = Duration::from_secs(10);

    const TEST_LIMITS: SupervisorLimits = SupervisorLimits {
        initial_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(80),
        stable_after: Duration::from_secs(60),
        ..LIMITS
    };

    fn command(mode: &str) -> ChildCommand {
        ChildCommand {
            program: PathBuf::from(FAKE_CHILD),
            config: PathBuf::from(format!("/fake-config/{mode}")),
        }
    }

    struct Supervised {
        readiness: Arc<Readiness>,
        sink: RecordingSink,
        stop: oneshot::Sender<()>,
        task: tokio::task::JoinHandle<()>,
    }

    fn start(command: ChildCommand) -> Supervised {
        let sink = RecordingSink::default();
        let readiness = Arc::new(Readiness::new(Arc::new(sink.clone())));
        // A ready pipeline, so that the child alone decides the gate.
        readiness.set_pipeline(Pipeline::Ready);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(supervise(
            command,
            Arc::clone(&readiness),
            Arc::new(sink.clone()),
            TEST_LIMITS,
            stopped,
        ));
        Supervised {
            readiness,
            sink,
            stop,
            task,
        }
    }

    impl Supervised {
        async fn wait_until(&self, done: impl Fn(&Self) -> bool) {
            tokio::time::timeout(WAIT, async {
                while !done(self) {
                    sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the condition did not become true");
        }

        /// Asks for the stop and returns the time until the supervisor
        /// returned.
        async fn stop(self) -> (RecordingSink, Duration) {
            let started = Instant::now();
            self.stop.send(()).unwrap();
            tokio::time::timeout(WAIT, self.task)
                .await
                .expect("the supervisor did not stop")
                .unwrap();
            (self.sink, started.elapsed())
        }
    }

    fn events(capture: &capture::Capture) -> Vec<Value> {
        capture
            .text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn with_message<'a>(events: &'a [Value], message: &str) -> Vec<&'a Value> {
        events
            .iter()
            .filter(|event| event["message"] == message)
            .collect()
    }

    fn process_exists(pid: u64) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[tokio::test]
    async fn a_child_that_exits_closes_the_gate_and_restarts_with_growing_delays() {
        let (capture, _guard) = capture::install(ALLOWED_FIELDS);
        let supervised = start(command("exit"));
        let exits = |s: &Supervised| {
            s.sink
                .events()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ChildExited { .. }))
                .count()
        };
        supervised.wait_until(|s| exits(s) >= 4).await;
        let (sink, _) = supervised.stop().await;

        for event in sink.events() {
            if let EventKind::ChildExited { exit } = event.kind {
                assert_eq!(exit, ChildExit::Code(3));
            }
        }
        // Each run opens the gate and each exit closes it.
        let names = sink.names();
        assert_eq!(
            names[..6],
            [
                "gate_opened",
                "gate_closed",
                "child_exited",
                "gate_opened",
                "gate_closed",
                "child_exited"
            ]
        );
        let events = events(&capture);
        let delays: Vec<u64> = with_message(&events, "child_restart_scheduled")
            .iter()
            .map(|event| event["delay_ms"].as_u64().unwrap())
            .collect();
        assert_eq!(delays[..4], [20, 40, 80, 80]);
        let exited = with_message(&events, "child_exited");
        assert_eq!(exited[0]["exit_code"], 3);
    }

    #[tokio::test]
    async fn the_stop_kills_a_running_child_without_an_exit_event() {
        let (capture, _guard) = capture::install(ALLOWED_FIELDS);
        let supervised = start(command("run"));
        supervised
            .wait_until(|s| s.readiness.state().is_open())
            .await;
        // Let the child write its first lines.
        supervised
            .wait_until(|_| with_message(&events(&capture), "child_output").len() >= 3)
            .await;

        let (sink, elapsed) = supervised.stop().await;
        assert!(
            elapsed < TEST_LIMITS.kill_wait + TEST_LIMITS.output_drain,
            "{elapsed:?}"
        );
        assert_eq!(sink.names(), ["gate_opened", "gate_closed"]);

        let events = events(&capture);
        let killed = with_message(&events, "child_killed");
        assert_eq!(killed.len(), 1, "{events:?}");
        assert_eq!(killed[0]["exit_signal"], 9);
        assert_eq!(with_message(&events, "child_restart_scheduled").len(), 0);
        let pid = with_message(&events, "child_started")[0]["pid"]
            .as_u64()
            .unwrap();
        assert!(!process_exists(pid), "the child still runs");
    }

    #[tokio::test]
    async fn no_new_child_starts_after_the_gate_is_shut() {
        let (capture, _guard) = capture::install(ALLOWED_FIELDS);
        let supervised = start(command("run"));
        supervised
            .wait_until(|s| s.readiness.state().is_open())
            .await;
        supervised.readiness.shut();
        let pid = with_message(&events(&capture), "child_started")[0]["pid"]
            .as_u64()
            .unwrap();
        // The child ends by itself during the shutdown.
        let killed = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        supervised
            .wait_until(|_| !with_message(&events(&capture), "child_not_restarted").is_empty())
            .await;
        // Ten restart delays later, still no new child.
        sleep(TEST_LIMITS.max_delay * 10).await;
        let (sink, elapsed) = supervised.stop().await;

        let events = events(&capture);
        assert_eq!(with_message(&events, "child_started").len(), 1);
        assert_eq!(with_message(&events, "child_restart_scheduled").len(), 1);
        // No child to kill: the stop does not wait for the kill bound.
        assert!(elapsed < TEST_LIMITS.kill_wait, "{elapsed:?}");
        assert_eq!(
            sink.events().last().map(|event| event.kind.clone()),
            Some(EventKind::ChildExited {
                exit: ChildExit::Signal(9)
            })
        );
    }

    #[tokio::test]
    async fn a_dropped_supervisor_kills_its_child() {
        let (capture, _guard) = capture::install(ALLOWED_FIELDS);
        let supervised = start(command("run"));
        supervised
            .wait_until(|s| s.readiness.state().is_open())
            .await;
        let pid = with_message(&events(&capture), "child_started")[0]["pid"]
            .as_u64()
            .unwrap();
        // An abort drops the child handle without the stop of step 4, for
        // example when the shutdown aborts the task at its bound.
        supervised.task.abort();
        supervised.task.await.ok();
        tokio::time::timeout(WAIT, async {
            while process_exists(pid) {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the child still runs after the drop of its handle");
    }

    #[tokio::test]
    async fn the_child_output_reaches_the_log_without_the_vin() {
        let (capture, _guard) = capture::install(ALLOWED_FIELDS);
        let supervised = start(command("run"));
        supervised
            .wait_until(|_| with_message(&events(&capture), "child_output").len() >= 3)
            .await;
        supervised.stop().await;

        let text = capture.text();
        assert!(!text.contains("0FAKEVXN000000001"), "{text}");
        let events = events(&capture);
        let lines: Vec<(&str, &str)> = with_message(&events, "child_output")
            .iter()
            .map(|event| {
                (
                    event["stream"].as_str().unwrap(),
                    event["child_line"].as_str().unwrap(),
                )
            })
            .collect();
        assert!(
            lines.contains(&("stdout", "connection from [vin] accepted")),
            "{lines:?}"
        );
        assert!(
            lines.contains(&("stderr", "error for vin [vin]")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn a_program_that_cannot_start_is_recorded_and_retried() {
        let supervised = start(ChildCommand {
            program: PathBuf::from("/nonexistent/fleet-telemetry"),
            config: PathBuf::from("/nonexistent/config.json"),
        });
        supervised.wait_until(|s| s.sink.events().len() >= 2).await;
        let (sink, _) = supervised.stop().await;
        for event in sink.events() {
            assert_eq!(
                event.kind,
                EventKind::ChildExited {
                    exit: ChildExit::SpawnFailed
                }
            );
        }
    }

    #[test]
    fn the_delay_doubles_up_to_30_s_and_resets_after_a_stable_run() {
        let mut delays = Backoff::new(LIMITS);
        let seconds: Vec<u64> = (0..7)
            .map(|_| delays.next(Duration::from_secs(1)).as_secs())
            .collect();
        assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(delays.next(Duration::from_secs(30)), Duration::from_secs(1));
        assert_eq!(delays.next(Duration::ZERO), Duration::from_secs(2));
    }

    #[test]
    fn the_exit_status_becomes_a_code_or_a_signal() {
        assert_eq!(exit_of(ExitStatus::from_raw(3 << 8)), ChildExit::Code(3));
        assert_eq!(exit_of(ExitStatus::from_raw(9)), ChildExit::Signal(9));
    }
}
