//! The rows of one transaction, built from the queued records without I/O.
//!
//! The writer ([`super::writer`]) builds one [`Plan`] for each
//! transaction. The plan holds the rows of each table in key order, and the
//! results of the comparison inside the batch: equal copies of a key are
//! dropped with a count, and a record with a different value for a key
//! that an earlier record of the batch holds is a conflict (ADR 0002,
//! "Idempotency").

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use jiff::Timestamp;
use serde_json::Value;
use uuid::Uuid;

use super::error::SqlState;
use crate::record::{Decoded, SignalValue, UnprocessedReason};

/// The ID of a row of `raw.vehicle`.
pub(super) type VehicleId = i16;

/// One queued message for the writer: the two ZMQ frames as received and
/// the result of their decode.
///
/// The writer stores the frames in `raw.unprocessed_record` when the
/// message, or a part of it, cannot be stored as rows. `Debug` shows only
/// the frame sizes and the record type, never the bytes.
#[derive(Clone, Copy)]
pub struct QueuedRecord<'a> {
    /// The first frame: the topic.
    pub topic: &'a [u8],
    /// The second frame: the protobuf payload.
    pub payload: &'a [u8],
    /// The decode of the two frames ([`crate::decode::decode`]).
    pub decoded: &'a Decoded,
}

impl fmt::Debug for QueuedRecord<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.decoded {
            Decoded::VehicleData(_) => "VehicleData",
            Decoded::Alerts(_) => "Alerts",
            Decoded::Errors(_) => "Errors",
            Decoded::Connectivity(_) => "Connectivity",
            Decoded::Unprocessed(_) => "Unprocessed",
        };
        f.debug_struct("QueuedRecord")
            .field("topic_bytes", &self.topic.len())
            .field("payload_bytes", &self.payload.len())
            .field("decoded", &kind)
            .finish()
    }
}

impl<'a> QueuedRecord<'a> {
    /// The receive time of the message.
    pub(super) fn received_at(&self) -> Timestamp {
        match self.decoded {
            Decoded::VehicleData(record) => record.received_at,
            Decoded::Alerts(record) => record.received_at,
            Decoded::Errors(record) => record.received_at,
            Decoded::Connectivity(record) => record.received_at,
            Decoded::Unprocessed(record) => record.received_at,
        }
    }

    /// The VIN, when the record has at least one row for the data tables.
    pub(super) fn vin_with_rows(&self) -> Option<&'a str> {
        let has_rows = match self.decoded {
            Decoded::VehicleData(record) => !record.signals.is_empty(),
            Decoded::Alerts(record) => !record.alerts.is_empty(),
            Decoded::Errors(record) => !record.errors.is_empty(),
            Decoded::Connectivity(_) => true,
            Decoded::Unprocessed(_) => false,
        };
        let vin = match self.decoded {
            Decoded::VehicleData(record) => &record.vin,
            Decoded::Alerts(record) => &record.vin,
            Decoded::Errors(record) => &record.vin,
            Decoded::Connectivity(record) => &record.vin,
            Decoded::Unprocessed(_) => return None,
        };
        has_rows.then(|| vin.expose())
    }
}

/// Truncates an instant to whole microseconds, toward the past.
///
/// PostgreSQL keeps microseconds (ADR 0002, "Time columns"). The decode
/// already truncates the times of the payload; the receive time and the
/// event times come from the clock with nanoseconds. The storage truncates
/// every instant before it binds it, so that a value never rounds to a
/// later instant and a retry binds the same value.
pub(super) fn floor_micros(instant: Timestamp) -> Timestamp {
    let nanos = instant.as_nanosecond();
    // `Timestamp::MIN` is a whole second, so the floor of a valid instant
    // is valid too; the fallback keeps the function total.
    Timestamp::from_nanosecond(nanos - nanos.rem_euclid(1_000)).unwrap_or(instant)
}

/// The reason of a row of `raw.unprocessed_record`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Reason {
    /// A reason of the decode.
    Decode(UnprocessedReason),
    /// A key of the record holds another value in the batch or in the
    /// table.
    ConflictingDuplicate,
    /// The database rejected the record with a data error.
    DatabaseRejected,
}

