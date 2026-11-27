//! The battery view under `/api/v1` (ADR 0012).
//!
//! Slice S3 (`feat/battery-api`) fills this module: `GET
//! /api/v1/battery?range=...`, its DTOs, and the read from the database.
//! Slice S1 mounts the empty router.
//!
//! Extension points for S3:
//!
//! - Register the handler with `utoipa_axum::routes!` in [`router`]. The
//!   router is nested at `/api/v1`.
//! - Proposed location of the data source: a trait `BatterySource` in
//!   `crates/server/src/battery/source.rs`, with the PostgreSQL adapter in
//!   `battery/postgres.rs` and the fake of the test server (feature
//!   `test-server`) in `battery/fake.rs`. [`crate::app::AppState`] gets
//!   one field with the source, for example `Arc<dyn BatterySource>`.
//! - Use [`crate::wire::Instant`] for each instant of the DTOs.
//! - The axum feature `query` is not enabled yet. Enable it in
//!   `crates/server/Cargo.toml` and map `QueryRejection` to
//!   `ProblemCode::InvalidRequest` in `crate::problem`.

use utoipa_axum::router::OpenApiRouter;

use crate::app::AppState;

/// The routes of the battery API. Empty until slice S3.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
