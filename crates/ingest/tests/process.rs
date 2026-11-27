//! Tests of the `ingest` binary as a process: commands, configuration
//! failures, the health endpoint, the closed gate, the child supervision
//! with a fake `fleet-telemetry`, and the shutdown sequence on SIGTERM and
//! SIGINT. The tests of `migrate` against a database are in
//! `tests/storage/`, because the binary always uses the schema `raw`. The
//! tests of the delivery path with the Rust fake child are in
//! `tests/pipeline.rs` and `tests/storage/pipeline.rs`.
//!
//! The fake child is `tests/fixtures/fake-fleet-telemetry`. It opens no
//! port, so the SUB socket never connects, the gate stays closed, and no
//! connection reaches the child.
//!
//! The `cfg(test)` attribute marks the helper functions as test code for
//! Clippy, so that they may use `unwrap` and `panic` like the tests do.
#![cfg(test)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_ingest");
const FAKE_CHILD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-fleet-telemetry"
);

/// An invented VIN-shaped token that the fake child prints.
const FAKE_VIN: &str = "0FAKEVXN000000001";

/// Time limit for each wait on the process. A failing test then stops
/// instead of hanging.
const WAIT: Duration = Duration::from_secs(10);

/// A loopback port that is free now.
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The variables of a valid `run` configuration with the fake child in the
/// given mode (`run` or `exit`).
///
/// The shell fake child opens no port, so the SUB socket never connects
/// and the gate stays closed. No database answers: port 1 of the loopback
/// interface refuses the connection. The final flush therefore cannot
/// store the events, and its bound is short (a debug build reads
/// `INGEST_TEST_FLUSH_DEADLINE_MS`).
fn run_vars(mode: &str) -> Vec<(&'static str, String)> {
    vec![
        ("PORT", "0".to_owned()),
        ("INGEST_GATE_PORT", "0".to_owned()),
        ("INGEST_UPSTREAM_PORT", free_port().to_string()),
        ("INGEST_ZMQ_PORT", free_port().to_string()),
        ("INGEST_METRICS_PORT", free_port().to_string()),
        ("INGEST_CHILD_PROGRAM", FAKE_CHILD.to_owned()),
        ("INGEST_CHILD_CONFIG", format!("/fake-config/{mode}")),
        (
            "DATABASE_URL",
            "postgresql://telemetry_ingest:hunter2@127.0.0.1:1/neondb".to_owned(),
        ),
        ("INGEST_TEST_FLUSH_DEADLINE_MS", "500".to_owned()),
    ]
}

/// The exit code when the final flush cannot store the events: no
/// database answers in these tests (ADR 0001, "Shutdown", step 6).
const FLUSH_INCOMPLETE: i32 = 1;

/// A running `ingest` process. Drop kills the process, so that a failed
/// assertion cannot leave a server running.
struct Process {
    child: Child,
    lines: Receiver<String>,
    events: Vec<Value>,
}

impl Process {
    /// Starts the binary with a cleared environment and the given variables.
    fn spawn<V: AsRef<str>>(args: &[&str], vars: &[(&str, V)]) -> Self {
        Self::spawn_with_input(args, vars, None)
    }

