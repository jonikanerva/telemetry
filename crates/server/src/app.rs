//! The router: routes, middleware, static files, and the shared state.
//!
//! Routes:
//!
//! | Path | Response |
//! | ---- | -------- |
//! | `GET /health` | [`crate::health`] |
//! | `/api/v1/...` | [`crate::auth::router`] and [`crate::battery::router`] |
//! | other `/api` and `/api/...` | 404 problem `not_found`, every method |
//! | `GET /assets/...` | A file of `$WEB_STATIC_DIR/assets`, with a `.gz` sibling for `Accept-Encoding: gzip`; `Cache-Control: public, max-age=31536000, immutable`. A missing file is a 404 problem, never `index.html`. |
//! | `GET /robots.txt` | `User-agent: *` and `Disallow: /` |
//! | other `GET` and `HEAD` | `index.html` with the version, `Cache-Control: no-store` |
//! | other methods | 405 problem `method_not_allowed` |
//!
//! Middleware, from the outside in:
//!
//! 1. Security headers on every response (CSP, HSTS, and the others of
//!    [`SECURITY_HEADERS`]), and `Cache-Control: no-store` when the
//!    response sets no `Cache-Control`.
//! 2. The request event: method, route template, status, latency, and
//!    request id ([`crate::logging`]).
//! 3. The problem safety net: a library error response without a problem
//!    body (for example the empty 405 of axum or the 404 of the file
//!    service) becomes the problem of its status
//!    ([`crate::problem::code_for_status`]).
//! 4. The panic containment: a panic becomes the 500 problem
//!    `internal_error`.
//! 5. The request id and its span. The id is made inside the panic
//!    containment (web profile §8).
//! 6. The request timeout ([`REQUEST_TIMEOUT`]): 503 problem `timeout`.
//! 7. The body limit ([`BODY_LIMIT_BYTES`]): 413 problem
//!    `payload_too_large` for a declared length above the limit; the axum
//!    body extractors apply the same limit to a body without a length.
//!
//! [`AppState`] is the state of the API handlers. It holds the version and
//! one field for each API slice: `auth` ([`crate::auth::State`]) and
//! `battery` ([`crate::battery::State`]). Each slice defines its state type
//! and its `FromRef<AppState>` implementation in its own module, so a
//! handler takes `State<auth::State>` or `State<battery::State>`. A slice
//! changes no file of this module when its state gets fields. Each field
//! must be cheap to clone (an `Arc` or a small value).
//!
//! Files of each slice (no change in a file of S1 is needed):
//!
//! - S2 (session API): `crates/server/src/auth/**`.
//! - S3 (battery API): `crates/server/src/battery/**`.
//!
//! The mount points are `crate::auth::router` and
//! `crate::battery::router` (nested at `/api/v1` by `crate::openapi`), the
//! `Config::from_vars`, `State::new`, `test_state`, and `unit_test_state`
//! of each slice, and the `State` types above.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request};
use axum::http::header::{ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{Next, from_fn, map_response};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, get_service};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::services::ServeDir;
use tracing::{Instrument, info, info_span, warn};

use crate::config::{Config, StaticFiles, Version};
use crate::openapi;
use crate::problem::{PROBLEM_CONTENT_TYPE, Problem, ProblemCode, code_for_status};
use crate::{auth, battery};

/// The longest time that a request can take before the 503 problem
/// `timeout`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest request body.
pub const BODY_LIMIT_BYTES: usize = 16 * 1024;

/// The `Content-Security-Policy` of every response.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// The headers that every response gets.
pub const SECURITY_HEADERS: [(&str, &str); 8] = [
    ("content-security-policy", CONTENT_SECURITY_POLICY),
    ("strict-transport-security", "max-age=31536000"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    (
        "permissions-policy",
        "geolocation=(), camera=(), microphone=()",
    ),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
    ("x-robots-tag", "noindex"),
];

/// `Cache-Control` of the files under `/assets/`. Vite puts a content hash
/// in each file name, so a file never changes.
pub const ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// The body of `GET /robots.txt`.
pub const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /\n";

/// The logged route of a request that matched no route template.
pub const FALLBACK_ROUTE: &str = "fallback";

/// The logged route of a request under `/assets`.
pub const ASSETS_ROUTE: &str = "/assets/*";

/// The state of the API handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    version: Version,
    /// The state of the session API.
    pub auth: auth::State,
    /// The state of the battery API.
    pub battery: battery::State,
}

