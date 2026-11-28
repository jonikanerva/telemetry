//! Samples, buckets, and segments of a series.

use jiff::Timestamp;

use super::ContractViolation;
use super::gaps::Gap;
use super::range::Window;
use super::value::{Exclusion, Extracted, RawValue, extract};

/// One usable value of a field at its vehicle time (`created_at`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Vehicle clock.
    pub at: Timestamp,
    /// A finite value.
    pub value: f64,
}

/// One bucket of usable samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// The time of the newest sample in the bucket.
    pub at: Timestamp,
    /// The value of the newest sample.
    pub last: f64,
    /// The smallest value.
    pub min: f64,
    /// The largest value.
    pub max: f64,
    /// The number of samples, at least 1.
    pub count: u32,
}

impl Point {
    /// A point of one sample.
    #[must_use]
    pub const fn single(sample: Sample) -> Self {
        Self {
            at: sample.at,
            last: sample.value,
            min: sample.value,
            max: sample.value,
            count: 1,
        }
    }
}

/// Counts of rows that are not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExcludedCounts {
    /// Rows with the invalid marker.
    pub invalid_marker: u32,
    /// Rows without a usable number.
    pub not_numeric: u32,
}

/// The time columns of a stored row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowTimes {
    /// Vehicle clock (`created_at`).
    pub created_at: Timestamp,
    /// Ingest clock (`received_at`).
    pub received_at: Timestamp,
}

/// What the tile of one field needs: read before `window.to()`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TileSnapshot {
    /// The newest row, usable or not.
    pub latest_row: Option<RowTimes>,
    /// The newest usable sample.
    pub latest_usable: Option<Sample>,
}

/// What the series of one field needs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesSnapshot {
    /// The newest usable sample before `window.from()`.
    pub carry_in: Option<Sample>,
    /// The oldest usable sample in the window.
    pub first_in_range: Option<Sample>,
    /// The buckets of the window in order of time ([`bucket_points`]).
    pub points: Vec<Point>,
    /// The rows of the window that are not usable.
    pub excluded: ExcludedCounts,
}

/// One stored signal row of one field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalRow<'a> {
    /// The time columns.
    pub times: RowTimes,
    /// The value columns.
    pub value: RawValue<'a>,
}

