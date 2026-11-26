//! Tests of the gate.
//!
//! The tests with real sockets use the real clock and loopback addresses
//! only. The tests of rule 1 use in-memory streams and a paused clock, so
//! that a timer cannot fire early while a real socket waits for the kernel.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::mpsc;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};
use tokio::task::JoinHandle;

use super::*;
use crate::events::recording::RecordingSink;
use crate::logging::{ALLOWED_FIELDS, capture};
use crate::readiness::{Pipeline, Readiness};
use crate::watchdog::{GATE_ACCEPTOR, Task, Watchdog};

/// Time limit for each wait in a test. A failing test then stops instead
/// of hanging.
const WAIT: Duration = Duration::from_secs(10);

/// A gate on a loopback port with a test listener as `fleet-telemetry`.
struct TestGate {
    address: SocketAddr,
    upstream: TcpListener,
    readiness: Arc<Readiness>,
    sink: RecordingSink,
    task: JoinHandle<()>,
}

impl TestGate {
    async fn start(limits: GateLimits, heartbeat: Heartbeat) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let sink = RecordingSink::default();
        let readiness = Arc::new(Readiness::new(Arc::new(sink.clone())));
        let task = tokio::spawn(run(
            listener,
            upstream.local_addr().unwrap(),
            readiness.subscribe(),
            Arc::new(sink.clone()),
            heartbeat,
            limits,
        ));
        Self {
            address,
            upstream,
            readiness,
            sink,
            task,
        }
    }

    async fn start_open() -> Self {
        let gate = Self::start(LIMITS, Watchdog::new().register(GATE_ACCEPTOR)).await;
        gate.open();
        gate
    }

    fn open(&self) {
        self.readiness.set_child_running(true);
        self.readiness.set_pipeline(Pipeline::Ready);
    }

    async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.address).await.unwrap()
    }

    async fn accept_upstream(&self) -> TcpStream {
        tokio::time::timeout(WAIT, self.upstream.accept())
            .await
            .expect("no connection reached the upstream")
            .unwrap()
            .0
    }

    async fn stop(self) -> RecordingSink {
        self.readiness.shut();
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("the gate did not stop")
            .unwrap();
        self.sink
    }
}

/// Reads until the peer closes the connection. A reset counts as a close.
async fn read_until_closed(stream: &mut TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut received))
        .await
        .expect("the connection did not close")
        .ok();
    received
}

