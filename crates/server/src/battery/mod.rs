//! The battery view under `/api/v1` (ADR 0012).
//!
//! Slice S3 (`feat/battery-api`) fills this module: `GET
//! /api/v1/battery?range=...`, its DTOs, and the read from the database.
//! Slice S1 mounts the empty router.
//!
//! S3 owns `crates/server/src/battery/**` and needs no change in a file of
//! S1. The extension points:
//!
//! - [`router`]: register the handler with `utoipa_axum::routes!`. The
//!   router is nested at `/api/v1`.
//! - [`Config::from_vars`]: read the variables of the slice (for example
//!   `DATABASE_URL`) through [`Vars`].
//! - [`State`]: the data source. Proposed: a trait `BatterySource` in
//!   `battery/source.rs`, the PostgreSQL adapter in `battery/postgres.rs`,
//!   and the fake of the test server in `battery/fake.rs`; [`State`] holds
//!   an `Arc<dyn BatterySource>`. [`State::new`] builds the PostgreSQL
//!   source for `serve`, [`test_state`] the fake for `serve-test`. A
//!   handler takes `axum::extract::State<battery::State>`.
//! - The axum `Query` extractor is enabled; `Problem::from(QueryRejection)`
//!   gives the problem `invalid_request`.
//! - Use [`crate::wire::Instant`] for each instant of the DTOs, with the
//!   schema rules of `crate::wire`.
//! - `unit_test_state` gives the state that the router tests of S1 use;
//!   keep it valid when [`State`] gets fields.

use axum::extract::FromRef;
use utoipa_axum::router::OpenApiRouter;

use crate::app::AppState;
use crate::config::{ConfigError, Vars};

/// The configuration of this slice. Empty until slice S3.
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

/// The state of the handlers of this slice. Empty until slice S3.
///
/// A handler reads it with `axum::extract::State<battery::State>`: the
/// [`FromRef`] implementation below takes the field `battery` of
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
        state.battery.clone()
    }
}

/// The state of `serve-test` (feature `test-server`). Slice S3 builds the fake `BatterySource` here.
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

/// The routes of the battery API. Empty until slice S3.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
