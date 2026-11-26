//! Tests of the reader: the settle time, the backoff, the drain, the
//! frames, and the reader loop with real sockets of the `zeromq` crate.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeromq::prelude::SocketSend;

use super::*;
use crate::events::recording::RecordingSink;
use crate::pipeline::conditions::Database;
use crate::readiness::{Pipeline, Readiness};
use crate::watchdog::{READER, Watchdog};

#[tokio::test(start_paused = true)]
async fn the_socket_settles_250_ms_after_the_connect() {
    let settle = Limits::PRODUCTION.settle;
    let start = Instant::now();
    let mut state = Settle::default();
    assert_eq!(state.due(settle), None);
    assert!(!state.poll(start, settle));
    state.connected(start);
    assert_eq!(state.due(settle), Some(start + Duration::from_millis(250)));
    assert!(!state.poll(start + Duration::from_millis(249), settle));
    assert!(state.poll(start + Duration::from_millis(250), settle));
    assert!(state.is_settled());
    assert_eq!(state.due(settle), None);
    // Only the change counts.
    assert!(!state.poll(start + Duration::from_secs(1), settle));
}

#[tokio::test(start_paused = true)]
async fn a_replaced_socket_needs_a_new_connect_and_a_new_settle_time() {
    let settle = Limits::PRODUCTION.settle;
    let start = Instant::now();
    let mut state = Settle::default();
    state.connected(start);
    assert!(state.poll(start + settle, settle));
    state.lost();
    assert!(!state.is_settled());
    assert_eq!(state.due(settle), None);
    assert!(!state.poll(start + Duration::from_secs(10), settle));
    let again = start + Duration::from_secs(11);
    state.connected(again);
    assert!(!state.poll(again + Duration::from_millis(100), settle));
    assert!(state.poll(again + settle, settle));
}

#[test]
fn the_backoff_after_a_lost_peer_doubles_to_its_bound_and_resets() {
    let limits = Limits::PRODUCTION;
    let mut backoff = Backoff::new(limits.rebuild_backoff_first, limits.rebuild_backoff_max);
    let waits: Vec<u128> = (0..9).map(|_| backoff.take().as_millis()).collect();
    assert_eq!(waits, [0, 100, 200, 400, 800, 1_600, 3_200, 5_000, 5_000]);
    backoff.reset();
    assert_eq!(backoff.take(), Duration::ZERO);
}

#[test]
fn only_a_socket_with_unread_messages_is_drained_and_only_a_lost_peer_backs_off() {
    assert!(Rebuild::ChildStopped.drains());
    assert!(Rebuild::Stalled.drains());
    assert!(!Rebuild::PeerLost.drains());
    assert!(Rebuild::PeerLost.backs_off());
    assert!(Rebuild::SubscribeFailed.backs_off());
    assert!(!Rebuild::ChildStopped.backs_off());
    assert!(!Rebuild::Stalled.backs_off());
}

#[tokio::test(start_paused = true)]
async fn the_drain_ends_after_1_s_without_a_message_or_at_its_bound() {
    let start = Instant::now();
    let quiet = Duration::from_secs(1);
    let bound = Duration::from_secs(3);
    let mut drain = Drain::new(start, quiet, bound);
    assert_eq!(drain.end(), start + quiet);
    drain.message(start + Duration::from_millis(700));
    assert_eq!(drain.end(), start + Duration::from_millis(1700));
    // A steady stream ends at the bound.
    for millis in (800..3000).step_by(100) {
        drain.message(start + Duration::from_millis(millis));
    }
    assert_eq!(drain.end(), start + bound);
}

/// A message with these frames.
fn message(parts: &[&[u8]]) -> ZmqMessage {
    let mut message = ZmqMessage::from(parts[0].to_vec());
    for part in &parts[1..] {
        message.push_back(ZmqMessage::from(part.to_vec()).into_vec().remove(0));
    }
    message
}

#[test]
fn a_message_keeps_every_byte_of_its_frames() {
    assert_eq!(
        frames(message(&[b"tesla_telemetry_V", b"payload"])),
        (b"tesla_telemetry_V".to_vec(), b"payload".to_vec())
    );
    assert_eq!(
        frames(message(&[b"topic"])),
        (b"topic".to_vec(), Vec::new())
    );
    assert_eq!(
        frames(message(&[b"t", b"ab", b"cd"])),
        (b"t".to_vec(), b"abcd".to_vec())
    );
}

