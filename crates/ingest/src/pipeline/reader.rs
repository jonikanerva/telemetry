//! The ZMQ reader (ADR 0001, "Transport", "Delivery path", step 1, and
//! "SUB socket").
//!
//! - **Socket.** A SUB socket of the `zeromq` crate (pure Rust) connects to
//!   `tcp://127.0.0.1:<INGEST_ZMQ_PORT>`, where `fleet-telemetry` binds its
//!   PUB socket, and subscribes to the topic prefix `tesla_telemetry_`.
//! - **The reader owns the life of the socket.** It does not depend on the
//!   reconnect of the crate: `zeromq` 0.7.0 can send `Connected` for a peer
//!   that it never registered, and then never reconnects (review round 2
//!   of PR #22, finding B2; ADR 0001, "SUB socket"). The reader replaces
//!   the whole socket ([`Rebuild`]) when the child stops, when the socket
//!   reports `Disconnected`, when the subscription cannot be sent, and when
//!   the stall rule of the shortfall check finds no received record while
//!   the child dispatches ([`super::shortfall::Liveness`]). The crate has
//!   no option to turn off its own reconnect; a dropped socket stops it.
//! - **Connect, then subscribe.** Each socket connects without a
//!   subscription and subscribes after the connect. With no subscription,
//!   the crate registers each connected peer, so a peer that ends always
//!   ends in a `Disconnected` event. The subscription then goes to the
//!   registered peer, and the result of that send is the positive evidence
//!   of the connect. Each connect attempt has a bound of
//!   [`Limits::connect_attempt`] (5 s); after a failed attempt the reader
//!   waits 100 ms and makes a new socket.
//! - **Gate condition 2.** A socket is settled 250 ms after its connect and
//!   subscription ([`Limits::settle`]), so that the PUB side has applied
//!   the subscription. The settle time starts from the results of the
//!   reader's own calls, not from a `Connected` event, so a lost event
//!   cannot hold the condition false. The condition is false while the
//!   reader replaces a socket, with the reason `subscriber_stalled` after a
//!   stall. Invariant: while the child runs and the socket is not settled,
//!   the reader is in a bounded connect attempt, a bounded pause, a
//!   bounded drain, or the settle time (ADR 0001, "No permanent close").
//! - **Rebuild order.** The reader first clears gate condition 2, so the
//!   gate closes and no new record is dispatched. Then it reads the old
//!   socket to its end: the `Disconnected` event of its peer, after which
//!   no message of that peer can come. The drain ends earlier after
//!   [`Limits::rebuild_quiet`] (200 ms) without a message, and at the
//!   latest after [`Limits::rebuild_drain`] (1 s). Then the reader drops
//!   the socket and makes a new one. So a rebuild loses no record that
//!   the old socket received.
//! - **Waits.** After a lost peer, the next socket waits 0, then
//!   [`Limits::rebuild_backoff_first`] (100 ms) doubling to
//!   [`Limits::rebuild_backoff_max`] (5 s). After a stall, it waits 0,
//!   then [`Limits::stall_backoff_first`] (30 s) doubling to
//!   [`Limits::stall_backoff_max`] (15 minutes), with gate condition 2
//!   false, so a socket that stalls again and again cannot keep the gate
//!   open. A received record resets both waits; a child stop resets them
//!   and ends a wait.
//! - **Generation.** The first socket after a child stop starts a new
//!   generation of the received counts, because the counter of a new child
//!   starts at 0. The stop count of the gate state shows each stop, also
//!   when the watch channel merges a stop and the next start into one
//!   value.
//! - **Messages.** `fleet-telemetry` sends two frames: the topic and the
//!   payload (ADR 0001, fact 4). The first frame is the topic. The payload
//!   is the second frame; a message with one frame has an empty payload,
//!   and a message with more frames has the other frames joined in order,
//!   so that no received byte is lost. The decode then stores such a
//!   message as unprocessed when it does not decode. For each message the
//!   reader counts the raw topic for the shortfall check, decodes it with
//!   the receive time, and adds it to the queue.
//! - **Full queue.** The reader drops the newest message and counts it. It
//!   records a `records_dropped` event with the count and the receive
//!   times at the first drop, then at most once each 60 s, and the rest at
//!   its stop ([`crate::events::CountThrottle`]).
//! - **Heartbeat.** The reader beats at least once each second in every
//!   wait, also when no message arrives (watchdog task `reader`, 30 s).
//! - **Drain.** At the shutdown (ADR 0001, step 2) the reader reads until
//!   no message arrives for [`Limits::drain_quiet`] (1 s), at most for the
//!   bound of the request (3 s). Then it records the rest of the drop
//!   count, retires its heartbeat, and closes the socket.
//!
//! Logs: `subscriber_connected`, `subscriber_settled`,
//! `subscriber_disconnected`, `subscriber_rebuilt` with the reason,
//! `subscriber_connect_failed` and `subscriber_subscribe_failed` with a
//! static code, and `subscriber_drained` with the count of drained
//! messages. The reader never logs a frame. The crate writes its own
//! errors to the `log` crate, which this process does not read; the reader
//! forwards no text of the crate, because that text can hold data.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use jiff::Timestamp;
use tokio::sync::{oneshot, watch};
use tokio::time::{Instant, Interval, MissedTickBehavior, sleep, sleep_until};
use tracing::{info, warn};
use zeromq::prelude::{Socket, SocketRecv};
use zeromq::{SocketEvent, SocketOptions, SubSocket, ZmqError, ZmqMessage};

