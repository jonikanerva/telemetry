//! Login, session, and logout under `/api/v1` (ADR 0003, ADR 0012).
//!
//! Routes:
//!
//! | Route | Response |
//! | ----- | -------- |
//! | `GET /api/v1/session` | 200 [`Session`] for a valid session, else 401 `unauthenticated` |
//! | `POST /api/v1/session` `{"password": "..."}` | 200 [`Session`] and a new session cookie, 401 `invalid_credentials`, 429 `login_throttled`, 400, 413, 415 |
//! | `DELETE /api/v1/session` | 204 and a cookie that removes the session cookie, also without a session |
//!
//! The guard ([`protect`]) runs before every handler of a documented API
//! route (design §3):
//!
//! 1. A request with a method other than `GET` and `HEAD` must pass the
//!    origin rule ([`origin`]), else 403 `forbidden_origin`.
//! 2. Every API route except the routes of [`PUBLIC_ROUTES`] needs a valid
//!    session, else 401 `unauthenticated`. The rule is "deny unless
//!    listed": a new route of a later slice is protected without a change
//!    here. No handler behind the guard runs for a request without a
//!    session, so such a request never reaches the database.
//!
//! An unknown path under `/api` is not a documented route; it stays the
//! 404 problem `not_found` of `crate::app`.
//!
//! Parts:
//!
//! - [`token`]: the session token and its constant-time compare.
//! - [`cookie`]: the cookie name, its attributes, and the header parser.
//! - [`store`]: the sessions in memory, with the lifetimes and the bound.
//! - [`throttle`]: the global login throttle and the verification slot.
//! - [`password`]: the Argon2id hash, its startup check, the verifier,
//!   and the command `hash-password`.
//! - [`origin`]: the public origin and the CSRF rule.
//! - [`clock`]: the monotonic and wall clocks of the rules.
//!
//! Log events (field `reason` only, from a closed set of static words; no
//! password, hash, cookie, or token): `login_succeeded`, `login_failed`,
//! `login_throttled`, `login_rejected`, `login_failed_internal`,
//! `session_evicted`, `logout`, and `request_refused`.

pub mod clock;
pub mod cookie;
pub mod origin;
pub mod password;
pub mod store;
#[cfg(feature = "test-server")]
pub mod test_routes;
pub mod throttle;
pub mod token;

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{FromRef, MatchedPath, Request, State as Extract};
use axum::http::header::{CONTENT_TYPE, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use self::clock::{Clock, Now, SystemClock};
use self::origin::PublicOrigin;
use self::password::{Argon2Verifier, StoredHash, Verification, Verifier};
use self::store::{Deadlines, Removal, SessionLimits, SessionStore};
use self::throttle::{LoginThrottle, ThrottleLimits};
use crate::app::AppState;
use crate::config::{ConfigError, Vars};
use crate::problem::{Problem, ProblemCode, code_for_status};
use crate::wire::Instant;

/// The routes that need no session: the health check and the session
/// resource itself. The session handlers check the session themselves.
pub const PUBLIC_ROUTES: [&str; 2] = ["/health", "/api/v1/session"];

/// The largest login body in bytes.
pub const LOGIN_BODY_LIMIT: usize = 4 * 1024;

/// The configuration of the session API.
#[derive(Debug, Clone)]
pub struct Config {
    /// The Argon2id hash of the password, from `WEB_PASSWORD_HASH`.
    pub password_hash: StoredHash,
    /// The public origin, from `WEB_PUBLIC_ORIGIN`.
    pub origin: PublicOrigin,
}

impl Config {
    /// Reads and validates `WEB_PASSWORD_HASH` and `WEB_PUBLIC_ORIGIN`.
    /// An error names the variable and a static reason, never the value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for a missing or invalid variable.
    pub fn from_vars(vars: &Vars<'_>) -> Result<Self, ConfigError> {
        let password_hash = StoredHash::from_variable(vars.get(password::HASH_VARIABLE))?;
        let origin = PublicOrigin::from_variable(vars.get(origin::ORIGIN_VARIABLE))?;
        Ok(Self {
            password_hash,
            origin,
        })
    }
}

/// The state of the session API: one store, one throttle, and one
/// verifier for the process. Clones share them.
#[derive(Clone)]
pub struct State(Arc<Inner>);

struct Inner {
    origin: PublicOrigin,
    clock: Arc<dyn Clock>,
    store: Mutex<SessionStore>,
    throttle: LoginThrottle,
    verifier: Arc<dyn Verifier>,
    #[cfg(feature = "test-server")]
    test_clock: Option<Arc<clock::ShiftableClock>>,
    /// Counts of the test server, read by `GET /test-only/auth/stats`.
    #[cfg(feature = "test-server")]
    test_stats: test_routes::Stats,
}

impl fmt::Debug for State {
    // The store holds the tokens; print only the shape.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("auth::State")
            .field("origin", &self.0.origin)
            .finish_non_exhaustive()
    }
}

