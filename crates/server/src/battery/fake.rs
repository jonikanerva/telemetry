//! The fake source of the test server (feature `test-server`).
//!
//! The browser tests pick a scenario file for each browser context with
//! the cookie `telemetry_test_scenario=<name>`. The source reads
//! `<WEB_TEST_SCENARIO_DIR>/<name>.json` for each request and turns its
//! raw rows into a snapshot with the reference rules of
//! `telemetry_web_domain`, the rules that the PostgreSQL source equals.
//!
//! # Scenario format
//!
//! One JSON object; an unknown key at any level is an error.
//!
//! - `now` (required): the fixed clock of the request, a wire instant.
//! - `vehicles` (required): the vehicle ids. `[]` gives `no_data_yet`;
//!   two ids give 500 `data_contract_violation`.
//! - `signals`: `vehicle_id`, `field` (8 `Soc`, 158 `EnergyRemaining`,
//!   263 `NominalFullPackEnergyKwh`, 86 `OutsideTemp`; the consumption
//!   view also reads 10 `Gear` and 5 `Odometer`; other fields are
//!   ignored), `created_at`, `received_at`, `value_type` (the proto field
//!   number), and exactly one of `int_value`, `float_value` (a number, or
//!   `"NaN"`, `"Infinity"`, `"-Infinity"`), `text_value`, `bool_value`:
//!   the column that `value_type` needs. `value_type` 10 is the invalid
//!   marker, with `bool_value`.
//! - `connectivity`: `vehicle_id`, `connection_id` (UUID), `created_at`,
//!   `status` (1 CONNECTED, 2 DISCONNECTED).
//! - `ingest_events`: `run_id` (UUID), `occurred_at`, `kind`, and
//!   `reason`, `dropped_count`, `first_received_at`, `last_received_at`
//!   with the CHECK rules of `raw.ingest_event`.
//! - `fault`: `database_unavailable`, `service_busy`, `timeout`, or
//!   `internal_error`; the answer is that problem.
//! - `delay_ms`: 0 to 10000, a wait before the answer.
//!
//! ```json
//! {
//!   "now": "2026-11-27T12:00:00Z",
//!   "vehicles": [1],
//!   "signals": [
//!     {"vehicle_id": 1, "field": 8, "created_at": "2026-11-27T11:00:00Z",
//!      "received_at": "2026-11-27T11:00:01Z", "value_type": 5, "float_value": 80.5}
//!   ],
//!   "connectivity": [
//!     {"vehicle_id": 1, "connection_id": "7f1c2a3e-0000-4000-8000-000000000001",
//!      "created_at": "2026-11-27T10:59:00Z",
//!      "status": 1}
//!   ],
//!   "ingest_events": [
//!     {"run_id": "5b0e9d4c-0000-4000-8000-000000000001",
//!      "occurred_at": "2026-11-27T08:00:00Z", "kind": "started"}
//!   ]
//! }
//! ```
//!
//! `WEB_TEST_SCENARIO_DIR` is optional: without it every request gives
//! 500 `internal_error`.
//!
//! A scenario that breaks a rule of the raw tables
//! (`migrations/0001_raw_tables.sql`) gives 500 `internal_error` and the
//! log event `scenario_invalid`, so a typo in a test fails loudly. The
//! fake source checks these rules:
//!
//! - an unknown key at any level, or an id that is not a UUID;
//! - a vehicle id two times (`vehicle_pkey`);
//! - a signal or connectivity row of an unknown vehicle (the foreign
//!   keys);
//! - a signal without exactly one value column
//!   (`vehicle_signal_one_value`), a `value_type` of 0 or less
//!   (`vehicle_signal_value_type`), or a value column that does not
//!   match the `value_type` (`vehicle_signal_value_column`). The types
//!   7, 21, 31, and 39 need `latitude` or `json_value`, which the format
//!   does not have, so the fake source rejects them;
//! - two signals with one key `vehicle_id`, `field`, `created_at`
//!   (`vehicle_signal_pkey`);
//! - two connectivity rows with one key `connection_id`, `status`,
//!   `created_at` (`connectivity_event_pkey`);
//! - an unknown event kind (`ingest_event_kind`), two events with one
//!   key `run_id`, `kind`, `occurred_at` (`ingest_event_key`), and the
//!   rules of the counted events: the count and both times together
//!   (`ingest_event_window`), a count of 1 or more
//!   (`ingest_event_count`), and `first_received_at` at or before
//!   `last_received_at` (`ingest_event_window_order`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use telemetry_web_domain::battery::{
    ConnectivityRow, EventKind, EventRow, Field, PerField, RawValue, RowTimes, SeriesPair,
    SignalRow, Snapshot, VehicleCount, Window, newest_connectivity, select_events, series_snapshot,
    tile_snapshot,
};
use tracing::warn;