impl Reason {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Decode(reason) => reason.as_str(),
            Self::ConflictingDuplicate => "conflicting_duplicate",
            Self::DatabaseRejected => "database_rejected",
        }
    }
}

/// Every value of `raw.unprocessed_record.reason`. The CHECK constraint of
/// the column must allow each of them; a test checks it against the
/// migrated schema.
pub const UNPROCESSED_REASONS: [&str; 7] = [
    Reason::Decode(UnprocessedReason::DecodeFailed).as_str(),
    Reason::Decode(UnprocessedReason::UnknownTopic).as_str(),
    Reason::Decode(UnprocessedReason::MissingCreatedAt).as_str(),
    Reason::Decode(UnprocessedReason::UnknownValueVariant).as_str(),
    Reason::Decode(UnprocessedReason::UnknownFields).as_str(),
    Reason::ConflictingDuplicate.as_str(),
    Reason::DatabaseRejected.as_str(),
];

/// The key of `raw.vehicle_signal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct SignalKey {
    pub(super) vehicle_id: VehicleId,
    pub(super) field: i32,
    pub(super) created_at: Timestamp,
}

/// The value columns of a signal row.
///
/// Equality is the duplicate rule of the storage: two values are equal
/// when the variant (`value_type`) and every column are equal. Floats
/// compare by their bits, so a NaN equals the same NaN, and `0.0` and
/// `-0.0` differ. PostgreSQL stores and returns the bits of a `double
/// precision` unchanged (tested), so the rule is the same for a copy in
/// the batch and for a copy in the table. The rule is strict: every
/// difference that the columns can hold makes a conflict, and the
/// conflicting record is kept whole.
#[derive(Clone, PartialEq)]
pub(super) struct SignalValueColumns {
    pub(super) value_type: i16,
    pub(super) int_value: Option<i64>,
    pub(super) float_bits: Option<u64>,
    pub(super) latitude_bits: Option<u64>,
    pub(super) longitude_bits: Option<u64>,
    pub(super) bool_value: Option<bool>,
    pub(super) text_value: Option<String>,
    pub(super) json_value: Option<Value>,
}

impl SignalValueColumns {
    fn empty(value_type: i16) -> Self {
        Self {
            value_type,
            int_value: None,
            float_bits: None,
            latitude_bits: None,
            longitude_bits: None,
            bool_value: None,
            text_value: None,
            json_value: None,
        }
    }

    /// The columns of a value (ADR 0002, "Value representation").
    pub(super) fn from_value(value: &SignalValue) -> Self {
        let mut columns = Self::empty(value.value_type());
        match value {
            SignalValue::Text(text) => columns.text_value = Some(text.clone()),
            SignalValue::Int32(number) => columns.int_value = Some(i64::from(*number)),
            SignalValue::Int64(number) => columns.int_value = Some(*number),
            SignalValue::Float32(number) => columns.float_bits = Some(f64::from(*number).to_bits()),
            SignalValue::Float64(number) => columns.float_bits = Some(number.to_bits()),
            SignalValue::Bool(flag) | SignalValue::Invalid(flag) => {
                columns.bool_value = Some(*flag);
            }
            SignalValue::Location(location) => {
                columns.latitude_bits = Some(location.latitude.to_bits());
                columns.longitude_bits = Some(location.longitude.to_bits());
            }
            SignalValue::Enum(value) => columns.int_value = Some(i64::from(value.number())),
            SignalValue::Doors(object)
            | SignalValue::Time(object)
            | SignalValue::TireLocation(object) => {
                columns.json_value = Some(Value::Object(object.clone()));
            }
        }
        columns
    }
}

/// One row of `raw.vehicle_signal`.
pub(super) struct SignalRow {
    pub(super) key: SignalKey,
    pub(super) value: SignalValueColumns,
    pub(super) received_at: Timestamp,
    pub(super) is_resend: bool,
    /// The index of the record in the batch.
    pub(super) record: usize,
}

/// The key of `raw.connectivity_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct ConnectivityKey {
    pub(super) connection_id: Uuid,
    pub(super) status: i32,
    pub(super) created_at: Timestamp,
}

/// The value columns of a connectivity row.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ConnectivityValue {
    pub(super) vehicle_id: VehicleId,
    pub(super) network_interface: String,
}