/// The parts of a [`State`].
pub struct Parts {
    /// The public origin.
    pub origin: PublicOrigin,
    /// The clock of the rules.
    pub clock: Arc<dyn Clock>,
    /// The password check.
    pub verifier: Arc<dyn Verifier>,
    /// The session limits.
    pub sessions: SessionLimits,
    /// The throttle limits.
    pub throttle: ThrottleLimits,
}

impl State {
    /// The state of `serve`: the system clock, the Argon2id verifier, and
    /// the limits of the owner decision.
    #[must_use]
    pub fn new(config: &Config) -> Self {
        Self::from_parts(Parts {
            origin: config.origin.clone(),
            clock: Arc::new(SystemClock),
            verifier: Arc::new(Argon2Verifier::new(config.password_hash.clone())),
            sessions: SessionLimits::OWNER,
            throttle: ThrottleLimits::DEFAULT,
        })
    }

    /// A state from its parts.
    #[must_use]
    pub fn from_parts(parts: Parts) -> Self {
        let now = parts.clock.now();
        Self(Arc::new(Inner {
            origin: parts.origin,
            store: Mutex::new(SessionStore::new(parts.sessions)),
            throttle: LoginThrottle::new(parts.throttle, now.mono),
            clock: parts.clock,
            verifier: parts.verifier,
            #[cfg(feature = "test-server")]
            test_clock: None,
            #[cfg(feature = "test-server")]
            test_stats: test_routes::Stats::default(),
        }))
    }

    fn now(&self) -> Now {
        self.0.clock.now()
    }

    fn store(&self) -> std::sync::MutexGuard<'_, SessionStore> {
        // A panic while the lock is held cannot leave the store in a state
        // that breaks a rule: each method changes it in one step.
        self.0.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Uses the first valid session that the request presents. Returns its
    /// deadlines and the time of the check.
    fn session_of(&self, headers: &HeaderMap) -> (Option<Deadlines>, Now, bool) {
        let presented = cookie::presented(headers);
        let now = self.now();
        let mut store = self.store();
        let deadlines = presented
            .tokens
            .iter()
            .find_map(|token| store.touch(token, now));
        (deadlines, now, presented.any)
    }
}

impl FromRef<AppState> for State {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

/// The state of `serve-test` (feature `test-server`): the real verifier
/// with the hash of the configuration, the owner limits, and a clock that
/// the test-only route `POST /test-only/auth/advance` moves forward.
#[cfg(feature = "test-server")]
#[must_use]
pub fn test_state(config: &Config) -> State {
    let test_clock = Arc::new(clock::ShiftableClock::new(jiff::Timestamp::now()));
    let shared: Arc<dyn Clock> = Arc::clone(&test_clock) as Arc<dyn Clock>;
    let now = shared.now();
    State(Arc::new(Inner {
        origin: config.origin.clone(),
        store: Mutex::new(SessionStore::new(SessionLimits::OWNER)),
        throttle: LoginThrottle::new(ThrottleLimits::DEFAULT, now.mono),
        clock: shared,
        verifier: Arc::new(Argon2Verifier::new(config.password_hash.clone())),
        test_clock: Some(test_clock),
        test_stats: test_routes::Stats::default(),
    }))
}

/// The state of the unit tests of the router (`crate::app`).
#[cfg(test)]
pub(crate) fn unit_test_state() -> State {
    tests::state_with(Arc::new(tests::FakeVerifier::default()))
}

/// The response body of a valid session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Session {
    /// The absolute end of the session: 30 days after the login.
    pub expires_at: Instant,
    /// The end of the session without another request: 7 days after the
    /// last request, never later than `expires_at`.
    pub idle_expires_at: Instant,
}

