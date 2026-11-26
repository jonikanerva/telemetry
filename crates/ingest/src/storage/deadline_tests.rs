//! Every call closes its own connection, on every path.
//!
//! A fake server completes the PostgreSQL startup and then either reads
//! without an answer (a server that went silent in a session) or answers
//! every query with an error (an answered failure). These tests need no
//! database, so CI runs them on Linux too. They use a short deadline; the
//! code path is the one of [`RUN_CALL_DEADLINE`](super::RUN_CALL_DEADLINE).
//! The tests of the storage binary check the close after a success on the
//! real database (`pg_stat_activity`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use super::error::{ErrorCode, FailureClass, StorageError};
use super::events::write_events_within;
use super::migrate::check_migrations_within;
use super::rows::QueuedRecord;
use super::writer::Writer;
use crate::events::{EventKind, IngestEvent};
use crate::record::{Decoded, Signal, SignalValue, VehicleData, Vin};

/// The deadline of each attempt in these tests.
const DEADLINE: Duration = Duration::from_secs(2);

/// The time that the fake server may need to see a closed socket.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// What the fake server does after the startup.
#[derive(Clone, Copy)]
enum Mode {
    /// Reads and never answers.
    Silent,
    /// Answers each query (`Q`) and each `Sync` (`S`) with SQLSTATE 42P01
    /// and `ReadyForQuery`.
    Refuse,
}

/// Counts of the fake server.
#[derive(Default)]
struct Counts {
    accepted: AtomicUsize,
    /// Connections that sent `Terminate` before they closed.
    terminated: AtomicUsize,
    closed: AtomicUsize,
}

/// A server that answers the startup and then follows its [`Mode`].
struct FakeServer {
    port: u16,
    counts: Arc<Counts>,
}

