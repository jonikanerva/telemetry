//! Login, session, and logout under `/api/v1` (ADR 0003).
//!
//! Slice S2 (`feat/web-login`) fills this module: `GET`, `POST`, and
//! `DELETE /api/v1/session`, the session cookie, the CSRF origin check,
//! and the login throttle. Slice S1 mounts the empty router, so the
//! routes, the middleware, and the problem mapping are in place.
//!
//! S2 owns `crates/server/src/auth/**` and needs no change in a file of
//! S1. The extension points:
//!
//! - [`router`]: register each handler with `utoipa_axum::routes!`. The
//!   router is nested at `/api/v1`, so a route path here starts after
//!   `/api/v1`.
//! - [`Config::from_vars`]: read the variables of the slice (for example
//!   the password hash) through [`Vars`]. `crate::config` calls it and is
//!   still the only reader of the process environment.
//! - [`State`]: the password hash, the session store, and the throttle.
//!   [`State::new`] builds it for `serve`, [`test_state`] for `serve-test`.
//!   A handler takes `axum::extract::State<auth::State>`.
//! - [`crate::problem::Problem`] for every failure, for example
//!   `ProblemCode::Unauthenticated`, `InvalidCredentials`,
//!   `ForbiddenOrigin`, and `LoginThrottled` with
//!   `Problem::with_retry_after`. Body and query rejections map with
//!   `Problem::from`.
//! - Declare each problem response of an operation with
//!   `content_type = "application/problem+json"`.
//! - `unit_test_state` gives the state that the router tests of S1 use;
//!   keep it valid when [`State`] gets fields.

use axum::extract::FromRef;
use utoipa_axum::router::OpenApiRouter;

use crate::app::AppState;
use crate::config::{ConfigError, Vars};

/// The configuration of this slice. Empty until slice S2.
#[derive(Debug, Clone)]
pub struct Config {}

impl Config {
    /// Reads and validates the variables of this slice. `crate::config`
    /// calls it once at startup, after the common variables.
    ///
    /// Rules: read only through `vars`; return [`ConfigError`] with the
    /// variable name and a static reason, never the value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for a missing or invalid variable.
    pub fn from_vars(vars: &Vars<'_>) -> Result<Self, ConfigError> {
        let _ = vars;
        Ok(Self {})
    }
}

/// The state of the handlers of this slice. Empty until slice S2.
///
/// A handler reads it with `axum::extract::State<auth::State>`: the
/// [`FromRef`] implementation below takes the field `auth` of
/// [`AppState`].
#[derive(Debug, Clone)]
pub struct State {}

impl State {
    /// The state of `serve`.
    #[must_use]
    pub fn new(config: &Config) -> Self {
        let _ = config;
        Self {}
    }
}

impl FromRef<AppState> for State {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

/// The state of `serve-test` (feature `test-server`). Slice S2 can put a fixed clock or a test password here.
#[cfg(feature = "test-server")]
#[must_use]
pub fn test_state(config: &Config) -> State {
    State::new(config)
}

/// The state of the unit tests of the router (`crate::app`).
#[cfg(test)]
pub(crate) fn unit_test_state() -> State {
    State {}
}

/// The routes of the session API. Empty until slice S2.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
