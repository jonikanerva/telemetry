//! A running `ingest run` with the fake `fleet-telemetry` of
//! `examples/fake_fleet_telemetry.rs`, for the end-to-end tests.
//!
//! Two test binaries use this module: `tests/pipeline.rs` (no database,
//! part of CI) and `tests/storage/` (the test server).
//!
//! - The fake child is the example binary. `cargo nextest run` and `cargo
//!   test` build the examples of the package with the tests, into
//!   `target/<profile>/examples/`, next to the `deps/` directory of the test
//!   binary. [`fake_child`] finds it there.
//! - Each run gets its own free loopback ports and its own configuration
//!   file for the child, so tests can run in parallel.
//! - A failed wait prints only the names of the log events, never the log
//!   text: a test with a database URL must not print the URL, also when
//!   the process would leak it.
//! - Drop kills the process; the fake child ends when its parent ends.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_ingest");

/// The time limit of each wait. A failing test then stops instead of
/// hanging.
pub const WAIT: Duration = Duration::from_secs(30);

/// The invented VIN of the fake child.
pub const FAKE_VIN: &str = "0FAKEVXN000000001";

/// Values of the fake child that must never reach the log: the VIN, the
/// location, the battery level of record 0 to 99 (`50.0` to `50.99`), the
/// text values, the alert and error items, and the bytes of the junk
/// messages.
pub const VEHICLE_SENTINELS: &[&str] = &[FAKE_VIN, "60.169", "24.938", "SENTINEL", "JUNKPAYLOAD"];

/// The example binary of the fake child.
pub fn fake_child() -> PathBuf {
    // The test binary is in `target/<profile>/deps/` or, with the build
    // directory layout of Cargo 1.100, deeper below `target/<profile>/`.
    // The examples are in `target/<profile>/examples/`.
    let exe = std::env::current_exe().unwrap();
    exe.ancestors()
        .map(|dir| dir.join("examples").join("fake_fleet_telemetry"))
        .find(|path| path.is_file())
        .expect(
            "the example fake_fleet_telemetry is missing; cargo nextest builds it with the tests",
        )
}

/// A loopback port that is free now.
pub fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The ports of the child.
#[derive(Debug, Clone, Copy)]
pub struct ChildPorts {
    pub upstream: u16,
    pub zmq: u16,
    pub metrics: u16,
    pub control: u16,
}

/// A running `ingest run`.
pub struct Ingest {
    child: Child,
    lines: Receiver<String>,
    stderr: Receiver<String>,
    /// Every line of standard output so far, as written.
    pub raw: Vec<String>,
    /// The parsed log events so far.
    pub events: Vec<Value>,
    pub ports: ChildPorts,
    pub health_port: u16,
    pub gate_port: u16,
    config: PathBuf,
}

static RUNS: AtomicU64 = AtomicU64::new(0);

