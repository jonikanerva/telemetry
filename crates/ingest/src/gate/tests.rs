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
use crate::events::CountWindow;
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
    slots: Slots,
    task: JoinHandle<()>,
}

impl TestGate {
    async fn start(limits: GateLimits, heartbeat: Heartbeat) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let sink = RecordingSink::default();
        let readiness = Arc::new(Readiness::new(Arc::new(sink.clone())));
        let slots = Slots::new(&limits);
        let task = tokio::spawn(run(
            listener,
            upstream.local_addr().unwrap(),
            readiness.subscribe(),
            Arc::new(sink.clone()),
            heartbeat,
            limits,
            slots.clone(),
        ));
        Self {
            address,
            upstream,
            readiness,
            sink,
            slots,
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

/// Copies from `source` without pause, and reads `target` without pause.
/// Returns the time when the reader saw the end of the connection.
fn flood(mut source: TcpStream, mut target: TcpStream) -> oneshot::Receiver<Instant> {
    tokio::spawn(async move {
        let block = vec![1_u8; 256 * 1024];
        while source.write_all(&block).await.is_ok() {}
    });
    let (ended, end) = oneshot::channel();
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; 256 * 1024];
        while let Ok(read) = target.read(&mut buffer).await {
            if read == 0 {
                break;
            }
        }
        ended.send(Instant::now()).ok();
    });
    end
}

/// The longest accepted time from a gate close to the end of a connection
/// that carries a flow without pause. The close does not wait for the flow.
const CLOSE_UNDER_FLOW: Duration = Duration::from_secs(1);

