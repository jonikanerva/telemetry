//! The bounded memory queue of ADR 0001 ("Delivery path", steps 1 to 3).
//!
//! - **Items.** One item for each ZMQ message: the topic, the payload bytes
//!   as received, the receive time, and the decoded rows. An item stays in
//!   the queue until its rows commit, because the storage can need the
//!   original bytes (ADR 0002, "Unknown data").
//! - **Bound.** The queue holds at most [`Limits::queue_bytes`] (64 MiB),
//!   counted for each item as the topic and payload bytes plus an estimate
//!   of the decoded rows in memory ([`item_bytes`]). The estimate counts
//!   the heap data of each row and the size of the row structures, so it
//!   is close to the real memory use, not a lower bound of it.
//! - **Full queue.** An item that does not fit is not queued: the reader
//!   drops the newest message and counts it (ADR 0001, step 3).
//! - **Gate marks.** At 75 % of the bound the queue closes the gate (gate
//!   condition 4). The gate opens again when the queue holds less than 25 %
//!   and the last write succeeded, or when the queue is empty. The last
//!   25 % takes the records that are in flight when the gate closes.
//! - **Invariant: no permanent close.** The queue holds the gate closed
//!   only while it holds at least one item. While an item waits, the writer
//!   always has a next attempt (a flush at once, because the closed queue
//!   wants a flush, or a retry on the cadence, which never gives up), and a
//!   successful write opens the gate. An empty queue has no next write, so
//!   it opens the gate itself: for example after a failed write that
//!   committed every record, or after the writer removed a refused last
//!   record (ADR 0001, gate condition 4).
//! - **Order.** The reader adds items at the back. Only the writer removes
//!   items, from the front, after their commit. So the front of the queue
//!   does not change while the writer writes a batch, and the lock is never
//!   held across an `await`.
//! - **Wake.** The queue wakes the writer when the first item arrives (the
//!   flush interval starts), when [`Limits::flush_rows`] signal rows wait,
//!   and when the queue closes the gate.
//!
//! The lock order is the queue, then [`Conditions`]: a change of the gate
//! mark is reported inside the queue lock, so a late report cannot undo a
//! newer one.

use std::collections::VecDeque;
use std::mem::size_of;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::Timestamp;

use tokio::sync::Notify;
use tokio::time::Instant;

use super::Limits;
use super::conditions::Conditions;
use crate::record::{Decoded, JsonObject, Signal, SignalValue};
use crate::storage::QueuedRecord;

/// One received ZMQ message and its decode.
pub struct Item {
    /// The first frame: the topic.
    pub topic: Vec<u8>,
    /// The other frames: the payload.
    pub payload: Vec<u8>,
    /// The decode of the frames. It holds the receive time.
    pub decoded: Decoded,
    /// The time when the item was queued, for the flush interval.
    pub queued_at: Instant,
    bytes: usize,
    signal_rows: usize,
}

impl Item {
    /// A new item, queued now.
    #[must_use]
    pub fn new(topic: Vec<u8>, payload: Vec<u8>, decoded: Decoded) -> Self {
        let bytes = item_bytes(&topic, &payload, &decoded);
        let signal_rows = signal_rows(&decoded);
        Self {
            topic,
            payload,
            decoded,
            queued_at: Instant::now(),
            bytes,
            signal_rows,
        }
    }

    /// The bytes that the item counts against the bound.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// The signal rows of the item.
    #[must_use]
    pub const fn signal_rows(&self) -> usize {
        self.signal_rows
    }

    /// The receive time of the message.
    #[must_use]
    pub const fn received_at(&self) -> Timestamp {
        match &self.decoded {
            Decoded::VehicleData(record) => record.received_at,
            Decoded::Alerts(record) => record.received_at,
            Decoded::Errors(record) => record.received_at,
            Decoded::Connectivity(record) => record.received_at,
            Decoded::Unprocessed(record) => record.received_at,
        }
    }

    /// The record for the writer.
    #[must_use]
    pub fn queued(&self) -> QueuedRecord<'_> {
        QueuedRecord {
            topic: &self.topic,
            payload: &self.payload,
            decoded: &self.decoded,
        }
    }
}

/// The number of `raw.vehicle_signal` rows of a decode.
fn signal_rows(decoded: &Decoded) -> usize {
    match decoded {
        Decoded::VehicleData(record) => record.signals.len(),
        _ => 0,
    }
}

/// The memory estimate of one JSON entry beyond its key: the map node and
/// the value.
const JSON_ENTRY_BYTES: usize = 64;

/// The memory estimate of one tag of an error item beyond its key and value:
/// the map node.
const TAG_ENTRY_BYTES: usize = 48;