impl Session {
    fn from_deadlines(deadlines: Deadlines, now: Now) -> Self {
        Self {
            expires_at: Instant::truncated(now.wall_at(deadlines.absolute)),
            idle_expires_at: Instant::truncated(now.wall_at(deadlines.idle)),
        }
    }
}

/// The body of `POST /api/v1/session`.
#[derive(Debug, ToSchema)]
#[expect(
    dead_code,
    reason = "the type documents the body; the handler parses the JSON value"
)]
pub struct LoginRequest {
    /// The password: 1 to 1024 bytes of UTF-8.
    #[schema(min_length = 1, max_length = 1024, format = Password)]
    password: String,
}

/// A password from a login body. It never reaches a log.
struct Password(String);

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password([redacted])")
    }
}

/// The routes of the session API.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_session, login, logout))
}

/// Puts the guard in front of every route of `api` (design §3): the origin
/// rule for state-changing methods and the session rule for every route
/// outside [`PUBLIC_ROUTES`]. `crate::app` calls it with the documented
/// routes, so the rule needs no list of protected routes.
pub fn protect(api: axum::Router<AppState>, state: State) -> axum::Router<AppState> {
    api.route_layer(from_fn_with_state(state, guard))
}

async fn guard(Extract(auth): Extract<State>, request: Request, next: Next) -> Response {
    // A request without a matched route never reaches this layer
    // (`route_layer`); a missing value counts as a protected route.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned());
    let Some(route) = route else {
        return unauthenticated(false);
    };
    if !route.starts_with("/api/") {
        return next.run(request).await;
    }
    if origin::changes_state(request.method())
        && !origin::is_allowed(request.headers(), &auth.0.origin)
    {
        info!(reason = "origin", "request_refused");
        #[cfg(feature = "test-server")]
        auth.0.test_stats.count_refused_origin();
        return Problem::new(ProblemCode::ForbiddenOrigin).into_response();
    }
    if PUBLIC_ROUTES.contains(&route.as_str()) {
        return next.run(request).await;
    }
    let (session, _now, presented) = auth.session_of(request.headers());
    if session.is_none() {
        return unauthenticated(presented);
    }
    next.run(request).await
}

/// The 401 problem `unauthenticated`. When the request carried a session
/// cookie, the response also removes it from the browser.
fn unauthenticated(presented: bool) -> Response {
    let mut response = Problem::new(ProblemCode::Unauthenticated).into_response();
    if presented {
        response
            .headers_mut()
            .insert(SET_COOKIE, cookie::clear_cookie());
    }
    response
}

/// Returns the session of the request.
#[utoipa::path(
    get,
    path = "/session",
    operation_id = "getSession",
    tag = "session",
    responses(
        (status = 200, description = "The request has a valid session.", body = Session),
        (status = 401, description = "The request has no valid session.", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "The request body is larger than the limit.", body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "An unexpected error occurred.", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "The request took too long.", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn get_session(Extract(auth): Extract<State>, headers: HeaderMap) -> Response {
    match auth.session_of(&headers) {
        (Some(deadlines), now, _) => Json(Session::from_deadlines(deadlines, now)).into_response(),
        (None, _, presented) => unauthenticated(presented),
    }
}

/// Logs in with the password and starts a new session.
///
/// The response sets the session cookie. A session cookie of the request
/// ends first, so a login never keeps a token that the client chose.
#[utoipa::path(
    post,
    path = "/session",
    operation_id = "login",
    tag = "session",
    request_body(content = LoginRequest, content_type = "application/json"),
    responses(
        (status = 200, description = "The password is correct. The response sets the session cookie.", body = Session,
            headers(("Set-Cookie" = String, description = "The session cookie."))),
        (status = 400, description = "The body is not a valid login request.", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "The password is not correct.", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "The request did not come from the application origin.", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "The request body is larger than the limit.", body = Problem, content_type = "application/problem+json"),
        (status = 415, description = "The body is not JSON.", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Too many login attempts. Retry-After tells when to try again.", body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "An unexpected error occurred.", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "The request took too long.", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn login(
    Extract(auth): Extract<State>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let password = match read_login(&headers, body) {
        Ok(password) => password,
        Err(code) => {
            info!(reason = code.as_str(), "login_rejected");
            return Problem::new(code).into_response();
        }
    };
    let permit = match auth.0.throttle.admit(auth.now().mono) {
        Ok(permit) => permit,
        Err(refusal) => {
            info!(reason = refusal.reason(), "login_throttled");
            let problem = Problem::new(ProblemCode::LoginThrottled);
            let problem = match refusal.retry_after() {
                Some(retry) => problem.with_retry_after(retry),
                None => problem,
            };
            return problem.into_response();
        }
    };
    #[cfg(feature = "test-server")]
    auth.0.test_stats.count_verification();
    let verifier = Arc::clone(&auth.0.verifier);
    // The task owns the permit until the verification ends, also when the
    // client leaves and this future is dropped (web profile §9).
    let verification = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verifier.verify(password.0.as_bytes())
    })
    .await;
    match verification {
        Ok(Verification::Match) => start_session(&auth, &headers),
        Ok(Verification::Mismatch) => {
            info!(reason = "invalid_credentials", "login_failed");
            Problem::new(ProblemCode::InvalidCredentials).into_response()
        }
        Err(_) => {
            warn!(reason = "verification_task", "login_failed_internal");
            Problem::new(ProblemCode::InternalError).into_response()
        }
    }
}

fn start_session(auth: &State, headers: &HeaderMap) -> Response {
    let presented = cookie::presented(headers);
    let now = auth.now();
    let created = {
        let mut store = auth.store();
        // Fixation: the sessions that the request presents end; the new
        // session gets a new token.
        for token in &presented.tokens {
            store.remove(token);
        }
        store.create(now)
    };
    let Ok(created) = created else {
        warn!(reason = "random", "login_failed_internal");
        return Problem::new(ProblemCode::InternalError).into_response();
    };
    for removal in &created.removed {
        if *removal == Removal::Evicted {
            info!(reason = "limit", "session_evicted");
        }
    }
    info!("login_succeeded");
    let mut response = Json(Session::from_deadlines(created.deadlines, now)).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        cookie::set_cookie(&created.token, created.deadlines.max_age_seconds(now.mono)),
    );
    response
}