/// One row of `raw.connectivity_event`.
pub(super) struct ConnectivityRow {
    pub(super) key: ConnectivityKey,
    pub(super) value: ConnectivityValue,
    pub(super) received_at: Timestamp,
    pub(super) record: usize,
}

/// One row of `raw.vehicle_alert`. Every column except the times of the
/// record is part of the key, so a copy of a key is always a duplicate.
pub(super) struct AlertRow {
    pub(super) vehicle_id: VehicleId,
    pub(super) name: String,
    /// The audiences as a PostgreSQL array literal, for example `{1,2}`.
    /// UNNEST cannot bind an array of arrays, so the statement casts this
    /// text to `integer[]`. It holds only digits, `-`, `,`, and braces.
    pub(super) audiences: String,
    pub(super) started_at: Option<Timestamp>,
    pub(super) ended_at: Option<Timestamp>,
    pub(super) created_at: Timestamp,
    pub(super) received_at: Timestamp,
}

/// One row of `raw.vehicle_error`. Every column except the times of the
/// record is part of the key, so a copy of a key is always a duplicate.
pub(super) struct ErrorRow {
    pub(super) vehicle_id: VehicleId,
    pub(super) created_at: Option<Timestamp>,
    pub(super) name: String,
    pub(super) tags: Value,
    pub(super) body: String,
    pub(super) record_created_at: Timestamp,
    pub(super) received_at: Timestamp,
}

/// One row of `raw.unprocessed_record`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UnprocessedRow {
    pub(super) record: usize,
    pub(super) reason: Reason,
    pub(super) sqlstate: Option<SqlState>,
}

/// The rows of one transaction.
#[derive(Default)]
pub(super) struct Plan {
    pub(super) signals: Vec<SignalRow>,
    pub(super) connectivity: Vec<ConnectivityRow>,
    pub(super) alerts: Vec<AlertRow>,
    pub(super) errors: Vec<ErrorRow>,
    /// The rows that the decode asks for.
    pub(super) unprocessed: Vec<UnprocessedRow>,
    /// Signal and connectivity rows that an earlier record of the batch
    /// holds with an equal value.
    pub(super) duplicates: u64,
    /// The records with a key that an earlier record of the batch holds
    /// with another value.
    pub(super) conflicting: BTreeSet<usize>,
}

/// A record has rows, but the vehicle map has no ID for its VIN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MissingVehicle;

impl Plan {
    /// Builds the rows of `records`. `vehicles` must hold the ID of each
    /// VIN that [`QueuedRecord::vin_with_rows`] returns.
    pub(super) fn build(
        records: &[QueuedRecord<'_>],
        vehicles: &HashMap<String, VehicleId>,
    ) -> Result<Self, MissingVehicle> {
        let mut plan = Self::default();
        let mut signal_index = HashMap::new();
        let mut connectivity_index = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            if let Some(reason) = record.decoded.unprocessed_reason() {
                plan.unprocessed.push(UnprocessedRow {
                    record: index,
                    reason: Reason::Decode(reason),
                    sqlstate: None,
                });
            }
            let vehicle_id = match record.vin_with_rows() {
                Some(vin) => *vehicles.get(vin).ok_or(MissingVehicle)?,
                None => continue,
            };
            match record.decoded {
                Decoded::VehicleData(data) => {
                    for signal in &data.signals {
                        let row = SignalRow {
                            key: SignalKey {
                                vehicle_id,
                                field: signal.field,
                                created_at: floor_micros(data.created_at),
                            },
                            value: SignalValueColumns::from_value(&signal.value),
                            received_at: floor_micros(data.received_at),
                            is_resend: data.is_resend,
                            record: index,
                        };
                        plan.add_signal(row, &mut signal_index);
                    }
                }
                Decoded::Connectivity(event) => {
                    let row = ConnectivityRow {
                        key: ConnectivityKey {
                            connection_id: event.connection_id,
                            status: event.status,
                            created_at: floor_micros(event.created_at),
                        },
                        value: ConnectivityValue {
                            vehicle_id,
                            network_interface: event.network_interface.clone(),
                        },
                        received_at: floor_micros(event.received_at),
                        record: index,
                    };
                    plan.add_connectivity(row, &mut connectivity_index);
                }
                Decoded::Alerts(alerts) => {
                    plan.alerts
                        .extend(alerts.alerts.iter().map(|alert| AlertRow {
                            vehicle_id,
                            name: alert.name.clone(),
                            audiences: array_literal(&alert.audiences),
                            started_at: alert.started_at.map(floor_micros),
                            ended_at: alert.ended_at.map(floor_micros),
                            created_at: floor_micros(alerts.created_at),
                            received_at: floor_micros(alerts.received_at),
                        }));
                }
                Decoded::Errors(errors) => {
                    plan.errors.extend(errors.errors.iter().map(|error| {
                        ErrorRow {
                            vehicle_id,
                            created_at: error.created_at.map(floor_micros),
                            name: error.name.clone(),
                            tags: Value::Object(
                                error
                                    .tags
                                    .iter()
                                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                                    .collect(),
                            ),
                            body: error.body.clone(),
                            record_created_at: floor_micros(errors.created_at),
                            received_at: floor_micros(errors.received_at),
                        }
                    }));
                }
                Decoded::Unprocessed(_) => {}
            }
        }
        // Key order gives two writers the same lock order, so two
        // transactions with the same keys do not deadlock.
        plan.signals.sort_by_key(|row| row.key);
        plan.connectivity.sort_by_key(|row| row.key);
        Ok(plan)
    }

