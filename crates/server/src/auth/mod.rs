//! Login, session, and logout under `/api/v1` (ADR 0003).
//!
//! Slice S2 (`feat/web-login`) fills this module: `GET`, `POST`, and
//! `DELETE /api/v1/session`, the session cookie, the CSRF origin check,
//! and the login throttle. Slice S1 mounts the empty router, so the
//! routes, the middleware, and the problem mapping are in place.
//!
//! Extension points for S2:
//!
//! - Register each handler with `utoipa_axum::routes!` in [`router`]. The
//!   router is nested at `/api/v1` (`crate::openapi::documented_router`),
//!   so a route path here starts after `/api/v1`.
//! - Add the password hash and the session store to
//!   [`crate::app::AppState`], and read the variables in `crate::config`.
//! - Return [`crate::problem::Problem`] for every failure, for example
//!   `ProblemCode::Unauthenticated`, `InvalidCredentials`, `ForbiddenOrigin`,
//!   and `LoginThrottled` with `Problem::with_retry_after`. A JSON body
//!   rejection maps with `Problem::from(JsonRejection)`.
//! - Declare each problem response of an operation with
//!   `content_type = "application/problem+json"`.

use utoipa_axum::router::OpenApiRouter;

use crate::app::AppState;

/// The routes of the session API. Empty until slice S2.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
