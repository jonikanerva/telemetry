//! The drive rules with hand-written expected values.

use super::*;
use crate::battery::RangeKey;

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

const NOW_TEXT: &str = "2026-11-27T12:00:00Z";

fn window() -> Window {
    Window::ending_at(RangeKey::Week, at("2026-11-27T12:00:00Z")).unwrap()
}

fn shift(value_type: i16, int_value: Option<i64>) -> RawValue<'static> {
    RawValue {
        value_type,
        int_value,
        float_value: None,
        text_value: None,
        bool_value: if value_type == 10 { Some(true) } else { None },
    }
}

const P: i64 = 2;
const D: i64 = 5;
const R: i64 = 3;

/// The readings of `(time, ShiftState)` rows through the gear rule.
fn readings(rows: &[(&str, i64)]) -> Vec<(Timestamp, Gear)> {
    rows.iter()
        .filter_map(|(t, v)| gear(&shift(9, Some(*v))).map(|g| (at(t), g)))
        .collect()
}

fn input(before: Before, rows: &[(&str, i64)]) -> GearInput {
    GearInput {
        before,
        readings: readings(rows),
        truncated: false,
    }
}

fn s(field: ValueField, t: &str, value: f64) -> DriveSample {
    DriveSample {
        field,
        at: at(t),
        value,
    }
}

use ValueField::{EnergyRemaining as E, Odometer as O, OutsideTemp as T};

