//! Entry point of the `ingest` binary.
//!
//! Exit codes: 0 for success, 1 for a failure, 2 for an invalid command
//! line.

use std::process::ExitCode;

use ingest::cli::{self, Command};
use ingest::config::RunConfig;
use ingest::{lifecycle, logging};
use tracing::{error, info};

const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    if logging::init().is_err() {
        // Without a logger the process cannot report the failure. This
        // occurs only when a global logger already exists, and nothing in
        // this binary sets one before this call.
        return ExitCode::FAILURE;
    }
    match cli::parse(std::env::args_os().skip(1)) {
        Ok(Command::Run) => run(),
        Ok(Command::Migrate) => migrate(),
        Err(err) => {
            error!(error = %err, "invalid_command");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn run() -> ExitCode {
    let config = match RunConfig::from_vars(|name| std::env::var_os(name)) {
        Ok(config) => config,
        Err(err) => {
            error!(error = %err, "invalid_configuration");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!(error = %err, "runtime_start_failed");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(lifecycle::run(config));
    // Dropping a runtime waits without a limit for its blocking tasks.
    // `lifecycle::run` has already applied the shutdown deadline, so stop
    // the runtime without a wait.
    runtime.shutdown_background();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(error = %err, "run_failed");
            ExitCode::FAILURE
        }
    }
}

fn migrate() -> ExitCode {
    // `ingest migrate` applies the SQLx migrations that the binary embeds
    // (ADR 0002). The repository has no migrations yet.
    info!("no_migrations");
    ExitCode::SUCCESS
}