use super::Limits;
use super::conditions::Conditions;
use super::queue::{Item, Queue};
use super::shortfall::Received;
use crate::decode::decode;
use crate::events::{CountThrottle, DROPPED_QUEUE_FULL, EventKind, EventSink, IngestEvent};
use crate::readiness::GateState;
use crate::record::TOPIC_PREFIX;
use crate::watchdog::{BEAT_INTERVAL, Heartbeat};

/// The wait after a failed connect attempt.
const CONNECT_PAUSE: Duration = Duration::from_millis(100);

/// The settle time of gate condition 2 after the connect of one socket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settle {
    connected_at: Option<Instant>,
    settled: bool,
}

impl Settle {
    /// The socket is connected and subscribed at `now`.
    pub const fn connected(&mut self, now: Instant) {
        self.connected_at = Some(now);
        self.settled = false;
    }

    /// The socket is being replaced.
    pub const fn lost(&mut self) {
        self.connected_at = None;
        self.settled = false;
    }

    /// The time when the socket is settled, while it waits for it.
    #[must_use]
    pub fn due(&self, settle: Duration) -> Option<Instant> {
        if self.settled {
            return None;
        }
        self.connected_at.map(|at| at + settle)
    }

    /// Marks the socket as settled when the time has come. Returns true at
    /// the change.
    pub fn poll(&mut self, now: Instant, settle: Duration) -> bool {
        if self.due(settle).is_some_and(|due| now >= due) {
            self.settled = true;
            return true;
        }
        false
    }

    /// True when the socket is settled.
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        self.settled
    }
}

/// Why the reader replaces its socket (module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebuild {
    /// The gate state counts a new child stop.
    ChildStopped,
    /// The socket reported `Disconnected`, or its monitor ended.
    PeerLost,
    /// The send of the subscription after the connect failed.
    SubscribeFailed,
    /// The stall rule found no received record while the child dispatched.
    Stalled,
}

impl Rebuild {
    /// The static text of the log field `reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChildStopped => "child_stopped",
            Self::PeerLost => "peer_lost",
            Self::SubscribeFailed => "subscribe_failed",
            Self::Stalled => "stalled",
        }
    }

    /// True when the old socket can hold messages that the reader has not
    /// read. After `Disconnected`, no message of the peer can come.
    const fn drains(self) -> bool {
        !matches!(self, Self::PeerLost | Self::SubscribeFailed)
    }

    /// True when the next socket waits for the backoff of a lost peer. A
    /// peer that ends again and again must not make a busy loop.
    const fn backs_off(self) -> bool {
        matches!(self, Self::PeerLost | Self::SubscribeFailed)
    }
}

/// The wait before a new socket: none, then the first wait, doubling to
/// the longest one. A received message or a child stop resets it. The
/// reader has one for lost peers and one for stalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
    first: Duration,
    max: Duration,
}

impl Backoff {
    /// A backoff without a wait yet.
    #[must_use]
    pub const fn new(first: Duration, max: Duration) -> Self {
        Self {
            next: Duration::ZERO,
            first,
            max,
        }
    }

    /// The wait now; the next one is longer.
    pub fn take(&mut self) -> Duration {
        let wait = self.next;
        self.next = if wait.is_zero() {
            self.first
        } else {
            wait.saturating_mul(2).min(self.max)
        };
        wait
    }

    /// The next wait is none again.
    pub const fn reset(&mut self) {
        self.next = Duration::ZERO;
    }
}

/// The end of the drain: no message for the quiet time, or the bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drain {
    quiet: Duration,
    bound: Instant,
    quiet_until: Instant,
}

impl Drain {
    /// A drain that starts at `now`.
    #[must_use]
    pub fn new(now: Instant, quiet: Duration, bound: Duration) -> Self {
        Self {
            quiet,
            bound: now + bound,
            quiet_until: now + quiet,
        }
    }

