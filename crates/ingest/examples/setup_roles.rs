//! Runs `db/roles.sql` with the production names of ADR 0002, without
//! `psql`. The lead runs it once per Neon branch with the owner role
//! (`README.md`, "Database roles"). It is not part of the ingest image.
//!
//! Variables:
//!
//! | Variable | Content |
//! | -------- | ------- |
//! | `OWNER_DATABASE_URL` | URL of `neondb_owner` on the branch, database `neondb` |
//! | `TELEMETRY_INGEST_PASSWORD` | New password of `telemetry_ingest`; unset keeps its password and LOGIN |
//! | `TELEMETRY_MIGRATOR_PASSWORD` | New password of `telemetry_migrator`; unset keeps its password and LOGIN |
//! | `TELEMETRY_WEB_PASSWORD` | New password of `telemetry_web`; unset keeps its password and LOGIN |
//!
//! An unset password variable keeps the password and the LOGIN attribute
//! of its role, so a run for one role does not change the login of the
//! other roles.
//! Set only the variables of the roles that need a new password.
//!
//! A password must have 32 to 128 characters from `A`-`Z`, `a`-`z`, and
//! `0`-`9`, for example `openssl rand -hex 32`. Such a password needs no
//! escape in a URL and has more than the 60 bits of entropy that Neon
//! requires. The connection verifies the server certificate
//! (`sslmode=verify-full`). The program logs JSON events with static codes
//! only; it never logs a URL or a password.

use std::process::ExitCode;

use ingest::config::SecretString;
use ingest::logging;
use ingest::storage::{
    RUN_STATEMENT_TIMEOUT, RoleSetup, Schema, apply_roles, connect, connect_options,
};
use sqlx::Connection;
use tracing::{error, info};

/// The log target. The JSON formatter writes the fields of first-party
/// targets only (`ingest` and `ingest::...`).
const TARGET: &str = "ingest::setup_roles";

fn password(name: &'static str) -> Result<Option<SecretString>, &'static str> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value.into_string().map_err(|_| name)?;
    let valid =
        (32..=128).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if valid {
        Ok(Some(SecretString::new(value)))
    } else {
        Err(name)
    }
}

fn main() -> ExitCode {
    if logging::init().is_err() {
        return ExitCode::FAILURE;
    }
    let Some(url) = std::env::var_os("OWNER_DATABASE_URL").and_then(|url| url.into_string().ok())
    else {
        error!(target: TARGET,
            error = "OWNER_DATABASE_URL is not set",
            "invalid_configuration"
        );
        return ExitCode::FAILURE;
    };
    let passwords = password("TELEMETRY_INGEST_PASSWORD").and_then(|ingest| {
        Ok((
            ingest,
            password("TELEMETRY_MIGRATOR_PASSWORD")?,
            password("TELEMETRY_WEB_PASSWORD")?,
        ))
    });
    let (ingest_password, migrator_password, web_password) = match passwords {
        Ok(passwords) => passwords,
        Err(name) => {
            error!(target: TARGET, error = name, "invalid_password");
            return ExitCode::FAILURE;
        }
    };
    let setup = RoleSetup::production(ingest_password, migrator_password, web_password);
    let Some(public) = Schema::new("public") else {
        return ExitCode::FAILURE;
    };
    let options = match connect_options(&SecretString::new(url), &public, RUN_STATEMENT_TIMEOUT) {
        Ok(options) => options,
        Err(err) => {
            error!(target: TARGET, class = err.class.as_str(), code = %err.code, "roles_failed");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!(target: TARGET, error = %err, "runtime_start_failed");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        let mut conn = connect(&options).await?;
        let applied = apply_roles(&mut conn, &setup).await;
        // A failed close loses nothing: the server ends the session.
        let _closed = conn.close().await;
        applied
    });
    runtime.shutdown_background();
    match result {
        Ok(()) => {
            info!(target: TARGET, "roles_applied");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!(target: TARGET, class = err.class.as_str(), code = %err.code, "roles_failed");
            ExitCode::FAILURE
        }
    }
}