use super::source::{BatterySource, SnapshotFuture, SourceError, SourceRequest};
use crate::consumption::{ConsumptionFuture, ConsumptionSource};
use crate::wire::Instant;

/// The cookie that names the scenario.
pub const SCENARIO_COOKIE: &str = "telemetry_test_scenario";

/// The largest scenario file.
pub const MAX_SCENARIO_BYTES: u64 = 4 * 1024 * 1024;

/// The longest `delay_ms`.
pub const MAX_DELAY_MS: u32 = 10_000;

/// The fake source.
#[derive(Debug)]
pub struct FakeSource {
    dir: Option<PathBuf>,
}

impl FakeSource {
    /// A source that reads the scenarios of `dir`. Without a directory
    /// every request gives 500 `internal_error`.
    #[must_use]
    pub const fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    async fn read(&self, request: &SourceRequest) -> Result<Snapshot, SourceError> {
        let scenario = self.scenario(request).await?;
        snapshot(&scenario, request).map_err(|reason| {
            warn!(reason, "scenario_invalid");
            SourceError::Internal("scenario_invalid")
        })
    }

    async fn read_consumption(
        &self,
        request: &SourceRequest,
    ) -> Result<telemetry_web_domain::consumption::Snapshot, SourceError> {
        let scenario = self.scenario(request).await?;
        consumption_snapshot(&scenario, request).map_err(|reason| {
            warn!(reason, "scenario_invalid");
            SourceError::Internal("scenario_invalid")
        })
    }

    /// The scenario of a request after its delay, or the error of its
    /// fault.
    async fn scenario(&self, request: &SourceRequest) -> Result<Scenario, SourceError> {
        let scenario = load(self.dir.as_deref(), request.scenario.as_deref())
            .await
            .map_err(|reason| {
                warn!(reason, "scenario_invalid");
                SourceError::Internal("scenario_invalid")
            })?;
        if scenario.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(u64::from(scenario.delay_ms))).await;
        }
        if let Some(fault) = scenario.fault {
            return Err(match fault {
                Fault::DatabaseUnavailable => SourceError::Unavailable("scenario_fault"),
                Fault::ServiceBusy => SourceError::Busy,
                Fault::Timeout => SourceError::Timeout("scenario_fault"),
                Fault::InternalError => SourceError::Internal("scenario_fault"),
            });
        }
        Ok(scenario)
    }
}

impl BatterySource for FakeSource {
    fn snapshot<'a>(&'a self, request: &'a SourceRequest) -> SnapshotFuture<'a> {
        Box::pin(self.read(request))
    }
}

impl ConsumptionSource for FakeSource {
    fn consumption<'a>(&'a self, request: &'a SourceRequest) -> ConsumptionFuture<'a> {
        Box::pin(self.read_consumption(request))
    }
}

/// The value of the scenario cookie in the `Cookie` header values, if it
/// is a valid name.
#[must_use]
pub fn scenario_name<'a>(cookie_headers: impl IntoIterator<Item = &'a str>) -> Option<String> {
    cookie_headers
        .into_iter()
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SCENARIO_COOKIE)
        .map(|(_, value)| value.to_owned())
        .filter(|name| valid_name(name))
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

async fn load(dir: Option<&Path>, name: Option<&str>) -> Result<Scenario, &'static str> {
    let dir = dir.ok_or("no_scenario_dir")?;
    let name = name
        .filter(|name| valid_name(name))
        .ok_or("no_scenario_cookie")?;
    let path = dir.join(format!("{name}.json"));
    let metadata = tokio::fs::metadata(&path).await.map_err(|_| "unreadable")?;
    if metadata.len() > MAX_SCENARIO_BYTES {
        return Err("too_large");
    }
    let bytes = tokio::fs::read(&path).await.map_err(|_| "unreadable")?;
    let scenario: Scenario = serde_json::from_slice(&bytes).map_err(|_| "not_valid_json")?;
    if scenario.delay_ms > MAX_DELAY_MS {
        return Err("delay_too_long");
    }
    Ok(scenario)
}