/// Bytes from a fixed pseudo-random sequence, so that a lost, repeated, or
/// reordered block changes the result.
fn pattern(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[tokio::test]
async fn forwards_large_streams_in_both_directions_unchanged() {
    let gate = TestGate::start_open().await;
    let client = gate.connect().await;
    let upstream = gate.accept_upstream().await;
    let to_upstream = pattern(1, 4 * 1024 * 1024 + 17);
    let to_client = pattern(2, 4 * 1024 * 1024 + 123);

    let (mut client_read, mut client_write) = client.into_split();
    let (mut upstream_read, mut upstream_write) = upstream.into_split();
    let sent_up = to_upstream.clone();
    let sent_down = to_client.clone();
    let tasks = async move {
        tokio::try_join!(
            tokio::spawn(async move {
                client_write.write_all(&sent_up).await.unwrap();
                client_write.shutdown().await.unwrap();
            }),
            tokio::spawn(async move {
                upstream_write.write_all(&sent_down).await.unwrap();
                upstream_write.shutdown().await.unwrap();
            }),
            tokio::spawn(async move {
                let mut received = Vec::new();
                upstream_read.read_to_end(&mut received).await.unwrap();
                received
            }),
            tokio::spawn(async move {
                let mut received = Vec::new();
                client_read.read_to_end(&mut received).await.unwrap();
                received
            }),
        )
    };
    let ((), (), at_upstream, at_client) =
        tokio::time::timeout(WAIT, tasks).await.unwrap().unwrap();
    assert!(at_upstream == to_upstream, "the upstream got other bytes");
    assert!(at_client == to_client, "the client got other bytes");
    gate.stop().await;
}

#[tokio::test]
async fn forwards_a_half_close_from_the_client() {
    let gate = TestGate::start_open().await;
    let mut client = gate.connect().await;
    let mut upstream = gate.accept_upstream().await;

    client.write_all(b"request").await.unwrap();
    client.shutdown().await.unwrap();
    // The end of the stream reaches the upstream while the other direction
    // stays open.
    assert_eq!(read_until_closed(&mut upstream).await, b"request");
    let response = pattern(3, 1024 * 1024);
    upstream.write_all(&response).await.unwrap();
    drop(upstream);
    assert!(read_until_closed(&mut client).await == response);
    gate.stop().await;
}

#[tokio::test]
async fn forwards_a_half_close_from_the_upstream() {
    let gate = TestGate::start_open().await;
    let mut client = gate.connect().await;
    let mut upstream = gate.accept_upstream().await;

    upstream.write_all(b"greeting").await.unwrap();
    upstream.shutdown().await.unwrap();
    assert_eq!(read_until_closed(&mut client).await, b"greeting");
    let request = pattern(4, 1024 * 1024);
    client.write_all(&request).await.unwrap();
    drop(client);
    assert!(read_until_closed(&mut upstream).await == request);
    gate.stop().await;
}

#[tokio::test]
async fn closing_the_gate_closes_every_forwarded_connection() {
    let gate = TestGate::start_open().await;
    let mut pairs = Vec::new();
    for _ in 0..3 {
        let mut client = gate.connect().await;
        let mut upstream = gate.accept_upstream().await;
        upstream.write_all(b"x").await.unwrap();
        let mut byte = [0_u8; 1];
        client.read_exact(&mut byte).await.unwrap();
        pairs.push((client, upstream));
    }

    gate.readiness.set_child_running(false);
    for (client, upstream) in &mut pairs {
        assert_eq!(read_until_closed(client).await, b"");
        assert_eq!(read_until_closed(upstream).await, b"");
    }
    let sink = gate.stop().await;
    assert_eq!(sink.names(), ["gate_opened", "gate_closed"]);
}

#[tokio::test]
async fn a_closed_gate_closes_a_connection_without_a_byte_to_the_upstream() {
    let gate = TestGate::start(LIMITS, Watchdog::new().register(GATE_ACCEPTOR)).await;
    let mut early = gate.connect().await;
    // The peer may see a reset or a normal end. The test sends nothing, so
    // the close is normally a normal end.
    early.write_all(b"first").await.ok();
    assert_eq!(read_until_closed(&mut early).await, b"");

    gate.open();
    let mut late = gate.connect().await;
    late.write_all(b"second").await.unwrap();
    // The first connection that reaches the upstream is the late one.
    let mut upstream = gate.accept_upstream().await;
    let mut received = [0_u8; 6];
    upstream.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"second");
    gate.stop().await;
}

#[tokio::test]
async fn the_stop_closes_every_forwarded_connection_within_the_close_deadline() {
    let (capture, _guard) = capture::install(ALLOWED_FIELDS);
    let gate = TestGate::start_open().await;
    let mut pairs = Vec::new();
    for _ in 0..3 {
        let mut client = gate.connect().await;
        let mut upstream = gate.accept_upstream().await;
        upstream.write_all(b"x").await.unwrap();
        let mut byte = [0_u8; 1];
        client.read_exact(&mut byte).await.unwrap();
        pairs.push((client, upstream));
    }

    let started = Instant::now();
    let sink = gate.stop().await;
    assert!(
        started.elapsed() < LIMITS.close_deadline,
        "{:?}",
        started.elapsed()
    );
    for (client, upstream) in &mut pairs {
        assert_eq!(read_until_closed(client).await, b"");
        assert_eq!(read_until_closed(upstream).await, b"");
    }
    assert_eq!(sink.names(), ["gate_opened", "gate_closed"]);
    let events: Vec<serde_json::Value> = capture
        .text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let with_message = |message: &str| -> Vec<&serde_json::Value> {
        events
            .iter()
            .filter(|event| event["message"] == message)
            .collect()
    };
    let closed = with_message("gate_connection_closed");
    assert_eq!(closed.len(), 3, "{events:?}");
    for event in closed {
        assert_eq!(event["reason"], "gate_closed");
        assert_eq!(event["peer_class"], "loopback");
        assert!(event["first_byte_ms"].as_u64().is_some(), "{event}");
    }
    assert_eq!(with_message("gate_stopped").len(), 1);
    assert_eq!(with_message("gate_connections_aborted").len(), 0);
}

