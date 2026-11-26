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
//! - **Rule 3, TCP keep-alive.** Each accepted socket gets keep-alive with
//!   an idle time of 15 s, an interval of 15 s, and 9 probes, the values
//!   that Go sets for `fleet-telemetry` today. Each accepted socket and each
//!   socket to `fleet-telemetry` also gets `TCP_NODELAY`, which Go sets on
//!   its sockets by default, so that the gate does not delay small TLS
//!   records. A failed option is logged and the connection stays open: the
//!   history comes first, and the dead-peer bound then depends on the
//!   operating system defaults.
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
//!   (rule 1). When `fleet-telemetry` ends its side without a byte just
//!   before the 10 s, the close linger follows: at most 15 s.
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
//! - Every connection ends at once when the gate closes or the process
//!   stops.
//!
//! Bounds: each forwarded connection has two copy buffers of 8 KiB
//! ([`GateLimits::buffer_size`]), so 1024 connections use 16 MiB of buffers.
//! At the stop, the gate waits [`GateLimits::close_deadline`] for the
//! connection tasks and then aborts them. After a failed `accept`, for
//! example "too many open files", the acceptor waits 0.5 s.
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
use tokio::task::{JoinError, JoinSet};
use tokio::time::{Instant, MissedTickBehavior, sleep, sleep_until, timeout, timeout_at};
use tracing::{info, warn};

use crate::events::{CountWindow, EventKind, EventSink, IngestEvent};
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

