//! `GET /api/v1/battery`.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};

use super::dto::{BatteryQuery, BatteryView};
use crate::problem::{Problem, ProblemCode};

/// Returns the battery view of one range.
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
        (status = 400, description = "The query is not valid (`invalid_request`).", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "The request has no valid session (`unauthenticated`).", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "The request body is larger than the limit.", body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "Stored data breaks an assumption (`data_contract_violation`), or an unexpected failure (`internal_error`).", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "The database is not available (`database_unavailable`), the server is busy (`service_busy`), or the request took too long (`timeout`).", body = Problem, content_type = "application/problem+json"),
    ),
)]
pub async fn get_battery(
    State(state): State<super::State>,
    query: Result<Query<BatteryQuery>, QueryRejection>,
) -> Response {
    let Query(_query) = match query {
        Ok(query) => query,
        Err(rejection) => return Problem::from(rejection).into_response(),
    };
    let _ = state;
    // Contract freeze: the read path comes in the next commits.
    Problem::new(ProblemCode::InternalError).into_response()
}