/// Set in the second process of the bound test.
const MORE_DESCRIPTORS: &str = "INGEST_TEST_MORE_DESCRIPTORS";

#[test]
fn the_connection_after_the_bound_is_refused_and_recorded() {
    // The production bound of 1024. The test holds about 4,100 open
    // descriptors. The default soft limit of a macOS shell is 256, so the
    // test runs its body in a second process with a soft limit of 8192.
    // The hard limit must allow 8192.
    const NAME: &str = "gate::tests::the_connection_after_the_bound_is_refused_and_recorded";
    if std::env::var_os(MORE_DESCRIPTORS).is_none() {
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", "ulimit -S -n 8192 && exec \"$0\" \"$@\""])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads", "1"])
            .env(MORE_DESCRIPTORS, "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        // A wrong name runs no test and also succeeds.
        assert!(stdout.contains("1 passed"), "{stdout}\n{stderr}");
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(refuse_the_connection_after_the_bound());
}

async fn refuse_the_connection_after_the_bound() {
    let gate = TestGate::start_open().await;
    let mut held = Vec::new();
    for _ in 0..LIMITS.max_connections {
        let client = gate.connect().await;
        // Each admitted connection reaches the upstream before the next
        // connect, so the gate count is exact.
        let mut upstream = gate.accept_upstream().await;
        upstream.write_all(b"x").await.unwrap();
        held.push((client, upstream));
    }

    let mut refused = 0_u32;
    for _ in 0..2 {
        let mut extra = gate.connect().await;
        assert_eq!(read_until_closed(&mut extra).await, b"");
        refused += 1;
    }
    let full: Vec<CountWindow> = gate
        .sink
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::GateFull(window) => Some(*window),
            _ => None,
        })
        .collect();
    // The first refusal is recorded at once. The second waits for the
    // interval.
    assert_eq!(full.len(), 1, "{full:?}");
    assert_eq!(full[0].count, 1);

    // Close from the upstream side first with a reset, so that no socket
    // with a temporary port stays in TIME_WAIT after the test.
    for (_client, upstream) in &held {
        SockRef::from(upstream)
            .set_linger(Some(Duration::ZERO))
            .unwrap();
    }
    let clients: Vec<TcpStream> = held.into_iter().map(|(client, _)| client).collect();
    let sink = gate.stop().await;
    drop(clients);

    let counts: Vec<u32> = sink
        .events()
        .iter()
        .filter_map(|event| event.kind.window().map(|window| window.count))
        .collect();
    // The stop records the count that the interval held back.
    assert_eq!(counts, [1, 1]);
    assert_eq!(counts.iter().sum::<u32>(), refused);
    assert_eq!(
        sink.names(),
        ["gate_opened", "gate_full", "gate_closed", "gate_full"]
    );
}

#[tokio::test]
async fn accepted_sockets_get_keep_alive_and_no_delay() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let _client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (stream, _peer) = accept(&listener).await.unwrap();

    let socket = SockRef::from(&stream);
    assert!(socket.keepalive().unwrap());
    assert_eq!(
        socket.tcp_keepalive_time().unwrap(),
        Duration::from_secs(15)
    );
    assert_eq!(
        socket.tcp_keepalive_interval().unwrap(),
        Duration::from_secs(15)
    );
    assert_eq!(socket.tcp_keepalive_retries().unwrap(), 9);
    assert!(stream.nodelay().unwrap());
}

#[tokio::test]
async fn the_acceptor_beats_while_idle_and_retires_at_the_stop() {
    // The limit is 50 beat intervals, so that a busy test machine cannot
    // delay a beat past it.
    const ACCEPTOR: Task = Task {
        name: "gate_acceptor",
        limit: Duration::from_secs(1),
    };
    let watchdog = Watchdog::new();
    let heartbeat = watchdog.register(ACCEPTOR);
    let limits = GateLimits {
        beat_interval: Duration::from_millis(20),
        ..LIMITS
    };
    let gate = TestGate::start(limits, heartbeat).await;
    let (sender, late) = mpsc::channel();
    watchdog
        .start(Duration::from_millis(10), move |name| {
            sender.send(name).ok();
        })
        .unwrap();

    // No client connects. The acceptor must beat by itself.
    tokio::time::sleep(ACCEPTOR.limit * 2).await;
    assert_eq!(late.try_recv(), Err(mpsc::TryRecvError::Empty));

    // After the stop, the retired heartbeat is not late.
    gate.stop().await;
    tokio::time::sleep(ACCEPTOR.limit * 3 / 2).await;
    assert_eq!(late.try_recv(), Err(mpsc::TryRecvError::Empty));
}