/// A scenario file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    now: Instant,
    vehicles: Vec<i16>,
    #[serde(default)]
    signals: Vec<SignalJson>,
    #[serde(default)]
    connectivity: Vec<ConnectivityJson>,
    #[serde(default)]
    ingest_events: Vec<EventJson>,
    #[serde(default)]
    fault: Option<Fault>,
    #[serde(default)]
    delay_ms: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Fault {
    DatabaseUnavailable,
    ServiceBusy,
    Timeout,
    InternalError,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalJson {
    vehicle_id: i16,
    field: i32,
    created_at: Instant,
    received_at: Instant,
    value_type: i16,
    #[serde(default)]
    int_value: Option<i64>,
    #[serde(default)]
    float_value: Option<FloatJson>,
    #[serde(default)]
    text_value: Option<String>,
    #[serde(default)]
    bool_value: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
enum FloatJson {
    Number(f64),
    Special(SpecialFloat),
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum SpecialFloat {
    #[serde(rename = "NaN")]
    NaN,
    #[serde(rename = "Infinity")]
    Infinity,
    #[serde(rename = "-Infinity")]
    NegInfinity,
}

impl FloatJson {
    const fn value(self) -> f64 {
        match self {
            Self::Number(value) => value,
            Self::Special(SpecialFloat::NaN) => f64::NAN,
            Self::Special(SpecialFloat::Infinity) => f64::INFINITY,
            Self::Special(SpecialFloat::NegInfinity) => f64::NEG_INFINITY,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectivityJson {
    vehicle_id: i16,
    connection_id: String,
    created_at: Instant,
    status: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventJson {
    run_id: String,
    occurred_at: Instant,
    kind: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    dropped_count: Option<i64>,
    #[serde(default)]
    first_received_at: Option<Instant>,
    #[serde(default)]
    last_received_at: Option<Instant>,
}

fn uuid(text: &str) -> Result<u128, &'static str> {
    uuid::Uuid::parse_str(text)
        .map(|id| id.as_u128())
        .map_err(|_| "bad_uuid")
}

/// The snapshot of a scenario, through the reference rules.
fn snapshot(scenario: &Scenario, request: &SourceRequest) -> Result<Snapshot, &'static str> {
    let window = Window::ending_at(request.key, scenario.now.timestamp()).map_err(|_| "bad_now")?;
    let vehicles = match scenario.vehicles.as_slice() {
        [] => VehicleCount::None,
        [id] => VehicleCount::One(*id),
        _ => VehicleCount::Many,
    };
    check_rows(scenario)?;
    let events = scenario
        .ingest_events
        .iter()
        .map(event)
        .collect::<Result<Vec<_>, _>>()?;
    // The unique key of `raw.ingest_event`.
    let mut keys = std::collections::HashSet::new();
    if !events
        .iter()
        .all(|row| keys.insert((row.run_id, row.kind, row.occurred_at)))
    {
        return Err("duplicate_event_key");
    }
    let mut connectivity = Vec::with_capacity(scenario.connectivity.len());
    for row in &scenario.connectivity {
        if !scenario.vehicles.contains(&row.vehicle_id) {
            return Err("unknown_vehicle");
        }
        connectivity.push((
            row.vehicle_id,
            ConnectivityRow {
                connection_id: uuid(&row.connection_id)?,
                created_at: row.created_at.timestamp(),
                status: row.status,
            },
        ));
    }
    let VehicleCount::One(vehicle_id) = vehicles else {
        return Ok(Snapshot {
            window,
            vehicles,
            tiles: PerField::default(),
            series: SeriesPair::default(),
            events: telemetry_web_domain::battery::EventSnapshot::default(),
            connectivity: None,
        });
    };

    let rows_of = |field: Field| -> Vec<SignalRow<'_>> {
        scenario
            .signals
            .iter()
            .filter(|signal| signal.vehicle_id == vehicle_id && signal.field == field.number())
            .map(|signal| SignalRow {
                times: RowTimes {
                    created_at: signal.created_at.timestamp(),
                    received_at: signal.received_at.timestamp(),
                },
                value: RawValue {
                    value_type: signal.value_type,
                    int_value: signal.int_value,
                    float_value: signal.float_value.map(FloatJson::value),
                    text_value: signal.text_value.as_deref(),
                    bool_value: signal.bool_value,
                },
            })
            .collect()
    };
    let mut tiles = PerField::default();
    for field in Field::ALL {
        *tiles.get_mut(field) = tile_snapshot(&window, &rows_of(field));
    }
    let series = SeriesPair {
        soc: series_snapshot(&window, &rows_of(Field::Soc)).map_err(|_| "count")?,
        nominal_full_pack_energy: series_snapshot(&window, &rows_of(Field::NominalFullPackEnergy))
            .map_err(|_| "count")?,
    };
    let events = select_events(&window, &events);
    let vehicle_connectivity: Vec<ConnectivityRow> = connectivity
        .into_iter()
        .filter(|(id, _)| *id == vehicle_id)
        .map(|(_, row)| row)
        .collect();
    let connectivity = newest_connectivity(&vehicle_connectivity, window.to());
    Ok(Snapshot {
        window,
        vehicles,
        tiles,
        series,
        events,
        connectivity,
    })
}

fn raw(signal: &SignalJson) -> RawValue<'_> {
    RawValue {
        value_type: signal.value_type,
        int_value: signal.int_value,
        float_value: signal.float_value.map(FloatJson::value),
        text_value: signal.text_value.as_deref(),
        bool_value: signal.bool_value,
    }
}

/// The consumption snapshot of a scenario, through the rules of
/// `telemetry_web_domain::consumption`, as the SQL of
/// `crate::consumption::postgres` selects the rows.
fn consumption_snapshot(
    scenario: &Scenario,
    request: &SourceRequest,
) -> Result<telemetry_web_domain::consumption::Snapshot, &'static str> {
    use telemetry_web_domain::consumption::{
        GEAR_FIELD, MAX_DRIVE_SAMPLES, MAX_GEAR_ROWS, SampleInput, Snapshot, ValueField, gear,
        gear_input, periods, sample, sample_input, sample_windows,
    };
    // The status rows and the losses: the battery snapshot of the scenario.
    let battery = snapshot(scenario, request)?;
    let window = battery.window;
    let vehicles = battery.vehicles;
    let VehicleCount::One(vehicle_id) = vehicles else {
        return Ok(Snapshot {
            window,
            vehicles,
            gears: gear_input(None, Vec::new(), MAX_GEAR_ROWS),
            samples: SampleInput::default(),
            tiles: battery.tiles,
            connectivity: None,
            events: battery.events,
        });
    };
    let of_vehicle = || {
        scenario
            .signals
            .iter()
            .filter(move |signal| signal.vehicle_id == vehicle_id)
    };
    let mut readings: Vec<_> = of_vehicle()
        .filter(|signal| signal.field == GEAR_FIELD && signal.created_at.timestamp() < window.to())
        .filter_map(|signal| gear(&raw(signal)).map(|g| (signal.created_at.timestamp(), g)))
        .collect();
    readings.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    let before = readings
        .iter()
        .find(|(at, _)| *at < window.from())
        .map(|(_, g)| *g);
    let mut in_range: Vec<_> = readings
        .into_iter()
        .filter(|(at, _)| window.contains(*at))
        .collect();
    in_range.truncate(MAX_GEAR_ROWS + 1);
    let gears = gear_input(before, in_range, MAX_GEAR_ROWS);
    let windows = sample_windows(&periods(&gears));
    let mut rows: Vec<_> = of_vehicle()
        .filter_map(|signal| {
            let field = ValueField::from_number(signal.field)?;
            let at = signal.created_at.timestamp();
            windows
                .iter()
                .any(|(from, to)| *from <= at && at <= *to)
                .then(|| (at, sample(field, at, &raw(signal))))
        })
        .collect();
    rows.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    rows.truncate(MAX_DRIVE_SAMPLES + 1);
    Ok(Snapshot {
        window,
        vehicles,
        gears,
        samples: sample_input(rows, MAX_DRIVE_SAMPLES),
        tiles: battery.tiles,
        connectivity: battery.connectivity,
        events: battery.events,
    })
}

/// The value columns of the scenario format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueColumn {
    Int,
    Float,
    Text,
    Bool,
}

/// The column that `value_type` needs: the rule
/// `vehicle_signal_value_column`. `None` for the types that need
/// `latitude` or `json_value`, which the scenario format does not have.
const fn required_column(value_type: i16) -> Option<ValueColumn> {
    match value_type {
        1 => Some(ValueColumn::Text),
        4 | 5 => Some(ValueColumn::Float),
        6 | 10 => Some(ValueColumn::Bool),
        7 | 21 | 31 | 39 => None,
        _ => Some(ValueColumn::Int),
    }
}

/// The one value column of a signal, or `None` when the signal has no
/// value column or more than one (`vehicle_signal_one_value`).
fn value_column(signal: &SignalJson) -> Option<ValueColumn> {
    let present = [
        (signal.int_value.is_some(), ValueColumn::Int),
        (signal.float_value.is_some(), ValueColumn::Float),
        (signal.text_value.is_some(), ValueColumn::Text),
        (signal.bool_value.is_some(), ValueColumn::Bool),
    ];
    let mut columns = present
        .into_iter()
        .filter(|(is_some, _)| *is_some)
        .map(|(_, column)| column);
    match (columns.next(), columns.next()) {
        (Some(column), None) => Some(column),
        _ => None,
    }
}

/// The vehicles, the signal rows, and the connectivity rows against the
/// rules of `raw.vehicle`, `raw.vehicle_signal`, and
/// `raw.connectivity_event` (module documentation). Every signal is
/// checked, also a signal of a field that the view does not read.
fn check_rows(scenario: &Scenario) -> Result<(), &'static str> {
    let mut vehicles = std::collections::HashSet::new();
    if !scenario.vehicles.iter().all(|id| vehicles.insert(*id)) {
        return Err("duplicate_vehicle");
    }
    let mut signal_keys = std::collections::HashSet::new();
    for signal in &scenario.signals {
        if !vehicles.contains(&signal.vehicle_id) {
            return Err("unknown_vehicle");
        }
        let column = value_column(signal).ok_or("bad_value_columns")?;
        if signal.value_type <= 0 {
            return Err("bad_value_type");
        }
        if required_column(signal.value_type) != Some(column) {
            return Err("value_column_mismatch");
        }
        let key = (
            signal.vehicle_id,
            signal.field,
            signal.created_at.timestamp(),
        );
        if !signal_keys.insert(key) {
            return Err("duplicate_signal_key");
        }
    }
    let mut connectivity_keys = std::collections::HashSet::new();
    for row in &scenario.connectivity {
        let key = (
            uuid(&row.connection_id)?,
            row.status,
            row.created_at.timestamp(),
        );
        if !connectivity_keys.insert(key) {
            return Err("duplicate_connectivity_key");
        }
    }
    Ok(())
}

