//! HTTP tests of `GET /api/v1/battery`: real requests through the real
//! router and middleware, with a source that returns a fixed snapshot.
//!
//! Each body is checked against the committed `contract/openapi.json`
//! with a small validator of the schema forms that the contract uses.
//! The expected states come from the acceptance criteria of slice S3:
//! a client tells apart no data yet, data, no data in the range, a gap,
//! asleep or offline, stale, and unavailable.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{Request, StatusCode};
use jiff::Timestamp;
use serde_json::Value;
use telemetry_web_domain::battery::{
    ConnectivityRow, EventKind, EventRow, Field, PerField, RangeKey, RawValue, RowTimes,
    SeriesPair, SignalRow, Snapshot, VehicleCount, Window, newest_connectivity, select_events,
    series_snapshot, tile_snapshot,
};
use tower::ServiceExt;

use super::source::{BatterySource, SnapshotFuture, SourceError, SourceRequest};
use super::{State, handler};
use crate::app::{self, AppState};
use crate::config::{StaticFiles, Version};
use crate::problem::{PROBLEM_CONTENT_TYPE, ProblemCode};
use crate::test_support::TempDir;

/// A source that the router tests of `crate::app` never reach.
#[derive(Debug)]
pub(super) struct Unreachable;

impl BatterySource for Unreachable {
    fn snapshot<'a>(&'a self, _request: &'a SourceRequest) -> SnapshotFuture<'a> {
        Box::pin(async { Err(SourceError::Internal("unreachable")) })
    }
}

/// Raw rows of a test, turned into a snapshot by the reference rules.
#[derive(Debug, Clone, Default)]
struct Rows {
    vehicles: Vec<i16>,
    signals: Vec<(Field, &'static str, f64)>,
    invalid: Vec<(Field, &'static str)>,
    connectivity: Vec<(u128, &'static str, i32)>,
    events: Vec<EventRow>,
}

const NOW: &str = "2026-11-27T12:00:00Z";

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

impl Rows {
    fn one_vehicle() -> Self {
        Self {
            vehicles: vec![1],
            ..Self::default()
        }
    }

    fn snapshot(&self, key: RangeKey) -> Snapshot {
        let window = Window::ending_at(key, at(NOW)).unwrap();
        let vehicles = match self.vehicles.as_slice() {
            [] => VehicleCount::None,
            [id] => VehicleCount::One(*id),
            _ => VehicleCount::Many,
        };
        let rows_of = |field: Field| -> Vec<SignalRow<'static>> {
            let usable = self
                .signals
                .iter()
                .filter(|(f, _, _)| *f == field)
                .map(|(_, t, v)| (t, 5, Some(*v), None));
            let invalid = self
                .invalid
                .iter()
                .filter(|(f, _)| *f == field)
                .map(|(_, t)| (t, 10, None, Some(true)));
            usable
                .chain(invalid)
                .map(|(time, value_type, float_value, bool_value)| SignalRow {
                    times: RowTimes {
                        created_at: at(time),
                        received_at: at(time),
                    },
                    value: RawValue {
                        value_type,
                        int_value: None,
                        float_value,
                        text_value: None,
                        bool_value,
                    },
                })
                .collect()
        };
        let mut tiles = PerField::default();
        for field in Field::ALL {
            *tiles.get_mut(field) = tile_snapshot(&window, &rows_of(field));
        }
        let events = select_events(&window, &self.events);
        let connectivity: Vec<ConnectivityRow> = self
            .connectivity
            .iter()
            .map(|(id, time, status)| ConnectivityRow {
                connection_id: *id,
                created_at: at(time),
                status: *status,
            })
            .collect();
        Snapshot {
            window,
            vehicles,
            tiles,
            series: SeriesPair {
                soc: series_snapshot(&window, &rows_of(Field::Soc)).unwrap(),
                nominal_full_pack_energy: series_snapshot(
                    &window,
                    &rows_of(Field::NominalFullPackEnergy),
                )
                .unwrap(),
            },
            connectivity: newest_connectivity(&connectivity, window.to()),
            events,
        }
    }
}

/// A source that answers each request with the snapshot of its rows, or
/// with an error, and counts the calls.
#[derive(Debug)]
struct Fixed {
    answer: Result<Rows, SourceError>,
    calls: AtomicUsize,
}

impl BatterySource for Fixed {
    fn snapshot<'a>(&'a self, request: &'a SourceRequest) -> SnapshotFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.answer.clone().map(|rows| rows.snapshot(request.key));
        Box::pin(async move { answer })
    }
}

struct Answer {
    status: StatusCode,
    content_type: String,
    cache_control: String,
    retry_after: Option<String>,
    body: Value,
}

async fn get(answer: Result<Rows, SourceError>, uri: &str) -> (Answer, Arc<Fixed>) {
    send(answer, axum::http::Method::GET, uri).await
}

/// The session of a test request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCase {
    /// A login just before the request.
    Valid,
    /// No session cookie.
    Missing,
    /// The valid session cookie and a second session cookie.
    TwoCookies,
    /// A login 31 days before the request: past the absolute lifetime.
    Expired,
}

