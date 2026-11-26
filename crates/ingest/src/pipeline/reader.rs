//! The ZMQ reader (ADR 0001, "Transport" and "Delivery path", step 1).
//!
//! - **Socket.** One SUB socket of the `zeromq` crate (pure Rust) connects
//!   to `tcp://127.0.0.1:<INGEST_ZMQ_PORT>`, where `fleet-telemetry` binds
//!   its PUB socket, and subscribes to the topic prefix `tesla_telemetry_`.
//!   The monitor and the subscription exist before the connect, so the
//!   first `Connected` event reaches the reader and the subscription goes
//!   to each new peer.
//! - **Connect and reconnect.** The first connect retries by itself while
//!   the port refuses the connection; each attempt has a bound of
//!   [`Limits::connect_attempt`] (5 s), and a new attempt follows at once,
//!   so the reader beats its heartbeat while it waits. After the first
//!   connect, the crate reconnects by itself when the peer goes away: after
//!   100 ms, doubling to at most 30 s, and it sends the subscription again
//!   (`zeromq` 0.7.0, `src/reconnect.rs`). A child that restarts after a
//!   long backoff can therefore wait up to 30 s for the reconnect; the gate
//!   stays closed meanwhile, and the vehicle buffers.
//! - **Gate condition 2.** The SUB socket is settled 250 ms after its
//!   `Connected` event ([`Limits::settle`]), so that the PUB side has
//!   applied the subscription. The condition is cleared when the socket
//!   reports `Disconnected`, and when the child stops running (gate
//!   condition 1 changes to false). The second rule does not wait for the
//!   socket to see the end of the old peer, so a restarted child cannot
//!   open the gate before the new subscription has settled.
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
//! - **Heartbeat.** The reader beats at least once each second, also when
//!   no message arrives (watchdog task `reader`, 30 s).
//! - **Drain.** At the shutdown (ADR 0001, step 2) the reader reads until
//!   no message arrives for [`Limits::drain_quiet`] (1 s), at most for the
//!   bound of the request (3 s). Then it records the rest of the drop
//!   count, retires its heartbeat, and closes the socket.
//!
//! Logs: `subscriber_connected`, `subscriber_settled`,
//! `subscriber_disconnected`, `subscriber_connect_failed` with a static
//! code, and `subscriber_drained` with the count of drained messages. The
//! reader never logs a frame.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use jiff::Timestamp;
use tokio::sync::{oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, sleep, sleep_until};
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

/// Gate condition 2 as a state machine of the `Connected` and
/// `Disconnected` events, gate condition 1, and the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settle {
    connected_at: Option<Instant>,
    settled: bool,
    /// The next connect is the first one to a new child process.
    new_child: bool,
    child_running: bool,
}

impl Settle {
    /// No connection yet. `child_running` is gate condition 1 now.
    #[must_use]
    pub const fn new(child_running: bool) -> Self {
        Self {
            connected_at: None,
            settled: false,
            new_child: true,
            child_running,
        }
    }

    /// The socket connected at `now`. Returns true when this is the first
    /// connect to a new child: the received counts of the shortfall check
    /// start again.
    pub const fn connected(&mut self, now: Instant) -> bool {
        self.connected_at = Some(now);
        self.settled = false;
        let new_child = self.new_child;
        self.new_child = false;
        new_child
    }

    /// Gate condition 1 is now `running`. Returns true when the child
    /// stopped: the socket is not settled any more, and the next connect is
    /// to a new child. The socket can report `Disconnected` later than
    /// the supervisor reports the exit.
    pub const fn child(&mut self, running: bool) -> bool {
        let stopped = self.child_running && !running;
        self.child_running = running;
        if stopped {
            self.lost();
            self.new_child = true;
        }
        stopped
    }

    /// The socket lost its peer, or the child stopped.
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
    /// The gate state, for gate condition 1.
    pub gate: watch::Receiver<GateState>,
}

struct Loop {
    settle: Settle,
    drops: CountThrottle,
}

