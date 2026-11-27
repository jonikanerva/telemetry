//! Tests of the whole derivation, from rows to a view.

#![expect(
    clippy::float_cmp,
    reason = "the tests compare stored values, which the rules must keep exact"
)]

use super::series::RowTimes;
use super::*;

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn window() -> Window {
    Window::ending_at(RangeKey::Week, at("2026-11-27T12:00:00Z")).unwrap()
}

fn float_row(created_at: &str, value: f64) -> SignalRow<'static> {
    SignalRow {
        times: RowTimes {
            created_at: at(created_at),
            received_at: at(created_at),
        },
        value: RawValue {
            value_type: 5,
            int_value: None,
            float_value: Some(value),
            text_value: None,
            bool_value: None,
        },
    }
}

fn invalid_row(created_at: &str) -> SignalRow<'static> {
    SignalRow {
        value: RawValue {
            value_type: 10,
            float_value: None,
            bool_value: Some(true),
            ..float_row(created_at, 0.0).value
        },
        ..float_row(created_at, 0.0)
    }
}

/// A snapshot from rows of each field, through the reference rules.
fn snapshot(
    rows: &PerField<Vec<SignalRow<'static>>>,
    events: &[EventRow],
    connectivity: &[ConnectivityRow],
) -> Snapshot {
    let window = window();
    let mut tiles = PerField::<TileSnapshot>::default();
    for field in Field::ALL {
        *tiles.get_mut(field) = tile_snapshot(&window, rows.get(field));
    }
    let events = select_events(&window, events);
    let connectivity = newest_connectivity(connectivity, window.to());
    Snapshot {
        window,
        vehicles: VehicleCount::One(1),
        tiles,
        series: SeriesPair {
            soc: series_snapshot(&window, &rows.soc).unwrap(),
            nominal_full_pack_energy: series_snapshot(&window, &rows.nominal_full_pack_energy)
                .unwrap(),
        },
        events,
        connectivity,
    }
}

fn available(view: View) -> Available {
    match view {
        View::Available(available) => *available,
        View::NoDataYet { .. } => panic!("expected an available view"),
    }
}

#[test]
fn no_vehicle_and_no_battery_row_are_no_data_yet() {
    let mut empty = snapshot(&PerField::default(), &[], &[]);
    assert_eq!(
        derive_view(&empty),
        Ok(View::NoDataYet { window: window() })
    );
    empty.vehicles = VehicleCount::None;
    assert_eq!(
        derive_view(&empty),
        Ok(View::NoDataYet { window: window() })
    );
    // Connectivity rows alone are no battery data.
    let connected = [ConnectivityRow {
        connection_id: 1,
        created_at: at("2026-11-27T11:00:00Z"),
        status: 1,
    }];
    let only_connectivity = snapshot(&PerField::default(), &[], &connected);
    assert_eq!(
        derive_view(&only_connectivity),
        Ok(View::NoDataYet { window: window() })
    );
}

#[test]
fn a_second_vehicle_fails_the_request() {
    let mut two = snapshot(&PerField::default(), &[], &[]);
    two.vehicles = VehicleCount::Many;
    assert_eq!(derive_view(&two), Err(ContractViolation::SecondVehicle));
}

#[test]
fn a_car_asleep_for_the_whole_range_has_a_carry_in_and_no_points() {
    let rows = PerField {
        soc: vec![float_row("2026-11-10T08:00:00Z", 81.0)],
        ..PerField::default()
    };
    let disconnected = [
        ConnectivityRow {
            connection_id: 1,
            created_at: at("2026-11-10T07:50:00Z"),
            status: 1,
        },
        ConnectivityRow {
            connection_id: 1,
            created_at: at("2026-11-10T08:10:00Z"),
            status: 2,
        },
    ];
    let view = available(derive_view(&snapshot(&rows, &[], &disconnected)).unwrap());
    assert_eq!(view.series.soc.segments, Vec::<Vec<Point>>::new());
    assert_eq!(view.series.soc.carry_in.unwrap().value, 81.0);
    assert_eq!(
        view.connection,
        Connection::Disconnected {
            at: at("2026-11-10T08:10:00Z")
        }
    );
    assert_eq!(view.freshness, Freshness::NotExpected);
    assert_eq!(view.gaps, Limited::default());
    assert!(
        matches!(view.tiles.soc, Reading::Value { sample, newer_unusable_at: None, .. } if sample.value == 81.0)
    );
    assert!(matches!(
        view.tiles.outside_temp,
        Reading::Missing {
            latest_unusable_at: None,
            ..
        }
    ));
}

#[test]
fn the_receive_time_is_that_of_the_signals_and_a_disconnect_means_asleep() {
    // The battery rows are 2 hours old; a DISCONNECTED row came 10
    // minutes ago. The receive time is that of the signals, and no new
    // data is expected.
    let rows = PerField {
        soc: vec![float_row("2026-11-27T10:00:00Z", 70.0)],
        ..PerField::default()
    };
    let connectivity = [
        ConnectivityRow {
            connection_id: 1,
            created_at: at("2026-11-27T09:59:00Z"),
            status: 1,
        },
        ConnectivityRow {
            connection_id: 1,
            created_at: at("2026-11-27T11:50:00Z"),
            status: 2,
        },
    ];
    let view = available(derive_view(&snapshot(&rows, &[], &connectivity)).unwrap());
    assert_eq!(view.latest_received_at, at("2026-11-27T10:00:00Z"));
    assert_eq!(
        view.connection,
        Connection::Disconnected {
            at: at("2026-11-27T11:50:00Z")
        }
    );
    assert_eq!(view.freshness, Freshness::NotExpected);
    // The newest fact `connected`: the same data is stale.
    let view = available(derive_view(&snapshot(&rows, &[], &connectivity[..1])).unwrap());
    assert_eq!(
        view.connection,
        Connection::Connected {
            at: at("2026-11-27T09:59:00Z")
        }
    );
    assert_eq!(view.freshness, Freshness::Overdue);
}