/// Runs the gate until the state is shut, then closes every connection.
///
/// `upstream` is the loopback address of `fleet-telemetry`.
pub async fn run(
    listener: TcpListener,
    upstream: SocketAddr,
    mut gate: watch::Receiver<GateState>,
    sink: Arc<dyn EventSink>,
    heartbeat: Heartbeat,
    limits: GateLimits,
) {
    let permits = Arc::new(Semaphore::new(limits.max_connections));
    let mut connections = JoinSet::new();
    let mut refusals = Refusals::new(limits.gate_full_interval);
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
            accepted = accept(&listener) => match accepted {
                Ok((stream, peer)) => {
                    let accepted_at = Instant::now();
                    let state = *gate.borrow();
                    if !state.is_open() {
                        drop(stream);
                    } else if let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
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
                        if let Some(window) = refusals.refuse(Instant::now(), Timestamp::now()) {
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
    info!("gate_stopped");
    heartbeat.retire();
}

/// Accepts one connection and sets the socket options of gate rule 3.
///
/// # Errors
///
/// Returns the error of `accept`. A failed socket option is logged, and the
/// connection is returned.
pub async fn accept(listener: &TcpListener) -> io::Result<(TcpStream, SocketAddr)> {
    let (stream, peer) = listener.accept().await?;
    let options = SockRef::from(&stream)
        .set_tcp_keepalive(&KEEPALIVE)
        .and_then(|()| stream.set_nodelay(true));
    if let Err(error) = options {
        warn!(error = %error, "gate_socket_options_failed");
    }
    Ok((stream, peer))
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
    let connect = async move {
        let stream = TcpStream::connect(upstream).await?;
        if let Err(error) = stream.set_nodelay(true) {
            warn!(error = %error, "gate_socket_options_failed");
        }
        Ok(stream)
    };
    let report = forward(
        client,
        connect,
        admission.accepted_at,
        &limits,
        gate,
        admission.epoch,
    )
    .await;
    drop(admission.permit);
    info!(
        reason = report.end.as_str(),
        error = report.error.map(tracing::field::debug),
        duration_ms = millis(report.duration),
        first_byte_ms = report.first_byte.map(millis),
        peer_class = peer_class(peer.ip()),
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
async fn forward<C, U>(
    client: C,
    connect: impl Future<Output = io::Result<U>>,
    accepted_at: Instant,
    limits: &GateLimits,
    mut gate: watch::Receiver<GateState>,
    epoch: u64,
) -> Report
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = accepted_at + limits.first_byte_timeout;
    // A dropped sender also ends the wait: the process stops.
    let closed = async move {
        gate.wait_for(|state| state.epoch() != epoch).await.ok();
    };
    let mut closed = pin!(closed);

    let (end, error, first_byte) = 'run: {
        let upstream = tokio::select! {
            biased;
            () = &mut closed => break 'run (End::GateClosed, None, None),
            connected = timeout_at(deadline, connect) => match connected {
                Ok(Ok(upstream)) => upstream,
                Ok(Err(error)) => break 'run (End::UpstreamUnavailable, Some(error.kind()), None),
                Err(_elapsed) => break 'run (End::NoFirstByte, None, None),
            },
        };
        let (client_read, mut client_write) = tokio::io::split(client);
        let (upstream_read, mut upstream_write) = tokio::io::split(upstream);
        let (seen, mut first_seen) = oneshot::channel();
        // One buffer of `buffer_size` for each direction.
        let mut client_read = BufReader::with_capacity(limits.buffer_size, client_read);
        let mut upstream_read = BufReader::with_capacity(
            limits.buffer_size,
            FirstByte {
                inner: upstream_read,
                seen: Some(seen),
                at: None,
            },
        );
        let (end, error) = {
            // Each direction copies until its end of stream and then ends
            // the stream of the other side.
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
            let mut no_first_byte = pin!(sleep_until(deadline));
            let mut up_done = false;
            let mut waiting = true;
            loop {
                // `biased`: the copy reads a byte that arrived in time
                // before the deadline is checked.
                tokio::select! {
                    biased;
                    copied = &mut down => match copied {
                        Ok(()) if up_done => break (End::Finished, None),
                        Ok(()) => {
                            // `fleet-telemetry` ended its side, and the
                            // client has the last bytes and the end of the
                            // stream. Give the client a short time to end
                            // its side, then close the connection.
                            tokio::select! {
                                _ = timeout(limits.upstream_close_linger, &mut up) => {}
                                () = &mut closed => {}
                            }
                            break (End::UpstreamClosed, None);
                        }
                        Err(error) => break (End::Failed, Some(error.kind())),
                    },
                    copied = &mut up, if !up_done => match copied {
                        // The client ended its side. `fleet-telemetry`
                        // decides when the connection ends.
                        Ok(()) => up_done = true,
                        Err(error) => break (End::Failed, Some(error.kind())),
                    },
                    _ = &mut first_seen, if waiting => waiting = false,
                    () = &mut closed => break (End::GateClosed, None),
                    () = &mut no_first_byte, if waiting => break (End::NoFirstByte, None),
                }
            }
        };
        let first_byte = upstream_read
            .get_ref()
            .at
            .map(|at| at.saturating_duration_since(accepted_at));
        (end, error, first_byte)
    };
    Report {
        end,
        error,
        first_byte,
        duration: accepted_at.elapsed(),
    }
}

/// The stream to `fleet-telemetry`. It notes the time of the first byte
/// that it reads.
struct FirstByte<S> {
    inner: S,
    seen: Option<oneshot::Sender<()>>,
    at: Option<Instant>,
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
        if this.at.is_none() && matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() > before
        {
            this.at = Some(Instant::now());
            if let Some(seen) = this.seen.take() {
                seen.send(()).ok();
            }
        }
        polled
    }
}

/// The count of refused connections and the rate limit of `gate_full`.
#[derive(Debug)]
struct Refusals {
    interval: Duration,
    last_record: Option<Instant>,
    pending: Option<CountWindow>,
}

impl Refusals {
    const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_record: None,
            pending: None,
        }
    }

    /// Counts one refusal and returns the window to record, if one is due.
    fn refuse(&mut self, now: Instant, at: Timestamp) -> Option<CountWindow> {
        match &mut self.pending {
            Some(window) => window.add(at),
            None => self.pending = Some(CountWindow::one(at)),
        }
        self.due(now)
    }

    /// Returns the counted window when the interval since the last record
    /// has passed.
    fn due(&mut self, now: Instant) -> Option<CountWindow> {
        self.pending?;
        if let Some(last) = self.last_record
            && now.saturating_duration_since(last) < self.interval
        {
            return None;
        }
        self.last_record = Some(now);
        self.pending.take()
    }

    /// Returns the counted window without the rate limit, at the stop.
    const fn take(&mut self) -> Option<CountWindow> {
        self.pending.take()
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
