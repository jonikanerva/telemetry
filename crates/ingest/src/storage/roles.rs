//! Runs `db/roles.sql`: the roles, the schema, and the privileges of
//! ADR 0002 ("Roles and privileges").
//!
//! The lead runs it once against production with the Neon owner role
//! (`README.md`, "Database roles"); the tests run it with roles and a
//! schema of their own. Both use [`apply_roles`], so the tests check the
//! script that production uses.
//!
//! # Passwords
//!
//! The script sets each password with `ALTER ROLE ... PASSWORD`, as plain
//! text. Neon accepts nothing else: a SCRAM-SHA-256 verifier that a client
//! computes fails with SQLSTATE XX000, "Neon only supports being given
//! plaintext passwords" (tested on the branch `test`, 26 November 2026),
//! because the compute hands the password to the Neon control plane. The
//! password is a bound `set_config()` value and the script builds the
//! statement inside a `DO` block, so the client SQL text holds no password.
//! The statement that the block runs holds it: a server log line or an
//! error context of that statement can show it, on the Neon side. After a
//! suspected leak, run the procedure again with new passwords and update
//! the Railway variables (`README.md`, "Database roles").

use sqlx::{Connection, PgConnection};

use super::connect::{RUN_CALL_DEADLINE, Schema, within};
use super::error::{Phase, StorageError, classify};
use crate::config::SecretString;

/// The SQL script. It reads its parameters from session settings.
pub const ROLES_SQL: &str = include_str!("../../../../db/roles.sql");

/// The role names of ADR 0002.
pub const INGEST_ROLE: &str = "telemetry_ingest";
/// The role names of ADR 0002.
pub const MIGRATOR_ROLE: &str = "telemetry_migrator";

/// The parameters of `db/roles.sql`.
#[derive(Debug)]
pub struct RoleSetup {
    /// The role of `ingest run`.
    pub ingest_role: String,
    /// The role of `ingest migrate`. It owns the schema.
    pub migrator_role: String,
    /// The schema of the raw tables.
    pub schema: Schema,
    /// The new password of the ingest role. `None` keeps the password.
    pub ingest_password: Option<SecretString>,
    /// The new password of the migrator role. `None` keeps the password.
    pub migrator_password: Option<SecretString>,
}

impl RoleSetup {
    /// The production names of ADR 0002 with the given passwords.
    #[must_use]
    pub fn production(
        ingest_password: Option<SecretString>,
        migrator_password: Option<SecretString>,
    ) -> Self {
        Self {
            ingest_role: INGEST_ROLE.to_owned(),
            migrator_role: MIGRATOR_ROLE.to_owned(),
            schema: Schema::raw(),
            ingest_password,
            migrator_password,
        }
    }
}

/// Runs `db/roles.sql` in one transaction on a connection of the owner
/// role.
///
/// The parameters are bound values of `set_config(..., true)`, so they hold
/// only for this transaction and never appear in the SQL text that the
/// client sends (module documentation). The statements, also the wait for the advisory lock of the script, have the
/// statement timeout of the connection; the whole call has
/// [`RUN_CALL_DEADLINE`] on the client.
///
/// # Errors
///
/// Returns the classified [`StorageError`]. On an error, the transaction
/// rolls back and nothing changes.
pub async fn apply_roles(conn: &mut PgConnection, setup: &RoleSetup) -> Result<(), StorageError> {
    let text = |secret: &Option<SecretString>| {
        secret
            .as_ref()
            .map_or(String::new(), |secret| secret.expose().to_owned())
    };
    within(RUN_CALL_DEADLINE, async {
        let mut tx = conn
            .begin()
            .await
            .map_err(|error| classify(Phase::Statement, &error))?;
        // num_nulls() keeps the passwords out of the result row.
        sqlx::query_scalar!(
            "SELECT num_nulls(
                set_config('telemetry.ingest_role', $1, true),
                set_config('telemetry.migrator_role', $2, true),
                set_config('telemetry.schema', $3, true),
                set_config('telemetry.ingest_password', $4, true),
                set_config('telemetry.migrator_password', $5, true)
            ) AS \"nulls!\"",
            setup.ingest_role,
            setup.migrator_role,
            setup.schema.as_str(),
            text(&setup.ingest_password),
            text(&setup.migrator_password),
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| classify(Phase::Statement, &error))?;
        sqlx::raw_sql(ROLES_SQL)
            .execute(&mut *tx)
            .await
            .map_err(|error| classify(Phase::Statement, &error))?;
        tx.commit()
            .await
            .map_err(|error| classify(Phase::Statement, &error))
    })
    .await
}
