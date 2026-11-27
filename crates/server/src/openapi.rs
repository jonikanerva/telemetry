//! The documented routes and the export of the `OpenAPI` 3.1 document.
//!
//! [`documented_router`] is the one place that registers the routes of the
//! contract. The server (`crate::app`) and the export use it, so the
//! document follows the registered routes. The export needs no
//! configuration, no database, and no SPA file (web profile §2.1).
//!
//! The document is deterministic: utoipa keeps the paths and the schemas
//! in sorted maps, and the info fields are fixed here, not taken from the
//! crate version. The test-only routes (`crate::test_routes`) are plain
//! axum routes, so the document is the same with and without the feature
//! `test-server`.

use std::path::Path;

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::app::AppState;
use crate::health::{self, Health, HealthStatus};
use crate::problem::{Problem, ProblemCode};
use crate::{auth, battery};

/// The version of the API contract. It changes only with an incompatible
/// change of the contract, not with the crate version.
pub const API_VERSION: &str = "1";

/// The base path of the API routes.
pub const API_BASE: &str = "/api/v1";

#[derive(OpenApi)]
#[openapi(
    info(
        title = "telemetry web API",
        version = API_VERSION,
        description = "HTTP API of the telemetry web application. Every failure is application/problem+json.",
    ),
    components(schemas(Problem, ProblemCode, Health, HealthStatus)),
    tags((name = "health", description = "Liveness and version")),
)]
struct ApiDoc;

/// The routes of the contract: `/health` and the API under [`API_BASE`].
#[must_use]
pub fn documented_router() -> OpenApiRouter<AppState> {
    let mut document = ApiDoc::openapi();
    // utoipa fills the licence and the contact from the package metadata,
    // which has none; an empty licence object is not useful.
    document.info.license = None;
    document.info.contact = None;
    OpenApiRouter::with_openapi(document)
        .routes(routes!(health::health))
        .nest(API_BASE, auth::router().merge(battery::router()))
}

/// The `OpenAPI` document as pretty JSON with a trailing newline.
///
/// # Errors
///
/// Returns the serialisation error. It does not occur for the document
/// types of utoipa.
pub fn to_json() -> Result<String, serde_json::Error> {
    let document = documented_router().into_openapi();
    let mut json = serde_json::to_string_pretty(&document)?;
    json.push('\n');
    Ok(json)
}

/// A failure of the export.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The document cannot be serialised.
    #[error("cannot serialise the OpenAPI document")]
    Serialise,
    /// The file cannot be written. The text holds the `io::ErrorKind`, not
    /// the path.
    #[error("cannot write the OpenAPI document: {0}")]
    Write(std::io::ErrorKind),
}

/// Writes the document to `path`.
///
/// # Errors
///
/// Returns [`ExportError`] when the document cannot be serialised or the
/// file cannot be written.
pub fn export(path: &Path) -> Result<(), ExportError> {
    let json = to_json().map_err(|_| ExportError::Serialise)?;
    std::fs::write(path, json).map_err(|err| ExportError::Write(err.kind()))
}