    /// Starts the binary as [`Process::spawn`] does. With `input`, its
    /// standard input is a pipe that holds these bytes and then ends.
    fn spawn_with_input<V: AsRef<str>>(
        args: &[&str],
        vars: &[(&str, V)],
        input: Option<&[u8]>,
    ) -> Self {
        let mut child = Command::new(BINARY)
            .args(args)
            .env_clear()
            .envs(vars.iter().map(|(name, value)| (name, value.as_ref())))
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input).unwrap();
            // The drop closes the pipe: the reader then sees the end.
        }
        // A thread reads standard output, so that each wait for a line can
        // have a time limit.
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            events: Vec::new(),
        }
    }

    /// Reads log events until one matches `wanted`, and returns it.
    fn read_until(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        let started = Instant::now();
        loop {
            let remaining = WAIT.saturating_sub(started.elapsed());
            let line = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|error| panic!("no such event: {error}: {:?}", self.events));
            let event: Value = serde_json::from_str(&line).unwrap();
            self.events.push(event.clone());
            if wanted(&event) {
                return event;
            }
        }
    }

    /// Reads log events until the event with this message and returns its
    /// port.
    fn read_port(&mut self, message: &str) -> u16 {
        let event = self.read_until(|event| event["message"] == message);
        u16::try_from(event["port"].as_u64().unwrap()).unwrap()
    }

    fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([format!("-{signal}"), self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Waits for the exit and reads the remaining events. Standard error
    /// must stay empty: all output is JSON on standard output.
    fn wait(&mut self) -> ExitStatus {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < WAIT, "the process did not stop");
            std::thread::sleep(Duration::from_millis(20));
        };
        loop {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) => self.events.push(
                    serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON: {line}")),
                ),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("standard output did not close"),
            }
        }
        let mut stderr = String::new();
        let mut pipe = self.child.stderr.take().unwrap();
        pipe.read_to_string(&mut stderr).unwrap();
        assert_eq!(stderr, "");
        status
    }

    fn messages(&self) -> Vec<&str> {
        self.events
            .iter()
            .map(|event| event["message"].as_str().unwrap())
            .collect()
    }

    fn with_message(&self, message: &str) -> Vec<&Value> {
        self.events
            .iter()
            .filter(|event| event["message"] == message)
            .collect()
    }

    fn text(&self) -> String {
        self.events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // The process has already stopped in a passing test. Errors are
        // expected then, and do not matter.
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// True when `expected` appears in `actual` in the same order, with other
/// items between them allowed.
fn in_order(actual: &[&str], expected: &[&str]) -> bool {
    let mut rest = actual.iter();
    expected
        .iter()
        .all(|wanted| rest.any(|item| item == wanted))
}

/// Runs a command that stops by itself and returns its status and events.
fn run_to_end<V: AsRef<str>>(args: &[&str], vars: &[(&str, V)]) -> (ExitStatus, Process) {
    let mut process = Process::spawn(args, vars);
    let status = process.wait();
    (status, process)
}

fn get_health(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn process_exists(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Starts `ingest run` with the fake child in mode `run` and waits until
/// the child runs. Returns the process, the health port, the gate port, and
/// the child process ID.
fn start_with_running_child() -> (Process, u16, u16, u64) {
    let mut process = Process::spawn(&["run"], &run_vars("run"));
    let health_port = process.read_port("listening");
    let gate_port = process.read_port("gate_listening");
    let started = process.read_until(|event| event["message"] == "child_started");
    let pid = started["pid"].as_u64().unwrap();
    // The last line of the fake child before its wait loop.
    process.read_until(|event| event["message"] == "child_output" && event["stream"] == "stderr");
    (process, health_port, gate_port, pid)
}

fn run_and_stop_with(signal: &str) {
    let (mut process, health_port, gate_port, pid) = start_with_running_child();

    // The SUB socket never connects: the gate stays closed, so
    // `/health` answers 503 and the gate closes each connection at once.
    let health = get_health(health_port);
    assert!(health.starts_with("HTTP/1.1 503 "), "{health}");
    let mut client = TcpStream::connect(("127.0.0.1", gate_port)).unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    let mut received = Vec::new();
    client.read_to_end(&mut received).ok();
    assert_eq!(received, b"");

    let started = Instant::now();
    process.signal(signal);
    let status = process.wait();
    let elapsed = started.elapsed();

    assert_eq!(
        status.code(),
        Some(FLUSH_INCOMPLETE),
        "{:?}",
        process.events
    );
    // No client stalls and the SUB socket never connected, so no step but
    // the short flush waits for its bound.
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    let messages = process.messages();
    // A debug build warns about the test overrides before it starts.
    assert_eq!(
        messages[..3],
        ["test_overrides", "listening", "gate_listening"]
    );
    assert!(
        in_order(
            &messages,
            &[
                "shutdown_started",
                "gate_stopped",
                "child_killed",
                "ingest_event",
                "flush_incomplete",
                "shutdown_complete",
            ]
        ),
        "{messages:?}"
    );
    // The lost events: `started` and `stopped`.
    assert_eq!(process.with_message("flush_incomplete")[0]["events"], 2);
    assert_eq!(process.with_message("flush_incomplete")[0]["records"], 0);
    assert!(
        in_order(&messages, &["shutdown_started", "health_server_stopped"]),
        "{messages:?}"
    );
    assert_eq!(
        messages[messages.len() - 2..],
        ["shutdown_complete", "run_failed"]
    );
    assert_eq!(
        process.with_message("run_failed")[0]["error"],
        "the final flush did not store everything"
    );
    let shutdown = process.with_message("shutdown_started");
    assert_eq!(shutdown[0]["reason"], format!("SIG{signal}"));
    let kinds: Vec<&str> = process
        .with_message("ingest_event")
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["started", "stopped"]);
    assert_eq!(process.with_message("child_killed")[0]["exit_signal"], 9);
    assert!(!process_exists(pid), "the child still runs");
}

#[test]
fn run_stops_on_sigterm_in_the_order_of_adr_0001() {
    run_and_stop_with("TERM");
}

#[test]
fn run_stops_on_sigint_in_the_order_of_adr_0001() {
    run_and_stop_with("INT");
}

#[test]
fn run_stops_at_the_deadline_when_a_health_client_stalls() {
    let (mut process, health_port, _, pid) = start_with_running_child();

    // The client sends only a part of a request head and then waits.
    let mut client = TcpStream::connect(("127.0.0.1", health_port)).unwrap();
    client.write_all(b"GET /health HTTP/1.1\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    process.signal("TERM");
    let status = process.wait();
    let elapsed = started.elapsed();

    // `http::LIMITS.shutdown_deadline` is 3 s. The gate and the child stop
    // in parallel with the health server.
    assert_eq!(
        status.code(),
        Some(FLUSH_INCOMPLETE),
        "{:?}",
        process.events
    );
    assert!(elapsed >= Duration::from_secs(3), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    let messages = process.messages();
    assert!(
        in_order(
            &messages,
            &[
                "shutdown_started",
                "gate_stopped",
                "child_killed",
                "connections_closed_at_deadline",
                "health_server_stopped",
                "shutdown_complete",
            ]
        ),
        "{messages:?}"
    );
    assert_eq!(
        process.with_message("connections_closed_at_deadline")[0]["connections"],
        1
    );
    assert!(!process_exists(pid), "the child still runs");
}

#[test]
fn the_child_output_reaches_the_log_without_the_vin_and_the_environment() {
    let mut vars = run_vars("run");
    // ingest clears the environment of the child, so this value must not
    // reach it.
    vars.push(("INGEST_TEST_MARKER", "hunter2".to_owned()));
    // The standard input of ingest holds data. The child must not see it.
    let mut process = Process::spawn_with_input(&["run"], &vars, Some(b"stdin of ingest\n"));
    process.read_until(|event| event["message"] == "child_output" && event["stream"] == "stderr");
    process.signal("TERM");
    assert_eq!(process.wait().code(), Some(FLUSH_INCOMPLETE));

    let text = process.text();
    assert!(!text.contains(FAKE_VIN), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    let mut lines: Vec<(&str, &str)> = process
        .with_message("child_output")
        .iter()
        .map(|event| {
            (
                event["stream"].as_str().unwrap(),
                event["child_line"].as_str().unwrap(),
            )
        })
        .collect();
    // The two streams are separate pipes, so their lines can come in any
    // order. Lines of one stream keep their order: the stable sort puts
    // the stderr lines first and keeps the order inside each stream.
    lines.sort_by_key(|(stream, _)| *stream);
    assert_eq!(
        lines,
        [
            ("stderr", "error for vin [vin]"),
            ("stdout", "env=absent"),
            ("stdout", "stdin=empty"),
            ("stdout", "connection from [vin] accepted"),
        ]
    );
}

#[test]
fn a_child_that_exits_is_recorded_and_started_again_after_1_s() {
    let mut process = Process::spawn(&["run"], &run_vars("exit"));
    process.read_until(|event| event["message"] == "child_started");
    let exited = process.read_until(|event| event["message"] == "child_exited");
    assert_eq!(exited["exit_code"], 3);
    // The log task writes the line of the `ingest_event` record, so it can
    // come before or after the lines of the supervisor.
    let scheduled = process.read_until(|event| event["message"] == "child_restart_scheduled");
    assert_eq!(scheduled["delay_ms"], 1000);
    let restarted = process.read_until(|event| event["message"] == "child_started");
    // The restart waited for the delay. The log timestamps come from the
    // process itself, so the load of the test machine does not change
    // them. 1 ms of margin for the clock resolution.
    let time = |event: &Value| {
        event["timestamp"]
            .as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .unwrap()
    };
    let waited = time(&restarted).duration_since(time(&scheduled));
    assert!(
        waited >= jiff::SignedDuration::from_millis(999),
        "{waited:?}"
    );

    process.signal("TERM");
    assert_eq!(
        process.wait().code(),
        Some(FLUSH_INCOMPLETE),
        "{:?}",
        process.events
    );
    let recorded = process.with_message("ingest_event");
    let exits: Vec<&&Value> = recorded
        .iter()
        .filter(|event| event["kind"] == "child_exited")
        .collect();
    assert!(!exits.is_empty(), "{:?}", process.messages());
    assert_eq!(exits[0]["reason"], "exit code 3");
}

#[test]
fn run_without_port_stops_with_a_safe_message() {
    let (status, process) = run_to_end::<&str>(&["run"], &[]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(process.messages(), ["invalid_configuration"]);
    assert_eq!(process.events[0]["error"], "PORT is not set");
    assert_eq!(process.events[0]["level"], "ERROR");
}

#[test]
fn run_with_a_missing_child_variable_stops_with_a_safe_message() {
    let vars: Vec<_> = run_vars("run")
        .into_iter()
        .filter(|(name, _)| *name != "INGEST_CHILD_PROGRAM")
        .collect();
    let (status, process) = run_to_end(&["run"], &vars);
    assert_eq!(status.code(), Some(1));
    assert_eq!(process.messages(), ["invalid_configuration"]);
    assert_eq!(
        process.events[0]["error"],
        "INGEST_CHILD_PROGRAM is not set"
    );
}

#[test]
fn run_with_a_database_url_that_does_not_parse_stops_with_a_static_message() {
    let vars: Vec<(&str, String)> = run_vars("run")
        .into_iter()
        .map(|(name, value)| {
            if name == "DATABASE_URL" {
                (
                    name,
                    "postgresql://telemetry_ingest:hunter2@db.invalid:notaport/x".to_owned(),
                )
            } else {
                (name, value)
            }
        })
        .collect();
    let (status, process) = run_to_end(&["run"], &vars);
    assert_eq!(status.code(), Some(1));
    // The URL is checked before any port is bound and before the child
    // starts.
    assert_eq!(process.messages(), ["test_overrides", "run_failed"]);
    assert_eq!(
        process.events[1]["error"],
        "cannot use DATABASE_URL: database answered: configuration"
    );
    let text = process.text();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("db.invalid"), "{text}");
}

#[test]
fn run_with_an_invalid_port_does_not_log_the_value() {
    let value = "postgres://user:hunter2@db.invalid/x";
    let (status, process) = run_to_end(&["run"], &[("PORT", value)]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(process.messages(), ["invalid_configuration"]);
    let text = process.text();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("db.invalid"), "{text}");
}

#[test]
fn migrate_without_a_url_stops_with_a_safe_message() {
    let (status, process) = run_to_end::<&str>(&["migrate"], &[]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(process.messages(), ["invalid_configuration"]);
    assert_eq!(
        process.events[0]["error"],
        "MIGRATION_DATABASE_URL is not set"
    );
}

#[test]
fn migrate_without_a_database_logs_a_static_code_only() {
    // Port 1 on the loopback interface refuses the connection, so the
    // command fails before TLS and before any session.
    let url = "postgresql://telemetry_migrator:hunter2@127.0.0.1:1/neondb?sslmode=disable";
    let (status, process) = run_to_end(&["migrate"], &[("MIGRATION_DATABASE_URL", url)]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(process.messages(), ["migrate_failed"]);
    assert_eq!(process.events[0]["class"], "no_answer");
    assert_eq!(process.events[0]["code"], "io");
    let text = process.text();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("telemetry_migrator"), "{text}");
}

#[test]
fn an_invalid_command_line_exits_with_code_2() {
    for args in [&[][..], &["serve"][..], &["run", "extra"][..]] {
        let (status, process) = run_to_end::<&str>(args, &[]);
        assert_eq!(status.code(), Some(2), "{args:?}");
        assert_eq!(process.messages(), ["invalid_command"]);
        let text = process.text();
        assert!(!text.contains("serve"), "{text}");
        assert!(!text.contains("extra"), "{text}");
    }
}
