//! HTTP tests of `GET /api/v1/consumption` through the production router
//! assembly, with a source that returns a fixed snapshot and counts calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Request, StatusCode};
use jiff::Timestamp;
use serde_json::{Value, json};
use telemetry_web_domain::battery::{EventSnapshot, PerField, RangeKey, VehicleCount, Window};
use telemetry_web_domain::consumption::{
    DriveSample, Gear, MAX_GEAR_ROWS, SampleInput, Snapshot, ValueField, gear_input,
};
use tower::ServiceExt;

use super::{ConsumptionFuture, ConsumptionSource, State};
use crate::app::{self, AppState};
use crate::battery::source::{SourceError, SourceRequest};
use crate::config::{StaticFiles, Version};
use crate::problem::{PROBLEM_CONTENT_TYPE, ProblemCode};
use crate::test_support::TempDir;

const NOW: &str = "2026-11-27T12:00:00Z";
const ORIGIN: &str = "https://telemetry.test";

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

#[derive(Debug)]
struct Fixed {
    answer: Result<(), SourceError>,
    calls: AtomicUsize,
}

/// One drive of 12.5 mi and 4 kWh, and one ongoing drive.
fn snapshot(key: RangeKey) -> Snapshot {
    let window = Window::ending_at(key, at(NOW)).unwrap();
    let newest_first = vec![
        (at("2026-11-27T11:50:00Z"), Gear::Moving),
        (at("2026-11-27T10:30:00Z"), Gear::Park),
        (at("2026-11-27T10:00:00Z"), Gear::Moving),
    ];
    let sample = |field, t: &str, value| DriveSample {
        field,
        at: at(t),
        value,
    };
    Snapshot {
        window,
        vehicles: VehicleCount::One(1),
        gears: gear_input(Some(Gear::Park), newest_first, MAX_GEAR_ROWS),
        samples: SampleInput {
            samples: vec![
                sample(ValueField::Odometer, "2026-11-27T10:00:00Z", 1000.0),
                sample(ValueField::Odometer, "2026-11-27T10:30:00Z", 1012.5),
                sample(ValueField::EnergyRemaining, "2026-11-27T10:00:00Z", 60.0),
                sample(ValueField::EnergyRemaining, "2026-11-27T10:30:00Z", 56.0),
            ],
            cut_at: None,
        },
        tiles: PerField::default(),
        connectivity: None,
        events: EventSnapshot::default(),
    }
}

impl ConsumptionSource for Fixed {
    fn consumption<'a>(&'a self, request: &'a SourceRequest) -> ConsumptionFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.answer.map(|()| snapshot(request.key));
        Box::pin(async move { answer })
    }
}

async fn send(
    answer: Result<(), SourceError>,
    uri: &str,
    login: bool,
) -> (StatusCode, String, Value, usize) {
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
    let auth = crate::auth::State::from_parts(crate::auth::Parts {
        origin: PublicOrigin::parse(ORIGIN).unwrap(),
        clock: Arc::new(ShiftableClock::new(at(NOW))),
        verifier: Arc::new(FakeVerifier::default()),
        sessions: SessionLimits::OWNER,
        throttle: ThrottleLimits::DEFAULT,
    });
    let (battery, _) = crate::battery::unit_test_state();
    let state = AppState::new(
        Version::unknown(),
        auth,
        battery,
        State::with_source(source.clone()),
    );
    let router = app::router(state, &files);
    let mut request = Request::builder().uri(uri);
    if login {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/session")
                    .header("origin", ORIGIN)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = response.headers()[SET_COOKIE].to_str().unwrap();
        let pair = cookie.split(';').next().unwrap().to_owned();
        request = request.header(COOKIE, pair);
    }
    let response = router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .map_or_else(String::new, |v| v.to_str().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 1 << 22).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    (
        status,
        content_type,
        body,
        source.calls.load(Ordering::SeqCst),
    )
}

#[tokio::test]
async fn without_a_session_the_answer_is_401_and_the_source_is_not_read() {
    let (status, content_type, body, calls) =
        send(Ok(()), "/api/v1/consumption?range=7d", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(content_type, PROBLEM_CONTENT_TYPE);
    assert_eq!(body["code"], ProblemCode::Unauthenticated.as_str());
    assert_eq!(calls, 0);
}

#[tokio::test]
async fn the_drives_have_their_values_units_and_totals() {
    let (status, _, body, calls) = send(Ok(()), "/api/v1/consumption?range=7d", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls, 1);
    assert_eq!(body["generated_at"], NOW);
    assert_eq!(body["range"]["key"], "7d");
    assert_eq!(
        body["units"],
        json!({"distance": "mi", "energy": "kWh", "consumption": "Wh/km",
               "temperature": "°C", "speed": "km/h"})
    );
    assert_eq!(body["truncated"], false);
    // No battery row in the snapshot: no status, and no loss.
    assert_eq!(body["status"], Value::Null);
    assert_eq!(body["gaps"], json!({"items": [], "truncated": false}));
    let drives = body["drives"].as_array().unwrap();
    assert_eq!(drives.len(), 2);
    // Newest first: the ongoing drive has only its start.
    assert_eq!(
        drives[0],
        json!({"start": "2026-11-27T11:50:00Z", "end": null, "duration_seconds": null,
               "distance": null, "energy_used": null, "consumption": null,
               "mean_outside_temp": null, "mean_speed": null, "data_loss": false})
    );
    assert_eq!(drives[1]["end"], "2026-11-27T10:30:00Z");
    assert_eq!(drives[1]["duration_seconds"], 1800);
    assert_eq!(drives[1]["distance"], 12.5);
    assert_eq!(drives[1]["energy_used"], 4.0);
    assert_eq!(drives[1]["mean_outside_temp"], Value::Null);
    let wh_per_km = drives[1]["consumption"].as_f64().unwrap();
    assert!((wh_per_km - 198.839).abs() < 0.001, "{wh_per_km}");
    assert_eq!(body["totals"]["drives"], 1);
    assert_eq!(body["totals"]["distance"], 12.5);
    assert_eq!(body["totals"]["drives_without_consumption"], 0);
}

#[tokio::test]
async fn a_failed_read_is_a_problem_never_an_empty_list() {
    for (error, code) in [
        (
            SourceError::Unavailable("x"),
            ProblemCode::DatabaseUnavailable,
        ),
        (SourceError::Busy, ProblemCode::ServiceBusy),
        (SourceError::Timeout("x"), ProblemCode::Timeout),
        (SourceError::Internal("x"), ProblemCode::InternalError),
    ] {
        let (status, content_type, body, _) =
            send(Err(error), "/api/v1/consumption?range=24h", true).await;
        assert_eq!(status, code.status(), "{error:?}");
        assert_eq!(content_type, PROBLEM_CONTENT_TYPE);
        assert_eq!(body["code"], code.as_str());
        assert!(body.get("drives").is_none());
    }
}

#[tokio::test]
async fn a_bad_range_is_invalid_request_and_never_reaches_the_source() {
    let (status, _, body, calls) = send(Ok(()), "/api/v1/consumption?range=2d", true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], ProblemCode::InvalidRequest.as_str());
    assert_eq!(calls, 0);
}

#[test]
fn the_contract_declares_the_guard_answers() {
    assert_eq!(
        crate::auth::responses::guard_statuses("/api/v1/consumption", "get"),
        ["400", "401"]
    );
}