impl Reader {
    /// Runs the reader until `drain` gives the bound of the drain, then
    /// drains the socket and ends. A dropped sender ends the reader without
    /// a drain.
    pub async fn run(mut self, heartbeat: Heartbeat, mut drain: oneshot::Receiver<Duration>) {
        let mut options = SocketOptions::default();
        options.connect_timeout(self.limits.connect_attempt);
        let mut socket = SubSocket::with_options(options);
        let mut monitor = socket.monitor();
        let mut tick = tokio::time::interval(BEAT_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut state = Loop {
            settle: Settle::new(self.gate.borrow_and_update().child_running()),
            drops: CountThrottle::new(self.limits.drop_event_interval),
        };
        // No peer exists yet: the crate keeps the subscription and sends it
        // to each peer at the connect.
        if let Err(error) = socket.subscribe(TOPIC_PREFIX).await {
            warn!(code = error_code(&error), "subscriber_subscribe_failed");
        }

        // The first connect.
        loop {
            let attempt = socket.connect(&self.endpoint);
            tokio::pin!(attempt);
            let result = loop {
                tokio::select! {
                    result = &mut attempt => break Some(result),
                    _ = tick.tick() => heartbeat.beat(),
                    bound = &mut drain => {
                        // No message can have arrived: nothing to drain.
                        drop(bound);
                        break None;
                    }
                }
            };
            match result {
                None => {
                    heartbeat.retire();
                    return;
                }
                Some(Ok(())) => break,
                Some(Err(error)) => {
                    warn!(code = error_code(&error), "subscriber_connect_failed");
                    // A short pause before the next attempt, so that a fast
                    // failure is not a busy loop.
                    let pause = sleep(Duration::from_millis(100));
                    tokio::pin!(pause);
                    loop {
                        tokio::select! {
                            () = &mut pause => break,
                            _ = tick.tick() => heartbeat.beat(),
                        }
                    }
                }
            }
        }

        let bound = loop {
            heartbeat.beat();
            let settle_due = state.settle.due(self.limits.settle);
            tokio::select! {
                message = socket.recv() => self.message(message, &mut state),
                event = monitor.next() => self.event(event.as_ref(), &mut state),
                () = sleep_until(settle_due.unwrap_or_else(Instant::now)), if settle_due.is_some() => {
                    if state.settle.poll(Instant::now(), self.limits.settle) {
                        info!("subscriber_settled");
                        self.conditions.set_settled(true);
                    }
                }
                changed = self.gate.changed() => {
                    if changed.is_ok() {
                        self.gate_changed(&mut state);
                    }
                }
                _ = tick.tick() => {
                    if let Some(window) = state.drops.due(Instant::now()) {
                        self.record_drops(window);
                    }
                }
                bound = &mut drain => break bound.ok(),
            }
        };

        if let Some(bound) = bound {
            let mut end = Drain::new(Instant::now(), self.limits.drain_quiet, bound);
            let mut drained = 0_u64;
            loop {
                tokio::select! {
                    message = socket.recv() => {
                        end.message(Instant::now());
                        drained += 1;
                        self.message(message, &mut state);
                    }
                    () = sleep_until(end.end()) => break,
                    _ = tick.tick() => heartbeat.beat(),
                }
            }
            info!(count = drained, "subscriber_drained");
        }
        if let Some(window) = state.drops.take() {
            self.record_drops(window);
        }
        heartbeat.retire();
    }

    fn message(&self, message: Result<ZmqMessage, ZmqError>, state: &mut Loop) {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                // A broken peer. The crate removes the peer and reconnects.
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

    fn event(&self, event: Option<&SocketEvent>, state: &mut Loop) {
        match event {
            Some(SocketEvent::Connected(..)) => {
                info!("subscriber_connected");
                if state.settle.connected(Instant::now()) {
                    // The first connect to a new child: its counter of
                    // dispatched records starts at 0.
                    self.received.new_generation();
                }
            }
            Some(SocketEvent::Disconnected(..)) => {
                info!("subscriber_disconnected");
                state.settle.lost();
                self.conditions.set_settled(false);
            }
            Some(_) => {}
            None => {
                // The crate dropped the monitor. This does not occur while
                // the socket lives; the reader then waits for its stop.
                warn!("subscriber_monitor_closed");
            }
        }
    }

    fn gate_changed(&mut self, state: &mut Loop) {
        let running = self.gate.borrow_and_update().child_running();
        if state.settle.child(running) {
            self.conditions.set_settled(false);
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
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_socket_settles_250_ms_after_the_connect() {
        let settle = Limits::PRODUCTION.settle;
        let start = Instant::now();
        let mut state = Settle::new(true);
        assert_eq!(state.due(settle), None);
        assert!(!state.poll(start, settle));
        // The first connect is to a new child.
        assert!(state.connected(start));
        assert_eq!(state.due(settle), Some(start + Duration::from_millis(250)));
        assert!(!state.poll(start + Duration::from_millis(249), settle));
        assert!(state.poll(start + Duration::from_millis(250), settle));
        assert!(state.is_settled());
        assert_eq!(state.due(settle), None);
        // Only the change counts.
        assert!(!state.poll(start + Duration::from_secs(1), settle));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_peer_needs_a_new_connect_and_a_new_settle_time() {
        let settle = Limits::PRODUCTION.settle;
        let start = Instant::now();
        let mut state = Settle::new(true);
        state.connected(start);
        assert!(state.poll(start + settle, settle));
        state.lost();
        assert!(!state.is_settled());
        assert_eq!(state.due(settle), None);
        assert!(!state.poll(start + Duration::from_secs(10), settle));
        // A reconnect to the same child keeps the received counts.
        let again = start + Duration::from_secs(11);
        assert!(!state.connected(again));
        assert!(!state.poll(again + Duration::from_millis(100), settle));
        assert!(state.poll(again + settle, settle));
    }

    #[tokio::test(start_paused = true)]
    async fn a_child_stop_clears_the_settled_socket_before_its_disconnect() {
        let settle = Limits::PRODUCTION.settle;
        let start = Instant::now();
        let mut state = Settle::new(false);
        assert!(!state.child(true), "a start is no stop");
        state.connected(start);
        assert!(state.poll(start + settle, settle));
        // The supervisor reports the exit; the socket has not seen the end
        // of its peer yet.
        assert!(state.child(false));
        assert!(!state.is_settled());
        assert_eq!(state.due(settle), None);
        assert!(!state.child(false), "only the change counts");
        // The restarted child: the next connect starts a new generation and
        // a new settle time.
        assert!(!state.child(true));
        let again = start + Duration::from_secs(2);
        assert!(state.connected(again));
        assert!(!state.poll(again + Duration::from_millis(249), settle));
        assert!(state.poll(again + settle, settle));
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
}
