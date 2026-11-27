//! The mapping from the domain view to the wire types.

use telemetry_web_domain::battery as domain;
use telemetry_web_domain::battery::gaps as domain_gaps;

use super::dto;
use crate::wire::Instant;

/// A domain value that the wire contract cannot hold: a time with a part
/// below one microsecond, or a change that is not finite. Every stored
/// time has microsecond precision, the window end is truncated, and the
/// values are finite, so this does not occur in practice. The mapping
/// fails instead of a silent rounding or a `null` number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a value does not fit the wire contract")]
pub struct MapError;

fn instant(at: jiff::Timestamp) -> Result<Instant, MapError> {
    Instant::from_timestamp(at).map_err(|_| MapError)
}

/// The wire range key of a domain key.
#[must_use]
pub const fn range_key(key: domain::RangeKey) -> dto::RangeKey {
    match key {
        domain::RangeKey::Day => dto::RangeKey::Day,
        domain::RangeKey::Week => dto::RangeKey::Week,
        domain::RangeKey::Month => dto::RangeKey::Month,
        domain::RangeKey::Quarter => dto::RangeKey::Quarter,
        domain::RangeKey::Year => dto::RangeKey::Year,
    }
}

/// The domain range key of a wire key.
#[must_use]
pub const fn domain_key(key: dto::RangeKey) -> domain::RangeKey {
    match key {
        dto::RangeKey::Day => domain::RangeKey::Day,
        dto::RangeKey::Week => domain::RangeKey::Week,
        dto::RangeKey::Month => domain::RangeKey::Month,
        dto::RangeKey::Quarter => domain::RangeKey::Quarter,
        dto::RangeKey::Year => domain::RangeKey::Year,
    }
}

/// Maps a domain view.
///
/// # Errors
///
/// Returns [`MapError`] for a time below one microsecond.
pub fn view(view: &domain::View) -> Result<dto::BatteryView, MapError> {
    match view {
        domain::View::NoDataYet { window } => Ok(dto::BatteryView::NoDataYet {
            generated_at: instant(window.to())?,
            range: range(window)?,
        }),
        domain::View::Available(available) => available_view(available),
    }
}

fn range(window: &domain::Window) -> Result<dto::BatteryRange, MapError> {
    Ok(dto::BatteryRange {
        key: range_key(window.key()),
        from: instant(window.from())?,
        to: instant(window.to())?,
        bucket_seconds: window.key().bucket_seconds(),
    })
}

fn available_view(available: &domain::Available) -> Result<dto::BatteryView, MapError> {
    let window = &available.window;
    let gaps = available
        .gaps
        .items
        .iter()
        .map(gap)
        .collect::<Result<Vec<_>, _>>()?;
    let restarts = available
        .restarts
        .items
        .iter()
        .map(|at| instant(*at))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(dto::BatteryView::Available {
        generated_at: instant(window.to())?,
        range: range(window)?,
        status: Box::new(dto::BatteryStatus {
            connection: connection(available.connection)?,
            latest_data_at: instant(available.latest_data_at)?,
            latest_received_at: instant(available.latest_received_at)?,
            freshness: freshness(available.freshness),
        }),
        tiles: Box::new(dto::BatteryTiles {
            soc: reading(&available.tiles.soc)?,
            energy_remaining: reading(&available.tiles.energy_remaining)?,
            outside_temp: reading(&available.tiles.outside_temp)?,
            nominal_full_pack_energy: dto::CapacityTile {
                reading: reading(&available.tiles.nominal_full_pack_energy)?,
                change: change(available.capacity_change)?,
            },
        }),
        charts: Box::new(dto::BatteryCharts {
            soc: series(&available.series.soc)?,
            nominal_full_pack_energy: series(&available.series.nominal_full_pack_energy)?,
        }),
        gaps: dto::Gaps {
            items: gaps,
            truncated: available.gaps.truncated,
        },
        restarts: dto::Restarts {
            items: restarts,
            truncated: available.restarts.truncated,
        },
    })
}

fn connection(connection: domain::Connection) -> Result<dto::Connection, MapError> {
    Ok(match connection {
        domain::Connection::Connected { at } => dto::Connection::Connected { at: instant(at)? },
        domain::Connection::Disconnected { at } => {
            dto::Connection::Disconnected { at: instant(at)? }
        }
        domain::Connection::None => dto::Connection::None,
    })
}

