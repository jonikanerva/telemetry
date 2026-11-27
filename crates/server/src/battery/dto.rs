//! The wire types of `GET /api/v1/battery` (ADR 0012).
//!
//! The types are output types only. The schema rules of `crate::wire`
//! apply: an instant is a [`Instant`], a count has the bounds of `u32`, a
//! field that is always present and can be null has `required = true`,
//! and a tagged enum is a plain `oneOf`.
//!
//! Two clocks are on the wire:
//!
//! - **Vehicle clock.** `Point.at`, `Reading.measured_at`, and
//!   `Status.latest_data_at` are the `created_at` of the vehicle record.
//! - **Ingest clock.** `Gap.from`, `Gap.to`, and `Status.latest_received_at`
//!   are times of the ingest process (`received_at`, `occurred_at`).
//!
//! The vehicle can send old records late (resend), so a gap on the ingest
//! clock does not always lie at the same time as the lost samples on the
//! vehicle clock.
//!
//! Compatibility: within `/api/v1` a change only adds object keys. Every
//! enum and every union below is complete; a new value is a contract
//! change.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::wire::Instant;

/// The query of `GET /api/v1/battery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct BatteryQuery {
    /// The time range that ends now. Required.
    pub range: RangeKey,
}

/// A time range that ends at the time of the response. Each range has a
/// fixed length in hours, not calendar days, and a fixed bucket width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum RangeKey {
    /// 24 hours, buckets of 60 s.
    #[serde(rename = "24h")]
    Day,
    /// 168 hours, buckets of 600 s.
    #[serde(rename = "7d")]
    Week,
    /// 720 hours, buckets of 3600 s.
    #[serde(rename = "30d")]
    Month,
    /// 2160 hours, buckets of 10800 s.
    #[serde(rename = "90d")]
    Quarter,
    /// 8760 hours, buckets of 43200 s.
    #[serde(rename = "1y")]
    Year,
}

/// The body of a successful `GET /api/v1/battery`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BatteryView {
    /// No row of the four battery fields (`Soc`, `EnergyRemaining`,
    /// `NominalFullPackEnergyKwh`, `OutsideTemp`) exists before
    /// `generated_at`. Also when no vehicle exists yet.
    NoDataYet {
        /// The time of the response (server clock). Equal to `range.to`.
        generated_at: Instant,
        /// The requested range.
        range: BatteryRange,
    },
    /// At least one row of a battery field exists. The series of the range
    /// can still be empty: then no sample lies in the range.
    Available {
        /// The time of the response (server clock). Equal to `range.to`.
        generated_at: Instant,
        /// The requested range.
        range: BatteryRange,
        /// Connection, newest data, and freshness.
        status: Box<BatteryStatus>,
        /// The newest value of each field before `range.to`, whatever the
        /// range start.
        tiles: Box<BatteryTiles>,
        /// The series of the range.
        charts: Box<BatteryCharts>,
        /// The data-collection losses that touch the range.
        gaps: Gaps,
    },
}

/// The range of a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct BatteryRange {
    /// The requested range.
    pub key: RangeKey,
    /// The start, inclusive (server clock).
    pub from: Instant,
    /// The end, exclusive (server clock). Equal to `generated_at`.
    pub to: Instant,
    /// The width of one bucket of a series. Buckets start at whole
    /// multiples of the width since 2000-01-01T00:00:00Z (UTC).
    #[schema(minimum = 1, maximum = 4_294_967_295_u64)]
    pub bucket_seconds: u32,
}

/// Connection, newest data, and freshness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct BatteryStatus {
    /// The connection state of the vehicle as the stored events show it.
    pub connection: Connection,
    /// Vehicle clock: the newest `created_at` of a row of the four battery
    /// fields before `generated_at`, usable or not.
    pub latest_data_at: Instant,
    /// Ingest clock: the newest `received_at` among the newest stored row
    /// of each battery field and the newest stored connectivity event.
    /// The ingest process stores data at most about 15 minutes after it
    /// receives it.
    pub latest_received_at: Instant,
    /// Whether new data is expected and has arrived.
    pub freshness: Freshness,
}

