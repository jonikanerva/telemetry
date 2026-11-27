//! The gate: the public TCP port of the service (ADR 0001, "Layout").
//!
//! The gate listens on the public port and copies the bytes of each accepted
//! connection, in both directions, to `fleet-telemetry` on its loopback
//! port. It does not read or terminate TLS. One acceptor task owns the
//! listener, the connection set, and the refusal count. It follows the
//! [`GateState`] of [`crate::readiness`].
//!
//! - **Closed gate.** The acceptor accepts each connection and closes it at
//!   once. No byte reaches `fleet-telemetry`. The listener stays bound, so a
//!   wrong port fails the start, and no client waits in the listen backlog
//!   for a gate that opens later. The peer sees the same result as a refused
//!   connection and reconnects with its own backoff.
//! - **Gate closes.** Each forwarded connection keeps the epoch of the
//!   state at its accept. A change of the epoch closes the connection: both
//!   sockets close, and the vehicle keeps its data in its own buffer.
//! - **Rule 1, first answer within 10 s.** A connection that has no byte
//!   from `fleet-telemetry` 10 s after the accept closes. The 10 s also cover
//!   the connect to the loopback port. The time from the accept to the first
//!   byte from `fleet-telemetry` is the field `first_byte_ms` of the event
//!   `gate_connection_closed` (ADR 0001, "To verify at first deployment").
//! - **Rule 2, 1024 connections.** At the bound, the acceptor accepts each
//!   new connection and closes it at once. It records a `gate_full` event
//!   with the count and the time window at the first refusal, then at most
//!   one each [`GateLimits::gate_full_interval`] (60 s). At the stop of the
//!   gate it records the count that is not recorded yet, so that no refusal
//!   is lost; that last event can come sooner than 60 s after the one
//!   before.
//! - **Rule 3, TCP keep-alive.** Each admitted socket gets keep-alive with
//!   an idle time of 15 s, an interval of 15 s, and 9 probes, the values
//!   that Go sets for `fleet-telemetry` today. Each admitted socket and each
//!   socket to `fleet-telemetry` also gets `TCP_NODELAY`, which Go sets on
//!   its sockets by default, so that the gate does not delay small TLS
//!   records. The gate sets the options only after the admission, so a
//!   closed gate and a refused connection cost no system call. A failed
//!   option keeps the connection open: the history comes first, and the
//!   dead-peer bound of that connection then depends on the operating
//!   system defaults. The field `socket_options` of the connection event
//!   shows `set` when the gate set both options, `failed` for such a
//!   connection, or `peer_gone` when the peer had ended the connection
//!   before the options were set (no fault). The fields `keepalive` (for
//!   example `15/15/9`) and `nodelay` show the values that the admitted
//!   socket has, read back from the operating system.
//! - **`fleet-telemetry` closes.** When `fleet-telemetry` ends its side, the
//!   gate forwards the remaining bytes and the end of the stream to the
//!   client. Then it waits at most [`GateLimits::upstream_close_linger`]
//!   (5 s) for the client to end its side, and closes the whole connection.
//!   So the connection ends when `fleet-telemetry` closes it (ADR 0001), and
//!   the client cannot keep the slot. The wait lets a live client receive
//!   the last bytes and answer with its own end of the stream instead of a
//!   reset. 5 s allows two TCP retransmissions after the initial
//!   retransmission timeout of 1 s on a poor mobile link, the same reason
//!   as the 10 s of rule 1.
//! - **The client closes its side.** The gate forwards the end of the
//!   stream to `fleet-telemetry` and keeps the other direction open.
//!   `fleet-telemetry` then decides when the connection ends.
//!
//! Who can hold a slot, and for how long:
//!
//! - Any peer before the first byte from `fleet-telemetry`: at most 10 s
//!   (rule 1). The deadline stays active during the close linger, so a
//!   connection that `fleet-telemetry` ends without any byte also ends at
//!   10 s.
//! - A client after `fleet-telemetry` closed: at most 5 s after the gate
//!   has written the last bytes to the client socket. The gate sees the
//!   end of the stream only after it has forwarded the bytes before it. A
//!   live client that stops reading can therefore delay that point without
//!   a bound, when the unread bytes are more than the gate buffer of 8 KiB
//!   and the kernel send buffer can hold.
//! - A client that went away: the dead-peer bound of rule 3, about 150 s
//!   when no data is unacknowledged, about 925 to 1,045 s with
//!   unacknowledged data (Linux defaults, ADR 0001).
//! - A live client after the first byte, for example inside the TLS
//!   handshake or in a quiet session, and a client that closed only its
//!   side: as long as `fleet-telemetry` keeps the connection. This is the
//!   accepted risk of ADR 0001, lead decision 6.
//! - When the gate closes or the process stops, each connection ends at
//!   once. The bytes move in a task of their own, and the connection task
//!   only watches the gate state, the deadlines, and the end of the copy;
//!   it aborts the copy task and waits until the copy task has ended. So a
//!   copy that always has data cannot delay the gate close, rule 1, or the
//!   close linger.
//!
//! The stop: the gate waits [`GateLimits::close_deadline`] for the
//! connection tasks and then aborts them. An abort of a task only asks the
//! runtime to cancel it, and an aborted connection task only asks for the
//! cancel of its copy task. Until the runtime has cancelled a copy task, it
//! can still move bytes in both directions, without the controls. So the
//! stop then waits at most [`GateLimits::abort_wait`] until every slot is
//! free ([`Slots`]): a free slot means that the copy task of the connection
//! has ended and has dropped both sockets. `gate_stopped` comes after this
//! wait; a slot that is still taken at the bound is logged
//! (`gate_connections_open`). [`crate::lifecycle`] waits for the slots
//! again after the gate task has ended, because an abort of the gate task
//! ends its connection tasks in the same way.
//!
//! Bounds: each forwarded connection has two copy buffers of 8 KiB
//! ([`GateLimits::buffer_size`]), so 1024 connections use 16 MiB of buffers.
//! After a failed `accept`, for example "too many open files", the acceptor
//! waits 0.5 s.
//!
//! The acceptor beats its watchdog heartbeat each second, also when no
//! client connects.
//!
//! Logs: one `gate_connection_closed` event for each forwarded connection,
//! with the end reason, the duration, the time to the first byte, and the
//! class of the peer address (`peer_class`: loopback, private, shared,
//! link-local, or other). The class shows whether the container sees the
//! client address through the Railway TCP proxy (ADR 0001, "To verify at
//! first deployment") without a log of the address itself. Refused
//! connections have no event of their own; the `gate_full` count covers
//! them.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use jiff::Timestamp;
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf, copy_buf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior, sleep, sleep_until, timeout, timeout_at};
use tracing::{info, warn};