impl Ingest {
    /// Starts `ingest run` with the fake child, the database URL, and the
    /// extra variables, and waits until both ports listen.
    pub fn start(database_url: &str, extra: &[(&str, String)]) -> Self {
        let ports = ChildPorts {
            upstream: free_port(),
            zmq: free_port(),
            metrics: free_port(),
            control: free_port(),
        };
        let config = std::env::temp_dir().join(format!(
            "ingest-fake-{}-{}.json",
            std::process::id(),
            RUNS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(
            &config,
            serde_json::json!({
                "upstream_port": ports.upstream,
                "zmq_port": ports.zmq,
                "metrics_port": ports.metrics,
                "control_port": ports.control,
            })
            .to_string(),
        )
        .unwrap();
        let mut vars: Vec<(&str, String)> = vec![
            ("PORT", "0".to_owned()),
            ("INGEST_GATE_PORT", "0".to_owned()),
            ("INGEST_UPSTREAM_PORT", ports.upstream.to_string()),
            ("INGEST_ZMQ_PORT", ports.zmq.to_string()),
            ("INGEST_METRICS_PORT", ports.metrics.to_string()),
            (
                "INGEST_CHILD_PROGRAM",
                fake_child().to_string_lossy().into_owned(),
            ),
            ("INGEST_CHILD_CONFIG", config.to_string_lossy().into_owned()),
            ("DATABASE_URL", database_url.to_owned()),
        ];
        vars.extend(extra.iter().cloned());
        let mut child = Command::new(BINARY)
            .arg("run")
            .env_clear()
            .envs(vars.iter().map(|(name, value)| (name, value.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = forward(child.stdout.take().unwrap());
        let stderr = forward(child.stderr.take().unwrap());
        let mut ingest = Self {
            child,
            lines,
            stderr,
            raw: Vec::new(),
            events: Vec::new(),
            ports,
            health_port: 0,
            gate_port: 0,
            config,
        };
        ingest.health_port = ingest.port_of("listening");
        ingest.gate_port = ingest.port_of("gate_listening");
        ingest
    }

    fn port_of(&mut self, message: &str) -> u16 {
        let event = self.read_until(|event| event["message"] == message, message);
        u16::try_from(event["port"].as_u64().unwrap()).unwrap()
    }

    /// The names of the log events so far. A failure message shows only
    /// these names.
    pub fn messages(&self) -> Vec<&str> {
        self.events
            .iter()
            .map(|event| event["message"].as_str().unwrap_or("?"))
            .collect()
    }

    fn take(&mut self, line: String) -> Value {
        let event: Value = serde_json::from_str(&line)
            .unwrap_or_else(|_| panic!("a log line is not JSON (line {})", self.raw.len()));
        self.raw.push(line);
        self.events.push(event.clone());
        event
    }

    /// Reads log events until one matches `wanted`, and returns it. An
    /// event that was read before counts too.
    pub fn read_until(&mut self, wanted: impl Fn(&Value) -> bool, what: &str) -> Value {
        self.read_after(0, wanted, what)
    }

    /// Reads log events until one at index `start` or later matches
    /// `wanted`, and returns it.
    pub fn read_after(
        &mut self,
        start: usize,
        wanted: impl Fn(&Value) -> bool,
        what: &str,
    ) -> Value {
        if let Some(event) = self.events.iter().skip(start).find(|event| wanted(event)) {
            return event.clone();
        }
        let started = Instant::now();
        loop {
            let remaining = WAIT.saturating_sub(started.elapsed());
            let Ok(line) = self.lines.recv_timeout(remaining) else {
                panic!("no event {what}; events so far: {:?}", self.messages());
            };
            let event = self.take(line);
            if wanted(&event) {
                return event;
            }
        }
    }

    /// Reads the log events that have arrived, without a wait.
    pub fn poll(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.take(line);
        }
    }

    /// Reads log events until the `ingest_event` of this kind, and returns
    /// it. Events read before stay available.
    pub fn read_event(&mut self, kind: &str) -> Value {
        self.read_until(
            |event| event["message"] == "ingest_event" && event["kind"] == kind,
            kind,
        )
    }

    /// The kinds of the `ingest_event` log events so far.
    pub fn kinds(&self) -> Vec<&str> {
        self.with_message("ingest_event")
            .iter()
            .map(|event| event["kind"].as_str().unwrap_or("?"))
            .collect()
    }

    /// The log events with this message.
    pub fn with_message(&self, message: &str) -> Vec<&Value> {
        self.events
            .iter()
            .filter(|event| event["message"] == message)
            .collect()
    }

    /// The status code of `GET /health`.
    pub fn health(&self) -> u16 {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.health_port)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
            .get(9..12)
            .and_then(|code| code.parse().ok())
            .unwrap()
    }

    /// Waits until `/health` answers `status`.
    pub fn wait_for_health(&self, status: u16) {
        let started = Instant::now();
        while self.health() != status {
            assert!(started.elapsed() < WAIT, "/health did not answer {status}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A connection through the gate when the gate is open: the fake child
    /// answers `ready` at once. `None` when the gate closes the connection.
    pub fn vehicle(&self) -> Option<Vehicle> {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.gate_port)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(_) if line == "ready\n" => Some(Vehicle { stream, reader }),
            _ => None,
        }
    }

    /// Waits until the gate forwards a connection, and returns it.
    pub fn wait_for_vehicle(&self) -> Vehicle {
        let started = Instant::now();
        loop {
            if let Some(vehicle) = self.vehicle() {
                return vehicle;
            }
            assert!(started.elapsed() < WAIT, "the gate did not open");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Sends a command to the control port of the fake child and returns
    /// the answer.
    pub fn control(&self, command: &str) -> String {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.ports.control)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        command_on(
            &stream,
            &mut BufReader::new(stream.try_clone().unwrap()),
            command,
        )
    }

    /// Sends a signal to `ingest`.
    pub fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([format!("-{signal}"), self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Waits for the exit and reads the rest of the log. Standard error
    /// must stay empty: all output is JSON on standard output.
    pub fn wait(&mut self) -> ExitStatus {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < WAIT * 2, "the process did not stop");
            std::thread::sleep(Duration::from_millis(20));
        };
        loop {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) => {
                    self.take(line);
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("standard output did not close"),
            }
        }
        let stderr: Vec<String> = self.stderr.try_iter().collect();
        assert!(
            stderr.is_empty(),
            "standard error has {} lines",
            stderr.len()
        );
        status
    }

    /// Checks the complete log output: no line holds one of `secrets`
    /// ([`line_holds`]), and no first-party event has a field that the
    /// allowlist redacted. The failure names the secret by its index only.
    pub fn assert_log_holds_none_of(&self, secrets: &[&str]) {
        assert_ne!(self.raw.len(), 0);
        for (index, secret) in secrets.iter().enumerate() {
            let found = self.raw.iter().position(|line| line_holds(line, secret));
            assert_eq!(found, None, "the log holds secret number {index}");
        }
        for event in &self.events {
            let target = event["target"].as_str().unwrap_or("");
            if target == "ingest" || target.starts_with("ingest::") {
                let redacted: Vec<&String> = event
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(_, value)| *value == "[redacted]")
                    .map(|(key, _)| key)
                    .collect();
                assert!(
                    redacted.is_empty(),
                    "event {} has redacted fields {redacted:?}",
                    event["message"]
                );
            }
        }
    }
}

/// Whether the raw log line `line` holds `secret` in a field name or a
/// value, at any depth, except in the timestamp that the formatter writes.
///
/// The search runs over the raw text, so it also sees escaped text and
/// numbers as written. Only the value of the top-level `"timestamp"` pair is
/// blanked first, and only when it is a valid instant: its fraction of a
/// second can hold a numeric sentinel by chance. For example a line written
/// at second 24 and millisecond 938 holds the longitude sentinel `24.938`
/// (PR #25, flake protocol). A timestamp value that is not an instant stays
/// in the search.
pub fn line_holds(line: &str, secret: &str) -> bool {
    let timestamp = serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|event| event["timestamp"].as_str().map(str::to_owned))
        .filter(|value| value.parse::<jiff::Timestamp>().is_ok());
    match timestamp {
        Some(value) => {
            let pair = format!("\"timestamp\":\"{value}\"");
            line.replacen(&pair, "\"timestamp\":\"\"", 1)
                .contains(secret)
        }
        None => line.contains(secret),
    }
}

#[test]
fn a_sentinel_only_in_the_timestamp_is_not_a_leak() {
    let sentinel = "24.938";
    let line = r#"{"timestamp":"2026-11-27T03:30:24.938123456Z","level":"INFO","target":"ingest","message":"started"}"#;
    // The search over the whole raw line, as before this change, finds it.
    assert!(line.contains(sentinel));
    assert!(!line_holds(line, sentinel));
}

#[test]
fn a_sentinel_in_any_other_place_is_a_leak() {
    let sentinel = "24.938";
    let timestamp = r#""timestamp":"2026-11-27T03:30:24.938123456Z""#;
    for rest in [
        // A string value, a number, a nested value, an array, a field
        // name, and an escaped string.
        r#""message":"child_output","child_line":"lon 24.938""#,
        r#""message":"x","count":24.938445"#,
        r#""message":"x","span":{"inner":{"value":"24.938"}}"#,
        r#""message":"x","items":[1,"a 24.938 b"]"#,
        r#""message":"x","f24.938":1"#,
        r#""message":"x","child_line":"\"24.938\"""#,
    ] {
        let line = format!("{{{timestamp},{rest}}}");
        assert!(line_holds(&line, sentinel), "{line}");
    }
    // A timestamp value that is not a valid instant stays in the search.
    let line = r#"{"timestamp":"lon 24.938","message":"x"}"#;
    assert!(line_holds(line, sentinel));
    // A second timestamp pair, not written by the formatter, stays too.
    let line = r#"{"timestamp":"2026-11-27T03:30:24.938123456Z","span":{"timestamp":"2026-11-27T03:30:24.938123456Z"}}"#;
    assert!(line_holds(line, sentinel));
    // A line that is not JSON is searched as a whole.
    assert!(line_holds("not json 24.938", sentinel));
}

impl Drop for Ingest {
    fn drop(&mut self) {
        // The process has already stopped in a passing test. Errors are
        // expected then, and do not matter.
        self.child.kill().ok();
        self.child.wait().ok();
        std::fs::remove_file(&self.config).ok();
    }
}

fn forward(output: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    lines
}

fn command_on(mut stream: &TcpStream, reader: &mut BufReader<TcpStream>, command: &str) -> String {
    stream.write_all(format!("{command}\n").as_bytes()).unwrap();
    // A child that exits on the command closes the connection: the answer
    // is empty then.
    let mut answer = String::new();
    reader.read_line(&mut answer).ok();
    answer.trim_end().to_owned()
}

/// A connection through the gate to the fake child.
pub struct Vehicle {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Vehicle {
    /// Sends a command on this connection and returns the answer, or an
    /// empty text when the gate closed the connection.
    pub fn command(&mut self, command: &str) -> String {
        if (&self.stream)
            .write_all(format!("{command}\n").as_bytes())
            .is_err()
        {
            return String::new();
        }
        let mut answer = String::new();
        self.reader.read_line(&mut answer).ok();
        answer.trim_end().to_owned()
    }

    /// True when the gate has closed this connection.
    pub fn is_closed(&mut self) -> bool {
        let mut byte = [0_u8; 1];
        matches!(self.reader.read(&mut byte), Ok(0) | Err(_))
    }
}

impl Drop for Vehicle {
    fn drop(&mut self) {
        self.stream.shutdown(Shutdown::Both).ok();
    }
}