impl AppState {
    /// The state of a server with this version and these slice states.
    #[must_use]
    pub const fn new(version: Version, auth: auth::State, battery: battery::State) -> Self {
        Self {
            version,
            auth,
            battery,
        }
    }

    /// The state of `serve`: each slice builds its state from its
    /// configuration.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config.version.clone(),
            auth::State::new(&config.auth),
            battery::State::new(&config.battery),
        )
    }

    /// The state of `serve-test`: each slice builds its test state, for
    /// example a fake data source.
    #[cfg(feature = "test-server")]
    #[must_use]
    pub fn for_test_server(config: &Config) -> Self {
        Self::new(
            config.version.clone(),
            auth::test_state(&config.auth),
            battery::test_state(&config.battery),
        )
    }

    /// The state of the unit tests of the router.
    #[cfg(test)]
    pub(crate) fn for_unit_tests(version: Version) -> Self {
        Self::new(version, auth::unit_test_state(), battery::unit_test_state())
    }

    /// The version of the server.
    #[must_use]
    pub const fn version(&self) -> &Version {
        &self.version
    }
}

/// The router of `serve`.
pub fn router(state: AppState, static_files: &StaticFiles) -> Router {
    build(state, static_files, Router::new())
}

/// The router of `serve-test`: the router of `serve` plus the test-only
/// routes of [`crate::test_routes`].
#[cfg(feature = "test-server")]
pub fn test_server_router(state: AppState, static_files: &StaticFiles) -> Router {
    build(state, static_files, crate::test_routes::router())
}

/// Builds the router with `extra` routes, which get the same middleware.
pub(crate) fn build(
    state: AppState,
    static_files: &StaticFiles,
    extra: Router<AppState>,
) -> Router {
    let (api, _document) = openapi::documented_router().split_for_parts();
    // The session guard runs before every documented API handler.
    let api = auth::protect(api, state.auth.clone());
    let index: Arc<str> = Arc::from(static_files.render_index(state.version()));
    let assets = ServeDir::new(static_files.assets_dir())
        .precompressed_gzip()
        .append_index_html_on_directories(false)
        .redirect_to_trailing_slash(false);

    Router::new()
        .merge(api)
        .route("/api", any(api_not_found))
        .route("/api/", any(api_not_found))
        .route("/api/{*rest}", any(api_not_found))
        .route("/robots.txt", get(robots))
        .nest_service(
            "/assets",
            get_service(assets).layer(map_response(asset_cache_control)),
        )
        .merge(extra)
        .fallback(move |method: Method| {
            let index = Arc::clone(&index);
            async move { spa(&method, &index) }
        })
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state)
        .layer(from_fn(limit_declared_length))
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .layer(from_fn(request_timeout))
        .layer(from_fn(request_id))
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(from_fn(problem_safety_net))
        .layer(from_fn(log_request))
        .layer(from_fn(security_headers))
}

async fn api_not_found() -> Problem {
    Problem::new(ProblemCode::NotFound)
}

async fn method_not_allowed() -> Problem {
    Problem::new(ProblemCode::MethodNotAllowed)
}

async fn robots() -> Response {
    ([(CONTENT_TYPE, "text/plain; charset=utf-8")], ROBOTS_TXT).into_response()
}

/// The SPA entry for every other `GET` and `HEAD`, so the router of the
/// SPA handles deep links. Other methods get 405.
fn spa(method: &Method, index: &str) -> Response {
    if method == Method::GET || method == Method::HEAD {
        (
            [
                (CONTENT_TYPE, "text/html; charset=utf-8"),
                (CACHE_CONTROL, "no-store"),
            ],
            index.to_string(),
        )
            .into_response()
    } else {
        let mut response = Problem::new(ProblemCode::MethodNotAllowed).into_response();
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET,HEAD"));
        response
    }
}

