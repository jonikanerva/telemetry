//! Web application server: the HTTP API, the SPA files, and the export of
//! the `OpenAPI` contract.
//!
//! Modules:
//!
//! - [`config`]: the only reader of the environment. It validates the
//!   configuration once, at startup.
//! - [`app`]: the router, the middleware, the static files, and
//!   [`app::AppState`].
//! - [`problem`]: the public error contract (RFC 9457 problem details).
//! - [`health`]: `GET /health`.
//! - [`openapi`]: the documented routes and the `OpenAPI` export.
//! - [`auth`], [`battery`], and [`consumption`]: the API routes under `/api/v1`. Later
//!   slices fill them.
//! - [`wire`]: wire types that several API modules share.
//! - [`logging`]: JSON logs with an allowlist of fields.
//! - [`server`]: the HTTP/1 accept loop and its limits.
//! - [`shutdown`]: the signals and the shutdown deadline.
//! - `test_routes`: test-only routes for the browser tests (feature
//!   `test-server`).
//! - `tls`: the TLS of the command `serve-test` (feature `test-server`).
//! - [`cli`]: the command line.
//!
//! The ingest image (`ingest/Dockerfile`) does not build this crate. It
//! reads only `Cargo.toml` of this crate and makes a stub target, so a
//! change of the sources does not change the ingest image. Do not declare
//! target paths in `Cargo.toml`: the stub works only with the automatic
//! target discovery of Cargo. This crate shares no code with `ingest`.

pub mod app;
pub mod auth;
pub mod battery;
pub mod cli;
pub mod config;
pub mod consumption;
pub mod health;
pub mod logging;
pub mod openapi;
pub mod problem;
pub mod server;
pub mod shutdown;
#[cfg(feature = "test-server")]
pub mod test_routes;
#[cfg(feature = "test-server")]
pub mod tls;
pub mod wire;

#[cfg(test)]
pub(crate) mod test_support;
