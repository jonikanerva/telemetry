//! `GET /api/v1/battery`.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use telemetry_web_domain::battery::derive_view;
use tracing::warn;

use super::dto::{BatteryQuery, BatteryView};
use super::map;
use super::source::{SourceError, SourceRequest};
use crate::problem::{Problem, ProblemCode};

/// Returns the battery view of one range.
///
/// The session guard (`crate::auth`) runs first: without a valid session
/// the answer is 401 `unauthenticated` (declared by
/// `auth::responses`), and no read starts.
///
/// Each request reads the database once, in one read-only snapshot. A
/// database that cannot be reached gives 503 `database_unavailable`,
/// never an empty result.
#[utoipa::path(
    get,
    path = "/battery",
    operation_id = "getBattery",
    tag = "battery",
    params(BatteryQuery),
    responses(
        (status = 200, description = "The battery view of the range.", body = BatteryView),
        (status = 400, description = "The query is not valid, or the request carries more than one session cookie (`invalid_request`).", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "The request body is larger than the limit.", body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "Stored data breaks an assumption (`data_contract_violation`), or an unexpected failure (`internal_error`).", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "The database is not available (`database_unavailable`), the server is busy (`service_busy`), or the request took too long (`timeout`).", body = Problem, content_type = "application/problem+json"),
    ),
)]
pub async fn get_battery(
    State(state): State<super::State>,
    headers: HeaderMap,
    query: Result<Query<BatteryQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => return Problem::from(rejection).into_response(),
    };
    let request = SourceRequest {
        key: map::domain_key(query.range),
        scenario: scenario(&headers),
    };
    let snapshot = match state.source().snapshot(&request).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(error = %error, "battery_read_failed");
            return problem(error).into_response();
        }
    };
    let view = match derive_view(&snapshot) {
        Ok(view) => view,
        Err(violation) => {
            warn!(error = %violation, "battery_contract_violation");
            return Problem::new(ProblemCode::DataContractViolation).into_response();
        }
    };
    match map::view(&view) {
        Ok(body) => Json(body).into_response(),
        Err(error) => {
            warn!(error = %error, "battery_map_failed");
            Problem::new(ProblemCode::InternalError).into_response()
        }
    }
}

/// The problem of a source error.
#[must_use]
pub fn problem(error: SourceError) -> Problem {
    Problem::new(match error {
        SourceError::Unavailable(_) => ProblemCode::DatabaseUnavailable,
        SourceError::Busy => ProblemCode::ServiceBusy,
        SourceError::Timeout(_) => ProblemCode::Timeout,
        SourceError::Contract(_) => ProblemCode::DataContractViolation,
        SourceError::Internal(_) => ProblemCode::InternalError,
    })
}

/// The scenario cookie of the test server. The release build never reads
/// it.
#[cfg(feature = "test-server")]
fn scenario(headers: &HeaderMap) -> Option<String> {
    super::fake::scenario_name(
        headers
            .get_all(axum::http::header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok()),
    )
}

#[cfg(not(feature = "test-server"))]
const fn scenario(_headers: &HeaderMap) -> Option<String> {
    None
}
