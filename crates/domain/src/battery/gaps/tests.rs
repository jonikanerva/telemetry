//! Tests of the losses and the restart markers.

use jiff::SignedDuration;

use super::super::range::RangeKey;
use super::*;

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

/// 2026-11-26T12:00:00Z to 2026-11-27T12:00:00Z.
fn window() -> Window {
    Window::ending_at(RangeKey::Day, at("2026-11-27T12:00:00Z")).unwrap()
}

fn tick() -> SignedDuration {
    SignedDuration::from_micros(1)
}

fn event(run_id: RunId, occurred_at: Timestamp, kind: EventKind) -> EventRow {
    EventRow {
        run_id,
        occurred_at,
        kind,
        reason: None,
        dropped_count: None,
        first_received_at: None,
        last_received_at: None,
    }
}

fn drop_row(reason: &str, count: i64, first: Timestamp, last: Timestamp) -> EventRow {
    EventRow {
        reason: Some(reason.to_owned()),
        dropped_count: Some(count),
        first_received_at: Some(first),
        last_received_at: Some(last),
        ..event(1, last + tick(), EventKind::RecordsDropped)
    }
}

#[test]
fn a_loss_is_selected_when_its_interval_meets_the_window() {
    let window = window();
    let (from, to) = (window.from(), window.to());
    let hour = SignedDuration::from_hours(1);
    let rows = [
        // Ends one microsecond before the window: not selected.
        drop_row(REASON_QUEUE_FULL, 1, from - hour, from - tick()),
        // Ends at the window start: selected.
        drop_row(REASON_QUEUE_FULL, 2, from - hour, from),
        // Starts one microsecond before the window end: selected.
        drop_row(REASON_DATABASE_REFUSED, 3, to - tick(), to - tick()),
        // Starts at the window end: not selected.
        drop_row(REASON_QUEUE_FULL, 4, to, to),
    ];
    let counts: Vec<Option<i64>> = select_events(&window, &rows)
        .losses
        .items
        .iter()
        .map(|row| row.dropped_count)
        .collect();
    assert_eq!(counts, [Some(2), Some(3)]);
}

#[test]
fn only_rows_that_state_a_loss_are_gaps() {
    let window = window();
    let first = at("2026-11-27T10:00:00Z");
    let counted = |kind: EventKind, reason: Option<&str>| EventRow {
        reason: reason.map(str::to_owned),
        dropped_count: Some(5),
        first_received_at: Some(first),
        last_received_at: Some(first),
        ..event(1, first + tick(), kind)
    };
    let rows = [
        counted(EventKind::RecordsDropped, Some(REASON_QUEUE_FULL)),
        counted(EventKind::RecordsDropped, Some(REASON_DATABASE_REFUSED)),
        counted(EventKind::ZmqShortfall, None),
        // The vehicle keeps refused data: no recorded loss.
        counted(EventKind::GateFull, None),
        // A reason that this version does not know: not read.
        counted(EventKind::RecordsDropped, Some("new_reason")),
    ];
    let gaps = derive_gaps(&select_events(&window, &rows).losses).unwrap();
    let kinds: Vec<(GapKind, CountUnit)> = gaps
        .items
        .iter()
        .map(|gap| (gap.kind, gap.count.of))
        .collect();
    assert_eq!(
        kinds,
        [
            (GapKind::QueueFull, CountUnit::Records),
            (GapKind::DatabaseRefused, CountUnit::Records),
            (GapKind::ZmqShortfall, CountUnit::Messages),
        ]
    );
    assert_eq!(
        gaps.items[0],
        Gap {
            kind: GapKind::QueueFull,
            from: first,
            to: first,
            count: GapCount {
                value: 5,
                of: CountUnit::Records,
            },
        }
    );
}

#[test]
fn restarts_are_the_started_rows_of_the_window() {
    let window = window();
    let (from, to) = (window.from(), window.to());
    let rows = [
        event(1, from - tick(), EventKind::Started),
        event(2, from, EventKind::Started),
        event(3, to - tick(), EventKind::Started),
        event(4, to, EventKind::Started),
        event(5, from + tick(), EventKind::Stopped),
    ];
    assert_eq!(
        select_events(&window, &rows).restarts,
        Limited {
            items: vec![from, to - tick()],
            truncated: false,
        }
    );
}

#[test]
fn a_limit_cuts_the_list_and_says_so() {
    let window = window();
    let start = at("2026-11-27T00:00:00Z");
    let mut rows: Vec<EventRow> = (0..=MAX_GAPS)
        .map(|index| {
            let first = start + SignedDuration::from_secs(i64::try_from(index).unwrap());
            drop_row(REASON_QUEUE_FULL, 1, first, first)
        })
        .collect();
    rows.extend((0..=MAX_RESTARTS).map(|index| {
        event(
            9,
            start + SignedDuration::from_secs(i64::try_from(index).unwrap()),
            EventKind::Started,
        )
    }));
    let snapshot = select_events(&window, &rows);
    assert_eq!(snapshot.losses.items.len(), 500);
    assert!(snapshot.losses.truncated);
    assert_eq!(snapshot.restarts.items.len(), 500);
    assert!(snapshot.restarts.truncated);
    // The cut keeps the oldest rows, whatever the input order.
    rows.reverse();
    assert_eq!(select_events(&window, &rows), snapshot);
    assert_eq!(snapshot.losses.items[0].first_received_at, Some(start));
    // At the limit: no cut.
    rows.retain(|row| row.first_received_at != Some(start) && row.occurred_at != start);
    let at_limit = select_events(&window, &rows);
    assert_eq!(at_limit.losses.items.len(), 500);
    assert!(!at_limit.losses.truncated);
    assert!(!at_limit.restarts.truncated);
}

#[test]
fn a_row_that_breaks_the_table_contract_fails_the_request() {
    let first = at("2026-11-27T10:00:00Z");
    let good = drop_row(REASON_QUEUE_FULL, 1, first, first);
    let zero = drop_row(REASON_QUEUE_FULL, 0, first, first);
    let reversed = drop_row(REASON_QUEUE_FULL, 1, first + tick(), first);
    let too_large = drop_row(REASON_QUEUE_FULL, i64::from(u32::MAX) + 1, first, first);
    let no_count = EventRow {
        dropped_count: None,
        ..good.clone()
    };
    assert_eq!(gap(&zero), Err(ContractViolation::BadEventRow));
    assert_eq!(gap(&reversed), Err(ContractViolation::BadEventRow));
    assert_eq!(gap(&no_count), Err(ContractViolation::BadEventRow));
    assert_eq!(gap(&too_large), Err(ContractViolation::CountTooLarge));
    // One bad row among good rows fails the whole list: the table
    // constraints forbid such a row, so it is a defect, not data.
    let losses = Limited {
        items: vec![good.clone(), zero, good],
        truncated: false,
    };
    assert_eq!(derive_gaps(&losses), Err(ContractViolation::BadEventRow));
}

#[test]
fn kinds_round_trip_through_their_text() {
    for kind in EventKind::ALL {
        assert_eq!(EventKind::parse(kind.as_str()), Ok(kind));
    }
    assert_eq!(
        EventKind::parse("other"),
        Err(ContractViolation::UnknownEventKind)
    );
}
