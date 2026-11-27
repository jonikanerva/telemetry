//! The connection fact of the vehicle and the freshness of the data
//! (ADR 0013).
//!
//! The connection is the newest stored `raw.connectivity_event` row, as it
//! is. The view claims no more than "connected at T" or "disconnected at
//! T": a process that ends writes no DISCONNECTED row, so a `connected`
//! fact can be old, and the freshness then shows stale data.

use jiff::{SignedDuration, Timestamp};

/// `status` CONNECTED of `raw.connectivity_event`.
pub const STATUS_CONNECTED: i32 = 1;

/// `status` DISCONNECTED of `raw.connectivity_event`.
pub const STATUS_DISCONNECTED: i32 = 2;

/// Data that the ingest process received this long ago or less is
/// current: the flush interval of 900 s plus 300 s for the write and a
/// margin.
pub const FRESHNESS_LIMIT: SignedDuration = SignedDuration::from_secs(1_200);

/// One `raw.connectivity_event` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectivityRow {
    /// The connection of `fleet-telemetry`.
    pub connection_id: u128,
    /// Server clock.
    pub created_at: Timestamp,
    /// The `ConnectivityEvent` number.
    pub status: i32,
}

/// The reference selection of the newest CONNECTED or DISCONNECTED row
/// before `to`: by `created_at`, then by connection and status (the key of
/// the table). The table does not restrict the status; a row with another
/// status is no connection fact and is not read. The PostgreSQL adapter
/// selects the same row with SQL.
#[must_use]
pub fn newest_connectivity(all: &[ConnectivityRow], to: Timestamp) -> Option<ConnectivityRow> {
    all.iter()
        .copied()
        .filter(|row| {
            row.created_at < to && matches!(row.status, STATUS_CONNECTED | STATUS_DISCONNECTED)
        })
        .max_by_key(|row| (row.created_at, row.connection_id, row.status))
}

/// The connection fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    /// The newest row is CONNECTED.
    Connected {
        /// Its `created_at`.
        at: Timestamp,
    },
    /// The newest row is DISCONNECTED: asleep or offline.
    Disconnected {
        /// Its `created_at`.
        at: Timestamp,
    },
    /// No row is stored.
    None,
}

/// The connection fact of the newest row ([`newest_connectivity`]).
#[must_use]
pub fn connection(newest: Option<ConnectivityRow>) -> Connection {
    match newest {
        None => Connection::None,
        Some(row) if row.status == STATUS_CONNECTED => Connection::Connected { at: row.created_at },
        Some(row) => Connection::Disconnected { at: row.created_at },
    }
}

/// Whether new data is expected and has arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Received within [`FRESHNESS_LIMIT`].
    Current,
    /// Older, and the newest connection fact is `connected`: stale.
    Overdue,
    /// Older, and the newest fact is `disconnected` or no row: the car is
    /// asleep or offline, so no new data is expected.
    NotExpected,
}

/// The freshness at `now` of signal data received at
/// `latest_received_at`.
#[must_use]
pub fn freshness(
    now: Timestamp,
    latest_received_at: Timestamp,
    connection: Connection,
) -> Freshness {
    if now.duration_since(latest_received_at) <= FRESHNESS_LIMIT {
        Freshness::Current
    } else if matches!(connection, Connection::Connected { .. }) {
        Freshness::Overdue
    } else {
        Freshness::NotExpected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn row(connection_id: u128, created_at: &str, status: i32) -> ConnectivityRow {
        ConnectivityRow {
            connection_id,
            created_at: at(created_at),
            status,
        }
    }

    #[test]
    fn the_newest_row_before_the_end_is_the_fact() {
        let end = at("2026-11-27T12:00:00Z");
        assert_eq!(newest_connectivity(&[], end), None);
        let rows = [
            row(1, "2026-11-27T10:00:00Z", STATUS_CONNECTED),
            row(1, "2026-11-27T10:05:00Z", STATUS_DISCONNECTED),
            // At the end: not read.
            row(2, "2026-11-27T12:00:00Z", STATUS_CONNECTED),
            // One instant, two rows: the key decides.
            row(3, "2026-11-27T11:59:59.999999Z", STATUS_CONNECTED),
            row(4, "2026-11-27T11:59:59.999999Z", STATUS_DISCONNECTED),
            // Another status: no connection fact.
            row(9, "2026-11-27T11:59:59.999999Z", 0),
        ];
        assert_eq!(newest_connectivity(&rows, end), Some(rows[4]));
        // One connection with CONNECTED and DISCONNECTED at one instant:
        // the status orders them, so the fact is DISCONNECTED.
        let same = [
            row(5, "2026-11-27T11:00:00Z", STATUS_DISCONNECTED),
            row(5, "2026-11-27T11:00:00Z", STATUS_CONNECTED),
        ];
        assert_eq!(
            connection(newest_connectivity(&same, end)),
            Connection::Disconnected {
                at: at("2026-11-27T11:00:00Z")
            }
        );
        assert_eq!(
            connection(newest_connectivity(&rows, end)),
            Connection::Disconnected {
                at: at("2026-11-27T11:59:59.999999Z")
            }
        );
        assert_eq!(
            connection(newest_connectivity(&rows[..1], end)),
            Connection::Connected {
                at: at("2026-11-27T10:00:00Z")
            }
        );
        assert_eq!(connection(None), Connection::None);
    }

    #[test]
    fn freshness_follows_the_age_and_the_connection() {
        let now = at("2026-11-27T12:00:00Z");
        let connected = Connection::Connected {
            at: at("2026-11-27T11:00:00Z"),
        };
        let disconnected = Connection::Disconnected {
            at: at("2026-11-27T11:00:00Z"),
        };
        let fresh = at("2026-11-27T11:40:00Z");
        let old = at("2026-11-27T11:39:59.999999Z");
        assert_eq!(freshness(now, fresh, connected), Freshness::Current);
        assert_eq!(freshness(now, fresh, disconnected), Freshness::Current);
        assert_eq!(freshness(now, old, connected), Freshness::Overdue);
        assert_eq!(freshness(now, old, disconnected), Freshness::NotExpected);
        assert_eq!(
            freshness(now, old, Connection::None),
            Freshness::NotExpected
        );
    }
}
