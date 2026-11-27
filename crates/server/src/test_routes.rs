//! Test-only routes of the browser tests (feature `test-server`).
//!
//! The release image never enables the feature, so these routes never
//! exist in production. They are plain axum routes, not part of the
//! `OpenAPI` document.
//!
//! Experiment E4 checks that each browser of the Playwright tests keeps a
//! `Secure` cookie with the `__Host-` prefix on `http://localhost`:
//!
//! - `GET /test-only/e4/cookie/set`: 204 with
//!   `Set-Cookie: __Host-e4=1; Path=/; Secure; HttpOnly; SameSite=Strict`.
//! - `GET /test-only/e4/cookie/check`: 200 `{"present": true}` when the
//!   request carries the cookie `__Host-e4=1`, else `{"present": false}`.

use axum::Json;
use axum::Router;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;

use crate::app::AppState;

/// The cookie of experiment E4.
pub const E4_SET_COOKIE: &str = "__Host-e4=1; Path=/; Secure; HttpOnly; SameSite=Strict";

const E4_PAIR: &str = "__Host-e4=1";

/// The test-only routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/test-only/e4/cookie/set", get(set_cookie))
        .route("/test-only/e4/cookie/check", get(check_cookie))
}

async fn set_cookie() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, HeaderValue::from_static(E4_SET_COOKIE))],
    )
        .into_response()
}

#[derive(Debug, Serialize)]
struct CookieCheck {
    present: bool,
}

async fn check_cookie(headers: HeaderMap) -> Json<CookieCheck> {
    let present = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .any(|pair| pair.trim() == E4_PAIR);
    Json(CookieCheck { present })
}