    /// A message arrived at `now`.
    pub fn message(&mut self, now: Instant) {
        self.quiet_until = now + self.quiet;
    }

    /// The time when the drain ends.
    #[must_use]
    pub fn end(&self) -> Instant {
        self.quiet_until.min(self.bound)
    }
}

/// A static code of a `zeromq` error for the log. The text of the error
/// can hold an address, so it is not logged.
const fn error_code(error: &ZmqError) -> &'static str {
    match error {
        ZmqError::Network(_) => "network",
        ZmqError::Codec(_) => "codec",
        ZmqError::Endpoint(_) => "endpoint",
        ZmqError::ConnectTimeout(_) => "timeout",
        ZmqError::NoMessage => "no_message",
        _ => "other",
    }
}

/// The topic and the payload of a message (module documentation).
fn frames(message: ZmqMessage) -> (Vec<u8>, Vec<u8>) {
    let mut frames = message.into_vec().into_iter();
    let topic = frames.next().map_or_default(|frame| frame.to_vec());
    let mut payload = frames.next().map_or_default(|frame| frame.to_vec());
    for frame in frames {
        payload.extend_from_slice(&frame);
    }
    (topic, payload)
}

/// The reader task.
pub struct Reader {
    /// The ZMQ endpoint, `tcp://127.0.0.1:<port>`.
    pub endpoint: String,
    /// The bounds.
    pub limits: Limits,
    /// The queue.
    pub queue: Arc<Queue>,
    /// The owner of gate condition 2.
    pub conditions: Arc<Conditions>,
    /// The received counts of the shortfall check.
    pub received: Arc<Received>,
    /// The sink of the `records_dropped` events.
    pub sink: Arc<dyn EventSink>,
    /// The gate state, for the child stops.
    pub gate: watch::Receiver<GateState>,
    /// The number of the socket that the stall rule found stalled
    /// ([`super::shortfall::Shortfall::stalled`]).
    pub stalled: watch::Receiver<Option<u64>>,
}

/// The monitor of one socket.
type Monitor = Pin<Box<dyn Stream<Item = SocketEvent> + Send>>;

/// One connected and subscribed socket.
struct Live {
    socket: SubSocket,
    monitor: Monitor,
    /// Its number ([`Received::connected`]).
    id: u64,
    /// The count of child stops at its connect.
    stops: u64,
}

/// The state of the reader across its sockets.
struct Loop {
    settle: Settle,
    drops: CountThrottle,
    /// The count of child stops of the current generation.
    generation_stops: Option<u64>,
    /// The wait after a lost peer.
    backoff: Backoff,
    /// The wait after a stall, with gate condition 2 false: a socket that
    /// stalls again and again must not open the gate for one detection
    /// window after each new socket (ADR 0001, "SUB socket").
    stall_backoff: Backoff,
    /// False after the sender of the gate state is gone.
    gate_open: bool,
    /// False after the sender of the stall requests is gone.
    stalled_open: bool,
}

/// The end of the time with one socket.
enum Served {
    Rebuild(Rebuild),
    /// The shutdown asks for the drain: the bound, or `None` for a dropped
    /// sender.
    Shutdown(Option<Duration>),
}

/// The waits of the reader: the beat and the shutdown request.
struct Waits<'a> {
    heartbeat: &'a Heartbeat,
    tick: &'a mut Interval,
    drain: &'a mut oneshot::Receiver<Duration>,
}

impl Waits<'_> {
    /// Waits `wait` and beats. Returns false when the shutdown asks for
    /// the end: no socket exists, so nothing is left to drain.
    async fn pause(&mut self, wait: Duration) -> bool {
        let pause = sleep(wait);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                () = &mut pause => return true,
                _ = self.tick.tick() => self.heartbeat.beat(),
                _ = &mut *self.drain => return false,
            }
        }
    }
}