/// A reader task on the real clock with its inputs.
struct Run {
    readiness: Arc<Readiness>,
    conditions: Arc<Conditions>,
    received: Arc<Received>,
    queue: Arc<Queue>,
    sink: RecordingSink,
    stalled: watch::Sender<Option<u64>>,
    drain: Option<oneshot::Sender<Duration>>,
    task: tokio::task::JoinHandle<()>,
}

impl Run {
    fn start(endpoint: String) -> Self {
        Self::start_with(endpoint, Limits::PRODUCTION)
    }

    #[expect(
        clippy::large_types_passed_by_value,
        reason = "a test helper that copies the limits once for each test"
    )]
    fn start_with(endpoint: String, limits: Limits) -> Self {
        let sink = RecordingSink::default();
        let readiness = Arc::new(Readiness::new(Arc::new(sink.clone())));
        readiness.set_child_running(true);
        let conditions = Arc::new(Conditions::new(
            Arc::clone(&readiness),
            Arc::new(sink.clone()),
        ));
        conditions.set_database(Database::Passed);
        let queue = Arc::new(Queue::new(
            &limits,
            Arc::clone(&conditions),
            Arc::new(tokio::sync::Notify::new()),
        ));
        let received = Arc::new(Received::default());
        let (stalled, stall_requests) = watch::channel(None);
        let reader = Reader {
            endpoint,
            limits,
            queue: Arc::clone(&queue),
            conditions: Arc::clone(&conditions),
            received: Arc::clone(&received),
            sink: Arc::new(sink.clone()),
            gate: readiness.subscribe(),
            stalled: stall_requests,
        };
        let (drain, drain_requests) = oneshot::channel();
        let task = tokio::spawn(reader.run(Watchdog::new().register(READER), drain_requests));
        Self {
            readiness,
            conditions,
            received,
            queue,
            sink,
            stalled,
            drain: Some(drain),
            task,
        }
    }

    /// Waits until `done` holds, at most `limit`.
    async fn wait_until(&self, limit: Duration, what: &str, done: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(
                start.elapsed() < limit,
                "{what}: pipeline {:?}, counts {:?}",
                self.conditions.pipeline(),
                self.received.snapshot()
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_ready_on(&self, socket: u64) {
        self.wait_until(Duration::from_secs(10), "ready", |run| {
            run.conditions.pipeline() == Pipeline::Ready && run.received.snapshot().socket == socket
        })
        .await;
    }

    fn closed_reasons(&self) -> Vec<&'static str> {
        self.sink
            .events()
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::GateClosed { reason } => Some(reason),
                _ => None,
            })
            .collect()
    }

    async fn stop(mut self) {
        self.drain
            .take()
            .unwrap()
            .send(Duration::from_millis(100))
            .unwrap();
        self.task.await.unwrap();
    }
}

async fn publisher() -> (zeromq::PubSocket, String) {
    let mut socket = zeromq::PubSocket::new();
    let endpoint = socket.bind("tcp://127.0.0.1:0").await.unwrap();
    (socket, endpoint.to_string())
}

#[tokio::test]
async fn a_child_stop_replaces_the_socket_without_a_disconnect() {
    // The PUB socket of the test stays open, so the SUB socket sees no
    // `Disconnected` event: only the child stop replaces the socket (review
    // round 1, mutation M7). The stop and the start come before the
    // reader runs, so the watch channel merges them (note N3).
    let (publisher, endpoint) = publisher().await;
    let run = Run::start(endpoint);
    run.wait_ready_on(1).await;
    assert_eq!(run.received.snapshot().generation, 1);
    run.readiness.set_child_running(false);
    run.readiness.set_child_running(true);
    run.wait_ready_on(2).await;
    // The new socket is the first one to a new child.
    assert_eq!(run.received.snapshot().generation, 2);
    assert!(
        run.closed_reasons().contains(&"subscriber_not_settled"),
        "{:?}",
        run.closed_reasons()
    );
    run.stop().await;
    drop(publisher);
}

