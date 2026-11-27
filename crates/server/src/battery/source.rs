//! The data source of the battery view.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use telemetry_web_domain::battery::{ContractViolation, RangeKey, Snapshot};

/// What one request asks of the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRequest {
    /// The range.
    pub key: RangeKey,
    /// The scenario of the test server (cookie `telemetry_test_scenario`).
    /// The PostgreSQL source ignores it.
    pub scenario: Option<String>,
}

/// Why a source gives no snapshot. Each variant maps to one problem code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The database cannot be reached, or it refuses work for a reason
    /// that can pass (`database_unavailable`).
    #[error("database unavailable: {0}")]
    Unavailable(&'static str),
    /// The concurrency bound is full (`service_busy`).
    #[error("too many database requests")]
    Busy,
    /// The read took longer than its deadline (`timeout`).
    #[error("database deadline: {0}")]
    Timeout(&'static str),
    /// The stored data breaks a contract (`data_contract_violation`).
    #[error("data contract violation: {0}")]
    Contract(ContractViolation),
    /// A failure that a retry does not fix, for example a missing
    /// privilege or a wrong password (`internal_error`).
    #[error("internal failure: {0}")]
    Internal(&'static str),
}

/// A boxed future of a source.
pub type SnapshotFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Snapshot, SourceError>> + Send + 'a>>;

/// A source of one [`Snapshot`] for each request.
///
/// The implementation owns its clock: the PostgreSQL source reads the
/// system clock, and the fake source the fixed time of its scenario.
pub trait BatterySource: Send + Sync + fmt::Debug {
    /// Reads the snapshot of the range that ends now.
    fn snapshot<'a>(&'a self, request: &'a SourceRequest) -> SnapshotFuture<'a>;
}