/// Sets the long cache time on a served file only. A 404 or 405 keeps the
/// `no-store` of the problem.
async fn asset_cache_control(mut response: Response) -> Response {
    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(ASSET_CACHE_CONTROL));
    }
    response
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    if !headers.contains_key(CACHE_CONTROL) {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

/// The request id of a response, for the request event.
#[derive(Debug, Clone, Copy)]
struct RequestId([u8; 16]);

impl RequestId {
    /// 16 random hex digits, or `None` when the system gives no random
    /// bytes.
    fn generate() -> Option<Self> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).ok()?;
        let mut text = [0_u8; 16];
        for (pair, byte) in text.as_chunks_mut::<2>().0.iter_mut().zip(bytes) {
            *pair = [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]];
        }
        Some(Self(text))
    }

    fn as_str(&self) -> &str {
        // The bytes are ASCII hex digits.
        std::str::from_utf8(&self.0).unwrap_or(NO_REQUEST_ID)
    }
}

/// The request id of a request without an id: the system gave no random
/// bytes, or a panic ended the request before its response.
const NO_REQUEST_ID: &str = "none";

async fn request_id(request: Request, next: Next) -> Response {
    let id = RequestId::generate();
    let span = info_span!(
        "request",
        request_id = id.as_ref().map_or(NO_REQUEST_ID, RequestId::as_str)
    );
    let mut response = next.run(request).instrument(span).await;
    if let Some(id) = id {
        response.extensions_mut().insert(id);
    }
    response
}

/// Writes one `request` event with allowlisted fields only. The route is
/// the matched template; the raw path and the query never reach the log.
async fn log_request(request: Request, next: Next) -> Response {
    let started = tokio::time::Instant::now();
    let method = method_name(request.method());
    let route = route_of(&request);
    let response = next.run(request).await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let request_id = response
        .extensions()
        .get::<RequestId>()
        .map_or(NO_REQUEST_ID, RequestId::as_str);
    info!(
        method,
        route = route.as_str(),
        status = response.status().as_u16(),
        latency_ms,
        request_id,
        "request"
    );
    response
}

/// The route template of a request, or a fixed word. The nested file
/// service sets no [`MatchedPath`] that this middleware can see, so its
/// requests get the fixed word [`ASSETS_ROUTE`].
fn route_of(request: &Request) -> String {
    if let Some(path) = request.extensions().get::<MatchedPath>() {
        return path.as_str().to_owned();
    }
    let path = request.uri().path();
    if path == "/assets" || path.starts_with("/assets/") {
        ASSETS_ROUTE.to_owned()
    } else {
        FALLBACK_ROUTE.to_owned()
    }
}

/// The method as one word of a closed set. An extension method is a
/// client-chosen text, so it becomes `other`.
fn method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        Method::OPTIONS => "OPTIONS",
        Method::CONNECT => "CONNECT",
        Method::TRACE => "TRACE",
        _ => "other",
    }
}

async fn problem_safety_net(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let is_problem = response
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes() == PROBLEM_CONTENT_TYPE.as_bytes());
    let Some(code) = code_for_status(status).filter(|_| !is_problem) else {
        return response;
    };
    let allow = response.headers().get(ALLOW).cloned();
    let mut problem = Problem::new(code).into_response();
    if let Some(allow) = allow {
        problem.headers_mut().insert(ALLOW, allow);
    }
    problem
}

fn panic_response(_payload: Box<dyn Any + Send + 'static>) -> Response {
    // The payload can hold a value of the request; it never reaches the
    // log. The panic hook (`crate::logging`) logs the source position.
    warn!("request_panicked");
    Problem::new(ProblemCode::InternalError).into_response()
}

async fn request_timeout(request: Request, next: Next) -> Response {
    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(request)).await {
        Ok(response) => response,
        Err(_elapsed) => Problem::new(ProblemCode::Timeout).into_response(),
    }
}

async fn limit_declared_length(request: Request, next: Next) -> Response {
    let too_large = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| {
            usize::try_from(length).map_or(true, |length| length > BODY_LIMIT_BYTES)
        });
    if too_large {
        return Problem::new(ProblemCode::PayloadTooLarge).into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests;