use crate::events::{CountThrottle, EventKind, EventSink, IngestEvent};
use crate::readiness::GateState;
use crate::watchdog::{self, Heartbeat};

/// Bounds of the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateLimits {
    /// Number of forwarded connections that can be open at the same time.
    pub max_connections: usize,
    /// Time from the accept to the first byte from `fleet-telemetry`.
    pub first_byte_timeout: Duration,
    /// Size of each of the two copy buffers of a connection.
    pub buffer_size: usize,
    /// Shortest time between two `gate_full` events.
    pub gate_full_interval: Duration,
    /// Time that the stop waits for the connection tasks.
    pub close_deadline: Duration,
    /// Time that the stop waits, after it has aborted the connection tasks
    /// at the close deadline, for every slot to be free.
    pub abort_wait: Duration,
    /// Longest time between two heartbeats of the acceptor.
    pub beat_interval: Duration,
    /// Longest wait for the client to end its side after `fleet-telemetry`
    /// ended its side.
    pub upstream_close_linger: Duration,
}

/// The bounds of ADR 0001. The close deadline is part of the shutdown
/// budget of [`crate::lifecycle`].
pub const LIMITS: GateLimits = GateLimits {
    max_connections: 1024,
    first_byte_timeout: Duration::from_secs(10),
    buffer_size: 8 * 1024,
    gate_full_interval: Duration::from_secs(60),
    close_deadline: Duration::from_millis(500),
    abort_wait: Duration::from_millis(200),
    beat_interval: watchdog::BEAT_INTERVAL,
    upstream_close_linger: Duration::from_secs(5),
};