#[test]
fn an_unusable_newest_row_is_visible_and_never_zero() {
    let rows = PerField {
        soc: vec![
            float_row("2026-11-27T10:00:00Z", 50.0),
            invalid_row("2026-11-27T10:01:00Z"),
        ],
        energy_remaining: vec![invalid_row("2026-11-27T10:02:00Z")],
        ..PerField::default()
    };
    let view = available(derive_view(&snapshot(&rows, &[], &[])).unwrap());
    assert!(matches!(
        view.tiles.soc,
        Reading::Value { sample, newer_unusable_at: Some(newer), .. }
            if sample.value == 50.0 && newer == at("2026-11-27T10:01:00Z")
    ));
    assert!(matches!(
        view.tiles.energy_remaining,
        Reading::Missing { latest_unusable_at: Some(latest), .. } if latest == at("2026-11-27T10:02:00Z")
    ));
    assert_eq!(view.series.soc.excluded.invalid_marker, 1);
    assert_eq!(view.latest_data_at, at("2026-11-27T10:02:00Z"));
    // Data from 1h58min ago with no connectivity row: no data expected.
    assert_eq!(view.connection, Connection::None);
    assert_eq!(view.freshness, Freshness::NotExpected);
}

#[test]
fn the_capacity_change_uses_the_carry_in_or_the_first_value() {
    let with_carry_in = PerField {
        nominal_full_pack_energy: vec![
            float_row("2026-11-01T08:00:00Z", 78.5),
            float_row("2026-11-25T08:00:00Z", 78.1),
        ],
        ..PerField::default()
    };
    let view = available(derive_view(&snapshot(&with_carry_in, &[], &[])).unwrap());
    assert!(matches!(
        view.capacity_change,
        CapacityChange::Value { from, to } if from.value == 78.5 && to.value == 78.1
    ));

    let in_range_only = PerField {
        nominal_full_pack_energy: vec![
            float_row("2026-11-24T08:00:00Z", 78.4),
            float_row("2026-11-25T08:00:00Z", 78.1),
        ],
        ..PerField::default()
    };
    let view = available(derive_view(&snapshot(&in_range_only, &[], &[])).unwrap());
    assert!(
        matches!(view.capacity_change, CapacityChange::Value { from, .. } if from.value == 78.4)
    );

    let one = PerField {
        nominal_full_pack_energy: vec![float_row("2026-11-25T08:00:00Z", 78.1)],
        ..PerField::default()
    };
    let view = available(derive_view(&snapshot(&one, &[], &[])).unwrap());
    assert_eq!(view.capacity_change, CapacityChange::NoBaseline);

    let none = PerField {
        soc: vec![float_row("2026-11-25T08:00:00Z", 50.0)],
        ..PerField::default()
    };
    let view = available(derive_view(&snapshot(&none, &[], &[])).unwrap());
    assert_eq!(view.capacity_change, CapacityChange::NoLatest);
}

#[test]
fn a_recorded_loss_splits_the_series_and_restarts_are_markers() {
    let rows = PerField {
        soc: vec![
            float_row("2026-11-27T09:00:00Z", 60.0),
            float_row("2026-11-27T11:00:00Z", 58.0),
        ],
        ..PerField::default()
    };
    let event = |kind: EventKind, occurred: &str| EventRow {
        run_id: 7,
        occurred_at: at(occurred),
        kind,
        reason: None,
        dropped_count: None,
        first_received_at: None,
        last_received_at: None,
    };
    let events = [
        EventRow {
            reason: Some("queue_full".to_owned()),
            dropped_count: Some(12),
            first_received_at: Some(at("2026-11-27T10:00:00Z")),
            last_received_at: Some(at("2026-11-27T10:05:00Z")),
            ..event(EventKind::RecordsDropped, "2026-11-27T10:06:00Z")
        },
        // A refused connection is no recorded loss: the vehicle keeps the
        // data.
        EventRow {
            dropped_count: Some(3),
            first_received_at: Some(at("2026-11-27T10:30:00Z")),
            last_received_at: Some(at("2026-11-27T10:31:00Z")),
            ..event(EventKind::GateFull, "2026-11-27T10:32:00Z")
        },
        event(EventKind::Started, "2026-11-27T10:40:00Z"),
        event(EventKind::Stopped, "2026-11-27T10:39:00Z"),
    ];
    let view = available(derive_view(&snapshot(&rows, &events, &[])).unwrap());
    assert_eq!(view.series.soc.segments.len(), 2);
    assert_eq!(
        view.gaps,
        Limited {
            items: vec![Gap {
                kind: gaps::GapKind::QueueFull,
                from: at("2026-11-27T10:00:00Z"),
                to: at("2026-11-27T10:05:00Z"),
                count: gaps::GapCount {
                    value: 12,
                    of: gaps::CountUnit::Records,
                },
            }],
            truncated: false,
        }
    );
    assert_eq!(
        view.restarts,
        Limited {
            items: vec![at("2026-11-27T10:40:00Z")],
            truncated: false,
        }
    );
}
