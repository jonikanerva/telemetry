//! The embedded migrations, `ingest migrate`, and the migration check of
//! `ingest run` (ADR 0002, "Migrations").

use std::collections::HashMap;
use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};

use super::connect::{
    CallConnection, MIGRATE_DEADLINE, RUN_CALL_DEADLINE, Schema, connect, within,
};
use super::error::{ErrorCode, FailureClass, Phase, StorageError, classify, classify_migrate};

/// The migrations in `migrations/` at the repository root. The binary
/// embeds them at build time; `build.rs` rebuilds the crate when the
/// directory changes.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// The result of `ingest migrate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrateOutcome {
    /// Migrations that this run applied.
    pub applied: usize,
    /// Migrations that the binary embeds.
    pub embedded: usize,
}

/// Applies the embedded migrations that the database does not have yet.
///
/// The connection must use [`super::connect_options`] with the role
/// `telemetry_migrator` (ADR 0002, "Roles and privileges"). The steps:
///
/// 1. Open one connection.
/// 2. Check that the first schema of the `search_path` exists and is
///    `schema`. Else the migrations would have no schema to create the
///    tables in.
/// 3. Run the `SQLx` migrator. It takes an advisory lock for the database,
///    so two runs at the same time apply each migration once. It records
///    each migration in the table `_sqlx_migrations` of `schema`.
///    Migrations that the database has and the binary does not know are
///    allowed: an earlier release can run its pre-deploy command after a
///    later release (expand and contract, ADR 0002).
/// 4. Close the connection.
///
/// Deadlines: the connect has [`super::CONNECT_TIMEOUT`]; each statement,
/// also the wait for the advisory lock, has the statement timeout of
/// `options` ([`super::MIGRATE_STATEMENT_TIMEOUT`] for `ingest migrate`);
/// the whole call has [`super::MIGRATE_DEADLINE`] on the client.
///
/// # Errors
///
/// Returns a [`StorageError`]. A changed checksum of an applied migration
/// is [`ErrorCode::MigrationChanged`]; a missing schema is
/// [`ErrorCode::SchemaMissing`].
pub async fn migrate(
    options: &PgConnectOptions,
    schema: &Schema,
) -> Result<MigrateOutcome, StorageError> {
    within(MIGRATE_DEADLINE, async {
        let mut conn = connect(options).await?;
        let result = migrate_on(&mut conn, schema).await;
        // A failed close loses nothing: the server ends the session when the
        // socket closes.
        let _closed = conn.close().await;
        result
    })
    .await
}

async fn migrate_on(
    conn: &mut PgConnection,
    schema: &Schema,
) -> Result<MigrateOutcome, StorageError> {
    let current: Option<String> = sqlx::query_scalar!("SELECT current_schema() AS \"schema\"")
        .fetch_one(&mut *conn)
        .await
        .map_err(|error| classify(Phase::Statement, &error))?;
    if current.as_deref() != Some(schema.as_str()) {
        return Err(StorageError::new(
            FailureClass::Answered,
            ErrorCode::SchemaMissing,
        ));
    }
    let before = applied_versions(conn).await?;
    let mut migrator = Migrator::with_migrations(MIGRATOR.iter().cloned().collect());
    migrator.set_ignore_missing(true);
    migrator
        .run(&mut *conn)
        .await
        .map_err(|error| classify_migrate(&error))?;
    let embedded = MIGRATOR.iter().count();
    let applied = MIGRATOR
        .iter()
        .filter(|migration| !before.contains_key(&migration.version))
        .count();
    Ok(MigrateOutcome { applied, embedded })
}

/// The applied migrations: version and checksum. An empty map when the
/// table `_sqlx_migrations` does not exist yet.
async fn applied_versions(conn: &mut PgConnection) -> Result<HashMap<i64, Vec<u8>>, StorageError> {
    let exists: Option<bool> =
        sqlx::query_scalar!("SELECT to_regclass('_sqlx_migrations') IS NOT NULL AS \"exists\"")
            .fetch_one(&mut *conn)
            .await
            .map_err(|error| classify(Phase::Statement, &error))?;
    if exists != Some(true) {
        return Ok(HashMap::new());
    }
    read_applied(conn).await
}