const ORIGIN: &str = "https://telemetry.test";

/// Sends one request through the production router assembly
/// (`app::router`, with the session guard of `crate::auth`) and a source
/// that counts its calls.
async fn send_with(
    answer: Result<Rows, SourceError>,
    method: axum::http::Method,
    uri: &str,
    session: SessionCase,
) -> (Answer, Arc<Fixed>) {
    use crate::auth::clock::ShiftableClock;
    use crate::auth::origin::PublicOrigin;
    use crate::auth::store::SessionLimits;
    use crate::auth::tests::{FakeVerifier, PASSWORD};
    use crate::auth::throttle::ThrottleLimits;

    let source = Arc::new(Fixed {
        answer,
        calls: AtomicUsize::new(0),
    });
    let dir = TempDir::new();
    dir.write("index.html", b"<meta content=\"__TELEMETRY_VERSION__\">");
    let files = StaticFiles::load(dir.path()).unwrap();
    let clock = Arc::new(ShiftableClock::new(at(NOW)));
    let auth = crate::auth::State::from_parts(crate::auth::Parts {
        origin: PublicOrigin::parse(ORIGIN).unwrap(),
        clock: Arc::clone(&clock) as Arc<dyn crate::auth::clock::Clock>,
        verifier: Arc::new(FakeVerifier::default()),
        sessions: SessionLimits::OWNER,
        throttle: ThrottleLimits::DEFAULT,
    });
    let state = AppState::new(Version::unknown(), auth, State::with_source(source.clone()));
    let router = app::router(state, &files);

    let mut cookie = None;
    if session != SessionCase::Missing {
        let login = Request::builder()
            .method("POST")
            .uri("/api/v1/session")
            .header("origin", ORIGIN)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "password": PASSWORD }).to_string()))
            .unwrap();
        let response = router.clone().oneshot(login).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let pair = response
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        cookie = Some(pair);
    }
    if session == SessionCase::Expired {
        clock.advance(std::time::Duration::from_hours(31 * 24));
    }
    if session == SessionCase::TwoCookies {
        let other = format!("__Host-telemetry_session={}", "cd".repeat(32));
        cookie = cookie.map(|pair| format!("{pair}; {other}"));
    }
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        request = request.header(axum::http::header::COOKIE, cookie);
    }
    let response = router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let header = |name| {
        response
            .headers()
            .get(name)
            .map(|value: &axum::http::HeaderValue| value.to_str().unwrap().to_owned())
    };
    let content_type = header(CONTENT_TYPE).unwrap_or_default();
    let cache_control = header(CACHE_CONTROL).unwrap_or_default();
    let retry_after = header(RETRY_AFTER);
    let bytes = to_bytes(response.into_body(), 1 << 22).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    (
        Answer {
            status,
            content_type,
            cache_control,
            retry_after,
            body,
        },
        source,
    )
}