#[tokio::test]
async fn a_lost_peer_is_replaced_without_a_child_stop() {
    // The PUB socket goes away and comes back on the same port, with no
    // child stop: the reader makes a new socket itself and does not wait
    // for the reconnect of the crate.
    let (mut publisher, endpoint) = publisher().await;
    let run = Run::start(endpoint.clone());
    run.wait_ready_on(1).await;
    drop(publisher.close().await);
    publisher = zeromq::PubSocket::new();
    publisher.bind(&endpoint).await.unwrap();
    run.wait_ready_on(2).await;
    // The same child: the generation stays.
    assert_eq!(run.received.snapshot().generation, 1);
    assert_eq!(run.closed_reasons(), ["subscriber_not_settled"]);
    publisher
        .send(message(&[b"tesla_telemetry_V", b"x"]))
        .await
        .unwrap();
    run.wait_until(Duration::from_secs(5), "received", |run| {
        run.received.snapshot().vehicle_data == 1
    })
    .await;
    run.stop().await;
    drop(publisher);
}

#[tokio::test]
async fn a_stall_request_replaces_only_the_named_socket() {
    let (publisher, endpoint) = publisher().await;
    let run = Run::start(endpoint);
    run.wait_ready_on(1).await;
    // A request for another socket does nothing.
    run.stalled.send_replace(Some(7));
    sleep(Duration::from_millis(500)).await;
    assert_eq!(run.received.snapshot().socket, 1);
    assert_eq!(run.conditions.pipeline(), Pipeline::Ready);
    run.stalled.send_replace(Some(1));
    run.wait_ready_on(2).await;
    // The same child: the generation stays, and the gate closed with the
    // reason of the stall.
    assert_eq!(run.received.snapshot().generation, 1);
    assert_eq!(run.closed_reasons(), ["subscriber_stalled"]);
    run.stop().await;
    drop(publisher);
}

#[tokio::test]
async fn a_second_stall_in_a_row_waits_with_the_gate_closed() {
    let (mut publisher, endpoint) = publisher().await;
    let limits = Limits {
        stall_backoff_first: Duration::from_millis(600),
        ..Limits::PRODUCTION
    };
    let run = Run::start_with(endpoint, limits);
    run.wait_ready_on(1).await;
    // The first stall: a new socket at once.
    run.stalled.send_replace(Some(1));
    run.wait_ready_on(2).await;
    // The second stall in a row: the gate stays closed for the wait.
    let stalled_at = Instant::now();
    run.stalled.send_replace(Some(2));
    run.wait_until(Duration::from_secs(5), "closed", |run| {
        run.conditions.pipeline() == Pipeline::NotReady("subscriber_stalled")
    })
    .await;
    run.wait_ready_on(3).await;
    assert!(
        stalled_at.elapsed() >= Duration::from_millis(600),
        "{:?}",
        stalled_at.elapsed()
    );
    // A received record ends the row: the next stall gets a new socket at
    // once again.
    publisher
        .send(message(&[b"tesla_telemetry_V", b"x"]))
        .await
        .unwrap();
    run.wait_until(Duration::from_secs(5), "received", |run| {
        run.received.snapshot().vehicle_data == 1
    })
    .await;
    let stalled_at = Instant::now();
    run.stalled.send_replace(Some(3));
    run.wait_ready_on(4).await;
    assert!(
        stalled_at.elapsed() < Duration::from_millis(600),
        "{:?}",
        stalled_at.elapsed()
    );
    run.stop().await;
    drop(publisher);
}

#[tokio::test]
async fn a_rebuild_reads_every_message_that_the_old_socket_received() {
    let (mut publisher, endpoint) = publisher().await;
    let run = Run::start(endpoint);
    run.wait_ready_on(1).await;
    let count = 300_u64;
    for index in 0..count {
        let payload = index.to_string();
        publisher
            .send(message(&[b"tesla_telemetry_V", payload.as_bytes()]))
            .await
            .unwrap();
    }
    // At once: most of the messages wait in the socket of the reader.
    run.stalled.send_replace(Some(1));
    run.wait_ready_on(2).await;
    assert_eq!(run.received.snapshot().vehicle_data, count);
    assert_eq!(run.queue.stats().items, usize::try_from(count).unwrap());
    run.stop().await;
    drop(publisher);
}