async fn read_applied(conn: &mut PgConnection) -> Result<HashMap<i64, Vec<u8>>, StorageError> {
    let rows = sqlx::query!("SELECT version, checksum FROM _sqlx_migrations WHERE success")
        .fetch_all(&mut *conn)
        .await
        .map_err(|error| classify(Phase::Statement, &error))?;
    Ok(rows
        .into_iter()
        .map(|row| (row.version, row.checksum))
        .collect())
}

/// The migration check of `ingest run` (ADR 0002, "Migrations").
///
/// One round trip: it reads `_sqlx_migrations` and checks that every
/// migration that the binary embeds is applied with the same checksum.
/// Migrations that the binary does not know are allowed. The call has
/// [`super::RUN_CALL_DEADLINE`] on the client.
///
/// | Result | Meaning for the gate (ADR 0001, condition 3) |
/// | ------ | -------------------------------------------- |
/// | `Ok(())` | The check passed. |
/// | [`FailureClass::NoAnswer`] | No session: retry; degraded mode after 120 s. |
/// | [`FailureClass::Answered`] | Authentication error, [`ErrorCode::MigrationMissing`] (also when the table does not exist), [`ErrorCode::MigrationChanged`], or another answered error: the gate stays closed. |
///
/// # Errors
///
/// Returns the [`StorageError`] of the table above.
pub async fn check_migrations(pool: &PgPool) -> Result<(), StorageError> {
    check_migrations_within(pool, RUN_CALL_DEADLINE).await
}

/// [`check_migrations`] with another deadline, for the tests.
pub(crate) async fn check_migrations_within(
    pool: &PgPool,
    deadline: Duration,
) -> Result<(), StorageError> {
    within(deadline, check(pool)).await
}

async fn check(pool: &PgPool) -> Result<(), StorageError> {
    let mut call = CallConnection::acquire(pool).await?;
    let read = read_applied(call.get()?).await;
    call.finish(
        read.as_ref()
            .map_or_else(|error| error.answered(), |_| true),
    );
    let applied = match read {
        Ok(applied) => applied,
        // 42P01: the table `_sqlx_migrations` does not exist, so no
        // migration is applied.
        Err(error)
            if error
                .sqlstate()
                .is_some_and(|state| state.as_str() == "42P01") =>
        {
            return Err(StorageError::new(
                FailureClass::Answered,
                ErrorCode::MigrationMissing,
            ));
        }
        Err(error) => return Err(error),
    };
    compare(&applied)
}

fn compare(applied: &HashMap<i64, Vec<u8>>) -> Result<(), StorageError> {
    for migration in MIGRATOR.iter() {
        match applied.get(&migration.version) {
            None => {
                return Err(StorageError::new(
                    FailureClass::Answered,
                    ErrorCode::MigrationMissing,
                ));
            }
            Some(checksum) if checksum.as_slice() != &*migration.checksum => {
                return Err(StorageError::new(
                    FailureClass::Answered,
                    ErrorCode::MigrationChanged,
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_applied() -> HashMap<i64, Vec<u8>> {
        MIGRATOR
            .iter()
            .map(|migration| (migration.version, migration.checksum.to_vec()))
            .collect()
    }

    #[test]
    fn the_binary_embeds_the_migrations() {
        assert!(MIGRATOR.iter().count() >= 1);
        assert_eq!(MIGRATOR.iter().next().map(|m| m.version), Some(1));
    }

    #[test]
    fn every_embedded_migration_must_be_applied() {
        assert_eq!(compare(&all_applied()), Ok(()));
        let mut missing = all_applied();
        missing.remove(&1);
        assert_eq!(
            compare(&missing).unwrap_err().code,
            ErrorCode::MigrationMissing
        );
        assert_eq!(
            compare(&HashMap::new()).unwrap_err().code,
            ErrorCode::MigrationMissing
        );
    }

    #[test]
    fn a_changed_checksum_fails() {
        let mut changed = all_applied();
        changed.insert(1, vec![0; 48]);
        let error = compare(&changed).unwrap_err();
        assert_eq!(error.code, ErrorCode::MigrationChanged);
        assert_eq!(error.class, FailureClass::Answered);
    }

    #[test]
    fn unknown_applied_migrations_are_allowed() {
        let mut later = all_applied();
        later.insert(9_999, vec![1; 48]);
        assert_eq!(compare(&later), Ok(()));
    }
}