async fn send(
    answer: Result<Rows, SourceError>,
    method: axum::http::Method,
    uri: &str,
) -> (Answer, Arc<Fixed>) {
    send_with(answer, method, uri, SessionCase::Valid).await
}

/// The view of a successful request, checked against the contract.
async fn view(rows: Rows, range: &str) -> Value {
    let (answer, _) = get(Ok(rows), &format!("/api/v1/battery?range={range}")).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.content_type, "application/json");
    assert_eq!(answer.cache_control, "no-store");
    check(
        &answer.body,
        &json!({"$ref": "#/components/schemas/BatteryView"}),
    );
    answer.body
}

use serde_json::json;

fn contract() -> &'static Value {
    static CONTRACT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    CONTRACT.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/openapi.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    })
}

/// Checks `value` against `schema` for the forms of the contract:
/// `$ref`, `oneOf` (exactly one match), `type`, `enum`, `required`,
/// `properties` (no undeclared key), `items`, `minimum`, `maximum`, and
/// `format: date-time` (the strict wire instant).
fn check(value: &Value, schema: &Value) {
    assert!(matches(value, schema), "{value} does not match {schema}");
}

fn matches(value: &Value, schema: &Value) -> bool {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.trim_start_matches("#/components/schemas/");
        return matches(value, &contract()["components"]["schemas"][name]);
    }
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        return options
            .iter()
            .filter(|option| matches(value, option))
            .count()
            == 1;
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        return false;
    }
    let type_ok = match schema.get("type").and_then(Value::as_str) {
        None => true,
        Some("null") => value.is_null(),
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("number") => value.as_f64().is_some_and(f64::is_finite),
        Some("integer") => value.is_u64() || value.is_i64(),
        Some("array") => value.is_array(),
        Some("object") => value.is_object(),
        Some(_) => false,
    };
    if !type_ok {
        return false;
    }
    if let Some(number) = value.as_f64() {
        if schema
            .get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|min| number < min)
        {
            return false;
        }
        if schema
            .get("maximum")
            .and_then(Value::as_f64)
            .is_some_and(|max| number > max)
        {
            return false;
        }
    }
    if schema.get("format").and_then(Value::as_str) == Some("date-time")
        && value
            .as_str()
            .is_none_or(|text| text.parse::<crate::wire::Instant>().is_err())
    {
        return false;
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array())
        && !array.iter().all(|item| matches(item, items))
    {
        return false;
    }
    if let Some(object) = value.as_object() {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array)
            && !required
                .iter()
                .all(|key| key.as_str().is_some_and(|key| object.contains_key(key)))
        {
            return false;
        }
        if let Some(properties) = properties {
            for (key, item) in object {
                match properties.get(key) {
                    Some(property) if matches(item, property) => {}
                    _ => return false,
                }
            }
        }
    }
    true
}

async fn problem(error: SourceError, code: ProblemCode, retry: Option<&str>) {
    let (answer, _) = get(Err(error), "/api/v1/battery?range=24h").await;
    assert_eq!(answer.status, code.status());
    assert_eq!(answer.content_type, PROBLEM_CONTENT_TYPE);
    assert_eq!(answer.body["code"], code.as_str());
    assert_eq!(answer.retry_after.as_deref(), retry);
    check(
        &answer.body,
        &json!({"$ref": "#/components/schemas/Problem"}),
    );
}

#[tokio::test]
async fn unavailable_states_are_problems_never_empty_results() {
    problem(
        SourceError::Unavailable("connect_refused"),
        ProblemCode::DatabaseUnavailable,
        Some("30"),
    )
    .await;
    problem(SourceError::Busy, ProblemCode::ServiceBusy, Some("5")).await;
    problem(
        SourceError::Timeout("read_deadline"),
        ProblemCode::Timeout,
        Some("5"),
    )
    .await;
    problem(
        SourceError::Internal("authentication"),
        ProblemCode::InternalError,
        None,
    )
    .await;
    problem(
        SourceError::Contract(telemetry_web_domain::battery::ContractViolation::BadEventRow),
        ProblemCode::DataContractViolation,
        None,
    )
    .await;
}