/// A child that ends right after the ZMTP handshake, with the READY of the
/// SUB socket unread (review round 2 of PR #22, finding B2; the fake peer
/// of the reviewer).
async fn child_that_ends_after_the_handshake(listener: tokio::net::TcpListener) {
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
        .await
        .unwrap()
        .unwrap();
    drop(listener);
    let mut greeting = [0_u8; 64];
    stream.read_exact(&mut greeting).await.unwrap();
    let mut answer = vec![0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0x7F, 3, 0];
    let mut mechanism = b"NULL".to_vec();
    mechanism.resize(20, 0);
    answer.extend(mechanism);
    answer.push(0);
    answer.extend([0_u8; 31]);
    stream.write_all(&answer).await.unwrap();
    let mut body = vec![5];
    body.extend(b"READY");
    body.push(11);
    body.extend(b"Socket-Type");
    body.extend(3_u32.to_be_bytes());
    body.extend(b"PUB");
    let mut frame = vec![0x04, u8::try_from(body.len()).unwrap()];
    frame.extend(body);
    stream.write_all(&frame).await.unwrap();
}

/// How the trial meets the child that ends after the handshake.
#[derive(Debug, Clone, Copy)]
enum Trial {
    /// After a healthy first child; the stop comes at once.
    Restart,
    /// After a healthy first child; the stop comes 100 ms later.
    LateStop,
    /// The first child of the reader.
    FirstChild,
    /// The first child of the reader, and no child stop is reported: only
    /// the socket itself can show the end of the peer (connect, then
    /// subscribe).
    NoStop,
}

/// One trial of the reproduction of the reviewer: a healthy child, a child
/// that ends after the handshake, then a healthy child again. Returns true
/// when the last child's records arrive within 10 s.
async fn trial(kind: Trial) -> bool {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("tcp://127.0.0.1:{port}");
    let run = match kind {
        Trial::FirstChild | Trial::NoStop => {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .unwrap();
            let run = Run::start(endpoint.clone());
            child_that_ends_after_the_handshake(listener).await;
            run
        }
        Trial::Restart | Trial::LateStop => {
            let mut first = zeromq::PubSocket::new();
            first.bind(&endpoint).await.unwrap();
            let run = Run::start(endpoint.clone());
            run.wait_ready_on(1).await;
            drop(first.close().await);
            run.readiness.set_child_running(false);
            run.readiness.set_child_running(true);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .unwrap();
            child_that_ends_after_the_handshake(listener).await;
            if matches!(kind, Trial::LateStop) {
                sleep(Duration::from_millis(100)).await;
            }
            run
        }
    };
    if !matches!(kind, Trial::NoStop) {
        run.readiness.set_child_running(false);
        sleep(Duration::from_millis(20)).await;
        run.readiness.set_child_running(true);
    }
    let mut last = zeromq::PubSocket::new();
    last.bind(&endpoint).await.unwrap();
    let start = Instant::now();
    let mut arrived = false;
    while start.elapsed() < Duration::from_secs(10) {
        // A send without a subscriber is no error of the test.
        let _sent = last.send(message(&[b"tesla_telemetry_V", b"x"])).await;
        sleep(Duration::from_millis(100)).await;
        if run.received.snapshot().vehicle_data > 0 {
            arrived = true;
            break;
        }
    }
    let end = run.conditions.pipeline();
    run.stop().await;
    drop(last);
    if !arrived {
        eprintln!("trial {kind:?}: no record arrived; pipeline {end:?}");
    }
    arrived
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_that_ends_after_the_handshake_does_not_stop_the_subscription() {
    // Review round 2 of PR #22, finding B2. The trigger is a race, so the
    // trial count can be raised with `READER_B2_TRIALS` for evidence runs;
    // the suite runs each kind once.
    let trials: usize = std::env::var("READER_B2_TRIALS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let kinds = [
        Trial::Restart,
        Trial::LateStop,
        Trial::FirstChild,
        Trial::NoStop,
    ];
    let mut failed = Vec::new();
    for round in 0..trials {
        for kind in kinds {
            if !trial(kind).await {
                failed.push((round, kind));
            }
        }
    }
    let total = trials * kinds.len();
    eprintln!("B2 trials: {} of {total} recovered", total - failed.len());
    assert!(failed.is_empty(), "{failed:?}");
}