/// An open gate state for the tests of [`forward`]. Keep the [`Readiness`]
/// while the test runs: a dropped sender closes the connection.
fn open_gate() -> (Readiness, watch::Receiver<GateState>) {
    let readiness = Readiness::new(Arc::new(RecordingSink::default()));
    readiness.set_child_running(true);
    readiness.set_pipeline(Pipeline::Ready);
    let receiver = readiness.subscribe();
    (readiness, receiver)
}

/// Starts [`forward`] on in-memory streams. Returns the client end, the
/// upstream end, and the task.
fn start_forward(
    gate: watch::Receiver<GateState>,
) -> (DuplexStream, DuplexStream, JoinHandle<Report>) {
    let (client, gate_client) = duplex(64 * 1024);
    let (gate_upstream, upstream) = duplex(64 * 1024);
    let epoch = gate.borrow().epoch();
    let accepted_at = Instant::now();
    let task = tokio::spawn(async move {
        forward(
            gate_client,
            async { Ok(gate_upstream) },
            accepted_at,
            &LIMITS,
            gate,
            epoch,
        )
        .await
    });
    (client, upstream, task)
}

/// Runs a test body with a bound of one hour on the paused clock. The
/// paused clock jumps to the next timer when every task waits, so the bound
/// fails a test whose body never ends at once instead of letting it hang.
/// One hour is far above every bound of the gate.
async fn bounded<F: Future>(body: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(3600), body)
        .await
        .expect("the test did not end within one hour of paused time")
}