impl Reader {
    /// Runs the reader until `drain` gives the bound of the drain, then
    /// drains the socket and ends. A dropped sender ends the reader without
    /// a drain.
    pub async fn run(mut self, heartbeat: Heartbeat, mut drain: oneshot::Receiver<Duration>) {
        let mut tick = tokio::time::interval(BEAT_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut waits = Waits {
            heartbeat: &heartbeat,
            tick: &mut tick,
            drain: &mut drain,
        };
        let mut state = Loop {
            settle: Settle::default(),
            drops: CountThrottle::new(self.limits.drop_event_interval),
            generation_stops: None,
            backoff: Backoff::new(
                self.limits.rebuild_backoff_first,
                self.limits.rebuild_backoff_max,
            ),
            stall_backoff: Backoff::new(
                self.limits.stall_backoff_first,
                self.limits.stall_backoff_max,
            ),
            gate_open: true,
            stalled_open: true,
        };
        loop {
            let Some(mut live) = self.connect(&mut waits, &mut state).await else {
                break;
            };
            match self.serve(&mut live, &mut waits, &mut state).await {
                Served::Shutdown(Some(bound)) => {
                    self.drain(&mut live, bound, &mut waits, &mut state).await;
                    break;
                }
                Served::Shutdown(None) => break,
                Served::Rebuild(reason) => {
                    if !self.replace(live, reason, &mut waits, &mut state).await {
                        break;
                    }
                }
            }
        }
        if let Some(window) = state.drops.take() {
            self.record_drops(window);
        }
        heartbeat.retire();
    }

    /// Makes sockets until one is connected and subscribed. Returns `None`
    /// when the shutdown asks for the end.
    async fn connect(&mut self, waits: &mut Waits<'_>, state: &mut Loop) -> Option<Live> {
        loop {
            let stops = self.gate.borrow_and_update().child_stops();
            let mut options = SocketOptions::default();
            options.connect_timeout(self.limits.connect_attempt);
            let mut socket = SubSocket::with_options(options);
            let monitor: Monitor = Box::pin(socket.monitor());
            let connected = {
                let attempt = socket.connect(&self.endpoint);
                tokio::pin!(attempt);
                loop {
                    tokio::select! {
                        result = &mut attempt => break result,
                        _ = waits.tick.tick() => waits.heartbeat.beat(),
                        _ = &mut *waits.drain => return None,
                    }
                }
            };
            if let Err(error) = connected {
                warn!(code = error_code(&error), "subscriber_connect_failed");
                if !waits.pause(CONNECT_PAUSE).await {
                    return None;
                }
                continue;
            }
            if self.gate.borrow_and_update().child_stops() != stops {
                // The peer can be the child that stopped.
                info!(
                    reason = Rebuild::ChildStopped.as_str(),
                    "subscriber_rebuilt"
                );
                continue;
            }
            let subscribed = {
                let subscribe = tokio::time::timeout(
                    self.limits.connect_attempt,
                    socket.subscribe(TOPIC_PREFIX),
                );
                tokio::pin!(subscribe);
                loop {
                    tokio::select! {
                        result = &mut subscribe => break result,
                        _ = waits.tick.tick() => waits.heartbeat.beat(),
                        _ = &mut *waits.drain => return None,
                    }
                }
            };
            let failed = match subscribed {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error_code(&error)),
                Err(_) => Some("timeout"),
            };
            if let Some(code) = failed {
                warn!(code, "subscriber_subscribe_failed");
                info!(
                    reason = Rebuild::SubscribeFailed.as_str(),
                    "subscriber_rebuilt"
                );
                if !waits.pause(state.backoff.take()).await {
                    return None;
                }
                continue;
            }
            let new_child = state.generation_stops != Some(stops);
            if new_child {
                state.backoff.reset();
                state.stall_backoff.reset();
            }
            state.generation_stops = Some(stops);
            let id = self.received.connected(new_child);
            state.settle.connected(Instant::now());
            info!("subscriber_connected");
            return Some(Live {
                socket,
                monitor,
                id,
                stops,
            });
        }
    }

    /// Reads one socket until it must be replaced or the shutdown asks for
    /// the drain.
    async fn serve(&mut self, live: &mut Live, waits: &mut Waits<'_>, state: &mut Loop) -> Served {
        loop {
            waits.heartbeat.beat();
            let settle_due = state.settle.due(self.limits.settle);
            tokio::select! {
                message = live.socket.recv() => {
                    if message.is_ok() {
                        state.backoff.reset();
                        state.stall_backoff.reset();
                    }
                    self.message(message, state);
                }
                event = live.monitor.next() => match event {
                    Some(SocketEvent::Disconnected(..)) => {
                        info!("subscriber_disconnected");
                        return Served::Rebuild(Rebuild::PeerLost);
                    }
                    Some(_) => {}
                    None => {
                        warn!("subscriber_monitor_closed");
                        return Served::Rebuild(Rebuild::PeerLost);
                    }
                },
                () = sleep_until(settle_due.unwrap_or_else(Instant::now)), if settle_due.is_some() => {
                    if state.settle.poll(Instant::now(), self.limits.settle) {
                        info!("subscriber_settled");
                        self.conditions.set_settled(true);
                    }
                }
                changed = self.gate.changed(), if state.gate_open => {
                    if changed.is_err() {
                        state.gate_open = false;
                    } else if self.gate.borrow_and_update().child_stops() != live.stops {
                        return Served::Rebuild(Rebuild::ChildStopped);
                    }
                }
                changed = self.stalled.changed(), if state.stalled_open => {
                    if changed.is_err() {
                        state.stalled_open = false;
                    } else if *self.stalled.borrow_and_update() == Some(live.id) {
                        return Served::Rebuild(Rebuild::Stalled);
                    }
                }
                _ = waits.tick.tick() => {
                    if let Some(window) = state.drops.due(Instant::now()) {
                        self.record_drops(window);
                    }
                }
                bound = &mut *waits.drain => return Served::Shutdown(bound.ok()),
            }
        }
    }

