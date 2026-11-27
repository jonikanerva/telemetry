//! Entry point of the `ingest` binary.
//!
//! Exit codes: 0 for success, 1 for a failure, 2 for an invalid command
//! line.

use std::process::ExitCode;

use ingest::cli::{self, Command};
use ingest::config::{MigrateConfig, RunConfig, TestOverrides};
use ingest::storage::{self, MIGRATE_STATEMENT_TIMEOUT, Schema};
use ingest::{lifecycle, logging};
use tracing::{error, info, warn};

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
    let var = |name: &str| std::env::var_os(name);
    let mut config = match RunConfig::from_vars(var) {
        Ok(config) => config,
        Err(err) => {
            error!(error = %err, "invalid_configuration");
            return ExitCode::FAILURE;
        }
    };
    // Only a debug build reads the test overrides (`ingest::config`, "Test
    // overrides"). The image runs a release build, which never reads them.
    if cfg!(debug_assertions) {
        match TestOverrides::from_vars(var) {
            Ok(overrides) if overrides.is_empty() => {}
            Ok(overrides) => {
                warn!("test_overrides");
                config = config.with_overrides(overrides);
            }
            Err(err) => {
                error!(error = %err, "invalid_configuration");
                return ExitCode::FAILURE;
            }
        }
    }
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

/// Applies the embedded migrations with `MIGRATION_DATABASE_URL` (ADR
/// 0002, "Migrations"). Railway runs this as the pre-deploy command; a
/// failure stops the deployment, and the old version keeps running.
fn migrate() -> ExitCode {
    let config = match MigrateConfig::from_vars(|name| std::env::var_os(name)) {
        Ok(config) => config,
        Err(err) => {
            error!(error = %err, "invalid_configuration");
            return ExitCode::FAILURE;
        }
    };
    let schema = Schema::raw();
    let options =
        match storage::connect_options(&config.database_url, &schema, MIGRATE_STATEMENT_TIMEOUT) {
            Ok(options) => options,
            Err(err) => {
                error!(class = err.class.as_str(), code = %err.code, "migrate_failed");
                return ExitCode::FAILURE;
            }
        };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!(error = %err, "runtime_start_failed");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(storage::migrate(&options, &schema));
    runtime.shutdown_background();
    match result {
        Ok(outcome) => {
            info!(
                applied = outcome.applied,
                embedded = outcome.embedded,
                "migrated"
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            // The class and the code are static text or a SQLSTATE; the
            // text of the database error is never logged (ADR 0001).
            error!(class = err.class.as_str(), code = %err.code, "migrate_failed");
            ExitCode::FAILURE
        }
    }
}