/// Opens a connection with a first byte from the upstream, starts a flow
/// in one direction, closes the gate, and returns the time to the end.
async fn close_time_under_flow(to_upstream: bool) -> Duration {
    let gate = TestGate::start_open().await;
    let mut client = gate.connect().await;
    let mut upstream = gate.accept_upstream().await;
    upstream.write_all(b"x").await.unwrap();
    let mut first = [0_u8; 1];
    client.read_exact(&mut first).await.unwrap();
    let end = if to_upstream {
        flood(client, upstream)
    } else {
        flood(upstream, client)
    };
    // The flow runs for a while before the close.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let closed_at = Instant::now();
    gate.readiness.set_child_running(false);
    let ended = tokio::time::timeout(WAIT, end)
        .await
        .expect("the gate close did not end the connection")
        .unwrap();
    gate.stop().await;
    ended.saturating_duration_since(closed_at)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gate_close_ends_a_connection_with_a_flow_to_fleet_telemetry() {
    for _ in 0..2 {
        let waited = close_time_under_flow(true).await;
        assert!(waited < CLOSE_UNDER_FLOW, "{waited:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gate_close_ends_a_connection_with_a_flow_to_the_client() {
    for _ in 0..2 {
        let waited = close_time_under_flow(false).await;
        assert!(waited < CLOSE_UNDER_FLOW, "{waited:?}");
    }
}

#[tokio::test]
async fn a_cancelled_connection_task_ends_its_copy_task() {
    // Review round 2, finding A: the connection task can be cancelled
    // before it aborts the copy task (the abort of the stop at the close
    // deadline, or a dropped connection set). The copy task must end with
    // it and close both streams.
    let (_readiness, gate) = open_gate();
    let (mut client, mut upstream, task) = start_forward(gate);
    upstream.write_all(b"s").await.unwrap();
    let mut first = [0_u8; 1];
    client.read_exact(&mut first).await.unwrap();

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // Both peers see the end, and a later write of the client reaches no
    // one.
    let mut rest = Vec::new();
    let read = tokio::time::timeout(WAIT, client.read_to_end(&mut rest)).await;
    assert_eq!(read.expect("the client saw no end").unwrap(), 0);
    assert!(client.write_all(b"after").await.is_err());
    let mut received = Vec::new();
    let read = tokio::time::timeout(WAIT, upstream.read_to_end(&mut received)).await;
    read.expect("the upstream saw no end").unwrap();
    assert_eq!(received, b"");
}

/// A client stream that records the largest read request, and panics on
/// a read when `panic` is set. It writes nothing and never ends.
struct Probe {
    largest_read: Arc<std::sync::atomic::AtomicUsize>,
    panic: bool,
}

impl AsyncRead for Probe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        assert!(!self.panic, "test panic in the copy task");
        self.largest_read
            .fetch_max(buf.remaining(), std::sync::atomic::Ordering::Relaxed);
        let coop = std::task::ready!(tokio::task::coop::poll_proceed(cx));
        let room = buf.initialize_unfilled().len();
        buf.advance(room);
        coop.made_progress();
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Probe {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn each_copy_direction_reads_into_a_buffer_of_8_kib() {
    let (readiness, gate) = open_gate();
    let epoch = gate.borrow().epoch();
    let largest = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let client = Probe {
        largest_read: Arc::clone(&largest),
        panic: false,
    };
    let largest_upstream = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let upstream = Probe {
        largest_read: Arc::clone(&largest_upstream),
        panic: false,
    };
    let task = tokio::spawn(async move {
        forward(
            client,
            async { Ok(upstream) },
            Instant::now(),
            &LIMITS,
            gate,
            epoch,
            (),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    readiness.set_child_running(false);
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    let read = std::sync::atomic::Ordering::Relaxed;
    assert_eq!(largest.load(read), LIMITS.buffer_size);
    assert_eq!(largest_upstream.load(read), LIMITS.buffer_size);
    assert_eq!(LIMITS.buffer_size, 8 * 1024);
}

#[tokio::test]
async fn a_panic_in_the_copy_task_is_reported_as_a_panic() {
    let (capture, _guard) = capture::install(ALLOWED_FIELDS);
    let (_readiness, gate) = open_gate();
    let epoch = gate.borrow().epoch();
    let client = Probe {
        largest_read: Arc::default(),
        panic: true,
    };
    let (gate_upstream, _upstream) = duplex(1024);
    let report = tokio::time::timeout(
        WAIT,
        forward(
            client,
            async { Ok(gate_upstream) },
            Instant::now(),
            &LIMITS,
            gate,
            epoch,
            (),
        ),
    )
    .await
    .unwrap();
    assert_eq!(report.end, End::Panicked);
    assert_eq!(report.end.as_str(), "copy_task_panicked");
    let text = capture.text();
    assert!(
        text.contains("\"message\":\"gate_copy_task_panicked\""),
        "{text}"
    );
    assert!(!text.contains("test panic"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn an_end_of_stream_is_not_a_first_byte() {
    let (_readiness, gate) = open_gate();
    let (_client, mut upstream, task) = start_forward(gate);
    bounded(async move {
        // `fleet-telemetry` ends its side without a byte just before the
        // 10 s; the client stays.
        tokio::time::sleep(Duration::from_millis(9_900)).await;
        upstream.shutdown().await.unwrap();
        let report = task.await.unwrap();
        assert_eq!(report.first_byte, None);
        // The rule 1 deadline stays active during the close linger, so the
        // connection ends at 10 s, not after the linger.
        assert_eq!(report.end, End::NoFirstByte);
        assert_eq!(report.duration, Duration::from_secs(10));
    })
    .await;
}

#[tokio::test]
async fn forward_returns_only_after_both_streams_are_closed() {
    // The caller returns the permit when `forward` returns, so both streams
    // must be closed by then, also when a control ends the connection.
    let (readiness, gate) = open_gate();
    let epoch = gate.borrow().epoch();
    let (mut client, gate_client) = duplex(1024);
    let (gate_upstream, mut upstream) = duplex(1024);
    upstream.write_all(b"s").await.unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        readiness.set_child_running(false);
    });
    let report = tokio::time::timeout(
        WAIT,
        forward(
            gate_client,
            async { Ok(gate_upstream) },
            Instant::now(),
            &LIMITS,
            gate,
            epoch,
            (),
        ),
    )
    .await
    .unwrap();
    assert_eq!(report.end, End::GateClosed);
    // Each read is polled once, without a yield, so no other task runs
    // between the return and these reads: the end of each stream must be
    // there at once.
    let mut first = [0_u8; 1];
    assert!(matches!(
        poll_once(client.read_exact(&mut first)).await,
        Poll::Ready(Ok(1))
    ));
    let mut rest = Vec::new();
    let at_once = poll_once(client.read_to_end(&mut rest)).await;
    assert!(matches!(at_once, Poll::Ready(Ok(0))), "{at_once:?}");
    let at_once = poll_once(upstream.read_to_end(&mut rest)).await;
    assert!(matches!(at_once, Poll::Ready(Ok(0))), "{at_once:?}");
}

/// Polls a future one time and returns the result, without a yield.
async fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
    let mut future = pin!(future);
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

/// The abort of the gate task by shutdown step 1, then the wait of step 1
/// for the slots. When the join of the aborted gate task returns, the
/// runtime may not have cancelled the connection tasks and their copy
/// tasks yet, and a copy task can still move bytes. After the wait, no copy
/// task is left: both sockets are closed, and no later byte passes.
async fn abort_the_gate_and_wait_for_the_slots() {
    let gate = TestGate::start_open().await;
    let mut client = gate.connect().await;
    let mut upstream = gate.accept_upstream().await;
    upstream.write_all(b"x").await.unwrap();
    let mut first = [0_u8; 1];
    client.read_exact(&mut first).await.unwrap();

    gate.task.abort();
    assert!(gate.task.await.unwrap_err().is_cancelled());
    assert_eq!(gate.slots.wait_free(Instant::now() + WAIT).await, 0);
    // Checked in the kernel, without a yield, so that no task can end the
    // connection between the wait and the check.
    assert!(
        closed_without_yield(&upstream),
        "the upstream socket is open"
    );
    assert!(closed_without_yield(&client), "the client socket is open");
    client.write_all(b"after").await.ok();
    assert_eq!(read_until_closed(&mut upstream).await, b"");
    assert_eq!(read_until_closed(&mut client).await, b"");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_gate_task_ends_every_forwarded_connection() {
    abort_the_gate_and_wait_for_the_slots().await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_aborted_gate_task_ends_every_forwarded_connection_on_one_thread() {
    // On one thread, in a spawned task, the copy task is still alive when
    // the join of the gate task returns (the run queue holds its cancel
    // after this task). Without the wait for the slots, the sockets stay
    // open in every run.
    tokio::spawn(abort_the_gate_and_wait_for_the_slots())
        .await
        .unwrap();
}

/// Whether the peer has closed `stream`, seen in the kernel without a
/// yield to the runtime. Waits at most 1 s for the end of the stream, with
/// the thread blocked, so on a current-thread runtime no task runs meanwhile.
fn closed_without_yield(stream: &TcpStream) -> bool {
    let socket = SockRef::from(stream);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let mut byte = [std::mem::MaybeUninit::uninit(); 1];
        match socket.peek(&mut byte) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            // The end of the stream or a reset.
            Ok(0) | Err(_) => return true,
            Ok(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Limits with a short wait for the slots, for the tests of the stop.
const SHORT_ABORT_WAIT: GateLimits = GateLimits {
    abort_wait: Duration::from_millis(300),
    ..LIMITS
};

#[tokio::test]
async fn the_stop_waits_for_a_taken_slot_before_gate_stopped() {
    // A slot that is still taken stands for a copy task that the runtime
    // has not cancelled yet.
    let gate = TestGate::start(SHORT_ABORT_WAIT, Watchdog::new().register(GATE_ACCEPTOR)).await;
    let slot = gate.slots.try_take().unwrap();
    let started = Instant::now();
    tokio::spawn(async move {
        sleep(Duration::from_millis(100)).await;
        drop(slot);
    });
    let (capture, _guard) = capture::install(ALLOWED_FIELDS);
    gate.stop().await;
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(100), "{elapsed:?}");
    assert!(elapsed < SHORT_ABORT_WAIT.abort_wait, "{elapsed:?}");
    let text = capture.text();
    assert!(text.contains("gate_stopped"), "{text}");
    assert!(!text.contains("gate_connections_open"), "{text}");
}

#[tokio::test]
async fn the_stop_logs_a_slot_that_is_still_taken_at_its_bound() {
    let gate = TestGate::start(SHORT_ABORT_WAIT, Watchdog::new().register(GATE_ACCEPTOR)).await;
    let slot = gate.slots.try_take().unwrap();
    let started = Instant::now();
    let (capture, _guard) = capture::install(ALLOWED_FIELDS);
    gate.stop().await;
    assert!(started.elapsed() >= SHORT_ABORT_WAIT.abort_wait);
    let lines: Vec<serde_json::Value> = capture
        .text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let messages: Vec<&str> = lines
        .iter()
        .filter_map(|line| line["message"].as_str())
        .collect();
    assert_eq!(messages, ["gate_connections_open", "gate_stopped"]);
    assert_eq!(lines[0]["connections"], 1);
    drop(slot);
}

/// A client stream whose first read blocks its worker thread until the
/// test lets it go. It notes when it is dropped.
struct BlockingClient {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

impl AsyncRead for BlockingClient {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.entered.send(()).ok();
        self.release.recv().ok();
        // No waker: the task is cancelled after this poll.
        Poll::Pending
    }
}

impl AsyncWrite for BlockingClient {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for BlockingClient {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_frees_the_slot_only_after_the_copy_task_has_dropped_its_sockets() {
    // The copy task is inside a read on a worker thread when the
    // connection task is aborted, so the runtime cannot cancel the copy
    // task until that read returns. Meanwhile the slot must stay taken.
    let semaphore = Arc::new(Semaphore::new(1));
    let slot = Arc::clone(&semaphore).try_acquire_owned().unwrap();
    let (entered, in_read) = mpsc::channel();
    let (let_go, release) = mpsc::channel();
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = BlockingClient {
        entered,
        release,
        dropped: Arc::clone(&dropped),
    };
    let (gate_upstream, _upstream) = duplex(1024);
    let (_readiness, gate) = open_gate();
    let epoch = gate.borrow().epoch();
    let task = tokio::spawn(async move {
        forward(
            client,
            async { Ok(gate_upstream) },
            Instant::now(),
            &LIMITS,
            gate,
            epoch,
            slot,
        )
        .await
    });
    tokio::task::spawn_blocking(move || in_read.recv_timeout(WAIT))
        .await
        .unwrap()
        .unwrap();

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let early = tokio::time::timeout(Duration::from_millis(100), semaphore.acquire()).await;
    assert!(early.is_err(), "the slot is free while the copy task runs");
    assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));

    let_go.send(()).unwrap();
    let _free = tokio::time::timeout(WAIT, semaphore.acquire())
        .await
        .unwrap()
        .unwrap();
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}

/// An in-memory stream for the tests of a flow without pause. A flowing
/// stream always has data to read; a silent stream never has data. Both
/// take every byte that is written at once. So a copy from a flowing
/// stream always has work and returns `Pending` only when the cooperative
/// budget of its task runs out, the condition of review finding 1. The
/// tests need no loopback socket, so they do not disturb other tests.
struct Endless {
    flowing: bool,
}

impl AsyncRead for Endless {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.flowing {
            // Like a socket, each read uses the cooperative budget of the
            // task.
            let coop = std::task::ready!(tokio::task::coop::poll_proceed(cx));
            let room = buf.initialize_unfilled().len();
            buf.advance(room);
            coop.made_progress();
            Poll::Ready(Ok(()))
        } else {
            // Never ready: the copy task is aborted, not woken.
            Poll::Pending
        }
    }
}

impl AsyncWrite for Endless {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let coop = std::task::ready!(tokio::task::coop::poll_proceed(cx));
        coop.made_progress();
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Starts [`forward`] with a flow without pause from the client
/// (`to_upstream`) or from the upstream, on the current thread.
fn start_endless_flow(
    gate: watch::Receiver<GateState>,
    to_upstream: bool,
    limits: GateLimits,
) -> JoinHandle<Report> {
    let epoch = gate.borrow().epoch();
    let client = Endless {
        flowing: to_upstream,
    };
    let upstream = Endless {
        flowing: !to_upstream,
    };
    let accepted_at = Instant::now();
    tokio::spawn(async move {
        forward(
            client,
            async { Ok(upstream) },
            accepted_at,
            &limits,
            gate,
            epoch,
            (),
        )
        .await
    })
}

async fn endless_flow_ends_at_a_gate_close(to_upstream: bool) {
    let (readiness, gate) = open_gate();
    let task = start_endless_flow(gate, to_upstream, LIMITS);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!task.is_finished());
    let closed_at = Instant::now();
    readiness.set_child_running(false);
    let report = tokio::time::timeout(CLOSE_UNDER_FLOW, task)
        .await
        .expect("the gate close did not end the flow")
        .unwrap();
    assert_eq!(report.end, End::GateClosed);
    assert!(closed_at.elapsed() < CLOSE_UNDER_FLOW);
}

#[tokio::test]
async fn a_gate_close_ends_an_endless_flow_to_fleet_telemetry() {
    endless_flow_ends_at_a_gate_close(true).await;
}

#[tokio::test]
async fn a_gate_close_ends_an_endless_flow_to_the_client() {
    endless_flow_ends_at_a_gate_close(false).await;
}

#[tokio::test]
async fn rule_1_ends_a_client_flood_that_fleet_telemetry_never_answers() {
    // The client sends without pause; the upstream takes every byte and
    // never answers.
    let limits = GateLimits {
        first_byte_timeout: Duration::from_millis(200),
        ..LIMITS
    };
    let (_readiness, gate) = open_gate();
    let task = start_endless_flow(gate, true, limits);
    let report = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("rule 1 did not end the flood")
        .unwrap();
    assert_eq!(report.end, End::NoFirstByte);
    assert_eq!(report.first_byte, None);
    assert!(report.duration >= Duration::from_millis(200));
    assert!(report.duration < Duration::from_secs(1), "{report:?}");
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
    let gate = TestGate::start(SHORT_LINGER, Watchdog::new().register(GATE_ACCEPTOR)).await;
    gate.open();
    let mut client = gate.connect().await;
    let mut upstream = gate.accept_upstream().await;

    client.write_all(b"request").await.unwrap();
    client.shutdown().await.unwrap();
    // The end of the stream reaches the upstream while the other direction
    // stays open.
    assert_eq!(read_until_closed(&mut upstream).await, b"request");
    // `fleet-telemetry` decides when the connection ends: the gate keeps
    // the other direction open, also longer than the close linger.
    tokio::time::sleep(SHORT_LINGER.upstream_close_linger * 2).await;
    let response = pattern(3, 1024 * 1024);
    upstream.write_all(&response).await.unwrap();
    drop(upstream);
    assert!(read_until_closed(&mut client).await == response);
    gate.stop().await;
}

/// The production limits with a close linger of 0.5 s.
const SHORT_LINGER: GateLimits = GateLimits {
    upstream_close_linger: Duration::from_millis(500),
    ..LIMITS
};

#[tokio::test]
async fn fleet_telemetry_closing_its_side_ends_the_connection_after_the_linger() {
    let gate = TestGate::start(SHORT_LINGER, Watchdog::new().register(GATE_ACCEPTOR)).await;
    gate.open();
    let mut client = gate.connect().await;
    let mut upstream = gate.accept_upstream().await;

    // `fleet-telemetry` sends its last bytes and ends its side, but keeps
    // reading.
    let response = pattern(4, 1024 * 1024);
    upstream.write_all(&response).await.unwrap();
    upstream.shutdown().await.unwrap();
    // The client gets every byte and then the end of the stream.
    let mut received = vec![0_u8; response.len()];
    client.read_exact(&mut received).await.unwrap();
    assert!(received == response, "the client got other bytes");
    let mut more = [0_u8; 1];
    let read = tokio::time::timeout(WAIT, client.read(&mut more)).await;
    assert_eq!(read.unwrap().unwrap(), 0, "no end of the stream");
    let end_seen = Instant::now();

    // The client stays silent and keeps its side open. The gate closes the
    // whole connection after the linger, so the upstream sees the close.
    assert_eq!(read_until_closed(&mut upstream).await, b"");
    let waited = end_seen.elapsed();
    assert!(
        waited >= SHORT_LINGER.upstream_close_linger / 2,
        "{waited:?}"
    );
    assert!(waited < Duration::from_secs(3), "{waited:?}");
    gate.stop().await;
}

#[tokio::test]
async fn a_client_that_answers_the_close_of_fleet_telemetry_ends_at_once() {
    // The production linger of 5 s: the end must come much sooner.
    let gate = TestGate::start_open().await;
    let mut client = gate.connect().await;
    let mut upstream = gate.accept_upstream().await;

    upstream.write_all(b"last").await.unwrap();
    upstream.shutdown().await.unwrap();
    assert_eq!(read_until_closed(&mut client).await, b"last");
    let started = Instant::now();
    client.write_all(b"bye").await.unwrap();
    drop(client);
    // The bytes of the client during the linger still reach the upstream.
    assert_eq!(read_until_closed(&mut upstream).await, b"bye");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
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
        // The gate set the options of rule 3 on the socket that it admitted.
        assert_eq!(event["socket_options"], "set", "{event}");
        // Read back from the admitted socket.
        assert_eq!(event["keepalive"], "15/15/9", "{event}");
        assert_eq!(event["nodelay"], true, "{event}");
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
    let (stream, _peer) = listener.accept().await.unwrap();
    set_client_options(&stream).unwrap();

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
async fn the_socket_to_fleet_telemetry_gets_no_delay() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let stream = connect_upstream(listener.local_addr().unwrap())
        .await
        .unwrap();
    assert!(stream.nodelay().unwrap());
}

#[tokio::test]
async fn a_peer_that_reset_the_connection_is_not_an_options_failure() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (stream, _peer) = listener.accept().await.unwrap();
    // A reset from the peer before the options are set.
    client.set_zero_linger().unwrap();
    drop(client);
    tokio::time::sleep(Duration::from_millis(50)).await;
    // macOS rejects the options on such a socket; Linux accepts them.
    let outcome = SocketOptions::of(&stream);
    assert!(
        matches!(outcome, SocketOptions::Set | SocketOptions::PeerGone),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_closed_connection_gives_its_slot_back() {
    // A bound of 2, and 6 connections one after the other: each closed
    // connection must free its slot for the next one.
    let limits = GateLimits {
        max_connections: 2,
        ..LIMITS
    };
    let gate = TestGate::start(limits, Watchdog::new().register(GATE_ACCEPTOR)).await;
    gate.open();
    let mut held = gate.connect().await;
    let mut held_upstream = gate.accept_upstream().await;
    for round in 0..6 {
        // The gate frees the slot of the previous round a moment after
        // its close, so a connect can be refused before that. Try again
        // until the gate admits a connection; without a freed slot, no
        // connection is ever admitted.
        let admitted = tokio::time::timeout(WAIT, async {
            loop {
                let mut client = gate.connect().await;
                client.write_all(b"admitted").await.ok();
                let mut byte = [0_u8; 1];
                let upstream = tokio::select! {
                    accepted = gate.upstream.accept() => Some(accepted.unwrap().0),
                    // Refused: the gate closed the connection.
                    _ = client.read(&mut byte) => None,
                };
                if let Some(upstream) = upstream {
                    break (client, upstream);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let (client, mut upstream) =
            admitted.unwrap_or_else(|_| panic!("round {round}: no slot came back"));
        let mut received = [0_u8; 8];
        upstream.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"admitted", "round {round}");
        drop(client);
        drop(upstream);
    }
    held.write_all(b"still").await.unwrap();
    let mut received = [0_u8; 5];
    held_upstream.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"still");
    gate.stop().await;
}

#[tokio::test]
async fn a_panic_in_a_connection_task_is_logged() {
    let (capture, _guard) = capture::install(ALLOWED_FIELDS);
    let mut tasks = JoinSet::new();
    tasks.spawn(async { panic!("test panic") });
    log_task_failure(tasks.join_next().await.unwrap());
    // A task that the stop aborts is not a failure.
    tasks.spawn(std::future::pending::<()>());
    tasks.abort_all();
    log_task_failure(tasks.join_next().await.unwrap());
    let text = capture.text();
    assert_eq!(
        text.matches("gate_connection_task_panicked").count(),
        1,
        "{text}"
    );
    assert!(!text.contains("test panic"), "{text}");
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
            (),
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
async fn a_silent_client_keeps_the_slot_for_at_most_5_s_after_fleet_telemetry_closes() {
    let (_readiness, gate) = open_gate();
    let (mut client, mut upstream, task) = start_forward(gate);
    bounded(async move {
        upstream.write_all(b"last").await.unwrap();
        upstream.shutdown().await.unwrap();
        let mut received = Vec::new();
        client.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"last");

        // The client does not end its side.
        let report = task.await.unwrap();
        assert_eq!(report.end, End::UpstreamClosed);
        assert_eq!(report.duration, Duration::from_secs(5));
        assert_eq!(report.first_byte, Some(Duration::ZERO));
        let mut rest = Vec::new();
        assert_eq!(upstream.read_to_end(&mut rest).await.unwrap(), 0);
        drop(client);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_client_half_close_leaves_the_end_to_fleet_telemetry() {
    let (_readiness, gate) = open_gate();
    let (mut client, mut upstream, task) = start_forward(gate);
    bounded(async move {
        upstream.write_all(b"s").await.unwrap();
        client.shutdown().await.unwrap();
        let mut received = Vec::new();
        upstream.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"");

        // Much longer than the close linger: the connection stays open.
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert!(!task.is_finished(), "the gate ended the connection");
        upstream.write_all(b"answer").await.unwrap();
        drop(upstream);
        let report = task.await.unwrap();
        assert_eq!(report.end, End::Finished);
        assert_eq!(report.duration, Duration::from_secs(600));
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).await.unwrap();
        assert_eq!(answer, b"sanswer");
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
        (),
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
        (),
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