/// One event row with the rules `ingest_event_kind`,
/// `ingest_event_window`, `ingest_event_count`, and
/// `ingest_event_window_order` of `raw.ingest_event`. The caller checks
/// the unique key `ingest_event_key`.
fn event(json: &EventJson) -> Result<EventRow, &'static str> {
    let kind = EventKind::parse(&json.kind).map_err(|_| "unknown_kind")?;
    let counted = matches!(
        kind,
        EventKind::RecordsDropped | EventKind::ZmqShortfall | EventKind::GateFull
    );
    let values = usize::from(json.dropped_count.is_some())
        + usize::from(json.first_received_at.is_some())
        + usize::from(json.last_received_at.is_some());
    if values != if counted { 3 } else { 0 } {
        return Err("bad_event_window");
    }
    if json.dropped_count.is_some_and(|count| count <= 0) {
        return Err("bad_event_count");
    }
    if let (Some(first), Some(last)) = (json.first_received_at, json.last_received_at)
        && first.timestamp() > last.timestamp()
    {
        return Err("bad_event_window_order");
    }
    Ok(EventRow {
        run_id: uuid(&json.run_id)?,
        occurred_at: json.occurred_at.timestamp(),
        kind,
        reason: json.reason.clone(),
        dropped_count: json.dropped_count,
        first_received_at: json.first_received_at.map(Instant::timestamp),
        last_received_at: json.last_received_at.map(Instant::timestamp),
    })
}