/// TCP keep-alive of each accepted socket (ADR 0001, gate rule 3).
pub const KEEPALIVE: TcpKeepalive = TcpKeepalive::new()
    .with_time(Duration::from_secs(15))
    .with_interval(Duration::from_secs(15))
    .with_retries(9);

/// Wait after a failed `accept` before the next attempt. It is shorter than
/// the heartbeat interval.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// The connection slots of rule 2.
///
/// A forwarded connection holds its slot until its copy task has ended and
/// has dropped both sockets, also when an abort ends the connection task or
/// the copy task. Before the copy task starts, the connection task holds
/// the slot. So a free slot means that no task of that connection can move
/// a byte any more, and the stop can wait until every slot is free. This
/// holds also after an abort of the gate task, which ends the connection
/// tasks only when the runtime cancels them.
#[derive(Debug, Clone)]
pub struct Slots {
    semaphore: Arc<Semaphore>,
    count: u32,
}

impl Slots {
    /// Slots for [`GateLimits::max_connections`] connections.
    #[must_use]
    pub fn new(limits: &GateLimits) -> Self {
        let count = u32::try_from(limits.max_connections).unwrap_or(u32::MAX);
        Self {
            semaphore: Arc::new(Semaphore::new(count as usize)),
            count,
        }
    }

    /// Takes a free slot, if one exists.
    pub(crate) fn try_take(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.semaphore).try_acquire_owned().ok()
    }

    /// Waits until every slot is free, at most until `deadline`, and
    /// returns the number of slots that are still taken (0 when every
    /// connection has ended). Call it only after the acceptor has stopped.
    pub async fn wait_free(&self, deadline: Instant) -> usize {
        match timeout_at(deadline, self.semaphore.acquire_many(self.count)).await {
            // The slots go back when `_all` is dropped.
            Ok(Ok(_all)) => 0,
            Ok(Err(_)) | Err(_) => {
                (self.count as usize).saturating_sub(self.semaphore.available_permits())
            }
        }
    }
}