/// The connection state of the vehicle.
///
/// The stored data cannot tell a sleeping car from a car without network:
/// both are `disconnected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    /// A connection of the current ingest run has a CONNECTED event and no
    /// DISCONNECTED event.
    Connected {
        /// Server clock: the CONNECTED event of the oldest open connection.
        since: Instant,
    },
    /// No connection of the current ingest run is open.
    Disconnected {
        /// The newest DISCONNECTED event (server clock), or the start of
        /// the current ingest run (ingest clock), whichever is later.
        since: Instant,
    },
    /// No connectivity event is stored.
    Unknown,
}

/// Whether new data is expected and has arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// `generated_at - latest_received_at` is 1200 s or less.
    Current,
    /// Older than 1200 s, and the connection is `connected` or `unknown`:
    /// the data is stale.
    Overdue,
    /// Older than 1200 s, and the connection is `disconnected`: the car is
    /// asleep or offline, so no new data is expected.
    NotExpected,
}

/// The newest value of each field before `range.to`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BatteryTiles {
    /// Field `Soc`: usable state of charge.
    pub soc: Reading,
    /// Field `EnergyRemaining`.
    pub energy_remaining: Reading,
    /// Field `OutsideTemp`.
    pub outside_temp: Reading,
    /// Field `NominalFullPackEnergyKwh`, with its change over the range.
    pub nominal_full_pack_energy: CapacityTile,
}

/// The newest usable value of a field.
///
/// A row is usable when it holds a finite number: a double or float, an
/// integer of at most 2^53 in magnitude, or a text of the form
/// `-?[0-9]+(.[0-9]+)?` with at most 32 characters. The invalid marker of
/// the vehicle and every other value are not usable. A value is never
/// rounded and never replaced with zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reading {
    /// A usable value exists.
    Value {
        /// The value as the vehicle sent it.
        value: f64,
        /// The unit of the value.
        unit: Unit,
        /// Vehicle clock: the `created_at` of the value.
        measured_at: Instant,
        /// Vehicle clock: the `created_at` of the newest row that is newer
        /// than the value and not usable (the invalid marker or not a
        /// number), or null when the value is the newest row.
        #[schema(required = true)]
        newer_unusable_at: Option<Instant>,
    },
    /// No usable value exists before `range.to`.
    Missing {
        /// The unit of the field.
        unit: Unit,
        /// Vehicle clock: the `created_at` of the newest row that is not
        /// usable, or null when the field has no row.
        #[schema(required = true)]
        latest_unusable_at: Option<Instant>,
    },
}

/// The unit of a field. The vehicle sends no unit; the server knows it
/// from the documentation of the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct Unit {
    /// The unit.
    pub symbol: UnitSymbol,
    /// Where the unit comes from.
    pub source: UnitSource,
}

/// A unit symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnitSymbol {
    /// Percent.
    Percent,
    /// Kilowatt-hour.
    KilowattHour,
    /// Degree Celsius.
    DegreeCelsius,
}

/// Where a unit comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnitSource {
    /// Tesla documents the unit of the field.
    Documented,
    /// Tesla does not state the unit; the server assumes it.
    Assumed,
}

/// The capacity tile: the newest value and its change over the range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
pub struct CapacityTile {
    /// The newest usable value.
    pub reading: Reading,
    /// The change from the baseline to the newest value.
    pub change: CapacityChange,
}

/// The change of the capacity over the range.
///
/// The baseline is the newest usable value at or before `range.from`, or
/// else the oldest usable value in the range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapacityChange {
    /// Both values exist and are different rows.
    Value {
        /// `to_value - from_value`, in the unit of the reading.
        delta: f64,
        /// The baseline value.
        from_value: f64,
        /// Vehicle clock: the `created_at` of the baseline.
        from_at: Instant,
        /// The newest value.
        to_value: f64,
        /// Vehicle clock: the `created_at` of the newest value.
        to_at: Instant,
    },
    /// The change cannot be computed.
    Unknown {
        /// Why the change is unknown.
        reason: ChangeUnknownReason,
    },
}

/// Why a change is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeUnknownReason {
    /// No usable value exists before `range.to`.
    NoLatest,
    /// The only usable value is the newest value itself.
    NoBaseline,
}

