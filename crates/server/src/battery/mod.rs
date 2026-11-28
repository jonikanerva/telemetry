//! The battery view under `/api/v1` (ADR 0013).
//!
//! `GET /api/v1/battery?range=...` returns the battery view of one range
//! ([`dto::BatteryView`]). The handler ([`handler`]) asks a
//! [`source::BatterySource`] for one snapshot of the raw tables, derives
//! the view with the pure rules of `telemetry_web_domain`, and maps it to
//! the wire types ([`map`]).
//!
//! Sources:
//!
//! - [`postgres::PostgresSource`] (`serve`): one connection and one
//!   read-only snapshot for each request, as the role `telemetry_web`,
//!   with bounded concurrency and deadlines.
//! - `fake::FakeSource` (feature `test-server`, without `DATABASE_URL`):
//!   the scenario files of the browser tests.
//!
//! Scenario files of the test server: `<WEB_TEST_SCENARIO_DIR>/<name>.json`,
//! picked with the cookie `telemetry_test_scenario=<name>`. The file is one
//! JSON object without unknown keys: `now` (the fixed clock) and
//! `vehicles` (the ids) are required; `signals`, `connectivity`, and
//! `ingest_events` hold rows with the columns of the raw tables (no VIN,
//! no location); `fault` (`database_unavailable`, `service_busy`,
//! `timeout`, `internal_error`) and `delay_ms` (0 to 10000) are optional.
//! `fake` documents the format with an example.

pub mod dto;
#[cfg(feature = "test-server")]
pub mod fake;
pub mod handler;
pub mod map;
pub mod postgres;
pub mod source;

use std::sync::Arc;

use axum::extract::FromRef;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use self::postgres::{DatabaseOptions, PostgresSource};
use self::source::BatterySource;
use crate::app::AppState;
use crate::config::{ConfigError, Vars};
use crate::consumption;

/// The variable of the database URL of the role `telemetry_web`.
pub const DATABASE_URL: &str = "DATABASE_URL";

/// The variable of the scenario directory of the test server.
#[cfg(feature = "test-server")]
pub const WEB_TEST_SCENARIO_DIR: &str = "WEB_TEST_SCENARIO_DIR";

/// The configuration of this slice.
#[derive(Debug, Clone)]
pub enum Config {
    /// Read the database (`DATABASE_URL`).
    Database(Box<DatabaseOptions>),
    /// Read the scenario files of the browser tests (feature
    /// `test-server`, no `DATABASE_URL`; `WEB_TEST_SCENARIO_DIR` names the
    /// directory).
    #[cfg(feature = "test-server")]
    Scenarios(Option<std::path::PathBuf>),
}

impl Config {
    /// Reads and validates the variables of this slice.
    ///
    /// - `DATABASE_URL`: required by the release build. A PostgreSQL URL
    ///   of the role `telemetry_web`. The server forces
    ///   `sslmode=verify-full` and the schema `raw`. The value is a
    ///   secret; no error holds it.
    /// - `WEB_TEST_SCENARIO_DIR` (feature `test-server` only): read when
    ///   `DATABASE_URL` is not set.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for a missing or invalid variable.
    pub fn from_vars(vars: &Vars<'_>) -> Result<Self, ConfigError> {
        match vars.get(DATABASE_URL) {
            Some(url) => {
                let options = url.to_str().and_then(DatabaseOptions::from_url).ok_or(
                    ConfigError::Invalid {
                        name: DATABASE_URL,
                        reason: "expected a PostgreSQL URL",
                    },
                )?;
                Ok(Self::Database(Box::new(options)))
            }
            #[cfg(feature = "test-server")]
            None => Ok(Self::Scenarios(
                vars.get(WEB_TEST_SCENARIO_DIR)
                    .map(std::path::PathBuf::from),
            )),
            #[cfg(not(feature = "test-server"))]
            None => Err(ConfigError::Missing { name: DATABASE_URL }),
        }
    }
}

/// The state of the handlers of this slice: the data source.
///
/// A handler reads it with `axum::extract::State<battery::State>`: the
/// [`FromRef`] implementation below takes the field `battery` of
/// [`AppState`].
#[derive(Debug, Clone)]
pub struct State {
    source: Arc<dyn BatterySource>,
}

impl State {
    /// The states of `serve`: the battery API and the consumption API
    /// share one source, so they share its read slots.
    #[must_use]
    pub fn new(config: &Config) -> (Self, consumption::State) {
        match config {
            Config::Database(options) => {
                let source = Arc::new(PostgresSource::new((**options).clone()));
                (
                    Self::with_source(source.clone()),
                    consumption::State::with_source(source),
                )
            }
            #[cfg(feature = "test-server")]
            Config::Scenarios(dir) => {
                let source = Arc::new(fake::FakeSource::new(dir.clone()));
                (
                    Self::with_source(source.clone()),
                    consumption::State::with_source(source),
                )
            }
        }
    }

    /// A state with a given source.
    #[must_use]
    pub fn with_source(source: Arc<dyn BatterySource>) -> Self {
        Self { source }
    }

    /// The data source.
    #[must_use]
    pub fn source(&self) -> &dyn BatterySource {
        self.source.as_ref()
    }
}

impl FromRef<AppState> for State {
    fn from_ref(state: &AppState) -> Self {
        state.battery.clone()
    }
}

/// The state of `serve-test` (feature `test-server`): the fake source
/// without `DATABASE_URL`, else the database.
#[cfg(feature = "test-server")]
#[must_use]
pub fn test_state(config: &Config) -> (State, consumption::State) {
    State::new(config)
}

/// The states of the unit tests of the router (`crate::app`): a source
/// that is never reached in those tests answers `internal_error`.
#[cfg(test)]
pub(crate) fn unit_test_state() -> (State, consumption::State) {
    let source = Arc::new(tests::Unreachable);
    (
        State::with_source(source.clone()),
        consumption::State::with_source(source),
    )
}

/// The routes of the battery API: `GET /battery` under `/api/v1`.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(handler::get_battery))
}

#[cfg(test)]
mod tests;
