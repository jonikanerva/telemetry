//! Test-only routes of the session API (feature `test-server`).
//!
//! The browser tests share one test server, so each test starts from a
//! known state:
//!
//! - `POST /test-only/auth/reset`: ends every session, fills the throttle
//!   bucket, and sets the clock offset back to zero. 204.
//! - `POST /test-only/auth/advance?seconds=N`: moves the clock of the
//!   session rules forward by `N` seconds (1 to 31536000), for the expiry
//!   journeys. 204, or 400 for another value.
//! - `GET /test-only/auth/stats`: the counts since the last reset, as JSON
//!   `{"refused_origin": n, "verifications": n}`: requests that the origin
//!   rule refused, and password verifications that started. The CSRF
//!   journey checks with them that a request of another site starts no
//!   verification.
//!
//! The release image never enables the feature, so these routes never
//! exist in production. `crate::test_routes` mounts them outside the API
//! router, so the guard of `crate::auth` does not apply to them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State as Extract};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::State;
use crate::app::AppState;

/// The largest step of `advance`: one year.
pub const MAX_ADVANCE_SECONDS: u64 = 365 * 86_400;

/// The test-only routes of the session API.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/test-only/auth/reset", post(reset))
        .route("/test-only/auth/advance", post(advance))
        .route("/test-only/auth/stats", get(stats))
}

/// The counts of the test server.
#[derive(Debug, Default)]
pub struct Stats {
    refused_origin: AtomicU64,
    verifications: AtomicU64,
}

impl Stats {
    pub(super) fn count_refused_origin(&self) {
        self.refused_origin.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn count_verification(&self) {
        self.verifications.fetch_add(1, Ordering::Relaxed);
    }

    fn reset(&self) {
        self.refused_origin.store(0, Ordering::Relaxed);
        self.verifications.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Serialize)]
struct StatsBody {
    refused_origin: u64,
    verifications: u64,
}

async fn stats(Extract(auth): Extract<State>) -> Json<StatsBody> {
    let stats = &auth.0.test_stats;
    Json(StatsBody {
        refused_origin: stats.refused_origin.load(Ordering::Relaxed),
        verifications: stats.verifications.load(Ordering::Relaxed),
    })
}

async fn reset(Extract(auth): Extract<State>) -> Response {
    if let Some(clock) = &auth.0.test_clock {
        clock.reset();
    }
    auth.store().clear();
    auth.0.test_stats.reset();
    auth.0.throttle.reset(auth.now().mono);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
struct Advance {
    seconds: u64,
}

async fn advance(
    Extract(auth): Extract<State>,
    query: Result<Query<Advance>, QueryRejection>,
) -> Response {
    let Ok(Query(Advance { seconds })) = query else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match (&auth.0.test_clock, seconds) {
        (Some(clock), 1..=MAX_ADVANCE_SECONDS) => {
            clock.advance(Duration::from_secs(seconds));
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}