const fn freshness(freshness: domain::Freshness) -> dto::Freshness {
    match freshness {
        domain::Freshness::Current => dto::Freshness::Current,
        domain::Freshness::Overdue => dto::Freshness::Overdue,
        domain::Freshness::NotExpected => dto::Freshness::NotExpected,
    }
}

const fn unit(unit: domain::Unit) -> dto::Unit {
    use telemetry_web_domain::battery::value::{UnitSource, UnitSymbol};
    dto::Unit {
        symbol: match unit.symbol {
            UnitSymbol::Percent => dto::UnitSymbol::Percent,
            UnitSymbol::KilowattHour => dto::UnitSymbol::KilowattHour,
            UnitSymbol::DegreeCelsius => dto::UnitSymbol::DegreeCelsius,
        },
        source: match unit.source {
            UnitSource::Documented => dto::UnitSource::Documented,
            UnitSource::Assumed => dto::UnitSource::Assumed,
        },
    }
}

fn reading(reading: &domain::Reading) -> Result<dto::Reading, MapError> {
    Ok(match *reading {
        domain::Reading::Value {
            sample,
            unit: u,
            newer_unusable_at,
        } => dto::Reading::Value {
            value: sample.value,
            unit: unit(u),
            measured_at: instant(sample.at)?,
            newer_unusable_at: newer_unusable_at.map(instant).transpose()?,
        },
        domain::Reading::Missing {
            unit: u,
            latest_unusable_at,
        } => dto::Reading::Missing {
            unit: unit(u),
            latest_unusable_at: latest_unusable_at.map(instant).transpose()?,
        },
    })
}

fn change(change: domain::CapacityChange) -> Result<dto::CapacityChange, MapError> {
    Ok(match change {
        domain::CapacityChange::Value { from, to } => dto::CapacityChange::Value {
            delta: Some(to.value - from.value)
                .filter(|delta| delta.is_finite())
                .ok_or(MapError)?,
            from_value: from.value,
            from_at: instant(from.at)?,
            to_value: to.value,
            to_at: instant(to.at)?,
        },
        domain::CapacityChange::NoLatest => dto::CapacityChange::Unknown {
            reason: dto::ChangeUnknownReason::NoLatest,
        },
        domain::CapacityChange::NoBaseline => dto::CapacityChange::Unknown {
            reason: dto::ChangeUnknownReason::NoBaseline,
        },
    })
}

fn point(point: &domain::Point) -> Result<dto::Point, MapError> {
    Ok(dto::Point {
        at: instant(point.at)?,
        last: point.last,
        min: point.min,
        max: point.max,
        count: point.count,
    })
}

fn series(series: &domain::SeriesView) -> Result<dto::Series, MapError> {
    Ok(dto::Series {
        unit: unit(series.unit),
        carry_in: series
            .carry_in
            .map(|sample| point(&domain::Point::single(sample)))
            .transpose()?,
        segments: series
            .segments
            .iter()
            .map(|segment| segment.iter().map(point).collect())
            .collect::<Result<_, _>>()?,
        excluded: dto::ExcludedRows {
            invalid_marker: series.excluded.invalid_marker,
            not_numeric: series.excluded.not_numeric,
        },
    })
}

fn gap(gap: &domain::Gap) -> Result<dto::Gap, MapError> {
    use domain_gaps::{CountUnit, GapKind};
    Ok(dto::Gap {
        kind: match gap.kind {
            GapKind::QueueFull => dto::GapKind::QueueFull,
            GapKind::DatabaseRefused => dto::GapKind::DatabaseRefused,
            GapKind::ZmqShortfall => dto::GapKind::ZmqShortfall,
        },
        from: instant(gap.from)?,
        to: instant(gap.to)?,
        count: dto::GapCount {
            value: gap.count.value,
            of: match gap.count.of {
                CountUnit::Records => dto::CountUnit::Records,
                CountUnit::Messages => dto::CountUnit::Messages,
            },
        },
    })
}