impl FakeServer {
    fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let counts = Arc::new(Counts::default());
        let shared = Arc::clone(&counts);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                shared.accepted.fetch_add(1, Ordering::SeqCst);
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    if serve(stream, mode) {
                        shared.terminated.fetch_add(1, Ordering::SeqCst);
                    }
                    shared.closed.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        Self { port, counts }
    }

    /// Settings without TLS: the fake server has no certificate. The path
    /// after the startup is the same as with TLS.
    fn options(&self) -> PgConnectOptions {
        PgConnectOptions::new_without_pgpass()
            .host("127.0.0.1")
            .port(self.port)
            .username("test")
            .database("test")
            .ssl_mode(PgSslMode::Disable)
    }

    fn accepted(&self) -> usize {
        self.counts.accepted.load(Ordering::SeqCst)
    }

    fn terminated(&self) -> usize {
        self.counts.terminated.load(Ordering::SeqCst)
    }

    /// Waits until the server has seen `count` closed sockets, at most
    /// [`CLOSE_WAIT`]. Returns the count that it saw.
    fn wait_closed(&self, count: usize) -> usize {
        let start = Instant::now();
        loop {
            let closed = self.counts.closed.load(Ordering::SeqCst);
            if closed >= count || start.elapsed() > CLOSE_WAIT {
                return closed;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Reads one message: the type byte, then a length that counts itself.
fn read_message(stream: &mut TcpStream) -> Option<u8> {
    let mut kind = [0_u8; 1];
    stream.read_exact(&mut kind).ok()?;
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).ok()?;
    let length = usize::try_from(u32::from_be_bytes(length)).ok()?;
    let mut body = vec![0_u8; length.saturating_sub(4)];
    stream.read_exact(&mut body).ok()?;
    Some(kind[0])
}

/// An `ErrorResponse` with SQLSTATE 42P01 and a `ReadyForQuery`.
fn refusal() -> Vec<u8> {
    let mut fields = Vec::new();
    for (code, text) in [
        (b'S', "ERROR"),
        (b'V', "ERROR"),
        (b'C', "42P01"),
        (b'M', "test"),
    ] {
        fields.push(code);
        fields.extend_from_slice(text.as_bytes());
        fields.push(0);
    }
    fields.push(0);
    let length = u32::try_from(fields.len() + 4).unwrap();
    let mut message = vec![b'E'];
    message.extend_from_slice(&length.to_be_bytes());
    message.extend_from_slice(&fields);
    message.extend_from_slice(&[b'Z', 0, 0, 0, 5, b'I']);
    message
}

/// Answers the startup and then follows `mode` until the client closes
/// the socket. Returns true when the client sent `Terminate`.
fn serve(mut stream: TcpStream, mode: Mode) -> bool {
    // The startup message has no type byte: a length, then the body.
    let mut length = [0_u8; 4];
    if stream.read_exact(&mut length).is_err() {
        return false;
    }
    let length = usize::try_from(u32::from_be_bytes(length)).unwrap();
    let mut body = vec![0_u8; length.saturating_sub(4)];
    if stream.read_exact(&mut body).is_err() {
        return false;
    }
    // 'R' AuthenticationOk, then 'Z' ReadyForQuery with status idle.
    let answer = [b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'];
    if stream.write_all(&answer).is_err() {
        return false;
    }
    let mut terminated = false;
    while let Some(kind) = read_message(&mut stream) {
        match (mode, kind) {
            (_, b'X') => terminated = true,
            (Mode::Refuse, b'Q' | b'S') if stream.write_all(&refusal()).is_err() => break,
            _ => {}
        }
    }
    terminated
}

const TIMEOUT: (FailureClass, ErrorCode) = (FailureClass::NoAnswer, ErrorCode::Timeout);

fn class_and_code(error: StorageError) -> (FailureClass, ErrorCode) {
    (error.class, error.code)
}

/// Checks call number `call` (from 1) on a silent server: it ended at its
/// deadline, it opened its own connection, and the server saw that socket
/// close, so no connection outlives the call.
fn assert_dropped(server: &FakeServer, call: usize, elapsed: Duration) {
    assert!(elapsed >= DEADLINE, "call {call}: {elapsed:?}");
    assert!(
        elapsed < DEADLINE + Duration::from_secs(2),
        "call {call}: {elapsed:?}"
    );
    assert_eq!(server.accepted(), call, "call {call}: no own connection");
    assert_eq!(
        server.wait_closed(call),
        call,
        "call {call}: the server did not see the socket close"
    );
    // A dropped connection sends no `Terminate`.
    assert_eq!(server.terminated(), 0, "call {call}");
}

/// Checks call number `call` (from 1) on a refusing server: it opened its
/// own connection, sent `Terminate`, and the server saw the socket close.
fn assert_closed_gracefully(server: &FakeServer, call: usize) {
    assert_eq!(server.accepted(), call, "call {call}: no own connection");
    assert_eq!(
        server.wait_closed(call),
        call,
        "call {call}: the server did not see the socket close"
    );
    assert_eq!(server.terminated(), call, "call {call}: no Terminate");
}

fn signal_record() -> Decoded {
    Decoded::VehicleData(VehicleData {
        vin: Vin::new("TESTVIN0000000001".to_owned()),
        created_at: Timestamp::UNIX_EPOCH,
        received_at: Timestamp::UNIX_EPOCH,
        is_resend: false,
        signals: vec![Signal {
            field: 1,
            value: SignalValue::Int32(1),
        }],
        unknown_value_variants: 0,
        unknown_fields: false,
    })
}

fn started_event() -> [IngestEvent; 1] {
    [IngestEvent {
        occurred_at: Timestamp::UNIX_EPOCH,
        kind: EventKind::Started,
    }]
}

#[tokio::test]
async fn a_write_past_its_deadline_drops_its_connection() {
    let server = FakeServer::start(Mode::Silent);
    let mut writer = Writer::new(server.options()).with_call_deadline(DEADLINE);
    let decoded = signal_record();
    let records = [QueuedRecord {
        topic: b"V",
        payload: b"p",
        decoded: &decoded,
    }];
    for call in 1..=3 {
        let mut beats = 0;
        let started = Instant::now();
        let error = writer
            .write(&records, &mut || beats += 1)
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(class_and_code(error.error), TIMEOUT, "call {call}");
        assert_eq!(error.committed, 0);
        assert_eq!((error.outcome.attempts, beats), (1, 1));
        assert_dropped(&server, call, elapsed);
    }
}

#[tokio::test]
async fn an_event_insert_past_its_deadline_drops_its_connection() {
    let server = FakeServer::start(Mode::Silent);
    let events = started_event();
    for call in 1..=3 {
        let mut beats = 0;
        let started = Instant::now();
        let error = write_events_within(
            &server.options(),
            uuid::Uuid::nil(),
            &events,
            &mut || beats += 1,
            DEADLINE,
        )
        .await
        .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(class_and_code(error), TIMEOUT, "call {call}");
        assert_eq!(beats, 1);
        assert_dropped(&server, call, elapsed);
    }
}

#[tokio::test]
async fn a_migration_check_past_its_deadline_drops_its_connection() {
    let server = FakeServer::start(Mode::Silent);
    for call in 1..=3 {
        let started = Instant::now();
        let error = check_migrations_within(&server.options(), DEADLINE)
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(class_and_code(error), TIMEOUT, "call {call}");
        assert_dropped(&server, call, elapsed);
    }
}

#[tokio::test]
async fn every_call_closes_its_connection_after_an_answered_error() {
    let server = FakeServer::start(Mode::Refuse);
    let answered = |error: StorageError| {
        assert_eq!(error.class, FailureClass::Answered, "{error}");
    };

    let mut writer = Writer::new(server.options()).with_call_deadline(DEADLINE);
    let decoded = signal_record();
    let records = [QueuedRecord {
        topic: b"V",
        payload: b"p",
        decoded: &decoded,
    }];
    let error = writer.write(&records, &mut || {}).await.unwrap_err();
    answered(error.error);
    assert_closed_gracefully(&server, 1);

    let error = write_events_within(
        &server.options(),
        uuid::Uuid::nil(),
        &started_event(),
        &mut || {},
        DEADLINE,
    )
    .await
    .unwrap_err();
    answered(error);
    assert_closed_gracefully(&server, 2);

    // 42P01 on `_sqlx_migrations`: no migration is applied.
    let error = check_migrations_within(&server.options(), DEADLINE)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::MigrationMissing);
    assert_closed_gracefully(&server, 3);
}

#[tokio::test]
async fn an_empty_call_opens_no_connection() {
    let server = FakeServer::start(Mode::Silent);
    let mut writer = Writer::new(server.options());
    let mut beats = 0;
    let outcome = writer.write(&[], &mut || beats += 1).await.unwrap();
    assert_eq!((outcome.attempts, beats), (0, 0));
    let outcome = write_events_within(
        &server.options(),
        uuid::Uuid::nil(),
        &[],
        &mut || beats += 1,
        DEADLINE,
    )
    .await
    .unwrap();
    assert_eq!((outcome.inserted, beats), (0, 0));
    assert_eq!(server.accepted(), 0);
}
