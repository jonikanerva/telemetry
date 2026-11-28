//! The consumption view under `/api/v1`: the drives of a range and their
//! Wh/km.
//!
//! `GET /api/v1/consumption?range=...` reads one snapshot of the Gear,
//! Odometer, `EnergyRemaining`, and `OutsideTemp` rows, derives the drives
//! with the rules of `telemetry_web_domain::consumption`, and returns
//! [`dto::ConsumptionView`]. The route is behind the session guard, as
//! the battery API.
//!
//! The sources are the sources of the battery API: the PostgreSQL source
//! (the same read slots, deadlines, and error mapping, ADR 0013) and the
//! fake source of the test server (the same scenario files).

pub mod dto;
pub mod postgres;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRef, Query, State as Extract};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use telemetry_web_domain::consumption::{Snapshot, derive_view};
use tracing::warn;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use self::dto::{ConsumptionQuery, ConsumptionView};
use crate::app::AppState;
use crate::battery::handler::{problem, scenario};
use crate::battery::map::domain_key;
use crate::battery::source::{SourceError, SourceRequest};
use crate::problem::{Problem, ProblemCode};

/// A boxed future of a source.
pub type ConsumptionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Snapshot, SourceError>> + Send + 'a>>;

/// A source of one consumption [`Snapshot`] for each request.
pub trait ConsumptionSource: Send + Sync + fmt::Debug {
    /// Reads the snapshot of the range that ends now.
    fn consumption<'a>(&'a self, request: &'a SourceRequest) -> ConsumptionFuture<'a>;
}

/// The state of the handler: the data source.
#[derive(Debug, Clone)]
pub struct State {
    source: Arc<dyn ConsumptionSource>,
}

impl State {
    /// A state with a given source.
    #[must_use]
    pub fn with_source(source: Arc<dyn ConsumptionSource>) -> Self {
        Self { source }
    }
}

impl FromRef<AppState> for State {
    fn from_ref(state: &AppState) -> Self {
        state.consumption.clone()
    }
}

/// The routes: `GET /consumption` under `/api/v1`.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_consumption))
}

/// Returns the drives of one range.
///
/// The session guard (`crate::auth`) runs first: without a valid session
/// the answer is 401 `unauthenticated`, and no read starts. A database
/// that cannot be reached gives 503 `database_unavailable`, never an empty
/// list.
#[utoipa::path(
    get,
    path = "/consumption",
    operation_id = "getConsumption",
    tag = "consumption",
    params(ConsumptionQuery),
    responses(
        (status = 200, description = "The drives of the range.", body = ConsumptionView),
        (status = 400, description = "The query is not valid, or the request carries more than one session cookie (`invalid_request`).", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "The request body is larger than the limit.", body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "Stored data breaks an assumption (`data_contract_violation`), or an unexpected failure (`internal_error`).", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "The database is not available (`database_unavailable`), the server is busy (`service_busy`), or the request took too long (`timeout`).", body = Problem, content_type = "application/problem+json"),
    ),
)]
pub async fn get_consumption(
    Extract(state): Extract<State>,
    headers: HeaderMap,
    query: Result<Query<ConsumptionQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => return Problem::from(rejection).into_response(),
    };
    let request = SourceRequest {
        key: domain_key(query.range),
        scenario: scenario(&headers),
    };
    let snapshot = match state.source.consumption(&request).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(error = %error, "consumption_read_failed");
            return problem(error).into_response();
        }
    };
    let view = match derive_view(&snapshot) {
        Ok(view) => view,
        Err(violation) => {
            warn!(error = %violation, "consumption_contract_violation");
            return Problem::new(ProblemCode::DataContractViolation).into_response();
        }
    };
    match dto::view(&view) {
        Ok(body) => Json::<ConsumptionView>(body).into_response(),
        Err(error) => {
            warn!(error = %error, "consumption_map_failed");
            Problem::new(ProblemCode::InternalError).into_response()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