/// The reference rule of [`TileSnapshot`]: the rows of one field, in any
/// order. The PostgreSQL adapter computes the same values with SQL; a
/// database test compares both.
#[must_use]
pub fn tile_snapshot(window: &Window, rows: &[SignalRow<'_>]) -> TileSnapshot {
    let before_end = rows.iter().filter(|row| row.times.created_at < window.to());
    let latest_row = before_end
        .clone()
        .map(|row| row.times)
        .max_by_key(|t| t.created_at);
    let latest_usable = before_end.filter_map(usable).max_by_key(|sample| sample.at);
    TileSnapshot {
        latest_row,
        latest_usable,
    }
}

/// The reference rule of [`SeriesSnapshot`]: the rows of one field, in
/// any order.
///
/// # Errors
///
/// Returns [`ContractViolation::CountTooLarge`] when a count does not fit
/// in `u32`.
pub fn series_snapshot(
    window: &Window,
    rows: &[SignalRow<'_>],
) -> Result<SeriesSnapshot, ContractViolation> {
    let carry_in = rows
        .iter()
        .filter(|row| row.times.created_at < window.from())
        .filter_map(usable)
        .max_by_key(|sample| sample.at);
    let in_range: Vec<&SignalRow<'_>> = rows
        .iter()
        .filter(|row| window.contains(row.times.created_at))
        .collect();
    let mut invalid_marker: u64 = 0;
    let mut not_numeric: u64 = 0;
    let mut samples = Vec::with_capacity(in_range.len());
    for row in in_range {
        match extract(&row.value) {
            Extracted::Usable(value) => samples.push(Sample {
                at: row.times.created_at,
                value,
            }),
            Extracted::Excluded(Exclusion::InvalidMarker) => invalid_marker += 1,
            Extracted::Excluded(Exclusion::NotNumeric) => not_numeric += 1,
        }
    }
    let first_in_range = samples.iter().copied().min_by_key(|sample| sample.at);
    let points = bucket_points(window, &samples)?;
    Ok(SeriesSnapshot {
        carry_in,
        first_in_range,
        points,
        excluded: ExcludedCounts {
            invalid_marker: count(invalid_marker)?,
            not_numeric: count(not_numeric)?,
        },
    })
}

fn usable(row: &SignalRow<'_>) -> Option<Sample> {
    match extract(&row.value) {
        Extracted::Usable(value) => Some(Sample {
            at: row.times.created_at,
            value,
        }),
        Extracted::Excluded(_) => None,
    }
}

fn count(value: u64) -> Result<u32, ContractViolation> {
    u32::try_from(value).map_err(|_| ContractViolation::CountTooLarge)
}

/// Groups the samples of the window into buckets ([`Window::bucket_start`]).
/// Samples outside the window are ignored. The points are in order of
/// time. Two samples with the same time cannot exist (the primary key of
/// `raw.vehicle_signal`); if they do, the later one in the input wins.
///
/// # Errors
///
/// Returns [`ContractViolation::CountTooLarge`] when a bucket holds more
/// than `u32::MAX` samples.
pub fn bucket_points(window: &Window, samples: &[Sample]) -> Result<Vec<Point>, ContractViolation> {
    let mut sorted: Vec<Sample> = samples
        .iter()
        .copied()
        .filter(|sample| window.contains(sample.at))
        .collect();
    sorted.sort_by_key(|sample| sample.at);
    let mut points: Vec<(Timestamp, Point)> = Vec::new();
    for sample in sorted {
        let start = window.bucket_start(sample.at);
        match points.last_mut() {
            Some((bucket, point)) if *bucket == start => {
                point.at = sample.at;
                point.last = sample.value;
                point.min = point.min.min(sample.value);
                point.max = point.max.max(sample.value);
                point.count = point
                    .count
                    .checked_add(1)
                    .ok_or(ContractViolation::CountTooLarge)?;
            }
            _ => points.push((start, Point::single(sample))),
        }
    }
    Ok(points.into_iter().map(|(_, point)| point).collect())
}

/// True when a gap lies between the times `a` and `b` (`a < b`): the gap
/// interval meets the open interval `(a, b)`.
///
/// The gap is on the ingest clock and the points are on the vehicle
/// clock (ADR 0013, decision 4); the comparison is an approximation.
#[must_use]
pub fn gap_between(gap: &Gap, a: Timestamp, b: Timestamp) -> bool {
    gap.from < b && gap.to > a
}

/// Splits points (in order of time) into segments. A new segment starts
/// only where a gap lies between two neighbouring points. A time without
/// points never splits a segment.
#[must_use]
pub fn segments(points: &[Point], gaps: &[Gap]) -> Vec<Vec<Point>> {
    let mut result: Vec<Vec<Point>> = Vec::new();
    for point in points {
        let split = match result.last().and_then(|segment| segment.last()) {
            Some(previous) => gaps
                .iter()
                .any(|gap| gap_between(gap, previous.at, point.at)),
            None => true,
        };
        if split {
            result.push(vec![*point]);
        } else if let Some(segment) = result.last_mut() {
            segment.push(*point);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;
    use proptest::prelude::*;

    use super::super::gaps::{CountUnit, GapCount, GapKind};
    use super::super::range::RangeKey;
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn a_gap_that_only_touches_a_point_does_not_split() {
        let gap = |from: &str, to: &str| Gap {
            kind: GapKind::QueueFull,
            from: at(from),
            to: at(to),
            count: GapCount {
                value: 1,
                of: CountUnit::Records,
            },
        };
        let a = at("2026-11-27T10:00:00Z");
        let b = at("2026-11-27T10:01:00Z");
        // The open interval (a, b): a gap that ends at a or starts at b
        // only touches it.
        assert!(!gap_between(
            &gap("2026-11-27T09:59:00Z", "2026-11-27T10:00:00Z"),
            a,
            b
        ));
        assert!(!gap_between(
            &gap("2026-11-27T10:01:00Z", "2026-11-27T10:02:00Z"),
            a,
            b
        ));
        assert!(gap_between(
            &gap("2026-11-27T09:59:00Z", "2026-11-27T10:00:00.000001Z"),
            a,
            b
        ));
        assert!(gap_between(
            &gap("2026-11-27T10:00:59.999999Z", "2026-11-27T10:02:00Z"),
            a,
            b
        ));
    }

    fn window() -> Window {
        Window::ending_at(RangeKey::Week, at("2026-11-27T12:00:00Z")).unwrap()
    }

    fn gap(from: Timestamp, to: Timestamp) -> Gap {
        Gap {
            kind: GapKind::QueueFull,
            from,
            to,
            count: GapCount {
                value: 1,
                of: CountUnit::Records,
            },
        }
    }

    fn row(created_at: &str, value_type: i16, float_value: Option<f64>) -> SignalRow<'static> {
        SignalRow {
            times: RowTimes {
                created_at: at(created_at),
                received_at: at(created_at),
            },
            value: RawValue {
                value_type,
                int_value: None,
                float_value,
                text_value: None,
                bool_value: float_value.is_none().then_some(true),
            },
        }
    }

    #[test]
    fn a_bucket_keeps_the_newest_value_and_the_extremes() {
        let samples = [
            Sample {
                at: at("2026-11-27T10:01:00Z"),
                value: 50.0,
            },
            Sample {
                at: at("2026-11-27T10:09:00Z"),
                value: 48.0,
            },
            Sample {
                at: at("2026-11-27T10:05:00Z"),
                value: 55.0,
            },
            Sample {
                at: at("2026-11-27T10:10:00Z"),
                value: 47.0,
            },
        ];
        let points = bucket_points(&window(), &samples).unwrap();
        assert_eq!(
            points,
            [
                Point {
                    at: at("2026-11-27T10:09:00Z"),
                    last: 48.0,
                    min: 48.0,
                    max: 55.0,
                    count: 3
                },
                Point {
                    at: at("2026-11-27T10:10:00Z"),
                    last: 47.0,
                    min: 47.0,
                    max: 47.0,
                    count: 1
                },
            ]
        );
    }

    #[test]
    fn samples_outside_the_window_are_not_in_a_point() {
        let window = window();
        let samples = [
            Sample {
                at: window.from() - SignedDuration::from_micros(1),
                value: 1.0,
            },
            Sample {
                at: window.to(),
                value: 2.0,
            },
            Sample {
                at: window.from(),
                value: 3.0,
            },
        ];
        let points = bucket_points(&window, &samples).unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].last, 3.0);
    }

    #[test]
    fn the_series_snapshot_counts_each_excluded_row() {
        let rows = [
            row("2026-11-20T10:00:00Z", 5, Some(40.0)),
            row("2026-11-27T10:00:00Z", 5, Some(50.0)),
            row("2026-11-27T10:01:00Z", 10, None),
            row("2026-11-27T10:02:00Z", 5, Some(f64::NAN)),
            row("2026-11-27T10:03:00Z", 6, None),
            row("2026-11-27T10:04:00Z", 5, Some(51.0)),
            row("2026-11-27T12:00:00Z", 5, Some(99.0)),
        ];
        let snapshot = series_snapshot(&window(), &rows).unwrap();
        assert_eq!(snapshot.carry_in.unwrap().value, 40.0);
        assert_eq!(snapshot.first_in_range.unwrap().value, 50.0);
        assert_eq!(snapshot.points.len(), 1);
        assert_eq!(snapshot.points[0].count, 2);
        assert_eq!(snapshot.points[0].last, 51.0);
        assert_eq!(
            snapshot.excluded,
            ExcludedCounts {
                invalid_marker: 1,
                not_numeric: 2
            }
        );
    }

    /// The window is `[from, to)`: a sample exactly at `from` is in the
    /// range and is not the carry-in; a sample exactly at `to` is in
    /// neither.
    #[test]
    fn a_sample_at_the_window_start_is_in_the_range_not_the_carry_in() {
        let window = window();
        let from = window.from().to_string();
        let to = window.to().to_string();
        let rows = [row(&from, 5, Some(70.0)), row(&to, 5, Some(71.0))];
        let snapshot = series_snapshot(&window, &rows).unwrap();
        assert_eq!(snapshot.carry_in, None);
        assert_eq!(snapshot.first_in_range.unwrap().value, 70.0);
        assert_eq!(snapshot.points.len(), 1);
        assert_eq!(snapshot.points[0].last, 70.0);
    }

    #[test]
    fn the_tile_snapshot_sees_an_unusable_newest_row() {
        let rows = [
            row("2026-11-27T10:00:00Z", 5, Some(50.0)),
            row("2026-11-27T10:01:00Z", 10, None),
            row("2026-11-27T12:00:00Z", 5, Some(99.0)),
        ];
        let tile = tile_snapshot(&window(), &rows);
        assert_eq!(
            tile.latest_row.unwrap().created_at,
            at("2026-11-27T10:01:00Z")
        );
        assert_eq!(tile.latest_usable.unwrap().value, 50.0);
        assert_eq!(tile_snapshot(&window(), &[]), TileSnapshot::default());
    }

    #[test]
    fn sleep_does_not_split_a_segment() {
        let points = [
            Point::single(Sample {
                at: at("2026-11-21T10:00:00Z"),
                value: 80.0,
            }),
            Point::single(Sample {
                at: at("2026-11-26T10:00:00Z"),
                value: 75.0,
            }),
        ];
        assert_eq!(segments(&points, &[]).len(), 1);
    }

    #[test]
    fn a_gap_between_points_splits_and_a_gap_outside_does_not() {
        let points = [
            Point::single(Sample {
                at: at("2026-11-21T10:00:00Z"),
                value: 80.0,
            }),
            Point::single(Sample {
                at: at("2026-11-22T10:00:00Z"),
                value: 79.0,
            }),
            Point::single(Sample {
                at: at("2026-11-23T10:00:00Z"),
                value: 78.0,
            }),
        ];
        let inside = gap(at("2026-11-22T11:00:00Z"), at("2026-11-22T11:00:00Z"));
        let before = gap(at("2026-11-20T00:00:00Z"), at("2026-11-21T09:00:00Z"));
        let result = segments(&points, &[inside, before]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 2);
        assert_eq!(result[1].len(), 1);
        assert_eq!(segments(&[], &[inside]), Vec::<Vec<Point>>::new());
    }

    fn sample_strategy() -> impl Strategy<Value = Vec<Sample>> {
        let base = at("2026-11-20T12:00:00Z").as_second();
        proptest::collection::vec((0_i64..604_800, -1_000.0_f64..1_000.0), 0..200).prop_map(
            move |items| {
                let mut seen = std::collections::BTreeMap::new();
                for (offset, value) in items {
                    seen.insert(offset, value);
                }
                seen.into_iter()
                    .map(|(offset, value)| Sample {
                        at: Timestamp::from_second(base + offset).unwrap(),
                        value,
                    })
                    .collect()
            },
        )
    }

    proptest! {
        #[test]
        fn each_point_holds_min_le_last_le_max(samples in sample_strategy()) {
            let points = bucket_points(&window(), &samples).unwrap();
            let total: u32 = points.iter().map(|p| p.count).sum();
            prop_assert_eq!(total as usize, samples.len());
            for pair in points.windows(2) {
                prop_assert!(pair[0].at < pair[1].at);
            }
            for point in &points {
                prop_assert!(point.min <= point.last && point.last <= point.max);
                prop_assert!(point.count >= 1);
            }
        }

        #[test]
        fn segments_never_cross_a_gap(
            samples in sample_strategy(),
            raw_gaps in proptest::collection::vec((0_i64..604_800, 0_i64..7_200), 0..6),
        ) {
            let base = at("2026-11-20T12:00:00Z").as_second();
            let gaps: Vec<Gap> = raw_gaps
                .into_iter()
                .map(|(start, length)| gap(
                    Timestamp::from_second(base + start).unwrap(),
                    Timestamp::from_second(base + start + length).unwrap(),
                ))
                .collect();
            let points = bucket_points(&window(), &samples).unwrap();
            let result = segments(&points, &gaps);
            let flat: Vec<Point> = result.iter().flatten().copied().collect();
            prop_assert_eq!(&flat, &points);
            for segment in &result {
                prop_assert!(!segment.is_empty());
                for pair in segment.windows(2) {
                    for gap in &gaps {
                        // The rule of ADR 0013, written out here, not
                        // taken from `gap_between`.
                        prop_assert!(!(gap.from < pair[1].at && gap.to > pair[0].at));
                    }
                }
            }
            for pair in result.windows(2) {
                let (a, b) = (pair[0].last().unwrap().at, pair[1][0].at);
                prop_assert!(gaps.iter().any(|gap| gap.from < b && gap.to > a));
            }
        }
    }
}
