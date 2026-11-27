//! `GET /health`: liveness and the version of the server.
//!
//! The endpoint needs no session and no database, so a deployment does
//! not depend on Neon and does not wake it. The Railway healthcheck calls
//! it, and the SPA compares the version with its own.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use crate::app::AppState;
use crate::problem::Problem;

/// The state of the server. The only value is `ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// The server answers requests.
    Ok,
}

/// The body of `GET /health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Health {
    /// Always `ok`.
    pub status: HealthStatus,
    /// The commit SHA of the deployment, or `unknown`.
    #[schema(example = "0123abc")]
    pub version: String,
}

/// Returns the state and the version of the server.
#[utoipa::path(
    get,
    path = "/health",
    operation_id = "getHealth",
    tag = "health",
    responses(
        (status = 200, description = "The server answers requests.", body = Health),
        (status = 413, description = "The request body is larger than the limit.", body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "An unexpected error occurred.", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "The request took too long.", body = Problem, content_type = "application/problem+json"),
    ),
)]
pub async fn health(State(state): State<AppState>) -> Response {
    let body = Health {
        status: HealthStatus::Ok,
        version: state.version().as_str().to_owned(),
    };
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