/// The series of the range.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BatteryCharts {
    /// Field `Soc`.
    pub soc: Series,
    /// Field `NominalFullPackEnergyKwh`.
    pub nominal_full_pack_energy: Series,
}

/// One series: the usable samples of a field in the range, in buckets.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Series {
    /// The unit of the values.
    pub unit: Unit,
    /// The newest usable sample before `range.from`, as one point with
    /// `count` 1, or null. It lets a client tell "no sample in the range"
    /// from "no sample at all". It joins the first segment unless a gap
    /// lies between them.
    #[schema(required = true)]
    pub carry_in: Option<Point>,
    /// The points of the range in order of `at`. A new segment starts only
    /// where a gap lies between two points; a time without samples (the
    /// car asleep) does not split a segment. Empty when no usable sample
    /// lies in the range.
    pub segments: Vec<Vec<Point>>,
    /// The rows of the field in the range that are not usable. They are
    /// in no point.
    pub excluded: ExcludedRows,
}

/// One bucket of usable samples.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
pub struct Point {
    /// Vehicle clock: the `created_at` of the newest sample in the bucket.
    pub at: Instant,
    /// The value of the newest sample in the bucket.
    pub last: f64,
    /// The smallest value in the bucket.
    pub min: f64,
    /// The largest value in the bucket.
    pub max: f64,
    /// The number of usable samples in the bucket, at least 1.
    #[schema(minimum = 1, maximum = 4_294_967_295_u64)]
    pub count: u32,
}

/// Counts of rows that are not usable, by reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ExcludedRows {
    /// Rows with the invalid marker of the vehicle.
    #[schema(minimum = 0, maximum = 4_294_967_295_u64)]
    pub invalid_marker: u32,
    /// Rows that hold no usable number: a text that is not a plain
    /// decimal number, a value that is not finite, an integer beyond
    /// 2^53, a boolean, or another value type.
    #[schema(minimum = 0, maximum = 4_294_967_295_u64)]
    pub not_numeric: u32,
}

/// The data-collection losses that touch the range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Gaps {
    /// At most 500 gaps, in order of `from`, then `to`. The list is
    /// truncated when `total` is larger than its length.
    pub items: Vec<Gap>,
    /// The number of gaps that touch the range.
    #[schema(minimum = 0, maximum = 4_294_967_295_u64)]
    pub total: u32,
}

/// One data-collection loss. A sleeping car is never a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct Gap {
    /// The cause of the loss.
    pub kind: GapKind,
    /// Whether data was surely lost.
    pub certainty: GapCertainty,
    /// Ingest clock: the start of the loss. `from <= to`.
    pub from: Instant,
    /// Ingest clock: the end of the loss.
    pub to: Instant,
    /// Whether `from` and `to` are recorded times or an estimate.
    pub bounds: GapBounds,
    /// The recorded number of lost items, or null when no count exists.
    #[schema(required = true)]
    pub count: Option<GapCount>,
}

/// The cause of a loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// An ingest run ended without its `stopped` event (a crash, or a
    /// final flush that failed). The queue of that run can be lost.
    UncleanRestart,
    /// The memory queue of the ingest process was full; records were
    /// dropped.
    QueueFull,
    /// The database refused records and their unprocessed rows.
    DatabaseRefused,
    /// Messages of the server did not reach the ingest reader.
    ZmqShortfall,
    /// The gate refused vehicle connections at its bound.
    GateFull,
    /// The server process of the vehicle connections ended while the gate
    /// was open.
    ChildExited,
}

/// Whether data was surely lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapCertainty {
    /// Data was lost.
    Loss,
    /// Data can be lost; the stored events cannot show it.
    PossibleLoss,
}

/// Whether the bounds of a gap are recorded times or an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapBounds {
    /// Recorded times of the ingest process.
    Exact,
    /// An estimate from the times of other events.
    Estimated,
}

/// A recorded number of lost items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct GapCount {
    /// The number, at least 1.
    #[schema(minimum = 1, maximum = 4_294_967_295_u64)]
    pub value: u32,
    /// What the number counts.
    pub of: CountUnit,
}

/// What a gap count counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CountUnit {
    /// Received records.
    Records,
    /// Messages of the server.
    Messages,
    /// Vehicle connections.
    Connections,
}