    fn add_signal(&mut self, row: SignalRow, index: &mut HashMap<SignalKey, usize>) {
        match index.get(&row.key) {
            Some(&first) if self.signals[first].value == row.value => self.duplicates += 1,
            Some(_) => {
                self.conflicting.insert(row.record);
            }
            None => {
                index.insert(row.key, self.signals.len());
                self.signals.push(row);
            }
        }
    }

    fn add_connectivity(
        &mut self,
        row: ConnectivityRow,
        index: &mut HashMap<ConnectivityKey, usize>,
    ) {
        match index.get(&row.key) {
            Some(&first) if self.connectivity[first].value == row.value => self.duplicates += 1,
            Some(_) => {
                self.conflicting.insert(row.record);
            }
            None => {
                index.insert(row.key, self.connectivity.len());
                self.connectivity.push(row);
            }
        }
    }

    /// The unprocessed rows for the records in `conflicting`, one for each
    /// record, after the rows that the decode asks for.
    pub(super) fn unprocessed_rows(&self, conflicting: &BTreeSet<usize>) -> Vec<UnprocessedRow> {
        let mut rows = self.unprocessed.clone();
        rows.extend(conflicting.iter().map(|&record| UnprocessedRow {
            record,
            reason: Reason::ConflictingDuplicate,
            sqlstate: None,
        }));
        rows
    }
}

/// The VINs of `records` that have rows, each once, in the order of the
/// batch, with the receive time of the first record of each VIN.
pub(super) fn vins_with_rows<'a>(records: &[QueuedRecord<'a>]) -> Vec<(&'a str, Timestamp)> {
    let mut seen = HashSet::new();
    records
        .iter()
        .filter_map(|record| {
            let vin = record.vin_with_rows()?;
            seen.insert(vin)
                .then(|| (vin, floor_micros(record.received_at())))
        })
        .collect()
}

/// A PostgreSQL array literal of numbers, for example `{1,2}` or `{}`.
fn array_literal(numbers: &[i32]) -> String {
    let items: Vec<String> = numbers.iter().map(i32::to_string).collect();
    format!("{{{}}}", items.join(","))
}

/// The rows that a signal statement did not insert, compared with the
/// stored rows: returns the number of equal rows and the records with a
/// different value.
pub(super) fn compare_stored<K, V>(
    candidates: &[(K, &V, usize)],
    stored: &HashMap<K, V>,
) -> (u64, BTreeSet<usize>)
where
    K: Eq + std::hash::Hash,
    V: PartialEq,
{
    let mut duplicates = 0;
    let mut conflicting = BTreeSet::new();
    for (key, value, record) in candidates {
        match stored.get(key) {
            Some(stored) if stored == *value => duplicates += 1,
            // A missing stored row is a conflict too: the insert found a
            // row with this key, so the row exists. Keep the record.
            _ => {
                conflicting.insert(*record);
            }
        }
    }
    (duplicates, conflicting)
}

#[cfg(test)]
mod tests;