/// Each enum arm of the mapping, written out by hand. The expected wire
/// values are the enum values of `contract/openapi.json`; each test also
/// compares its list of values with the enum of the contract, so a
/// missing arm fails.
#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use telemetry_web_domain::battery::gaps::{CountUnit, Gap, GapCount, GapKind};

    use super::*;

    fn at(text: &str) -> jiff::Timestamp {
        text.parse().unwrap()
    }

    fn wire<T: serde::Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    /// The values of a string enum in `contract/openapi.json`, or of the
    /// `state` property of each variant of a tagged `oneOf`.
    fn contract_values(schema: &str) -> Vec<String> {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/openapi.json");
        let document: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let schema = &document["components"]["schemas"][schema];
        let values: Vec<&Value> = match schema["enum"].as_array() {
            Some(values) => values.iter().collect(),
            None => schema["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|variant| variant["properties"]["state"]["enum"].as_array().unwrap())
                .collect(),
        };
        let mut values: Vec<String> = values
            .into_iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        values.sort();
        values
    }

    fn sorted(values: &[&str]) -> Vec<String> {
        let mut values: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        values.sort();
        values
    }

    #[test]
    fn each_gap_arm_maps_to_its_contract_value() {
        let cases = [
            (
                GapKind::QueueFull,
                CountUnit::Records,
                json!({"kind": "queue_full", "count": {"value": 3, "of": "records"}}),
            ),
            (
                GapKind::DatabaseRefused,
                CountUnit::Records,
                json!({"kind": "database_refused", "count": {"value": 3, "of": "records"}}),
            ),
            (
                GapKind::ZmqShortfall,
                CountUnit::Messages,
                json!({"kind": "zmq_shortfall", "count": {"value": 3, "of": "messages"}}),
            ),
        ];
        for (kind, of, mut expected) in cases {
            let domain_gap = Gap {
                kind,
                from: at("2026-11-27T10:00:00Z"),
                to: at("2026-11-27T10:05:00.000001Z"),
                count: GapCount { value: 3, of },
            };
            expected["from"] = json!("2026-11-27T10:00:00Z");
            expected["to"] = json!("2026-11-27T10:05:00.000001Z");
            assert_eq!(wire(&gap(&domain_gap).unwrap()), expected, "{kind:?}");
        }
        assert_eq!(
            contract_values("GapKind"),
            sorted(&["queue_full", "database_refused", "zmq_shortfall"])
        );
        assert_eq!(
            contract_values("CountUnit"),
            sorted(&["records", "messages"])
        );
    }

    #[test]
    fn each_connection_arm_maps_to_its_contract_value() {
        let cases = [
            (
                domain::Connection::Connected {
                    at: at("2026-11-27T09:00:00Z"),
                },
                json!({"state": "connected", "at": "2026-11-27T09:00:00Z"}),
            ),
            (
                domain::Connection::Disconnected {
                    at: at("2026-11-27T09:30:00.000001Z"),
                },
                json!({"state": "disconnected", "at": "2026-11-27T09:30:00.000001Z"}),
            ),
            (domain::Connection::None, json!({"state": "none"})),
        ];
        for (domain_connection, expected) in cases {
            assert_eq!(
                wire(&connection(domain_connection).unwrap()),
                expected,
                "{domain_connection:?}"
            );
        }
        assert_eq!(
            contract_values("Connection"),
            sorted(&["connected", "disconnected", "none"])
        );
    }

    #[test]
    fn each_freshness_arm_maps_to_its_contract_value() {
        for (domain_freshness, expected) in [
            (domain::Freshness::Current, "current"),
            (domain::Freshness::Overdue, "overdue"),
            (domain::Freshness::NotExpected, "not_expected"),
        ] {
            assert_eq!(wire(&freshness(domain_freshness)), json!(expected));
        }
        assert_eq!(
            contract_values("Freshness"),
            sorted(&["current", "overdue", "not_expected"])
        );
    }

    #[test]
    fn each_unknown_change_arm_maps_to_its_contract_value() {
        for (domain_change, expected) in [
            (domain::CapacityChange::NoLatest, "no_latest"),
            (domain::CapacityChange::NoBaseline, "no_baseline"),
        ] {
            assert_eq!(
                wire(&change(domain_change).unwrap()),
                json!({"state": "unknown", "reason": expected})
            );
        }
        assert_eq!(
            contract_values("ChangeUnknownReason"),
            sorted(&["no_latest", "no_baseline"])
        );
    }
}