/// Runs the gate until the state is shut, then closes every connection.
///
/// `upstream` is the loopback address of `fleet-telemetry`. `slots` must
/// have been made with the same `limits`.
pub async fn run(
    listener: TcpListener,
    upstream: SocketAddr,
    mut gate: watch::Receiver<GateState>,
    sink: Arc<dyn EventSink>,
    heartbeat: Heartbeat,
    limits: GateLimits,
    slots: Slots,
) {
    let mut connections = JoinSet::new();
    let mut refusals = CountThrottle::new(limits.gate_full_interval);
    let mut tick = tokio::time::interval(limits.beat_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        heartbeat.beat();
        if gate.borrow_and_update().is_shut() {
            break;
        }
        tokio::select! {
            changed = gate.changed() => {
                // The sender is gone only when the process stops.
                if changed.is_err() {
                    break;
                }
            }
            _ = tick.tick() => {
                if let Some(window) = refusals.due(Instant::now()) {
                    sink.record(IngestEvent::now(EventKind::GateFull(window)));
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let accepted_at = Instant::now();
                    let state = *gate.borrow();
                    if !state.is_open() {
                        drop(stream);
                    } else if let Some(permit) = slots.try_take() {
                        connections.spawn(serve_connection(
                            stream,
                            peer,
                            upstream,
                            Admission { accepted_at, epoch: state.epoch(), permit },
                            gate.clone(),
                            limits,
                        ));
                    } else {
                        drop(stream);
                        if let Some(window) = refusals.count(Instant::now(), Timestamp::now()) {
                            sink.record(IngestEvent::now(EventKind::GateFull(window)));
                        }
                    }
                }
                Err(error) => {
                    warn!(error = %error, "gate_accept_failed");
                    tokio::select! {
                        () = sleep(ACCEPT_RETRY_DELAY) => {}
                        _ = gate.changed() => {}
                    }
                }
            },
        }
        while let Some(finished) = connections.try_join_next() {
            log_task_failure(finished);
        }
    }

    drop(listener);
    if let Some(window) = refusals.take() {
        sink.record(IngestEvent::now(EventKind::GateFull(window)));
    }
    // Each closed connection has its own `gate_connection_closed` event.
    // A count here would race with the connection tasks, which see the
    // shut state at the same time as the acceptor.
    let drained = timeout(limits.close_deadline, async {
        while let Some(finished) = connections.join_next().await {
            log_task_failure(finished);
        }
    })
    .await;
    if drained.is_err() {
        warn!(connections = connections.len(), "gate_connections_aborted");
        connections.shutdown().await;
    }
    // The abort of a connection task only asks the runtime to cancel its
    // copy task. Until the runtime has done so, the copy task can still move
    // bytes to `fleet-telemetry`, so the stop waits for the slots. Without an
    // abort, every slot is free already.
    let open = slots.wait_free(Instant::now() + limits.abort_wait).await;
    if open > 0 {
        warn!(connections = open, "gate_connections_open");
    }
    info!("gate_stopped");
    heartbeat.retire();
}

/// Sets the socket options of gate rule 3 on an admitted client socket:
/// TCP keep-alive and `TCP_NODELAY`.
///
/// # Errors
///
/// Returns the error of the operating system.
pub fn set_client_options(stream: &TcpStream) -> io::Result<()> {
    SockRef::from(stream).set_tcp_keepalive(&KEEPALIVE)?;
    stream.set_nodelay(true)
}

/// The outcome of [`set_client_options`] for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketOptions {
    Set,
    /// The peer was gone already, for example after a reset. This is not a
    /// fault of the configuration, and the connection ends by itself.
    PeerGone,
    Failed(io::ErrorKind),
}

impl SocketOptions {
    fn of(stream: &TcpStream) -> Self {
        match set_client_options(stream) {
            Ok(()) => Self::Set,
            Err(_) if stream.peer_addr().is_err() => Self::PeerGone,
            Err(error) => Self::Failed(error.kind()),
        }
    }

    /// The value of the log field `socket_options`. `set` is logged too, as
    /// positive evidence that the gate set the options (deployment check 5).
    const fn label(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::PeerGone => "peer_gone",
            Self::Failed(_) => "failed",
        }
    }
}

/// The options that an admitted socket has, read back from the operating
/// system for the log of the connection. They are the evidence of rule 3
/// on the running system (deployment check 5 of ADR 0001).
struct ObservedOptions {
    /// `idle/interval/probes` in seconds and probes, `off`, or `unknown`
    /// when the read fails. Numbers only.
    keepalive: String,
    /// `TCP_NODELAY`, when the read succeeds.
    nodelay: Option<bool>,
}

impl ObservedOptions {
    fn of(stream: &TcpStream) -> Self {
        let socket = SockRef::from(stream);
        let keepalive = match (
            socket.keepalive(),
            socket.tcp_keepalive_time(),
            socket.tcp_keepalive_interval(),
            socket.tcp_keepalive_retries(),
        ) {
            (Ok(true), Ok(idle), Ok(interval), Ok(probes)) => {
                format!("{}/{}/{probes}", idle.as_secs(), interval.as_secs())
            }
            (Ok(false), ..) => "off".to_owned(),
            _ => "unknown".to_owned(),
        };
        Self {
            keepalive,
            nodelay: stream.nodelay().ok(),
        }
    }
}

