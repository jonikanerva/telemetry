//! Connection settings: TLS, schema, timeouts, and the pool.

use std::future::Future;
use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{Connection, PgConnection, PgPool};

use super::error::{ErrorCode, FailureClass, Phase, StorageError, classify};
use crate::config::SecretString;

/// The name of a schema that holds the raw tables.
///
/// Production uses [`Schema::raw`]. The tests use a schema of their own
/// for each test, so that test runs do not share tables. The name goes into
/// the `search_path` of each connection and into DDL of the tests, so it
/// must be a plain identifier: 1 to 63 characters from `a`-`z`, `0`-`9`,
/// and `_`, starting with a letter or `_`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Schema(String);

impl Schema {
    /// The schema of ADR 0002.
    #[must_use]
    pub fn raw() -> Self {
        Self("raw".to_owned())
    }

    /// A schema with another name. Returns `None` when the name is not a
    /// plain identifier (type documentation).
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let mut bytes = name.bytes();
        let first = bytes.next()?;
        let plain = (first.is_ascii_lowercase() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            && name.len() <= 63;
        plain.then(|| Self(name.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The deadline of one statement of `ingest run` (ADR 0001, "Watchdog":
/// statement 30 s). The role `telemetry_ingest` has the same value
/// (`db/roles.sql`); the connection sets it too, so that the deadline does
/// not depend on the role.
pub const RUN_STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// The deadline of one statement of `ingest migrate`. A migration step can
/// rewrite a table, so it gets more time than a write. A step that needs
/// more than 5 minutes fails, and the deployment stops before the new
/// version runs.
pub const MIGRATE_STATEMENT_TIMEOUT: Duration = Duration::from_mins(5);

/// The time to open a connection (ADR 0001, "Delivery path", step 6:
/// connect timeout 15 s).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The client-side deadline of one call of `ingest run`: one transaction of
/// the writer, one event insert, or the migration check, from the acquire
/// of the connection to the commit.
///
/// The server ends a statement after [`RUN_STATEMENT_TIMEOUT`], but it
/// cannot end a wait of the client for a connection that went silent: the
/// operating system closes such a TCP connection only after many minutes.
/// This deadline closes the connection after 90 s: at most 15 s to connect
/// and some statements of at most 30 s each. The server rolls back an open
/// transaction of a closed connection. 90 s stays below the 120 s of the
/// writer heartbeat of the watchdog (ADR 0001).
pub const RUN_CALL_DEADLINE: Duration = Duration::from_secs(90);

/// The client-side deadline of `ingest migrate` and of the role script, from
/// the connect to the close: 15 minutes. The migrator waits for its
/// advisory lock and runs each migration in a statement of at most
/// [`MIGRATE_STATEMENT_TIMEOUT`].
pub const MIGRATE_DEADLINE: Duration = Duration::from_mins(15);

/// Runs `call` with a client-side deadline. After the deadline the future
/// is dropped, which closes its connection, and the result is
/// [`FailureClass::NoAnswer`] with [`ErrorCode::Timeout`].
pub(crate) async fn within<T>(
    deadline: Duration,
    call: impl Future<Output = Result<T, StorageError>>,
) -> Result<T, StorageError> {
    tokio::time::timeout(deadline, call)
        .await
        .unwrap_or(Err(StorageError::new(
            FailureClass::NoAnswer,
            ErrorCode::Timeout,
        )))
}

/// Builds the connection settings from a database URL.
///
/// The settings are the same for every command, except the statement
/// timeout:
///
/// - **TLS with certificate verification** (`sslmode=verify-full`), always.
///   The value replaces the `sslmode` of the URL and of `PGSSLMODE`, so a
///   URL cannot turn the verification off. `SQLx` verifies the server
///   certificate and the host name with rustls and the aws-lc-rs provider
///   against the Mozilla root certificates of the `webpki-roots` crate
///   (`STACK.md` §5 and §7). `SQLx` ignores the libpq parameter
///   `channel_binding` (it logs a warning and continues); the certificate
///   check protects against a server in the middle instead.
/// - **`search_path`** is the schema only. The SQL of the storage and of
///   the migrations names no schema, so every table resolves in this schema.
///   A schema that does not exist resolves no table, and a migration then
///   fails with SQLSTATE `3F000` instead of creating tables elsewhere.
/// - **`statement_timeout`** bounds each statement.
/// - **`client_min_messages`** is `error`, so the server sends no NOTICE or
///   WARNING text. `SQLx` would log such text, and it is database text
///   (ADR 0001, "Logging").
///
/// # Errors
///
/// Returns [`ErrorCode::Configuration`] when the URL does not parse. The
/// error does not hold the URL.
pub fn connect_options(
    url: &SecretString,
    schema: &Schema,
    statement_timeout: Duration,
) -> Result<PgConnectOptions, StorageError> {
    let options = PgConnectOptions::from_str(url.expose())
        .map_err(|_| StorageError::new(FailureClass::Answered, ErrorCode::Configuration))?;
    Ok(options
        .ssl_mode(PgSslMode::VerifyFull)
        .application_name("ingest")
        .options([
            ("search_path", schema.as_str().to_owned()),
            (
                "statement_timeout",
                format!("{}ms", statement_timeout.as_millis()),
            ),
            ("client_min_messages", "error".to_owned()),
        ]))
}

/// Opens one connection outside a pool, for `ingest migrate` and for the
/// role script, within [`CONNECT_TIMEOUT`]. The caller closes it.
///
/// # Errors
///
/// Returns the classified [`StorageError`] of the connect phase, or
/// [`FailureClass::NoAnswer`] with [`ErrorCode::Timeout`] when no session
/// starts within [`CONNECT_TIMEOUT`], for example when a peer accepts the
/// TCP connection and never answers.
pub async fn connect(options: &PgConnectOptions) -> Result<PgConnection, StorageError> {
    within(CONNECT_TIMEOUT, async {
        PgConnection::connect_with(options)
            .await
            .map_err(|error| classify(Phase::Connect, &error))
    })
    .await
}

/// The pool of `ingest run` (ADR 0001, "Delivery path", step 6): at most 2
/// connections, none while idle, an idle timeout of 30 s, and a connect
/// timeout of 15 s. No connection stays open between two flushes of 15
/// minutes, so the Neon compute can scale to zero. The pool opens no
/// connection before the first use.
#[must_use]
pub fn run_pool(options: PgConnectOptions) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .min_connections(0)
        .idle_timeout(Duration::from_secs(30))
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect_lazy_with(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_call_past_its_deadline_is_no_answer() {
        let pending = std::future::pending::<Result<(), StorageError>>();
        let error = within(Duration::from_secs(90), pending).await.unwrap_err();
        assert_eq!(
            error,
            StorageError::new(FailureClass::NoAnswer, ErrorCode::Timeout)
        );
        let done = within(Duration::from_secs(1), async { Ok::<_, StorageError>(7) }).await;
        assert_eq!(done, Ok(7));
    }

    #[test]
    fn schema_names_are_plain_identifiers() {
        assert_eq!(Schema::raw().as_str(), "raw");
        for name in ["raw", "_x", "itest_1764160000_ab12cd34", &"a".repeat(63)] {
            assert_eq!(
                Schema::new(name).map(|schema| schema.0),
                Some(name.to_owned())
            );
        }
        for name in [
            "",
            "Raw",
            "1raw",
            "raw-x",
            "raw x",
            "raw;drop",
            "\"raw\"",
            "räw",
            &"a".repeat(64),
        ] {
            assert_eq!(Schema::new(name), None, "{name:?}");
        }
    }

    fn url(text: &str) -> SecretString {
        SecretString::new(text.to_owned())
    }

    #[test]
    fn options_force_certificate_verification() {
        for text in [
            "postgresql://u:p@db.invalid/neondb",
            "postgresql://u:p@db.invalid/neondb?sslmode=require&channel_binding=require",
            "postgresql://u:p@db.invalid/neondb?sslmode=disable",
            "postgresql://u:p@db.invalid/neondb?sslmode=prefer",
        ] {
            let options =
                connect_options(&url(text), &Schema::raw(), RUN_STATEMENT_TIMEOUT).unwrap();
            assert!(
                matches!(options.get_ssl_mode(), PgSslMode::VerifyFull),
                "{text}"
            );
        }
    }

    #[test]
    fn options_set_the_schema_and_the_timeout() {
        let schema = Schema::new("itest_1").unwrap();
        let options = connect_options(
            &url("postgresql://u:p@db.invalid/neondb"),
            &schema,
            RUN_STATEMENT_TIMEOUT,
        )
        .unwrap();
        let startup = options.get_options().unwrap();
        assert!(startup.contains("-c search_path=itest_1"), "{startup}");
        assert!(
            startup.contains("-c statement_timeout=30000ms"),
            "{startup}"
        );
        assert!(
            startup.contains("-c client_min_messages=error"),
            "{startup}"
        );
    }

    #[test]
    fn a_bad_url_gives_a_configuration_error_without_the_url() {
        let error = connect_options(
            &url("postgresql://u:hunter2@db.invalid:notaport/x"),
            &Schema::raw(),
            RUN_STATEMENT_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Configuration);
        assert!(!format!("{error} {error:?}").contains("hunter2"));
    }
}
