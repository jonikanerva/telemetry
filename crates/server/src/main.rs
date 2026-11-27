//! Entry point of the `telemetry-web-server` binary.
//!
//! Exit codes: 0 for success, 1 for a failure, 2 for an invalid command
//! line.

use std::net::{IpAddr, Ipv4Addr};
use std::process::ExitCode;

use telemetry_web_server::app::{self, AppState};
use telemetry_web_server::cli::{self, Command};
use telemetry_web_server::config::Config;
use telemetry_web_server::server::{self, LIMITS, ServeOutcome};
use telemetry_web_server::shutdown::{self, ShutdownCause};
#[cfg(feature = "test-server")]
use telemetry_web_server::tls::TestTls;
use telemetry_web_server::{logging, openapi};
use tokio::net::TcpListener;
use tracing::{error, info};

const EXIT_USAGE: u8 = 2;

/// Which router, address, and transport a serve command uses.
#[derive(Clone, Copy)]
enum Mode {
    /// `serve`: the production router on all interfaces, plain HTTP.
    Production,
    /// `serve-test`: the router with the test-only routes on the loopback
    /// interface, HTTPS.
    #[cfg(feature = "test-server")]
    Test,
}

fn main() -> ExitCode {
    if logging::init().is_err() {
        // Without a logger the process cannot report the failure. This
        // occurs only when a global logger already exists, and nothing in
        // this binary sets one before this call.
        return ExitCode::FAILURE;
    }
    logging::install_panic_hook();
    match cli::parse(std::env::args_os().skip(1)) {
        Ok(Command::Serve) => serve(Mode::Production),
        #[cfg(feature = "test-server")]
        Ok(Command::ServeTest) => serve(Mode::Test),
        Ok(Command::ExportOpenapi(path)) => match openapi::export(&path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                error!(error = %err, "export_failed");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            error!(error = %err, "invalid_command");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn serve(mode: Mode) -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            error!(
                name = err.name(),
                reason = err.reason(),
                "invalid_configuration"
            );
            return ExitCode::FAILURE;
        }
    };
    #[cfg(feature = "test-server")]
    let tls = match mode {
        Mode::Production => None,
        Mode::Test => match telemetry_web_server::config::test_tls_from_env() {
            Ok(tls) => Some(tls),
            Err(err) => {
                error!(
                    name = err.name(),
                    reason = err.reason(),
                    "invalid_configuration"
                );
                return ExitCode::FAILURE;
            }
        },
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!(error = ?err.kind(), "runtime_start_failed");
            return ExitCode::FAILURE;
        }
    };
    #[cfg(feature = "test-server")]
    let code = runtime.block_on(run(config, mode, tls));
    #[cfg(not(feature = "test-server"))]
    let code = runtime.block_on(run(config, mode));
    // Dropping a runtime waits without a limit for its blocking tasks.
    // `server::serve` has already applied the shutdown deadline, so stop
    // the runtime without a wait.
    runtime.shutdown_background();
    code
}

async fn run(
    config: Config,
    mode: Mode,
    #[cfg(feature = "test-server")] tls: Option<TestTls>,
) -> ExitCode {
    let state = AppState::new(config.version.clone());
    let (address, router): (IpAddr, _) = match mode {
        Mode::Production => (
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            app::router(state, &config.static_files),
        ),
        #[cfg(feature = "test-server")]
        Mode::Test => (
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            app::test_server_router(state, &config.static_files),
        ),
    };
    let listener = match TcpListener::bind((address, config.port)).await {
        Ok(listener) => listener,
        Err(err) => {
            error!(error = ?err.kind(), port = config.port, "bind_failed");
            return ExitCode::FAILURE;
        }
    };
    info!(port = config.port, "listening");

    let (cause_tx, cause_rx) = tokio::sync::oneshot::channel();
    let shutdown = async move {
        let cause = shutdown::signal_received().await;
        cause_tx.send(cause).ok();
    };
    #[cfg(feature = "test-server")]
    let outcome = match tls {
        Some(tls) => server::serve_tls(listener, router, shutdown, LIMITS, tls).await,
        None => server::serve(listener, router, shutdown, LIMITS).await,
    };
    #[cfg(not(feature = "test-server"))]
    let outcome = server::serve(listener, router, shutdown, LIMITS).await;
    let cause = cause_rx.await.ok();
    match (outcome, cause) {
        (ServeOutcome::Drained, Some(ShutdownCause::Signal(_))) => ExitCode::SUCCESS,
        // The deadline ended open requests, or the signal set-up failed.
        _ => ExitCode::FAILURE,
    }
}