/// Connects to `fleet-telemetry` and sets `TCP_NODELAY` on the socket. A
/// failed option is logged, and the connection is returned.
///
/// # Errors
///
/// Returns the error of the connect.
pub async fn connect_upstream(address: SocketAddr) -> io::Result<TcpStream> {
    let stream = TcpStream::connect(address).await?;
    if let Err(error) = stream.set_nodelay(true) {
        warn!(error = %error, "gate_upstream_socket_options_failed");
    }
    Ok(stream)
}

/// What the acceptor gives to a forwarded connection.
struct Admission {
    accepted_at: Instant,
    epoch: u64,
    permit: OwnedSemaphorePermit,
}

async fn serve_connection(
    client: TcpStream,
    peer: SocketAddr,
    upstream: SocketAddr,
    admission: Admission,
    gate: watch::Receiver<GateState>,
    limits: GateLimits,
) {
    // Only an admitted connection gets the options, so refused connections
    // and a closed gate cost no system call and no log line.
    let options = SocketOptions::of(&client);
    // Read back from the operating system, not taken from `options`: the
    // log shows the values that the socket really has.
    let observed = ObservedOptions::of(&client);
    let report = forward(
        client,
        connect_upstream(upstream),
        admission.accepted_at,
        &limits,
        gate,
        admission.epoch,
        admission.permit,
    )
    .await;
    info!(
        reason = report.end.as_str(),
        error = report.error.map(tracing::field::debug),
        duration_ms = millis(report.duration),
        first_byte_ms = report.first_byte.map(millis),
        peer_class = peer_class(peer.ip()),
        socket_options = options.label(),
        keepalive = observed.keepalive.as_str(),
        nodelay = observed.nodelay,
        socket_options_error = match options {
            SocketOptions::Failed(kind) => Some(tracing::field::debug(kind)),
            SocketOptions::Set | SocketOptions::PeerGone => None,
        },
        "gate_connection_closed"
    );
}

/// How a forwarded connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// The client ended its side, then `fleet-telemetry` ended its side.
    Finished,
    /// `fleet-telemetry` ended its side first; the gate closed the
    /// connection at the latest after the close linger.
    UpstreamClosed,
    /// A read or a write failed.
    Failed,
    /// The connect to `fleet-telemetry` failed.
    UpstreamUnavailable,
    /// Rule 1: no byte from `fleet-telemetry` within the first-byte timeout.
    NoFirstByte,
    /// The gate closed.
    GateClosed,
    /// The copy task panicked.
    Panicked,
}

impl End {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::UpstreamClosed => "upstream_closed",
            Self::Failed => "io_error",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::NoFirstByte => "no_first_byte",
            Self::GateClosed => "gate_closed",
            Self::Panicked => "copy_task_panicked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Report {
    end: End,
    error: Option<io::ErrorKind>,
    /// Time from the accept to the first byte from `fleet-telemetry`.
    first_byte: Option<Duration>,
    /// Time from the accept to the end.
    duration: Duration,
}