#[tokio::test]
async fn a_bad_range_is_invalid_request_and_never_reaches_the_source() {
    for uri in [
        "/api/v1/battery",
        "/api/v1/battery?range=2d",
        "/api/v1/battery?range=7d&x=1",
        "/api/v1/battery?range=7D",
    ] {
        let (answer, source) = get(Ok(Rows::one_vehicle()), uri).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(answer.body["code"], "invalid_request");
        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn no_vehicle_or_no_battery_row_is_no_data_yet() {
    for rows in [Rows::default(), Rows::one_vehicle()] {
        let body = view(rows, "7d").await;
        assert_eq!(body["state"], "no_data_yet");
        assert_eq!(body["generated_at"], NOW);
        assert_eq!(body["range"]["from"], "2026-11-20T12:00:00Z");
        assert_eq!(body["range"]["bucket_seconds"], 600);
    }
}

#[tokio::test]
async fn a_second_vehicle_is_a_data_contract_violation() {
    let rows = Rows {
        vehicles: vec![1, 2],
        ..Rows::default()
    };
    let (answer, _) = get(Ok(rows), "/api/v1/battery?range=7d").await;
    assert_eq!(answer.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(answer.body["code"], "data_contract_violation");
}

#[tokio::test]
async fn data_in_the_range_is_available_and_current() {
    let rows = Rows {
        signals: vec![
            (Field::Soc, "2026-11-27T11:50:00Z", 80.5),
            (Field::Soc, "2026-11-27T11:51:00Z", 80.0),
            (Field::OutsideTemp, "2026-11-27T11:50:00Z", -3.5),
        ],
        connectivity: vec![(7, "2026-11-27T11:49:00Z", 1)],
        ..Rows::one_vehicle()
    };
    let body = view(rows, "24h").await;
    assert_eq!(body["state"], "available");
    assert_eq!(body["status"]["freshness"], "current");
    assert_eq!(
        body["status"]["connection"],
        json!({"state": "connected", "at": "2026-11-27T11:49:00Z"})
    );
    assert_eq!(
        body["charts"]["soc"]["segments"][0]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(body["tiles"]["soc"]["value"], 80.0);
    assert_eq!(
        body["tiles"]["outside_temp"]["unit"],
        json!({"symbol": "degree_celsius", "source": "assumed"})
    );
    assert_eq!(
        body["tiles"]["energy_remaining"],
        json!({"state": "missing", "unit": {"symbol": "kilowatt_hour", "source": "documented"}, "latest_unusable_at": null})
    );
    assert_eq!(body["gaps"], json!({"items": [], "truncated": false}));
    assert_eq!(body["restarts"], json!({"items": [], "truncated": false}));
}

#[tokio::test]
async fn asleep_for_the_whole_range_is_no_data_in_range_and_not_expected() {
    let rows = Rows {
        signals: vec![(Field::Soc, "2026-11-13T08:00:00Z", 81.0)],
        connectivity: vec![
            (7, "2026-11-13T07:59:00Z", 1),
            (7, "2026-11-13T08:30:00Z", 2),
        ],
        ..Rows::one_vehicle()
    };
    let body = view(rows, "7d").await;
    assert_eq!(body["state"], "available");
    assert_eq!(body["charts"]["soc"]["segments"], json!([]));
    assert_eq!(body["charts"]["soc"]["carry_in"]["last"], 81.0);
    assert_eq!(body["status"]["freshness"], "not_expected");
    assert_eq!(
        body["status"]["connection"],
        json!({"state": "disconnected", "at": "2026-11-13T08:30:00Z"})
    );
    assert_eq!(body["gaps"]["items"], json!([]));
}

#[tokio::test]
async fn old_data_while_connected_is_overdue() {
    let rows = Rows {
        signals: vec![(Field::Soc, "2026-11-27T10:00:00Z", 70.0)],
        connectivity: vec![(7, "2026-11-27T09:59:00Z", 1)],
        ..Rows::one_vehicle()
    };
    let body = view(rows, "24h").await;
    assert_eq!(body["status"]["freshness"], "overdue");
}

#[tokio::test]
async fn a_loss_is_a_gap_that_splits_the_line() {
    let rows = Rows {
        signals: vec![
            (Field::Soc, "2026-11-27T09:00:00Z", 60.0),
            (Field::Soc, "2026-11-27T11:00:00Z", 58.0),
        ],
        invalid: vec![(Field::Soc, "2026-11-27T11:30:00Z")],
        events: vec![EventRow {
            run_id: 0xabc,
            occurred_at: at("2026-11-27T10:15:00Z"),
            kind: EventKind::RecordsDropped,
            reason: Some("queue_full".to_owned()),
            dropped_count: Some(12),
            first_received_at: Some(at("2026-11-27T10:00:00Z")),
            last_received_at: Some(at("2026-11-27T10:10:00Z")),
        }],
        ..Rows::one_vehicle()
    };
    let body = view(rows, "24h").await;
    assert_eq!(
        body["charts"]["soc"]["segments"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        body["charts"]["soc"]["excluded"],
        json!({"invalid_marker": 1, "not_numeric": 0})
    );
    assert_eq!(
        body["gaps"]["items"][0],
        json!({
            "kind": "queue_full",
            "from": "2026-11-27T10:00:00Z",
            "to": "2026-11-27T10:10:00Z",
            "count": {"value": 12, "of": "records"},
        })
    );
    assert_eq!(
        body["tiles"]["soc"]["newer_unusable_at"],
        "2026-11-27T11:30:00Z"
    );
}

#[tokio::test]
async fn every_range_gives_a_valid_body() {
    for range in ["24h", "7d", "30d", "90d", "1y"] {
        let rows = Rows {
            signals: vec![
                (Field::NominalFullPackEnergy, "2025-12-01T00:00:00Z", 79.0),
                (Field::NominalFullPackEnergy, "2026-11-27T11:00:00Z", 78.0),
            ],
            ..Rows::one_vehicle()
        };
        let body = view(rows, range).await;
        assert_eq!(body["range"]["key"], range);
    }
}

#[tokio::test]
async fn another_method_is_405_and_never_reaches_the_source() {
    for method in [axum::http::Method::POST, axum::http::Method::DELETE] {
        let (answer, source) =
            send(Ok(Rows::one_vehicle()), method, "/api/v1/battery?range=7d").await;
        assert_eq!(answer.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(answer.body["code"], "method_not_allowed");
        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    }
}

/// Each variant of `SourceError` with its status, code, and `Retry-After`,
/// written out by hand. The statuses and codes are the 500 and 503
/// responses of `GET /api/v1/battery` in `contract/openapi.json`.
#[test]
fn the_problem_mapping_is_complete() {
    use axum::response::IntoResponse;

    // A new variant does not compile here until this test lists it.
    let listed = |error: &SourceError| match error {
        SourceError::Unavailable(_)
        | SourceError::Busy
        | SourceError::Timeout(_)
        | SourceError::Contract(_)
        | SourceError::Internal(_) => (),
    };
    for (error, status, code, retry_after) in [
        (
            SourceError::Unavailable("connect_refused"),
            503,
            "database_unavailable",
            Some(30),
        ),
        (SourceError::Busy, 503, "service_busy", Some(5)),
        (
            SourceError::Timeout("read_deadline"),
            503,
            "timeout",
            Some(5),
        ),
        (
            SourceError::Contract(telemetry_web_domain::battery::ContractViolation::BadEventRow),
            500,
            "data_contract_violation",
            None,
        ),
        (
            SourceError::Internal("authentication"),
            500,
            "internal_error",
            None,
        ),
    ] {
        listed(&error);
        let problem = handler::problem(error);
        let body = serde_json::to_value(&problem).unwrap();
        assert_eq!(body["code"], code, "{error:?}");
        assert_eq!(body["status"], status, "{error:?}");
        assert_eq!(
            body["retry_after_seconds"].as_u64(),
            retry_after,
            "{error:?}"
        );
        let response = problem.into_response();
        assert_eq!(response.status().as_u16(), status, "{error:?}");
        assert_eq!(
            response.headers().get(RETRY_AFTER).map(|value| value
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()),
            retry_after,
            "{error:?}"
        );
    }
}

/// The whole body of one view, written out by hand from the rows, so a
/// change of any mapped field (a swapped minimum and maximum, a lost
/// count, a wrong clock, a wrong unit) fails here.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one hand-written body of a whole view reads better than parts"
)]
async fn the_view_maps_each_field_to_the_contract() {
    let rows = Rows {
        signals: vec![
            (Field::Soc, "2026-11-26T10:00:00Z", 90.0),
            (Field::Soc, "2026-11-27T11:50:10Z", 81.0),
            (Field::Soc, "2026-11-27T11:50:40Z", 80.5),
            (Field::Soc, "2026-11-27T11:50:20Z", 82.0),
            (Field::NominalFullPackEnergy, "2026-11-20T00:00:00Z", 79.0),
            (Field::NominalFullPackEnergy, "2026-11-27T11:00:00Z", 78.5),
            (Field::EnergyRemaining, "2026-11-27T11:51:00Z", 61.25),
        ],
        invalid: vec![(Field::OutsideTemp, "2026-11-27T11:52:00Z")],
        connectivity: vec![(7, "2026-11-27T11:49:00Z", 1)],
        events: vec![
            EventRow {
                run_id: 0xabc,
                occurred_at: at("2026-11-27T10:00:00Z"),
                kind: EventKind::ZmqShortfall,
                reason: None,
                dropped_count: Some(3),
                first_received_at: Some(at("2026-11-27T09:58:00Z")),
                last_received_at: Some(at("2026-11-27T09:59:00Z")),
            },
            EventRow {
                run_id: 0xabd,
                occurred_at: at("2026-11-27T10:30:00Z"),
                kind: EventKind::Started,
                reason: None,
                dropped_count: None,
                first_received_at: None,
                last_received_at: None,
            },
        ],
        ..Rows::one_vehicle()
    };
    let body = view(rows, "24h").await;
    let percent = json!({"symbol": "percent", "source": "documented"});
    let kwh = json!({"symbol": "kilowatt_hour", "source": "documented"});
    assert_eq!(
        body,
        json!({
            "state": "available",
            "generated_at": "2026-11-27T12:00:00Z",
            "range": {
                "key": "24h",
                "from": "2026-11-26T12:00:00Z",
                "to": "2026-11-27T12:00:00Z",
                "bucket_seconds": 60
            },
            "status": {
                "connection": {"state": "connected", "at": "2026-11-27T11:49:00Z"},
                "latest_data_at": "2026-11-27T11:52:00Z",
                "latest_received_at": "2026-11-27T11:52:00Z",
                "freshness": "current"
            },
            "tiles": {
                "soc": {
                    "state": "value", "value": 80.5, "unit": percent,
                    "measured_at": "2026-11-27T11:50:40Z", "newer_unusable_at": null
                },
                "energy_remaining": {
                    "state": "value", "value": 61.25, "unit": kwh,
                    "measured_at": "2026-11-27T11:51:00Z", "newer_unusable_at": null
                },
                "outside_temp": {
                    "state": "missing",
                    "unit": {"symbol": "degree_celsius", "source": "assumed"},
                    "latest_unusable_at": "2026-11-27T11:52:00Z"
                },
                "nominal_full_pack_energy": {
                    "reading": {
                        "state": "value", "value": 78.5, "unit": kwh,
                        "measured_at": "2026-11-27T11:00:00Z", "newer_unusable_at": null
                    },
                    "change": {
                        "state": "value", "delta": -0.5,
                        "from_value": 79.0, "from_at": "2026-11-20T00:00:00Z",
                        "to_value": 78.5, "to_at": "2026-11-27T11:00:00Z"
                    }
                }
            },
            "charts": {
                "soc": {
                    "unit": percent,
                    "carry_in": {"at": "2026-11-26T10:00:00Z", "last": 90.0, "min": 90.0, "max": 90.0, "count": 1},
                    "segments": [[
                        {"at": "2026-11-27T11:50:40Z", "last": 80.5, "min": 80.5, "max": 82.0, "count": 3}
                    ]],
                    "excluded": {"invalid_marker": 0, "not_numeric": 0}
                },
                "nominal_full_pack_energy": {
                    "unit": kwh,
                    "carry_in": {"at": "2026-11-20T00:00:00Z", "last": 79.0, "min": 79.0, "max": 79.0, "count": 1},
                    "segments": [[
                        {"at": "2026-11-27T11:00:00Z", "last": 78.5, "min": 78.5, "max": 78.5, "count": 1}
                    ]],
                    "excluded": {"invalid_marker": 0, "not_numeric": 0}
                }
            },
            "gaps": {
                "items": [{
                    "kind": "zmq_shortfall",
                    "from": "2026-11-27T09:58:00Z", "to": "2026-11-27T09:59:00Z",
                    "count": {"value": 3, "of": "messages"}
                }],
                "truncated": false
            },
            "restarts": {
                "items": ["2026-11-27T10:30:00Z"],
                "truncated": false
            }
        })
    );
}

/// The session guard of `crate::auth` runs before the battery handler in
/// the production router assembly: a request without a valid session
/// never reaches the data source, so it never reaches the database.
#[tokio::test]
async fn only_a_valid_session_reaches_the_source() {
    let uri = "/api/v1/battery?range=7d";
    for (session, code) in [
        (SessionCase::Missing, ProblemCode::Unauthenticated),
        (SessionCase::Expired, ProblemCode::Unauthenticated),
        (SessionCase::TwoCookies, ProblemCode::InvalidRequest),
    ] {
        let (answer, source) = send_with(
            Ok(Rows::one_vehicle()),
            axum::http::Method::GET,
            uri,
            session,
        )
        .await;
        assert_eq!(answer.status, code.status(), "{session:?}");
        assert_eq!(answer.body["code"], code.as_str(), "{session:?}");
        assert_eq!(answer.content_type, PROBLEM_CONTENT_TYPE);
        check(
            &answer.body,
            &json!({"$ref": "#/components/schemas/Problem"}),
        );
        assert_eq!(source.calls.load(Ordering::SeqCst), 0, "{session:?}");
    }
    let (answer, source) = send_with(
        Ok(Rows::one_vehicle()),
        axum::http::Method::GET,
        uri,
        SessionCase::Valid,
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

/// The contract declares each answer of the guard for the battery
/// operation: 400 for two session cookies and 401 without a session; a
/// GET needs no origin, so no 403.
#[test]
fn the_contract_declares_the_guard_answers_of_the_battery_operation() {
    let responses = &contract()["paths"]["/api/v1/battery"]["get"]["responses"];
    for status in ["200", "400", "401", "413", "500", "503"] {
        assert!(responses.get(status).is_some(), "{status}");
    }
    assert!(responses.get("403").is_none());
    for status in ["400", "401"] {
        assert_eq!(
            responses[status]["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem"
        );
    }
    assert_eq!(
        crate::auth::responses::guard_statuses("/api/v1/battery", "get"),
        ["400", "401"]
    );
}