#[tokio::test(start_paused = true)]
async fn rule_1_closes_a_connection_without_a_first_byte_at_10_s() {
    let (_readiness, gate) = open_gate();
    let (mut client, mut upstream, task) = start_forward(gate);
    bounded(async move {
        // The client bytes reach the upstream before its first answer.
        client.write_all(b"client hello").await.unwrap();
        let mut hello = [0_u8; 12];
        upstream.read_exact(&mut hello).await.unwrap();
        assert_eq!(&hello, b"client hello");

        let report = task.await.unwrap();
        assert_eq!(report.end, End::NoFirstByte);
        assert_eq!(report.duration, Duration::from_secs(10));
        assert_eq!(report.first_byte, None);
        let mut rest = Vec::new();
        assert_eq!(client.read_to_end(&mut rest).await.unwrap(), 0);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn rule_1_keeps_a_connection_whose_first_byte_comes_in_time() {
    let (_readiness, gate) = open_gate();
    let (mut client, mut upstream, task) = start_forward(gate);
    bounded(async move {
        let server = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(9_999)).await;
            upstream.write_all(b"s").await.unwrap();
            // Hold the connection until the client ends it.
            let mut rest = Vec::new();
            upstream.read_to_end(&mut rest).await.unwrap();
        });
        let mut first = [0_u8; 1];
        client.read_exact(&mut first).await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!task.is_finished(), "rule 1 closed an answered connection");
        client.shutdown().await.unwrap();
        server.await.unwrap();

        let report = task.await.unwrap();
        assert_eq!(report.end, End::Finished);
        assert_eq!(report.first_byte, Some(Duration::from_millis(9_999)));
        // The first byte at 9.999 s, then 60 s of an open connection.
        assert_eq!(report.duration, Duration::from_millis(69_999));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn rule_1_covers_a_connect_that_does_not_finish() {
    let (_readiness, gate) = open_gate();
    let (_client, gate_client) = duplex(1024);
    let epoch = gate.borrow().epoch();
    let report = bounded(forward(
        gate_client,
        std::future::pending::<io::Result<DuplexStream>>(),
        Instant::now(),
        &LIMITS,
        gate,
        epoch,
    ))
    .await;
    assert_eq!(report.end, End::NoFirstByte);
    assert_eq!(report.duration, Duration::from_secs(10));
}

#[tokio::test(start_paused = true)]
async fn a_failed_upstream_connect_closes_the_connection() {
    let (_readiness, gate) = open_gate();
    let (_client, gate_client) = duplex(1024);
    let epoch = gate.borrow().epoch();
    let report = bounded(forward(
        gate_client,
        async { Err::<DuplexStream, _>(io::Error::from(io::ErrorKind::ConnectionRefused)) },
        Instant::now(),
        &LIMITS,
        gate,
        epoch,
    ))
    .await;
    assert_eq!(report.end, End::UpstreamUnavailable);
    assert_eq!(report.error, Some(io::ErrorKind::ConnectionRefused));
    assert_eq!(report.duration, Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_gate_that_closes_and_opens_again_still_ends_the_connection() {
    let (readiness, gate) = open_gate();
    let (mut client, mut upstream, task) = start_forward(gate);
    bounded(async move {
        upstream.write_all(b"s").await.unwrap();
        let mut first = [0_u8; 1];
        client.read_exact(&mut first).await.unwrap();

        // Both changes happen before the connection task runs again.
        readiness.set_child_running(false);
        readiness.set_child_running(true);
        let report = task.await.unwrap();
        assert_eq!(report.end, End::GateClosed);
        assert_eq!(report.first_byte, Some(Duration::ZERO));
        let mut rest = Vec::new();
        assert_eq!(client.read_to_end(&mut rest).await.unwrap(), 0);
    })
    .await;
}

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

#[test]
fn gate_full_is_recorded_at_once_and_then_at_most_once_each_interval() {
    let start = Instant::now();
    let after = |seconds| start + Duration::from_secs(seconds);
    let mut refusals = Refusals::new(Duration::from_secs(60));
    assert_eq!(refusals.due(start), None);

    assert_eq!(refusals.refuse(start, at(0)), Some(CountWindow::one(at(0))));
    assert_eq!(refusals.refuse(after(1), at(1)), None);
    assert_eq!(refusals.refuse(after(30), at(30)), None);
    assert_eq!(refusals.due(after(59)), None);
    assert_eq!(
        refusals.due(after(60)),
        Some(CountWindow {
            count: 2,
            first: at(1),
            last: at(30)
        })
    );
    // Nothing to record.
    assert_eq!(refusals.due(after(200)), None);
    // The last record was more than an interval ago.
    assert_eq!(
        refusals.refuse(after(201), at(201)),
        Some(CountWindow::one(at(201)))
    );
    assert_eq!(refusals.refuse(after(202), at(202)), None);
    assert_eq!(refusals.take(), Some(CountWindow::one(at(202))));
    assert_eq!(refusals.take(), None);
}

#[test]
fn the_peer_class_never_holds_the_address() {
    let class = |ip: IpAddr| peer_class(ip);
    assert_eq!(class(Ipv4Addr::LOCALHOST.into()), "loopback");
    assert_eq!(class(Ipv4Addr::new(10, 1, 2, 3).into()), "private");
    assert_eq!(class(Ipv4Addr::new(192, 168, 0, 1).into()), "private");
    assert_eq!(class(Ipv4Addr::new(100, 64, 0, 1).into()), "shared");
    assert_eq!(class(Ipv4Addr::new(100, 127, 255, 254).into()), "shared");
    assert_eq!(class(Ipv4Addr::new(100, 128, 0, 1).into()), "other");
    assert_eq!(class(Ipv4Addr::new(169, 254, 1, 1).into()), "link_local");
    assert_eq!(class(Ipv4Addr::new(203, 0, 113, 7).into()), "other");
    assert_eq!(class(Ipv6Addr::LOCALHOST.into()), "loopback");
    assert_eq!(
        class(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped().into()),
        "private"
    );
    assert_eq!(class("fd00::1".parse().unwrap()), "private");
    assert_eq!(class("fe80::1".parse().unwrap()), "link_local");
    assert_eq!(class("2001:db8::1".parse().unwrap()), "other");
}

#[test]
fn the_buffers_of_the_bound_are_16_mib() {
    // ADR 0001: two copy buffers of 8 KiB for each of 1024 connections.
    assert_eq!(
        LIMITS.max_connections * 2 * LIMITS.buffer_size,
        16 * 1024 * 1024
    );
    assert_eq!(LIMITS.first_byte_timeout, Duration::from_secs(10));
    assert_eq!(LIMITS.gate_full_interval, Duration::from_secs(60));
}
