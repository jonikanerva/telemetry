//! A call that passes its deadline closes its pool connection.
//!
//! A fake server completes the PostgreSQL startup and then reads without
//! an answer, as a server that went silent in a session. These tests need
//! no database, so CI runs them on Linux too. They use a short deadline;
//! the code path is the one of [`RUN_CALL_DEADLINE`](super::RUN_CALL_DEADLINE).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use super::connect::run_pool;
use super::error::{ErrorCode, FailureClass, StorageError};
use super::events::write_events_within;
use super::migrate::check_migrations_within;
use super::rows::QueuedRecord;
use super::writer::Writer;
use crate::events::{EventKind, IngestEvent};
use crate::record::{Decoded, Signal, SignalValue, VehicleData, Vin};

/// The deadline of each call in these tests.
const DEADLINE: Duration = Duration::from_secs(2);

/// The time that the fake server may need to see a closed socket.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Counts of the fake server.
#[derive(Default)]
struct Counts {
    accepted: AtomicUsize,
    closed: AtomicUsize,
}

/// A server that answers the startup and then nothing.
struct SilentServer {
    port: u16,
    counts: Arc<Counts>,
}

impl SilentServer {
    fn start() -> Self {
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
                    serve(stream);
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

/// Reads the startup message, answers `AuthenticationOk` and
/// `ReadyForQuery`, and then reads until the client closes the socket.
fn serve(mut stream: TcpStream) {
    let mut length = [0_u8; 4];
    if stream.read_exact(&mut length).is_err() {
        return;
    }
    let length = usize::try_from(u32::from_be_bytes(length)).unwrap();
    let mut body = vec![0_u8; length.saturating_sub(4)];
    if stream.read_exact(&mut body).is_err() {
        return;
    }
    // 'R' AuthenticationOk, then 'Z' ReadyForQuery with status idle.
    let answer = [b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'];
    if stream.write_all(&answer).is_err() {
        return;
    }
    let mut buffer = [0_u8; 4096];
    while matches!(stream.read(&mut buffer), Ok(read) if read > 0) {}
}

const TIMEOUT: (FailureClass, ErrorCode) = (FailureClass::NoAnswer, ErrorCode::Timeout);

fn class_and_code(error: StorageError) -> (FailureClass, ErrorCode) {
    (error.class, error.code)
}

/// Checks one call of number `call` (from 1): it ended at its deadline, its
/// pool slot is free, the server saw its socket close, and it used a new
/// connection.
fn assert_closed(server: &SilentServer, pool: &PgPool, call: usize, elapsed: Duration) {
    assert!(elapsed >= DEADLINE, "call {call}: {elapsed:?}");
    assert!(
        elapsed < DEADLINE + Duration::from_secs(2),
        "call {call}: {elapsed:?}"
    );
    assert_eq!(
        pool.size(),
        0,
        "call {call}: the pool still counts the connection"
    );
    assert_eq!(server.accepted(), call, "call {call}: no new connection");
    assert_eq!(
        server.wait_closed(call),
        call,
        "call {call}: the server did not see the socket close"
    );
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

#[tokio::test]
async fn a_write_past_its_deadline_closes_its_connection() {
    let server = SilentServer::start();
    let pool = run_pool(server.options());
    let mut writer = Writer::new(pool.clone()).with_call_deadline(DEADLINE);
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
        assert_closed(&server, &pool, call, elapsed);
    }
    pool.close().await;
}

#[tokio::test]
async fn an_event_insert_past_its_deadline_closes_its_connection() {
    let server = SilentServer::start();
    let pool = run_pool(server.options());
    let events = [IngestEvent {
        occurred_at: Timestamp::UNIX_EPOCH,
        kind: EventKind::Started,
    }];
    let run_id = uuid::Uuid::nil();
    for call in 1..=3 {
        let mut beats = 0;
        let started = Instant::now();
        let error = write_events_within(&pool, run_id, &events, &mut || beats += 1, DEADLINE)
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(class_and_code(error), TIMEOUT, "call {call}");
        assert_eq!(beats, 1);
        assert_closed(&server, &pool, call, elapsed);
    }
    pool.close().await;
}

#[tokio::test]
async fn a_migration_check_past_its_deadline_closes_its_connection() {
    let server = SilentServer::start();
    let pool = run_pool(server.options());
    for call in 1..=3 {
        let started = Instant::now();
        let error = check_migrations_within(&pool, DEADLINE).await.unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(class_and_code(error), TIMEOUT, "call {call}");
        assert_closed(&server, &pool, call, elapsed);
    }
    pool.close().await;
}
