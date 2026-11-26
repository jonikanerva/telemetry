//! Process lifecycle: shutdown signals and the `run` sequence.
//!
//! Sequence of the `run` command:
//!
//! 1. Install the SIGTERM and SIGINT handlers. A failure stops the start.
//! 2. Listen on `PORT` and serve the health endpoint ([`crate::http`]).
//! 3. On the first signal, stop the server. The server closes the open
//!    connections at its shutdown deadline of 3 s at the latest
//!    ([`http::LIMITS`]). Railway sends SIGKILL `drainingSeconds` (30 s)
//!    after SIGTERM. ADR 0001 gives the later shutdown steps at most 3 s
//!    for the ZMQ drain and 20 s for the flush, so the whole sequence
//!    stays below 30 s even when the steps run one after another.
//! 4. Return. The binary then stops the runtime without a wait.

use std::io;
use std::net::Ipv4Addr;

use tokio::net::TcpListener;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing::info;

use crate::config::RunConfig;
use crate::http;

/// A failure of the `run` command.
///
/// The text holds static words and operating system error text only.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A signal handler could not be installed.
    #[error("cannot install the {signal} handler: {source}")]
    Signal {
        /// Name of the signal.
        signal: &'static str,
        /// Error from the operating system.
        source: io::Error,
    },
    /// The server could not listen on the configured port.
    #[error("cannot listen on the configured port: {0}")]
    Bind(#[source] io::Error),
}

/// The handlers of the signals that stop the process.
pub struct ShutdownSignals {
    terminate: Signal,
    interrupt: Signal,
}

impl ShutdownSignals {
    /// Installs the SIGTERM and SIGINT handlers. Call it inside a Tokio
    /// runtime.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::Signal`] when a handler cannot be installed. The
    /// rule is the same for both signals: the process does not start.
    pub fn install() -> Result<Self, RunError> {
        Ok(Self {
            terminate: install(SignalKind::terminate(), "SIGTERM")?,
            interrupt: install(SignalKind::interrupt(), "SIGINT")?,
        })
    }

    /// Waits for the first SIGTERM or SIGINT and returns its name.
    pub async fn recv(mut self) -> &'static str {
        // `recv` returns `None` only when the signal driver stops. Treat
        // that as a shutdown request too, so that the process cannot run
        // without a working stop path.
        tokio::select! {
            _ = self.terminate.recv() => "SIGTERM",
            _ = self.interrupt.recv() => "SIGINT",
        }
    }
}

fn install(kind: SignalKind, name: &'static str) -> Result<Signal, RunError> {
    signal(kind).map_err(|source| RunError::Signal {
        signal: name,
        source,
    })
}

/// Runs the `run` command until a shutdown signal arrives and the server
/// has stopped.
///
/// # Errors
///
/// Returns [`RunError`] when a signal handler cannot be installed or the
/// port cannot be bound.
pub async fn run(config: RunConfig) -> Result<(), RunError> {
    let signals = ShutdownSignals::install()?;
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.port))
        .await
        .map_err(RunError::Bind)?;
    let port = listener.local_addr().map_err(RunError::Bind)?.port();
    info!(port, "listening");
    let shutdown = async move {
        let signal = signals.recv().await;
        info!(signal, "shutdown_started");
    };
    http::serve(listener, http::router(), shutdown, http::LIMITS).await;
    Ok(())
}