/// Checks the login request in this order: content type, size, JSON form,
/// password length. A failure costs no throttle token.
fn read_login(
    headers: &HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Password, ProblemCode> {
    if !is_json(headers) {
        return Err(ProblemCode::UnsupportedMediaType);
    }
    let body = body.map_err(|rejection| {
        code_for_status(rejection.status()).unwrap_or(ProblemCode::InvalidRequest)
    })?;
    if body.len() > LOGIN_BODY_LIMIT {
        return Err(ProblemCode::PayloadTooLarge);
    }
    // Decode through `Value`: a derived `Deserialize` would also accept a
    // JSON array in field order (web profile §2.3).
    let value: Value = serde_json::from_slice(&body).map_err(|_| ProblemCode::InvalidRequest)?;
    let Value::Object(object) = value else {
        return Err(ProblemCode::InvalidRequest);
    };
    if object.len() != 1 {
        return Err(ProblemCode::InvalidRequest);
    }
    match object.get("password") {
        Some(Value::String(password)) if password::PASSWORD_BYTES.contains(&password.len()) => {
            Ok(Password(password.clone()))
        }
        _ => Err(ProblemCode::InvalidRequest),
    }
}

/// One `Content-Type` of `application/json`, with optional parameters.
fn is_json(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CONTENT_TYPE).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value.to_str().is_ok_and(|text| {
            text.split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
        }),
        _ => false,
    }
}

/// Logs out: ends the session of the request and removes the cookie.
///
/// The response is 204 also without a session, so a repeated logout and a
/// logout after an expiry give the same result.
#[utoipa::path(
    delete,
    path = "/session",
    operation_id = "logout",
    tag = "session",
    responses(
        (status = 204, description = "The session ended. The response removes the session cookie.",
            headers(("Set-Cookie" = String, description = "A cookie that removes the session cookie."))),
        (status = 403, description = "The request did not come from the application origin.", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "The request body is larger than the limit.", body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "An unexpected error occurred.", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "The request took too long.", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn logout(Extract(auth): Extract<State>, headers: HeaderMap) -> Response {
    let presented = cookie::presented(&headers);
    let mut ended = false;
    {
        let mut store = auth.store();
        // Every presented session ends; no short circuit.
        for token in &presented.tokens {
            ended |= store.remove(token);
        }
    }
    info!(
        reason = if ended { "session" } else { "no_session" },
        "logout"
    );
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, cookie::clear_cookie());
    response
}

#[cfg(test)]
pub(crate) mod tests;