/// The bytes that one item counts against the bound (module
/// documentation).
#[must_use]
pub fn item_bytes(topic: &[u8], payload: &[u8], decoded: &Decoded) -> usize {
    let rows = match decoded {
        Decoded::VehicleData(record) => {
            record.vin.expose().len()
                + record.signals.len() * size_of::<Signal>()
                + record.signals.iter().map(signal_heap).sum::<usize>()
        }
        Decoded::Alerts(record) => {
            record.vin.expose().len()
                + record
                    .alerts
                    .iter()
                    .map(|alert| {
                        size_of_val(alert)
                            + alert.name.len()
                            + alert.audiences.len() * size_of::<i32>()
                    })
                    .sum::<usize>()
        }
        Decoded::Errors(record) => {
            record.vin.expose().len()
                + record
                    .errors
                    .iter()
                    .map(|error| {
                        size_of_val(error)
                            + error.name.len()
                            + error.body.len()
                            + error
                                .tags
                                .iter()
                                .map(|(key, value)| key.len() + value.len() + TAG_ENTRY_BYTES)
                                .sum::<usize>()
                    })
                    .sum::<usize>()
        }
        Decoded::Connectivity(record) => record.vin.expose().len() + record.network_interface.len(),
        Decoded::Unprocessed(_) => 0,
    };
    size_of::<Item>() + topic.len() + payload.len() + rows
}

fn signal_heap(signal: &Signal) -> usize {
    match &signal.value {
        SignalValue::Text(text) => text.len(),
        SignalValue::Doors(object)
        | SignalValue::Time(object)
        | SignalValue::TireLocation(object) => json_heap(object),
        _ => 0,
    }
}

fn json_heap(object: &JsonObject) -> usize {
    object
        .iter()
        .map(|(key, value)| key.len() + JSON_ENTRY_BYTES + value.as_str().map_or(0, str::len))
        .sum()
}

/// The counts of the queue, for the log and the final report. Numbers
/// only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Queued items (messages).
    pub items: usize,
    /// Bytes against the bound.
    pub bytes: usize,
    /// Signal rows of the queued items.
    pub signal_rows: usize,
}

/// The bounded queue. Share it with [`Arc`].
pub struct Queue {
    inner: Mutex<Inner>,
    capacity: usize,
    high_mark: usize,
    low_mark: usize,
    flush_rows: usize,
    conditions: Arc<Conditions>,
    wake: Arc<Notify>,
}

struct Inner {
    items: VecDeque<Arc<Item>>,
    stats: Stats,
    closed: bool,
    last_write_ok: bool,
}