    /// Replaces a socket in the order of the module documentation. Returns
    /// false when the shutdown asked for the end during the replacement.
    async fn replace(
        &mut self,
        mut live: Live,
        reason: Rebuild,
        waits: &mut Waits<'_>,
        state: &mut Loop,
    ) -> bool {
        info!(reason = reason.as_str(), "subscriber_rebuilt");
        state.settle.lost();
        if reason == Rebuild::Stalled {
            self.conditions.set_stalled();
        } else {
            self.conditions.set_settled(false);
        }
        if reason == Rebuild::ChildStopped {
            state.backoff.reset();
            state.stall_backoff.reset();
        }
        if reason.drains() {
            let mut end = Drain::new(
                Instant::now(),
                self.limits.rebuild_quiet,
                self.limits.rebuild_drain,
            );
            loop {
                tokio::select! {
                    message = live.socket.recv() => {
                        end.message(Instant::now());
                        self.message(message, state);
                    }
                    event = live.monitor.next() => {
                        if matches!(event, Some(SocketEvent::Disconnected(..)) | None) {
                            break;
                        }
                    }
                    () = sleep_until(end.end()) => break,
                    _ = waits.tick.tick() => waits.heartbeat.beat(),
                    bound = &mut *waits.drain => {
                        if let Ok(bound) = bound {
                            self.drain(&mut live, bound, waits, state).await;
                        }
                        return false;
                    }
                }
            }
        }
        let stops = live.stops;
        drop(live);
        let wait = if reason == Rebuild::Stalled {
            state.stall_backoff.take()
        } else if reason.backs_off() {
            state.backoff.take()
        } else {
            Duration::ZERO
        };
        // Gate condition 2 stays false during the wait. A child stop ends
        // the wait: the next socket is the first one to a new child.
        let pause = sleep(wait);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                () = &mut pause => return true,
                _ = waits.tick.tick() => waits.heartbeat.beat(),
                changed = self.gate.changed(), if state.gate_open => {
                    if changed.is_err() {
                        state.gate_open = false;
                    } else if self.gate.borrow_and_update().child_stops() != stops {
                        state.backoff.reset();
                        state.stall_backoff.reset();
                        return true;
                    }
                }
                _ = &mut *waits.drain => return false,
            }
        }
    }

    /// The drain of the shutdown (module documentation).
    async fn drain(
        &self,
        live: &mut Live,
        bound: Duration,
        waits: &mut Waits<'_>,
        state: &mut Loop,
    ) {
        let mut end = Drain::new(Instant::now(), self.limits.drain_quiet, bound);
        let mut drained = 0_u64;
        loop {
            tokio::select! {
                message = live.socket.recv() => {
                    end.message(Instant::now());
                    drained += 1;
                    self.message(message, state);
                }
                () = sleep_until(end.end()) => break,
                _ = waits.tick.tick() => waits.heartbeat.beat(),
            }
        }
        info!(count = drained, "subscriber_drained");
    }

    fn message(&self, message: Result<ZmqMessage, ZmqError>, state: &mut Loop) {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                // A broken peer: the crate removes it, and the socket
                // reports `Disconnected`.
                warn!(code = error_code(&error), "subscriber_receive_failed");
                return;
            }
        };
        let received_at = Timestamp::now();
        let (topic, payload) = frames(message);
        self.received.count(&topic);
        let decoded = decode(&topic, &payload, received_at);
        if !self.queue.push(Item::new(topic, payload, decoded))
            && let Some(window) = state.drops.count(Instant::now(), received_at)
        {
            self.record_drops(window);
        }
    }

    fn record_drops(&self, window: crate::events::CountWindow) {
        warn!(count = window.count, "records_dropped");
        self.sink
            .record(IngestEvent::now(EventKind::RecordsDropped {
                window,
                reason: DROPPED_QUEUE_FULL,
            }));
    }
}

#[cfg(test)]
mod tests;
