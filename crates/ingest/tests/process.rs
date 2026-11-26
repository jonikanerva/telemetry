//! Tests of the `ingest` binary as a process: commands, configuration
//! failures, the health endpoint, and the stop on SIGTERM and SIGINT.
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

/// Time limit for each wait on the process. A failing test then stops
/// instead of hanging.
const WAIT: Duration = Duration::from_secs(10);

/// A running `ingest` process. Drop kills the process, so that a failed
/// assertion cannot leave a server running.
struct Process {
    child: Child,
    lines: Receiver<String>,
}

impl Process {
    /// Starts the binary with a cleared environment and the given variables.
    fn spawn(args: &[&str], vars: &[(&str, &str)]) -> Self {
        let mut child = Command::new(BINARY)
            .args(args)
            .env_clear()
            .envs(vars.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
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
        Self { child, lines }
    }

    /// Reads log events until the `listening` event and returns its port.
    fn read_port(&self, events: &mut Vec<Value>) -> u16 {
        let started = Instant::now();
        loop {
            let remaining = WAIT.saturating_sub(started.elapsed());
            let line = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|error| panic!("no listening event: {error}"));
            let event: Value = serde_json::from_str(&line).unwrap();
            events.push(event.clone());
            if event["message"] == "listening" {
                return u16::try_from(event["port"].as_u64().unwrap()).unwrap();
            }
        }
    }

    fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([format!("-{signal}"), self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Waits for the exit and returns the status and the remaining events.
    /// Standard error must stay empty: all output is JSON on standard
    /// output.
    fn wait(&mut self, events: &mut Vec<Value>) -> ExitStatus {
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
                Ok(line) => events.push(
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
}

impl Drop for Process {
    fn drop(&mut self) {
        // The process has already stopped in a passing test. Errors are
        // expected then, and do not matter.
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn messages(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["message"].as_str().unwrap())
        .collect()
}

/// Runs a command that stops by itself and returns its status, its
/// standard output, and its log events.
fn run_to_end(args: &[&str], vars: &[(&str, &str)]) -> (ExitStatus, String, Vec<Value>) {
    let mut process = Process::spawn(args, vars);
    let mut events = Vec::new();
    let status = process.wait(&mut events);
    let stdout = events
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    (status, stdout, events)
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

fn run_and_stop_with(signal: &str) {
    let mut process = Process::spawn(&["run"], &[("PORT", "0")]);
    let mut events = Vec::new();
    let port = process.read_port(&mut events);

    let health = get_health(port);
    assert!(health.starts_with("HTTP/1.1 200 "), "{health}");

    process.signal(signal);
    let status = process.wait(&mut events);

    assert_eq!(status.code(), Some(0), "{events:?}");
    assert_eq!(
        messages(&events),
        ["listening", "shutdown_started", "shutdown_complete"]
    );
    assert_eq!(events[1]["signal"], format!("SIG{signal}"));
}

#[test]
fn run_serves_health_and_stops_on_sigterm() {
    run_and_stop_with("TERM");
}

#[test]
fn run_serves_health_and_stops_on_sigint() {
    run_and_stop_with("INT");
}

#[test]
fn run_stops_at_the_deadline_when_a_client_stalls() {
    let mut process = Process::spawn(&["run"], &[("PORT", "0")]);
    let mut events = Vec::new();
    let port = process.read_port(&mut events);

    // The client sends only a part of a request head and then waits.
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client.write_all(b"GET /health HTTP/1.1\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    process.signal("TERM");
    let status = process.wait(&mut events);
    let elapsed = started.elapsed();

    // `http::LIMITS.shutdown_deadline` is 3 s.
    assert_eq!(status.code(), Some(0), "{events:?}");
    assert!(elapsed >= Duration::from_secs(3), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(
        messages(&events),
        [
            "listening",
            "shutdown_started",
            "connections_closed_at_deadline",
            "shutdown_complete"
        ]
    );
    assert_eq!(events[2]["connections"], 1);
}

#[test]
fn run_without_port_stops_with_a_safe_message() {
    let (status, _, events) = run_to_end(&["run"], &[]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(messages(&events), ["invalid_configuration"]);
    assert_eq!(events[0]["error"], "PORT is not set");
    assert_eq!(events[0]["level"], "ERROR");
}

#[test]
fn run_with_an_invalid_port_does_not_log_the_value() {
    let value = "postgres://user:hunter2@db.invalid/x";
    let (status, stdout, events) = run_to_end(&["run"], &[("PORT", value)]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(messages(&events), ["invalid_configuration"]);
    assert!(!stdout.contains("hunter2"), "{stdout}");
    assert!(!stdout.contains("db.invalid"), "{stdout}");
}

#[test]
fn migrate_reports_that_no_migrations_exist() {
    let (status, _, events) = run_to_end(&["migrate"], &[]);
    assert_eq!(status.code(), Some(0));
    assert_eq!(messages(&events), ["no_migrations"]);
}

#[test]
fn an_invalid_command_line_exits_with_code_2() {
    for args in [&[][..], &["serve"][..], &["run", "extra"][..]] {
        let (status, stdout, events) = run_to_end(args, &[]);
        assert_eq!(status.code(), Some(2), "{args:?}");
        assert_eq!(messages(&events), ["invalid_command"]);
        assert!(!stdout.contains("serve"), "{stdout}");
        assert!(!stdout.contains("extra"), "{stdout}");
    }
}