/// Forwards one connection until it ends. `connect` opens the connection to
/// `fleet-telemetry`. The function is generic so that the tests can use
/// in-memory streams with a paused clock.
///
/// The bytes move in a task of their own ([`copy_both`]). This task only
/// watches the controls: the gate close, the first-byte deadline of rule 1,
/// and the close linger. When a control ends the connection, it aborts the
/// copy task, and the abort drops both sockets. A copy direction that
/// always has data uses the cooperative budget of its own task only, so it
/// cannot delay a control.
///
/// `slot` is the slot of the connection ([`Slots`]). This task holds it
/// until the copy task starts; then the copy task holds it until it has
/// dropped both sockets.
async fn forward<C, U, S>(
    client: C,
    connect: impl Future<Output = io::Result<U>>,
    accepted_at: Instant,
    limits: &GateLimits,
    mut gate: watch::Receiver<GateState>,
    epoch: u64,
    slot: S,
) -> Report
where
    C: AsyncRead + AsyncWrite + Send + 'static,
    U: AsyncRead + AsyncWrite + Send + 'static,
    S: Send + Unpin + 'static,
{
    let deadline = accepted_at + limits.first_byte_timeout;
    // A dropped sender also ends the wait: the process stops.
    let closed = async move {
        gate.wait_for(|state| state.epoch() != epoch).await.ok();
    };
    let mut closed = pin!(closed);

    let (end, error, first_byte_at) = 'run: {
        let upstream = tokio::select! {
            () = &mut closed => break 'run (End::GateClosed, None, None),
            connected = timeout_at(deadline, connect) => match connected {
                Ok(Ok(upstream)) => upstream,
                Ok(Err(error)) => break 'run (End::UpstreamUnavailable, Some(error.kind()), None),
                Err(_elapsed) => break 'run (End::NoFirstByte, None, None),
            },
        };
        let (seen, mut first_seen) = oneshot::channel();
        let (upstream_ended, mut upstream_end) = oneshot::channel();
        // The guard aborts the copy task also when this task is cancelled
        // before its own abort below, for example by the abort of the stop at
        // the close deadline. A dropped `JoinHandle` alone would detach the
        // copy task, and it would go on without the controls.
        let mut copy = AbortOnDrop(tokio::spawn(SlottedCopy {
            copy: Box::pin(copy_both(
                client,
                upstream,
                limits.buffer_size,
                seen,
                upstream_ended,
            )),
            _slot: slot,
        }));
        let mut no_first_byte = pin!(sleep_until(deadline));
        let mut linger = pin!(sleep_until(deadline));
        let mut first_byte_at = None;
        let mut waiting = true;
        let mut lingering = false;
        let mut copy_done = false;
        let (end, error) = loop {
            tokio::select! {
                () = &mut closed => break (End::GateClosed, None),
                copied = &mut copy.0 => {
                    copy_done = true;
                    break match copied {
                        Ok(Ok(end)) => (end, None),
                        Ok(Err(kind)) => (End::Failed, Some(kind)),
                        Err(_join) => {
                            warn!("gate_copy_task_panicked");
                            (End::Panicked, None)
                        }
                    };
                }
                at = &mut first_seen, if waiting => {
                    waiting = false;
                    first_byte_at = at.ok();
                }
                () = &mut no_first_byte, if waiting => {
                    // The copy task notes the time of the first byte when it
                    // reads it, so a byte that came in time keeps the
                    // connection also when this task sees it late.
                    match first_seen.try_recv() {
                        Ok(at) if at <= deadline => {
                            waiting = false;
                            first_byte_at = Some(at);
                        }
                        _ => break (End::NoFirstByte, None),
                    }
                }
                ended = &mut upstream_end, if !lingering => {
                    lingering = true;
                    if ended.is_ok() {
                        // `fleet-telemetry` ended its side, and the client
                        // has the last bytes and the end of the stream. Give
                        // the client a short time to end its side.
                        linger.as_mut().reset(Instant::now() + limits.upstream_close_linger);
                    } else {
                        // The copy task ended without that signal; its
                        // result follows.
                        linger.as_mut().reset(Instant::now() + NEVER);
                    }
                }
                () = &mut linger, if lingering => break (End::UpstreamClosed, None),
            }
        };
        if !copy_done {
            copy.0.abort();
            // The task drops both sockets when it ends. Only then does the
            // caller return the permit of the connection.
            (&mut copy.0).await.ok();
        }
        if waiting {
            first_byte_at = first_seen.try_recv().ok();
        }
        (end, error, first_byte_at)
    };
    Report {
        end,
        error,
        first_byte: first_byte_at.map(|at| at.saturating_duration_since(accepted_at)),
        duration: accepted_at.elapsed(),
    }
}

