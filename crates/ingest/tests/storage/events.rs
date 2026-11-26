//! The insert of `ingest_event` rows from the events of the event sink.

use ingest::events::{ChildExit, CountWindow, EventKind, IngestEvent};
use ingest::storage::{EventOutcome, write_events};
use jiff::Timestamp;
use sqlx::Connection;
use uuid::Uuid;

use crate::support::{BASE, at, count, with_migrated};

fn event(kind: EventKind, second: i64) -> IngestEvent {
    IngestEvent {
        occurred_at: at(BASE + second, 999_999_999),
        kind,
    }
}

fn window(count: u32) -> CountWindow {
    CountWindow {
        count,
        first: at(BASE - 60, 1),
        last: at(BASE, 0),
    }
}

/// One event of each kind of `events::EventKind`.
fn every_kind() -> Vec<IngestEvent> {
    vec![
        event(EventKind::Started, 0),
        event(EventKind::Stopped { reason: "SIGTERM" }, 1),
        event(EventKind::GateOpened, 2),
        event(
            EventKind::GateClosed {
                reason: "child_not_running",
            },
            3,
        ),
        event(EventKind::DegradedMode, 4),
        event(
            EventKind::ChildExited {
                exit: ChildExit::Code(1),
            },
            5,
        ),
        event(EventKind::RecordsDropped(window(3)), 6),
        event(EventKind::ZmqShortfall(window(1)), 7),
        event(EventKind::GateFull(window(u32::MAX)), 8),
    ]
}

#[tokio::test]
async fn events_of_every_kind_are_stored_once() {
    with_migrated(|db| async move {
        let options = db.options.clone();
        let run = Uuid::from_u128(42);
        let events = every_kind();
        // The pipeline writes from a task of its own; the spawn proves that
        // the future is `Send`.
        let first = {
            let (options, events) = (options.clone(), events.clone());
            tokio::spawn(async move { write_events(&options, run, &events, &mut || {}).await })
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(first, EventOutcome { inserted: 9, duplicates: 0, rejected: 0 });
        // A retry of the same events.
        let retry = write_events(&options, run, &events, &mut || {}).await.unwrap();
        assert_eq!(retry, EventOutcome { inserted: 0, duplicates: 9, rejected: 0 });
        // Another run is another key.
        let other = write_events(&options, Uuid::from_u128(43), &events[..1], &mut || {}).await.unwrap();
        assert_eq!(other.inserted, 1);

        let mut conn = db.owner().await;
        assert_eq!(count(&mut conn, "ingest_event").await, 10);
        let reasons: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT kind, reason FROM ingest_event WHERE reason IS NOT NULL ORDER BY kind")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(
            reasons,
            [
                ("child_exited".to_owned(), Some("exit code 1".to_owned())),
                ("gate_closed".to_owned(), Some("child_not_running".to_owned())),
                ("stopped".to_owned(), Some("SIGTERM".to_owned())),
            ]
        );
        let (occurred, dropped, first_at): (jiff_sqlx::Timestamp, i64, jiff_sqlx::Timestamp) = sqlx::query_as(
            "SELECT occurred_at, dropped_count, first_received_at FROM ingest_event WHERE kind = 'gate_full'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(occurred.to_jiff(), at(BASE + 8, 999_999_000), "truncated to microseconds");
        assert_eq!(dropped, i64::from(u32::MAX));
        assert_eq!(first_at.to_jiff(), at(BASE - 60, 0));
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn an_event_that_the_table_rejects_does_not_stop_the_others() {
    with_migrated(|db| async move {
        let options = db.options.clone();
        // `timestamptz` starts at 4713 BC, `jiff::Timestamp` earlier: the
        // insert fails with SQLSTATE 22008, a data error.
        let rejected = IngestEvent {
            occurred_at: Timestamp::MIN,
            kind: EventKind::Started,
        };
        let events = [
            event(EventKind::Started, 0),
            rejected.clone(),
            event(EventKind::GateFull(window(2)), 3),
        ];
        let mut beats = 0;
        let outcome = write_events(&options, Uuid::from_u128(1), &events, &mut || beats += 1)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            EventOutcome {
                inserted: 2,
                duplicates: 0,
                rejected: 1
            }
        );
        // One insert of all events, then one insert for each event.
        assert_eq!(beats, 1 + events.len());
        let single = write_events(&options, Uuid::from_u128(1), &[rejected], &mut || {})
            .await
            .unwrap();
        assert_eq!(single.rejected, 1);
        assert_eq!(
            write_events(&options, Uuid::from_u128(1), &[], &mut || {})
                .await
                .unwrap(),
            EventOutcome::default()
        );
    })
    .await;
}
