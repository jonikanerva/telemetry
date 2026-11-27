//! Shutdown signals and the shutdown deadline.
//!
//! Railway sends SIGTERM and waits for the draining time (15 s) before it
//! sends SIGKILL. At SIGTERM or SIGINT the server stops accepting, lets
//! each open request finish, and closes the connections that are still open
//! after [`SHUTDOWN_DEADLINE`] (10 s, below the draining time). Then the
//! binary ends the runtime with `shutdown_background`, because a dropped
//! Tokio runtime waits without a limit for its blocking tasks.
//!
//! Outcomes (the exit code of `src/main.rs`):
//!
//! - every connection closed before the deadline: exit code 0;
//! - connections still open at the deadline: event
//!   `connections_closed_at_deadline`, exit code 1;
//! - a signal handler cannot be set up: event `signal_setup_failed` with
//!   the signal name, then the same shutdown at once, exit code 1. The
//!   same rule applies to each signal.

use std::time::Duration;

use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info};

/// Time from the shutdown signal to the forced close of open connections.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// Why the server stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownCause {
    /// SIGTERM or SIGINT arrived. The value is the signal name.
    Signal(&'static str),
    /// A signal handler could not be set up. The value is the signal name.
    SignalSetupFailed(&'static str),
}

impl ShutdownCause {
    /// Whether the cause is a failure of the process.
    #[must_use]
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::SignalSetupFailed(_))
    }
}

/// Completes when SIGTERM or SIGINT arrives, or at once when a handler
/// cannot be set up.
pub async fn signal_received() -> ShutdownCause {
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        return setup_failed("SIGTERM");
    };
    let Ok(mut interrupt) = signal(SignalKind::interrupt()) else {
        return setup_failed("SIGINT");
    };
    let name = tokio::select! {
        _ = terminate.recv() => "SIGTERM",
        _ = interrupt.recv() => "SIGINT",
    };
    info!(signal = name, "shutdown_signal");
    ShutdownCause::Signal(name)
}

fn setup_failed(name: &'static str) -> ShutdownCause {
    error!(signal = name, "signal_setup_failed");
    ShutdownCause::SignalSetupFailed(name)
}