impl Queue {
    /// An empty queue with the bound and the flush rows of `limits`.
    /// `wake` is the wake of the writer.
    #[must_use]
    pub fn new(limits: &Limits, conditions: Arc<Conditions>, wake: Arc<Notify>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                items: VecDeque::new(),
                stats: Stats::default(),
                closed: false,
                last_write_ok: true,
            }),
            capacity: limits.queue_bytes,
            high_mark: limits.queue_bytes / 4 * 3,
            low_mark: limits.queue_bytes / 4,
            flush_rows: limits.flush_rows,
            conditions,
            wake,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds an item at the back. Returns `false` when the item does not
    /// fit: the queue is full, and the caller drops the item.
    pub fn push(&self, item: Item) -> bool {
        let mut inner = self.lock();
        let bytes = inner.stats.bytes.saturating_add(item.bytes);
        if bytes > self.capacity {
            return false;
        }
        let was_empty = inner.items.is_empty();
        let rows_before = inner.stats.signal_rows;
        inner.stats.bytes = bytes;
        inner.stats.signal_rows += item.signal_rows;
        inner.stats.items += 1;
        inner.items.push_back(Arc::new(item));
        let mut wake = was_empty
            || (rows_before < self.flush_rows && inner.stats.signal_rows >= self.flush_rows);
        if !inner.closed && inner.stats.bytes >= self.high_mark {
            inner.closed = true;
            self.conditions.set_queue_closed(true);
            wake = true;
        }
        drop(inner);
        if wake {
            self.wake.notify_one();
        }
        true
    }

    /// The items at the front for one transaction: at most `max_rows`
    /// signal rows and `max_bytes` bytes of topics and payloads, and always
    /// at least one item when the queue is not empty. The items stay in the
    /// queue until [`Queue::remove`].
    #[must_use]
    pub fn batch(&self, max_rows: usize, max_bytes: usize) -> Vec<Arc<Item>> {
        let inner = self.lock();
        let mut batch = Vec::new();
        let mut rows = 0_usize;
        let mut bytes = 0_usize;
        for item in &inner.items {
            let size = item.topic.len().saturating_add(item.payload.len());
            let full = rows.saturating_add(item.signal_rows) > max_rows
                || bytes.saturating_add(size) > max_bytes;
            if !batch.is_empty() && full {
                break;
            }
            rows = rows.saturating_add(item.signal_rows);
            bytes = bytes.saturating_add(size);
            batch.push(Arc::clone(item));
        }
        batch
    }

    /// Removes the `count` items at the front: their rows are committed,
    /// or they cannot be stored at all.
    pub fn remove(&self, count: usize) {
        let mut inner = self.lock();
        for _ in 0..count {
            let Some(item) = inner.items.pop_front() else {
                break;
            };
            inner.stats.bytes = inner.stats.bytes.saturating_sub(item.bytes);
            inner.stats.signal_rows = inner.stats.signal_rows.saturating_sub(item.signal_rows);
            inner.stats.items = inner.stats.items.saturating_sub(1);
        }
        self.reopen(&mut inner);
    }

    /// Records the result of the last write for the low mark.
    pub fn set_last_write_ok(&self, ok: bool) {
        let mut inner = self.lock();
        inner.last_write_ok = ok;
        self.reopen(&mut inner);
    }

    /// The only place that opens gate condition 4 again (module
    /// documentation, "Gate marks"). An empty queue always opens it: no
    /// item waits, so no write follows that could set `last_write_ok`.
    fn reopen(&self, inner: &mut Inner) {
        let may_open = inner.last_write_ok || inner.items.is_empty();
        if inner.closed && may_open && inner.stats.bytes < self.low_mark {
            inner.closed = false;
            self.conditions.set_queue_closed(false);
        }
    }

    /// The time when the oldest item was queued.
    #[must_use]
    pub fn oldest(&self) -> Option<Instant> {
        self.lock().items.front().map(|item| item.queued_at)
    }

    /// True when a flush is due now, independent of the age: the waiting
    /// signal rows reach [`Limits::flush_rows`], or the queue has closed
    /// the gate.
    #[must_use]
    pub fn wants_flush(&self) -> bool {
        let inner = self.lock();
        inner.closed || inner.stats.signal_rows >= self.flush_rows
    }

    /// The counts.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.lock().stats
    }

    /// True when the queue holds no item.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::Timestamp;

    use super::*;
    use crate::events::recording::RecordingSink;
    use crate::pipeline::conditions::Database;
    use crate::readiness::Readiness;
    use crate::record::{Unprocessed, UnprocessedReason, VehicleData, Vin};

    fn unprocessed() -> Decoded {
        Decoded::Unprocessed(Unprocessed {
            reason: UnprocessedReason::DecodeFailed,
            received_at: Timestamp::UNIX_EPOCH,
        })
    }

    fn vehicle_data(rows: usize) -> Decoded {
        Decoded::VehicleData(VehicleData {
            vin: Vin::new("TESTVIN0000000001".to_owned()),
            created_at: Timestamp::UNIX_EPOCH,
            received_at: Timestamp::UNIX_EPOCH,
            is_resend: false,
            signals: (0..rows)
                .map(|field| Signal {
                    field: i32::try_from(field).unwrap(),
                    value: SignalValue::Int64(1),
                })
                .collect(),
            unknown_value_variants: 0,
            unknown_fields: false,
        })
    }

    /// An item of exactly `bytes` bytes against the bound.
    fn item_of(bytes: usize) -> Item {
        let decoded = unprocessed();
        let empty = item_bytes(b"", b"", &decoded);
        Item::new(Vec::new(), vec![0; bytes - empty], decoded)
    }

    struct Fixture {
        queue: Queue,
        readiness: Arc<Readiness>,
        wake: Arc<Notify>,
    }

    fn fixture(capacity: usize, flush_rows: usize) -> Fixture {
        let sink = Arc::new(RecordingSink::default());
        let readiness = Arc::new(Readiness::new(sink.clone()));
        readiness.set_child_running(true);
        let conditions = Arc::new(Conditions::new(Arc::clone(&readiness), sink));
        conditions.set_settled(true);
        conditions.set_database(Database::Passed);
        let wake = Arc::new(Notify::new());
        let limits = Limits {
            queue_bytes: capacity,
            flush_rows,
            ..Limits::PRODUCTION
        };
        Fixture {
            queue: Queue::new(&limits, conditions, Arc::clone(&wake)),
            readiness,
            wake,
        }
    }

    async fn woken(wake: &Notify) -> bool {
        tokio::time::timeout(Duration::from_millis(1), wake.notified())
            .await
            .is_ok()
    }

    #[test]
    fn an_item_counts_its_frames_and_its_rows() {
        let empty = item_bytes(b"", b"", &unprocessed());
        assert_eq!(empty, size_of::<Item>());
        assert_eq!(item_bytes(b"topic", b"payload", &unprocessed()), empty + 12);
        let rows = item_bytes(b"", b"", &vehicle_data(10));
        assert_eq!(rows, empty + 17 + 10 * size_of::<Signal>());
        assert_eq!(
            Item::new(Vec::new(), Vec::new(), vehicle_data(10)).signal_rows(),
            10
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_queue_closes_the_gate_at_75_percent_and_opens_it_below_25_percent() {
        let fixture = fixture(4000, 5000);
        let queue = &fixture.queue;
        assert!(queue.push(item_of(1000)));
        assert!(queue.push(item_of(1000)));
        assert!(fixture.readiness.state().is_open());
        // 3000 bytes: the high mark.
        assert!(queue.push(item_of(1000)));
        assert_eq!(
            fixture.readiness.state().closed_reason(),
            Some("queue_high_mark")
        );
        assert!(queue.wants_flush());
        // 1000 bytes is not below the low mark.
        queue.remove(2);
        assert!(!fixture.readiness.state().is_open());
        queue.remove(1);
        assert!(fixture.readiness.state().is_open());
        assert!(!queue.wants_flush());
        assert_eq!(queue.stats(), Stats::default());
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_stays_closed_below_the_low_mark_after_a_failed_write() {
        let fixture = fixture(4000, 5000);
        let queue = &fixture.queue;
        for _ in 0..4 {
            assert!(queue.push(item_of(750)));
        }
        queue.set_last_write_ok(false);
        // 750 bytes: below the low mark, but an item waits for the retry.
        queue.remove(3);
        assert!(!fixture.readiness.state().is_open());
        queue.set_last_write_ok(true);
        assert!(fixture.readiness.state().is_open());
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_queue_never_holds_the_gate_closed() {
        // The invariant of the module documentation: after a failed write,
        // no later write comes when the queue is empty, in any order of the
        // two reports.
        for remove_first in [false, true] {
            let fixture = fixture(4000, 5000);
            let queue = &fixture.queue;
            for _ in 0..3 {
                assert!(queue.push(item_of(1000)));
            }
            assert!(!fixture.readiness.state().is_open());
            if remove_first {
                queue.remove(3);
                queue.set_last_write_ok(false);
            } else {
                queue.set_last_write_ok(false);
                queue.remove(3);
            }
            assert!(queue.is_empty());
            assert!(
                fixture.readiness.state().is_open(),
                "remove first: {remove_first}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_refuses_the_newest_item() {
        let fixture = fixture(4000, 5000);
        let queue = &fixture.queue;
        assert!(queue.push(item_of(3500)));
        assert!(!queue.push(item_of(501)));
        assert!(queue.push(item_of(500)));
        assert!(!queue.push(item_of(1024)));
        assert_eq!(queue.stats().bytes, 4000);
        assert_eq!(queue.stats().items, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_queue_wakes_the_writer_for_the_first_item_and_at_the_flush_rows() {
        let fixture = fixture(1 << 20, 10);
        let queue = &fixture.queue;
        let first = Instant::now();
        assert!(queue.push(Item::new(Vec::new(), Vec::new(), vehicle_data(4))));
        assert!(woken(&fixture.wake).await);
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(queue.push(Item::new(Vec::new(), Vec::new(), vehicle_data(4))));
        assert!(!woken(&fixture.wake).await);
        assert!(!queue.wants_flush());
        assert_eq!(queue.oldest(), Some(first));
        // 12 rows: the flush rows are reached.
        assert!(queue.push(Item::new(Vec::new(), Vec::new(), vehicle_data(4))));
        assert!(woken(&fixture.wake).await);
        assert!(queue.wants_flush());
        assert_eq!(queue.stats().signal_rows, 12);
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_holds_at_least_one_item_and_keeps_the_bounds() {
        let fixture = fixture(1 << 20, 5000);
        let queue = &fixture.queue;
        assert!(queue.batch(10, 10).is_empty());
        for rows in [3, 3, 3, 20] {
            assert!(queue.push(Item::new(b"t".to_vec(), b"p".to_vec(), vehicle_data(rows))));
        }
        let sizes = |rows, bytes| queue.batch(rows, bytes).len();
        assert_eq!(sizes(6, 1000), 2);
        assert_eq!(sizes(9, 1000), 3);
        // Two bytes for each item.
        assert_eq!(sizes(1000, 5), 2);
        // A record larger than the bound is a batch of its own.
        queue.remove(3);
        assert_eq!(sizes(5, 1000), 1);
        // The batch does not remove anything.
        assert_eq!(queue.stats().items, 1);
    }
}