/// A task handle that aborts its task when it is dropped.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The copy of one connection together with the slot of the connection.
///
/// Rust drops the fields of a struct in the order of their declaration. So
/// when the task ends, also by an abort, the copy and both of its sockets
/// are dropped first, and only then is the slot free.
struct SlottedCopy<F, S> {
    copy: Pin<Box<F>>,
    /// Held only for its drop.
    _slot: S,
}

impl<F: Future, S: Unpin> Future for SlottedCopy<F, S> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().copy.as_mut().poll(cx)
    }
}

/// A time far in the future for a timer that must not fire.
const NEVER: Duration = Duration::from_hours(24 * 365);

/// Copies both directions until both end, or until the caller aborts the
/// task. Each direction ends the stream of the other side at its own end.
/// `upstream_ended` gets a signal when `fleet-telemetry` ends its side
/// before the client. One buffer of `buffer_size` for each direction.
///
/// Returns [`End::UpstreamClosed`] when `fleet-telemetry` ended first, and
/// [`End::Finished`] when the client ended first.
async fn copy_both<C, U>(
    client: C,
    upstream: U,
    buffer_size: usize,
    seen: oneshot::Sender<Instant>,
    upstream_ended: oneshot::Sender<()>,
) -> Result<End, io::ErrorKind>
where
    C: AsyncRead + AsyncWrite,
    U: AsyncRead + AsyncWrite,
{
    let (client_read, mut client_write) = tokio::io::split(client);
    let (upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let mut client_read = BufReader::with_capacity(buffer_size, client_read);
    let mut upstream_read = BufReader::with_capacity(
        buffer_size,
        FirstByte {
            inner: upstream_read,
            seen: Some(seen),
        },
    );
    let up = async {
        copy_buf(&mut client_read, &mut upstream_write).await?;
        upstream_write.shutdown().await
    };
    let down = async {
        copy_buf(&mut upstream_read, &mut client_write).await?;
        client_write.shutdown().await
    };
    let mut up = pin!(up);
    let mut down = pin!(down);
    tokio::select! {
        copied = &mut up => {
            // The client ended its side. `fleet-telemetry` decides when the
            // connection ends.
            copied.map_err(|error| error.kind())?;
            down.await.map_err(|error| error.kind())?;
            Ok(End::Finished)
        }
        copied = &mut down => {
            copied.map_err(|error| error.kind())?;
            upstream_ended.send(()).ok();
            // The remaining bytes of the client still go to
            // `fleet-telemetry`; the caller ends this wait after the linger.
            // An error here comes from the closed side and changes nothing.
            up.await.ok();
            Ok(End::UpstreamClosed)
        }
    }
}

/// The stream to `fleet-telemetry`. It sends the time of the first byte
/// that it reads.
struct FirstByte<S> {
    inner: S,
    seen: Option<oneshot::Sender<Instant>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for FirstByte<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(())))
            && buf.filled().len() > before
            && let Some(seen) = this.seen.take()
        {
            seen.send(Instant::now()).ok();
        }
        polled
    }
}

/// A coarse class of a peer address. The log gets the class, never the
/// address.
fn peer_class(ip: IpAddr) -> &'static str {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    };
    match ip {
        IpAddr::V4(v4) if v4.is_loopback() => "loopback",
        IpAddr::V4(v4) if v4.is_private() => "private",
        // 100.64.0.0/10, the shared address space of RFC 6598.
        IpAddr::V4(v4) if v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64 => "shared",
        IpAddr::V4(v4) if v4.is_link_local() => "link_local",
        IpAddr::V6(v6) if v6.is_loopback() => "loopback",
        IpAddr::V6(v6) if v6.is_unique_local() => "private",
        IpAddr::V6(v6) if v6.is_unicast_link_local() => "link_local",
        _ => "other",
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Logs a connection task that panicked. A task that the stop aborted is
/// not a failure.
fn log_task_failure(finished: Result<(), JoinError>) {
    if let Err(error) = finished
        && error.is_panic()
    {
        warn!("gate_connection_task_panicked");
    }
}

#[cfg(test)]
mod tests;