fn view(gears: GearInput, samples: Vec<DriveSample>) -> View {
    derive_view(&Snapshot {
        window: window(),
        vehicles: VehicleCount::One(1),
        gears,
        samples: SampleInput {
            samples,
            cut_at: None,
        },
        ..empty_snapshot()
    })
    .unwrap()
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        window: window(),
        vehicles: VehicleCount::One(1),
        gears: gear_input(None, vec![], MAX_GEAR_ROWS),
        samples: SampleInput::default(),
        tiles: PerField::default(),
        connectivity: None,
        events: EventSnapshot::default(),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_gear_rule_reads_only_shift_state_p_r_n_d() {
    assert_eq!(gear(&shift(9, Some(2))), Some(Gear::Park));
    for value in [3, 4, 5] {
        assert_eq!(gear(&shift(9, Some(value))), Some(Gear::Moving));
    }
    // Unknown, Invalid, SNA, an unknown number, the invalid marker, and an
    // integer of another variant are no reading.
    for raw in [
        shift(9, Some(0)),
        shift(9, Some(1)),
        shift(9, Some(6)),
        shift(9, Some(7)),
        shift(10, None),
        shift(2, Some(2)),
    ] {
        assert_eq!(gear(&raw), None, "{raw:?}");
    }
}

#[test]
fn a_normal_drive_has_every_value() {
    // 10:00 D, 10:30 P. Odometer 1000.0 to 1012.5 mi (20.1168 km),
    // energy 60.0 to 56.0 kWh (4 kWh), temperatures -2 and 0 (mean -1).
    let v = view(
        input(
            Before::Reading(Gear::Park),
            &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:30:00Z", P)],
        ),
        vec![
            s(O, "2026-11-26T10:00:00Z", 1000.0),
            s(O, "2026-11-26T10:15:00Z", 1006.0),
            s(O, "2026-11-26T10:30:00Z", 1012.5),
            s(E, "2026-11-26T10:00:20Z", 60.0),
            s(E, "2026-11-26T10:29:40Z", 56.0),
            s(T, "2026-11-26T10:05:00Z", -2.0),
            s(T, "2026-11-26T10:10:00Z", 0.0),
            // Outside the drive: not used.
            s(O, "2026-11-26T10:30:01Z", 2000.0),
            s(T, "2026-11-26T09:59:59Z", 30.0),
        ],
    );
    let [drive] = v.drives.as_slice() else {
        panic!("{v:?}")
    };
    assert_eq!(drive.start, at("2026-11-26T10:00:00Z"));
    assert_eq!(drive.end, Some(at("2026-11-26T10:30:00Z")));
    assert_eq!(drive.duration, Some(SignedDuration::from_mins(30)));
    assert_eq!(drive.distance_mi, Some(12.5));
    assert_eq!(drive.energy_kwh, Some(4.0));
    // 4000 Wh / 20.1168 km = 198.8388...
    assert!(close(drive.wh_per_km.unwrap(), 4000.0 / 20.1168));
    assert!((drive.wh_per_km.unwrap() - 198.839).abs() < 0.001);
    assert_eq!(drive.mean_outside_temp_c, Some(-1.0));
    // 20.1168 km in 0.5 h.
    assert!(close(drive.mean_speed_kmh.unwrap(), 40.2336));
    assert_eq!(v.totals.drives, 1);
    assert_eq!(v.totals.distance_mi, Some(12.5));
    assert_eq!(v.totals.energy_kwh, Some(4.0));
    assert!(close(v.totals.wh_per_km.unwrap(), 4000.0 / 20.1168));
    assert_eq!(v.totals.without_consumption, 0);
    assert!(!v.truncated);
}

#[test]
fn two_drives_are_newest_first_and_the_totals_weight_by_distance() {
    // Drive 1: 10 mi, 3 kWh. Drive 2: 30 mi, 6 kWh. R inside drive 2 does
    // not split it. Total Wh/km: 9000 Wh / (40 * 1.609344 km).
    let v = view(
        input(
            Before::Nothing,
            &[
                ("2026-11-25T08:00:00Z", D),
                ("2026-11-25T08:20:00Z", P),
                ("2026-11-25T17:00:00Z", R),
                ("2026-11-25T17:00:10Z", D),
                ("2026-11-25T17:45:00Z", P),
            ],
        ),
        vec![
            s(O, "2026-11-25T08:00:30Z", 100.0),
            s(O, "2026-11-25T08:19:30Z", 110.0),
            s(E, "2026-11-25T08:00:30Z", 70.0),
            s(E, "2026-11-25T08:19:30Z", 67.0),
            s(O, "2026-11-25T17:00:30Z", 110.0),
            s(O, "2026-11-25T17:44:30Z", 140.0),
            s(E, "2026-11-25T17:00:30Z", 67.0),
            s(E, "2026-11-25T17:44:30Z", 61.0),
        ],
    );
    let starts: Vec<_> = v.drives.iter().map(|d| d.start).collect();
    assert_eq!(
        starts,
        [at("2026-11-25T17:00:00Z"), at("2026-11-25T08:00:00Z")]
    );
    assert_eq!(v.totals.drives, 2);
    assert_eq!(v.totals.distance_mi, Some(40.0));
    assert_eq!(v.totals.energy_kwh, Some(9.0));
    assert!(close(
        v.totals.wh_per_km.unwrap(),
        9000.0 / (40.0 * MILE_IN_KM)
    ));
}

#[test]
fn an_ongoing_drive_has_no_totals() {
    let v = view(
        input(Before::Reading(Gear::Park), &[("2026-11-27T11:50:00Z", D)]),
        vec![],
    );
    let [drive] = v.drives.as_slice() else {
        panic!("{v:?}")
    };
    assert_eq!(drive.end, None);
    assert_eq!(drive.duration, None);
    assert_eq!(drive.distance_mi, None);
    assert_eq!(v.totals.drives, 0);
    assert_eq!(v.totals.distance_mi, None);
    assert_eq!(v.totals.wh_per_km, None);
    assert_eq!(v.totals.without_consumption, 0);
}

#[test]
fn a_drive_without_odometer_has_unknown_distance_never_zero() {
    // One Odometer sample only: unknown. Energy is still known.
    let v = view(
        input(
            Before::Nothing,
            &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:30:00Z", P)],
        ),
        vec![
            s(O, "2026-11-26T10:10:00Z", 1000.0),
            s(E, "2026-11-26T10:00:00Z", 60.0),
            s(E, "2026-11-26T10:30:00Z", 58.5),
        ],
    );
    let drive = v.drives[0];
    assert_eq!(drive.distance_mi, None);
    assert_eq!(drive.wh_per_km, None);
    assert_eq!(drive.mean_speed_kmh, None);
    assert_eq!(drive.mean_outside_temp_c, None);
    assert_eq!(drive.energy_kwh, Some(1.5));
    // The totals cover the drives with a Wh/km value only.
    assert_eq!(v.totals.distance_mi, None);
    assert_eq!(v.totals.energy_kwh, None);
    assert_eq!(v.totals.wh_per_km, None);
    assert_eq!(v.totals.without_consumption, 1);
}

#[test]
fn invalid_gear_readings_neither_start_nor_end_a_drive() {
    // D, then Invalid (1), SNA (6), and the invalid marker, then P: one
    // drive from the D to the P.
    let rows: Vec<(Timestamp, RawValue<'static>)> = vec![
        (at("2026-11-26T10:00:00Z"), shift(9, Some(D))),
        (at("2026-11-26T10:05:00Z"), shift(9, Some(1))),
        (at("2026-11-26T10:06:00Z"), shift(10, None)),
        (at("2026-11-26T10:07:00Z"), shift(9, Some(6))),
        (at("2026-11-26T10:20:00Z"), shift(9, Some(P))),
        // An invalid reading after the P starts nothing.
        (at("2026-11-26T11:00:00Z"), shift(10, None)),
    ];
    let readings: Vec<_> = rows
        .iter()
        .rev()
        .filter_map(|(t, raw)| gear(raw).map(|g| (*t, g)))
        .collect();
    let gears = gear_input(Some(Gear::Park), readings, MAX_GEAR_ROWS);
    assert_eq!(
        periods(&gears),
        [Period {
            start: at("2026-11-26T10:00:00Z"),
            end: Some(at("2026-11-26T10:20:00Z")),
        }]
    );
}

#[test]
fn a_drive_across_the_range_start_is_left_out() {
    // The newest reading before the range is D: the first P in the range
    // ends that drive, and only the next drive counts.
    let gears = input(
        Before::Reading(Gear::Moving),
        &[
            ("2026-11-20T12:10:00Z", D),
            ("2026-11-20T12:20:00Z", P),
            ("2026-11-21T09:00:00Z", D),
            ("2026-11-21T09:30:00Z", P),
        ],
    );
    assert_eq!(
        periods(&gears),
        [Period {
            start: at("2026-11-21T09:00:00Z"),
            end: Some(at("2026-11-21T09:30:00Z")),
        }]
    );
    // Without any earlier reading, the first D starts a drive.
    let gears = input(Before::Nothing, &[("2026-11-20T12:10:00Z", D)]);
    assert_eq!(periods(&gears).len(), 1);
}

#[test]
fn zero_distance_has_a_distance_but_no_wh_per_km() {
    let v = view(
        input(
            Before::Nothing,
            &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:02:00Z", P)],
        ),
        vec![
            s(O, "2026-11-26T10:00:00Z", 500.0),
            s(O, "2026-11-26T10:02:00Z", 500.0),
            s(E, "2026-11-26T10:00:00Z", 60.0),
            s(E, "2026-11-26T10:02:00Z", 59.9),
        ],
    );
    let drive = v.drives[0];
    assert_eq!(drive.distance_mi, Some(0.0));
    assert_eq!(drive.mean_speed_kmh, Some(0.0));
    assert_eq!(drive.wh_per_km, None);
    assert_eq!(v.totals.without_consumption, 1);
}

#[test]
fn the_distance_threshold_is_one_kilometre() {
    // 0.62 mi = 0.99779 km: no Wh/km. 0.63 mi = 1.01389 km: Wh/km.
    for (miles, has_value) in [(0.62, false), (0.63, true)] {
        let v = view(
            input(
                Before::Nothing,
                &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:05:00Z", P)],
            ),
            vec![
                s(O, "2026-11-26T10:00:00Z", 10.0),
                s(O, "2026-11-26T10:05:00Z", 10.0 + miles),
                s(E, "2026-11-26T10:00:00Z", 60.0),
                s(E, "2026-11-26T10:05:00Z", 59.8),
            ],
        );
        assert_eq!(v.drives[0].wh_per_km.is_some(), has_value, "{miles}");
    }
}

#[test]
fn negative_energy_stays_negative() {
    // Downhill: EnergyRemaining rises from 50.0 to 50.5 kWh over 2 mi.
    let v = view(
        input(
            Before::Nothing,
            &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:05:00Z", P)],
        ),
        vec![
            s(O, "2026-11-26T10:00:00Z", 10.0),
            s(O, "2026-11-26T10:05:00Z", 12.0),
            s(E, "2026-11-26T10:00:00Z", 50.0),
            s(E, "2026-11-26T10:05:00Z", 50.5),
        ],
    );
    let drive = v.drives[0];
    assert_eq!(drive.energy_kwh, Some(-0.5));
    assert!(close(drive.wh_per_km.unwrap(), -500.0 / (2.0 * MILE_IN_KM)));
    assert!(v.totals.wh_per_km.unwrap() < 0.0);
}

#[test]
fn a_cut_gear_read_leaves_out_the_drive_before_the_first_read_p() {
    // Newest first, limit 3: the oldest reading (the D at 08:00) is cut.
    // The D at 09:00 follows no read P, so its drive is left out; the
    // drive after the P at 09:30 counts.
    let newest_first = readings(&[
        ("2026-11-26T10:30:00Z", P),
        ("2026-11-26T10:00:00Z", D),
        ("2026-11-26T09:30:00Z", P),
        ("2026-11-26T08:00:00Z", D),
    ]);
    let gears = gear_input(Some(Gear::Park), newest_first.clone(), 3);
    assert!(gears.truncated);
    assert_eq!(gears.before, Before::Cut);
    assert_eq!(
        periods(&gears),
        [Period {
            start: at("2026-11-26T10:00:00Z"),
            end: Some(at("2026-11-26T10:30:00Z")),
        }]
    );
    let v = view(gears, vec![]);
    assert!(v.truncated);
    // Under the limit nothing is cut.
    let gears = gear_input(Some(Gear::Park), newest_first, 4);
    assert!(!gears.truncated);
    assert_eq!(gears.before, Before::Reading(Gear::Park));
}

#[test]
fn a_cut_value_read_leaves_out_the_drives_that_can_miss_rows() {
    let gears = input(
        Before::Nothing,
        &[
            ("2026-11-26T08:00:00Z", D),
            ("2026-11-26T08:30:00Z", P),
            ("2026-11-26T10:00:00Z", D),
            ("2026-11-26T10:30:00Z", P),
        ],
    );
    let newest_first: Vec<(Timestamp, Option<DriveSample>)> = [
        s(O, "2026-11-26T10:30:00Z", 20.0),
        s(O, "2026-11-26T10:00:00Z", 10.0),
        s(O, "2026-11-26T08:30:00Z", 9.0),
        s(O, "2026-11-26T08:00:00Z", 1.0),
    ]
    .into_iter()
    .map(|sample| (sample.at, Some(sample)))
    .collect();
    let samples = sample_input(newest_first, 3);
    assert_eq!(samples.cut_at, Some(at("2026-11-26T08:30:00Z")));
    let v = derive_view(&Snapshot {
        window: window(),
        vehicles: VehicleCount::One(1),
        gears,
        samples,
        ..empty_snapshot()
    })
    .unwrap();
    assert!(v.truncated);
    let starts: Vec<_> = v.drives.iter().map(|d| d.start).collect();
    assert_eq!(starts, [at("2026-11-26T10:00:00Z")]);
    assert_eq!(v.drives[0].distance_mi, Some(10.0));
}

#[test]
fn a_value_row_that_is_not_usable_is_no_sample() {
    let raw = RawValue {
        value_type: 10,
        int_value: None,
        float_value: None,
        text_value: None,
        bool_value: Some(true),
    };
    assert_eq!(sample(O, at("2026-11-26T10:00:00Z"), &raw), None);
    let raw = RawValue {
        value_type: 5,
        int_value: None,
        float_value: Some(12.5),
        text_value: None,
        bool_value: None,
    };
    assert_eq!(
        sample(O, at("2026-11-26T10:00:00Z"), &raw),
        Some(s(O, "2026-11-26T10:00:00Z", 12.5))
    );
}

#[test]
fn no_vehicle_gives_no_drives_and_two_vehicles_fail() {
    let empty = Snapshot {
        vehicles: VehicleCount::None,
        ..empty_snapshot()
    };
    let v = derive_view(&empty).unwrap();
    assert_eq!(v.drives, Vec::new());
    assert_eq!(v.totals.drives, 0);
    let two = Snapshot {
        vehicles: VehicleCount::Many,
        ..empty
    };
    assert_eq!(derive_view(&two), Err(ContractViolation::SecondVehicle));
}

#[test]
fn the_limits_cover_a_year_at_the_configured_intervals() {
    // Odometer and EnergyRemaining every 30 s, OutsideTemp every 300 s,
    // one hour of driving each day.
    const ROWS_PER_YEAR: usize = (3600 / 30 * 2 + 3600 / 300) * 365;
    const _: () = assert!(ROWS_PER_YEAR < MAX_DRIVE_SAMPLES);
    const _: () = assert!(50 * 365 < MAX_GEAR_ROWS);
    assert_eq!(ROWS_PER_YEAR, 91_980);
}

#[test]
fn the_start_value_is_the_newest_sample_at_or_before_the_start() {
    // Odometer is sent on change: 100.0 at 09:40 (before the D at 10:00),
    // 110.0 at 10:25, nothing after; the P is at 10:30. EnergyRemaining
    // 70.0 at 08:59 is older than the lookback of one hour, so the start
    // value is the first sample inside the drive, 69.0 at 10:01.
    let v = view(
        input(
            Before::Nothing,
            &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:30:00Z", P)],
        ),
        vec![
            s(O, "2026-11-26T08:00:00Z", 90.0),
            s(O, "2026-11-26T09:40:00Z", 100.0),
            s(O, "2026-11-26T10:25:00Z", 110.0),
            s(E, "2026-11-26T08:59:59Z", 70.0),
            s(E, "2026-11-26T10:01:00Z", 69.0),
            s(E, "2026-11-26T10:29:00Z", 66.0),
            // Before the lookback and after the end: not used.
            s(T, "2026-11-26T09:59:00Z", 20.0),
        ],
    );
    let drive = v.drives[0];
    assert_eq!(drive.distance_mi, Some(10.0));
    assert_eq!(drive.energy_kwh, Some(3.0));
    assert_eq!(drive.mean_outside_temp_c, None);
}

#[test]
fn a_sample_before_the_start_and_no_change_is_zero_not_unknown() {
    // The newest sample before the start is the end value too: no change.
    let v = view(
        input(
            Before::Nothing,
            &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:01:00Z", P)],
        ),
        vec![s(O, "2026-11-26T09:30:00Z", 500.0)],
    );
    assert_eq!(v.drives[0].distance_mi, Some(0.0));
}

fn loss(from: &str, to: &str) -> Gap {
    use crate::battery::gaps::{CountUnit, GapCount, GapKind};
    Gap {
        kind: GapKind::QueueFull,
        from: at(from),
        to: at(to),
        count: GapCount {
            value: 1,
            of: CountUnit::Records,
        },
    }
}

#[test]
fn a_loss_marks_a_drive_when_it_meets_the_span_ends_included() {
    let period = Period {
        start: at("2026-11-26T10:00:00Z"),
        end: Some(at("2026-11-26T10:30:00Z")),
    };
    let to = at(NOW_TEXT);
    for (from, until, marked) in [
        // Ends exactly at the start.
        ("2026-11-26T09:00:00Z", "2026-11-26T10:00:00Z", true),
        // Starts exactly at the end.
        ("2026-11-26T10:30:00Z", "2026-11-26T11:00:00Z", true),
        // Covers the drive.
        ("2026-11-26T09:00:00Z", "2026-11-26T11:00:00Z", true),
        // Inside the drive.
        ("2026-11-26T10:10:00Z", "2026-11-26T10:11:00Z", true),
        // Just before and just after.
        ("2026-11-26T09:00:00Z", "2026-11-26T09:59:59.999999Z", false),
        ("2026-11-26T10:30:00.000001Z", "2026-11-26T11:00:00Z", false),
    ] {
        assert_eq!(
            meets_loss(period, to, &[loss(from, until)]),
            marked,
            "{from} {until}"
        );
    }
    // An ongoing drive spans to the window end.
    let ongoing = Period {
        start: period.start,
        end: None,
    };
    assert!(meets_loss(
        ongoing,
        to,
        &[loss("2026-11-27T11:00:00Z", "2026-11-27T11:01:00Z")]
    ));
}

#[test]
fn a_marked_drive_keeps_its_values_and_the_totals_count_it() {
    use crate::battery::gaps::{EventKind, EventRow};
    let events = EventSnapshot {
        losses: Limited {
            items: vec![EventRow {
                run_id: 1,
                occurred_at: at("2026-11-26T10:20:00Z"),
                kind: EventKind::RecordsDropped,
                reason: Some("queue_full".to_owned()),
                dropped_count: Some(3),
                first_received_at: Some(at("2026-11-26T10:10:00Z")),
                last_received_at: Some(at("2026-11-26T10:12:00Z")),
            }],
            truncated: false,
        },
        restarts: Limited::default(),
    };
    let v = derive_view(&Snapshot {
        gears: input(
            Before::Nothing,
            &[("2026-11-26T10:00:00Z", D), ("2026-11-26T10:30:00Z", P)],
        ),
        samples: SampleInput {
            samples: vec![
                s(O, "2026-11-26T10:00:00Z", 10.0),
                s(O, "2026-11-26T10:30:00Z", 20.0),
                s(E, "2026-11-26T10:00:00Z", 60.0),
                s(E, "2026-11-26T10:30:00Z", 57.0),
            ],
            cut_at: None,
        },
        events,
        ..empty_snapshot()
    })
    .unwrap();
    assert!(v.drives[0].data_loss);
    assert_eq!(v.drives[0].distance_mi, Some(10.0));
    assert_eq!(v.totals.with_data_loss, 1);
    assert_eq!(v.totals.distance_mi, Some(10.0));
    assert_eq!(v.gaps.items.len(), 1);
    // No battery row: no status.
    assert_eq!(v.status, None);
}

#[test]
fn the_sample_windows_share_no_instant() {
    // P at 08:30 ends one drive; the next starts at 08:40, so its lookback
    // reaches back past 08:30. The second window starts just after 08:30.
    let gears = input(
        Before::Nothing,
        &[
            ("2026-11-27T08:00:00Z", D),
            ("2026-11-27T08:30:00Z", P),
            ("2026-11-27T08:40:00Z", D),
            ("2026-11-27T09:00:00Z", P),
            // Exactly one hour later: the lookback starts at the end of
            // the previous drive.
            ("2026-11-27T10:00:00Z", D),
            ("2026-11-27T10:10:00Z", P),
        ],
    );
    assert_eq!(
        sample_windows(&periods(&gears)),
        [
            (at("2026-11-27T07:00:00Z"), at("2026-11-27T08:30:00Z")),
            (
                at("2026-11-27T08:30:00.000001Z"),
                at("2026-11-27T09:00:00Z")
            ),
            (
                at("2026-11-27T09:00:00.000001Z"),
                at("2026-11-27T10:10:00Z")
            ),
        ]
    );
}