#[cfg(test)]
mod tests {
    use telemetry_web_domain::battery::{RangeKey, VehicleCount, derive_view};

    use super::*;
    use crate::test_support::TempDir;

    const SCENARIO: &str = r#"{
        "now": "2026-11-27T12:00:00Z",
        "vehicles": [1],
        "signals": [
            {"vehicle_id": 1, "field": 8, "created_at": "2026-11-27T11:00:00Z",
             "received_at": "2026-11-27T11:00:01Z", "value_type": 5, "float_value": 80.5},
            {"vehicle_id": 1, "field": 8, "created_at": "2026-11-27T11:01:00Z",
             "received_at": "2026-11-27T11:01:01Z", "value_type": 5, "float_value": "NaN"},
            {"vehicle_id": 1, "field": 8, "created_at": "2026-11-27T11:02:00Z",
             "received_at": "2026-11-27T11:02:01Z", "value_type": 10, "bool_value": true}
        ],
        "connectivity": [
            {"vehicle_id": 1, "connection_id": "7f1c2a3e-0000-4000-8000-000000000001",
             "created_at": "2026-11-27T10:59:00Z", "status": 1}
        ],
        "ingest_events": [
            {"run_id": "5b0e9d4c-0000-4000-8000-000000000001",
             "occurred_at": "2026-11-27T08:00:00Z", "kind": "started"}
        ]
    }"#;

    fn request(name: &str) -> SourceRequest {
        SourceRequest {
            key: RangeKey::Day,
            scenario: Some(name.to_owned()),
        }
    }

    async fn read(text: &str) -> Result<Snapshot, SourceError> {
        let dir = TempDir::new();
        dir.write("case.json", text.as_bytes());
        FakeSource::new(Some(dir.path().to_path_buf()))
            .read(&request("case"))
            .await
    }

    #[test]
    fn the_cookie_names_the_scenario() {
        assert_eq!(
            scenario_name(["a=1; telemetry_test_scenario=asleep-2d; b=2"]),
            Some("asleep-2d".to_owned())
        );
        assert_eq!(scenario_name(["telemetry_test_scenario=../etc"]), None);
        assert_eq!(scenario_name(["telemetry_test_scenario="]), None);
        assert_eq!(scenario_name(["other=x"]), None);
        assert_eq!(scenario_name(std::iter::empty()), None);
    }

    #[tokio::test]
    async fn a_scenario_goes_through_the_reference_rules() {
        let snapshot = read(SCENARIO).await.unwrap();
        assert_eq!(snapshot.vehicles, VehicleCount::One(1));
        assert_eq!(snapshot.series.soc.points.len(), 1);
        assert_eq!(snapshot.series.soc.excluded.invalid_marker, 1);
        assert_eq!(snapshot.series.soc.excluded.not_numeric, 1);
        assert_eq!(
            snapshot.events.restarts.items,
            ["2026-11-27T08:00:00Z".parse::<jiff::Timestamp>().unwrap()]
        );
        assert!(derive_view(&snapshot).is_ok());
    }

    /// Every scenario file of `e2e/scenarios` loads; the files of this
    /// slice (`s3-*.json`) also give the state that their name names.
    #[tokio::test]
    async fn every_e2e_scenario_file_is_valid_and_gives_its_state() {
        use telemetry_web_domain::battery::{Connection, Freshness, View};

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e/scenarios");
        let source = FakeSource::new(Some(dir.clone()));
        let mut checked = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            let result = source.read(&request(&name)).await;
            assert!(
                !matches!(result, Err(SourceError::Internal("scenario_invalid"))),
                "{name}"
            );
            let view = result.map(|snapshot| derive_view(&snapshot).unwrap());
            let available = |view: &Result<View, SourceError>| match view {
                Ok(View::Available(available)) => Some(available.as_ref().clone()),
                _ => None,
            };
            match name.as_str() {
                "s3-no-data-yet" => assert!(matches!(view, Ok(View::NoDataYet { .. }))),
                "s3-data" => {
                    let view = available(&view).unwrap();
                    assert_eq!(view.freshness, Freshness::Current);
                    assert!(matches!(view.connection, Connection::Connected { .. }));
                    assert_eq!(view.series.soc.segments.len(), 1);
                    assert_eq!(view.gaps.items, []);
                }
                "s3-asleep" => {
                    let view = available(&view).unwrap();
                    assert_eq!(view.freshness, Freshness::NotExpected);
                    assert!(matches!(view.connection, Connection::Disconnected { .. }));
                    assert_eq!(view.series.soc.segments, Vec::<Vec<_>>::new());
                    assert!(view.series.soc.carry_in.is_some());
                }
                "s3-stale" => {
                    let view = available(&view).unwrap();
                    assert_eq!(view.freshness, Freshness::Overdue);
                    assert!(matches!(view.connection, Connection::Connected { .. }));
                }
                "s3-gap" => {
                    let view = available(&view).unwrap();
                    assert!(matches!(view.connection, Connection::Connected { .. }));
                    assert_eq!(view.gaps.items.len(), 1);
                    assert_eq!(view.series.soc.segments.len(), 2);
                }
                "s3-unavailable" => {
                    assert_eq!(
                        view.unwrap_err(),
                        SourceError::Unavailable("scenario_fault")
                    );
                }
                _ => continue,
            }
            checked.push(name);
        }
        checked.sort();
        assert_eq!(
            checked,
            [
                "s3-asleep",
                "s3-data",
                "s3-gap",
                "s3-no-data-yet",
                "s3-stale",
                "s3-unavailable"
            ]
        );
    }

    #[tokio::test]
    async fn faults_map_to_their_errors() {
        for (fault, expected) in [
            (
                "database_unavailable",
                SourceError::Unavailable("scenario_fault"),
            ),
            ("service_busy", SourceError::Busy),
            ("timeout", SourceError::Timeout("scenario_fault")),
            ("internal_error", SourceError::Internal("scenario_fault")),
        ] {
            let text =
                format!(r#"{{"now": "2026-11-27T12:00:00Z", "vehicles": [], "fault": "{fault}"}}"#);
            assert_eq!(read(&text).await.unwrap_err(), expected);
        }
    }

    #[tokio::test]
    async fn a_broken_scenario_fails_loudly() {
        for text in [
            r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [], "typo": 1}"#,
            r#"{"vehicles": []}"#,
            r#"{"now": "2026-11-27T12:00:00+02:00", "vehicles": []}"#,
            r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [], "delay_ms": 10001}"#,
            r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [1], "signals": [{"vehicle_id": 1, "field": 8,
                "created_at": "2026-11-27T11:00:00Z", "received_at": "2026-11-27T11:00:00Z",
                "value_type": 5, "float_value": 1.0, "int_value": 1}]}"#,
            r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [1], "signals": [{"vehicle_id": 2, "field": 8,
                "created_at": "2026-11-27T11:00:00Z", "received_at": "2026-11-27T11:00:00Z",
                "value_type": 5, "float_value": 1.0}]}"#,
            r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [1], "ingest_events": [{"run_id":
                "5b0e9d4c-0000-4000-8000-000000000001", "occurred_at": "2026-11-27T08:00:00Z",
                "kind": "gate_full"}]}"#,
            r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [1], "ingest_events": [
                {"run_id": "5b0e9d4c-0000-4000-8000-000000000001", "occurred_at": "2026-11-27T08:00:00Z", "kind": "started"},
                {"run_id": "5b0e9d4c-0000-4000-8000-000000000001", "occurred_at": "2026-11-27T08:00:00Z", "kind": "started"}]}"#,
            r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [1], "ingest_events": [{"run_id": "x",
                "occurred_at": "2026-11-27T08:00:00Z", "kind": "started"}]}"#,
        ] {
            assert_eq!(
                read(text).await.unwrap_err(),
                SourceError::Internal("scenario_invalid"),
                "{text}"
            );
        }
    }

    /// A scenario of vehicle 1 with the given rows, each a JSON text.
    fn rows(signals: &[&str], connectivity: &[&str], events: &[&str]) -> String {
        format!(
            r#"{{"now": "2026-11-27T12:00:00Z", "vehicles": [1], "signals": [{}],
                "connectivity": [{}], "ingest_events": [{}]}}"#,
            signals.join(","),
            connectivity.join(","),
            events.join(",")
        )
    }

    /// A signal of vehicle 1 with the given field, time, type, and value
    /// column, for example `"int_value": 1`.
    fn signal(field: i32, created_at: &str, value_type: i16, value: &str) -> String {
        format!(
            r#"{{"vehicle_id": 1, "field": {field}, "created_at": "{created_at}",
                "received_at": "2026-11-27T11:59:00Z", "value_type": {value_type}, {value}}}"#
        )
    }

    fn connection(id: &str, created_at: &str, status: i32) -> String {
        format!(
            r#"{{"vehicle_id": 1, "connection_id": "7f1c2a3e-0000-4000-8000-00000000000{id}",
                "created_at": "{created_at}", "status": {status}}}"#
        )
    }

    fn counted(count: i64, first: &str, last: &str) -> String {
        format!(
            r#"{{"run_id": "5b0e9d4c-0000-4000-8000-000000000001",
                "occurred_at": "2026-11-27T10:00:00Z", "kind": "gate_full",
                "dropped_count": {count}, "first_received_at": "{first}",
                "last_received_at": "{last}"}}"#
        )
    }

    const T1: &str = "2026-11-27T11:00:00Z";
    const T2: &str = "2026-11-27T11:01:00Z";

    async fn rejected(text: &str) {
        assert_eq!(
            read(text).await.unwrap_err(),
            SourceError::Internal("scenario_invalid"),
            "{text}"
        );
    }

    async fn accepted(text: &str) {
        assert!(read(text).await.is_ok(), "{text}");
    }

    /// `vehicle_signal_value_column`: each `value_type` needs its column.
    /// The pairs are written out from the CASE of the migration.
    #[tokio::test]
    async fn a_signal_needs_the_value_column_of_its_type() {
        let int = r#""int_value": 1"#;
        let float = r#""float_value": 1.5"#;
        let text = r#""text_value": "x""#;
        let boolean = r#""bool_value": true"#;
        for (value_type, column) in [
            (1, text),
            (2, int),
            (3, int),
            (4, float),
            (5, float),
            (6, boolean),
            (10, boolean),
            (8, int),
            (99, int),
        ] {
            // Field 9 is a field that the view does not read: the table
            // rule holds for every field.
            accepted(&rows(&[&signal(8, T1, value_type, column)], &[], &[])).await;
            accepted(&rows(&[&signal(9, T1, value_type, column)], &[], &[])).await;
        }
        for (value_type, column) in [
            (1, int),
            (2, float),
            (3, text),
            (4, int),
            (5, boolean),
            (6, int),
            (10, float),
            (8, text),
            (99, boolean),
            (7, float),
            (21, text),
            (31, int),
            (39, text),
            (0, int),
            (-1, int),
        ] {
            rejected(&rows(&[&signal(8, T1, value_type, column)], &[], &[])).await;
            rejected(&rows(&[&signal(9, T1, value_type, column)], &[], &[])).await;
        }
    }

    /// `vehicle_signal_pkey`: one row for each vehicle, field, and
    /// `created_at`.
    #[tokio::test]
    async fn two_signals_with_one_key_are_rejected() {
        let value = r#""float_value": 1.5"#;
        let other = r#""float_value": 2.5"#;
        rejected(&rows(
            &[&signal(8, T1, 5, value), &signal(8, T1, 5, other)],
            &[],
            &[],
        ))
        .await;
        accepted(&rows(
            &[&signal(8, T1, 5, value), &signal(8, T2, 5, other)],
            &[],
            &[],
        ))
        .await;
        accepted(&rows(
            &[&signal(8, T1, 5, value), &signal(158, T1, 5, other)],
            &[],
            &[],
        ))
        .await;
    }

    /// `connectivity_event_pkey`: one row for each connection, status, and
    /// `created_at`.
    #[tokio::test]
    async fn two_connectivity_rows_with_one_key_are_rejected() {
        rejected(&rows(
            &[],
            &[&connection("1", T1, 1), &connection("1", T1, 1)],
            &[],
        ))
        .await;
        accepted(&rows(
            &[],
            &[&connection("1", T1, 1), &connection("1", T1, 2)],
            &[],
        ))
        .await;
        accepted(&rows(
            &[],
            &[&connection("1", T1, 1), &connection("1", T2, 1)],
            &[],
        ))
        .await;
        accepted(&rows(
            &[],
            &[&connection("1", T1, 1), &connection("2", T1, 1)],
            &[],
        ))
        .await;
    }

    /// `vehicle_pkey`: one row for each vehicle id.
    #[tokio::test]
    async fn a_vehicle_id_two_times_is_rejected() {
        rejected(r#"{"now": "2026-11-27T12:00:00Z", "vehicles": [1, 1]}"#).await;
    }

    /// `ingest_event_count` and `ingest_event_window_order`.
    #[tokio::test]
    async fn a_counted_event_needs_a_count_and_ordered_times() {
        let started = r#"{"run_id": "5b0e9d4c-0000-4000-8000-000000000001",
            "occurred_at": "2026-11-27T08:00:00Z", "kind": "started"}"#;
        rejected(&rows(&[], &[], &[started, &counted(0, T1, T2)])).await;
        rejected(&rows(&[], &[], &[started, &counted(-1, T1, T2)])).await;
        rejected(&rows(&[], &[], &[started, &counted(1, T2, T1)])).await;
        accepted(&rows(&[], &[], &[started, &counted(1, T1, T2)])).await;
        accepted(&rows(&[], &[], &[started, &counted(1, T1, T1)])).await;
    }

    #[tokio::test]
    async fn no_directory_or_no_cookie_is_an_internal_error() {
        let source = FakeSource::new(None);
        assert_eq!(
            source.read(&request("case")).await.unwrap_err(),
            SourceError::Internal("scenario_invalid")
        );
        let dir = TempDir::new();
        let source = FakeSource::new(Some(dir.path().to_path_buf()));
        let no_cookie = SourceRequest {
            key: RangeKey::Day,
            scenario: None,
        };
        assert_eq!(
            source.read(&no_cookie).await.unwrap_err(),
            SourceError::Internal("scenario_invalid")
        );
    }
}
