//! The test database: guard, isolation, cleanup, and fixtures.
//!
//! Every test runs in a schema of its own on the test server, named
//! `itest_<unix seconds>_<random>`. A test that needs roles gets three
//! roles with the same prefix, because roles are shared by every database
//! of the server. The harness:
//!
//! - reads `TEST_DATABASE_URL` and stops with a clear message when it is
//!   missing;
//! - refuses the production endpoint, before any connection, and refuses a
//!   database that has a schema `raw` (production gets that schema at the
//!   release; the test branch must never have it);
//! - runs the test body in a task of its own, removes the schema and the
//!   roles afterwards, also when the body fails, and then fails the test;
//! - removes the objects of earlier runs that are older than 6 hours, for
//!   example after a killed run. A live run of another agent is younger, so
//!   it is not touched;
//! - closes every connection that it opens. The storage calls of a test
//!   close their own connections.

use std::collections::hash_map::RandomState;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ingest::config::SecretString;
use ingest::record::{
    Alert, Alerts, Connectivity, Decoded, Errors, Signal, SignalValue, Unprocessed,
    UnprocessedReason, VehicleData, VehicleError, Vin,
};
use ingest::storage::{
    MIGRATE_STATEMENT_TIMEOUT, MIGRATOR, QueuedRecord, RUN_STATEMENT_TIMEOUT, RoleSetup, Schema,
    apply_roles, connect_options, migrate,
};
use jiff::Timestamp;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{AssertSqlSafe, Connection, PgConnection, Row};

/// The Neon endpoint of the production branch (`STACK.md` §15).
const PRODUCTION_ENDPOINT: &str = "ep-young-dawn-bgyaj83l";

/// Objects of earlier runs older than this are removed.
const STALE_AFTER: Duration = Duration::from_hours(6);

/// An invented VIN: a real VIN never contains `I`.
pub const VIN: &str = "TESTVIN0000000001";

/// Reads `TEST_DATABASE_URL` and applies the production guard.
pub fn test_url() -> SecretString {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        panic!(
            "TEST_DATABASE_URL is not set. Run the database tests with mise run test, \
             or mise run test-neon for the tests that need Neon (README.md, Verification)."
        );
    };
    assert!(
        guard(&url).is_ok(),
        "TEST_DATABASE_URL names the production endpoint; the tests refuse it"
    );
    SecretString::new(url)
}

/// Refuses a URL that names the production endpoint anywhere: in the host,
/// in a `host` or `options` parameter, or with the `-pooler` suffix. The
/// error text does not hold the URL.
pub fn guard(url: &str) -> Result<(), &'static str> {
    if url.to_ascii_lowercase().contains(PRODUCTION_ENDPOINT) {
        return Err("production endpoint");
    }
    let options = PgConnectOptions::from_str(url).map_err(|_| "not a PostgreSQL URL")?;
    if options
        .get_host()
        .to_ascii_lowercase()
        .contains(PRODUCTION_ENDPOINT)
    {
        return Err("production endpoint");
    }
    Ok(())
}

/// The connection settings of the owner role for `schema`, as the storage
/// builds them for `ingest run`: `sslmode=verify-full`, the `search_path`,
/// the timeouts.
pub fn owner_options(schema: &Schema) -> PgConnectOptions {
    connect_options(&test_url(), schema, RUN_STATEMENT_TIMEOUT).unwrap()
}

/// The settings of `ingest migrate` for `schema`: the statement timeout of
/// 5 minutes also bounds the wait for the advisory lock of the migrator,
/// which parallel test runs share.
pub fn migrate_options(schema: &Schema) -> PgConnectOptions {
    connect_options(&test_url(), schema, MIGRATE_STATEMENT_TIMEOUT).unwrap()
}

/// A schema name that no other run uses.
fn new_prefix() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // `RandomState` takes its keys from the random source of the operating
    // system, so two processes get different values.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    format!("itest_{seconds}_{:016x}", hasher.finish())
}

/// The schema of one test and its settings.
pub struct TestDb {
    pub schema: Schema,
    pub options: PgConnectOptions,
}

impl TestDb {
    /// A connection of the owner role with the schema in its `search_path`.
    pub async fn owner(&self) -> PgConnection {
        PgConnection::connect_with(&self.options).await.unwrap()
    }

    /// Runs the embedded migrations as `ingest migrate` does.
    pub async fn migrate(&self) {
        migrate(&migrate_options(&self.schema), &self.schema)
            .await
            .unwrap();
    }

    /// Applies the embedded migrations with the `SQLx` migrator, but without
    /// its advisory lock. The lock is one for each database, so parallel
    /// tests would wait for each other; each test has its own schema, so
    /// it needs no lock. The tests of `schema.rs` and `roles.rs` use
    /// [`TestDb::migrate`], the path of `ingest migrate`.
    pub async fn migrate_unlocked(&self) {
        let mut migrator = Migrator::with_migrations(MIGRATOR.iter().cloned().collect());
        migrator.set_locking(false);
        let mut conn = self.owner().await;
        migrator.run(&mut conn).await.unwrap();
        conn.close().await.unwrap();
    }
}

/// The roles of one test.
pub struct TestRoles {
    pub setup: RoleSetup,
    pub ingest: PgConnectOptions,
    pub migrator: PgConnectOptions,
    /// The read-only role of the web application.
    pub web: PgConnectOptions,
    /// The password of the ingest role, for a URL of `ingest run`.
    pub ingest_password: SecretString,
}

/// Runs `body` with a new, empty schema, then removes it.
pub async fn with_schema<F, Fut>(body: F)
where
    F: FnOnce(TestDb) -> Fut,
    Fut: Future<Output = ()>,
{
    let prefix = new_prefix();
    let schema = Schema::new(&prefix).unwrap();
    let mut admin = admin().await;
    sweep(&mut admin).await;
    execute(&mut admin, &format!("CREATE SCHEMA {prefix}")).await;
    admin.close().await.unwrap();
    let db = TestDb {
        options: owner_options(&schema),
        schema,
    };
    let result = CatchUnwind(Box::pin(body(db))).await;
    cleanup(&prefix).await;
    finish(result);
}

/// Runs `body` with a new, migrated schema.
pub async fn with_migrated<F, Fut>(body: F)
where
    F: FnOnce(TestDb) -> Fut,
    Fut: Future<Output = ()>,
{
    with_schema(|db| async move {
        db.migrate_unlocked().await;
        body(db).await;
    })
    .await;
}

/// The roles whose password the first run of `db/roles.sql` gives.
#[derive(Debug, Clone, Copy)]
pub struct Given {
    pub ingest: bool,
    pub migrator: bool,
    pub web: bool,
}

impl Given {
    /// All three passwords, as the first production run gives them.
    pub const ALL: Self = Self {
        ingest: true,
        migrator: true,
        web: true,
    };
}

/// Runs `body` with roles and a schema made by `db/roles.sql`, as the
/// production procedure does. The schema is empty.
pub async fn with_roles<F, Fut>(body: F)
where
    F: FnOnce(TestDb, TestRoles) -> Fut,
    Fut: Future<Output = ()>,
{
    with_roles_given(Given::ALL, body).await;
}

/// Runs `body` with roles and a schema made by one run of `db/roles.sql`
/// that gives only the passwords of `given`. The connection settings of
/// [`TestRoles`] hold a new random password for each role, also for a
/// role whose password the run does not give: a login with that password
/// must fail.
pub async fn with_roles_given<F, Fut>(given: Given, body: F)
where
    F: FnOnce(TestDb, TestRoles) -> Fut,
    Fut: Future<Output = ()>,
{
    let prefix = new_prefix();
    let schema = Schema::new(&prefix).unwrap();
    let mut admin = admin().await;
    sweep(&mut admin).await;
    let ingest_password = random_password(&mut admin).await;
    let migrator_password = random_password(&mut admin).await;
    let web_password = random_password(&mut admin).await;
    let setup = RoleSetup {
        ingest_role: format!("{prefix}_ingest"),
        migrator_role: format!("{prefix}_migrator"),
        web_role: format!("{prefix}_web"),
        schema: schema.clone(),
        ingest_password: given
            .ingest
            .then(|| SecretString::new(ingest_password.clone())),
        migrator_password: given
            .migrator
            .then(|| SecretString::new(migrator_password.clone())),
        web_password: given.web.then(|| SecretString::new(web_password.clone())),
    };
    let applied = apply_roles(&mut admin, &setup).await;
    admin.close().await.unwrap();
    let db = TestDb {
        options: owner_options(&schema),
        schema: schema.clone(),
    };
    let roles = TestRoles {
        ingest: db
            .options
            .clone()
            .username(&setup.ingest_role)
            .password(&ingest_password),
        migrator: migrate_options(&schema)
            .username(&setup.migrator_role)
            .password(&migrator_password),
        web: db
            .options
            .clone()
            .username(&setup.web_role)
            .password(&web_password),
        ingest_password: SecretString::new(ingest_password),
        setup,
    };
    let result = match applied {
        Ok(()) => CatchUnwind(Box::pin(body(db, roles))).await,
        Err(error) => {
            // `resume_unwind` prints nothing, so print the cause here.
            eprintln!("db/roles.sql failed: {error}");
            Err(Box::new(format!("db/roles.sql failed: {error}")) as _)
        }
    };
    cleanup(&prefix).await;
    finish(result);
}

/// Fails the test with the panic of the body, after the cleanup.
fn finish(result: std::thread::Result<()>) {
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// A future that turns a panic of the inner future into an error, so that
/// the harness can clean up before the test fails. The connections of the
/// body are dropped before the cleanup runs.
struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> Future for CatchUnwind<F> {
    type Output = std::thread::Result<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.0.as_mut();
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

/// The connect attempts of [`admin`]. The Neon proxy can refuse a connect
/// under load, for example with `08P01` "Authentication timed out" when
/// four test runs start at the same time.
const ADMIN_CONNECT_ATTEMPTS: u32 = 3;

/// A connection of the owner role without a schema, after the guards.
///
/// The harness opens one for the setup and a new one for the cleanup, so
/// no admin connection stays idle while the test body runs. Its statement
/// timeout is 5 minutes ([`MIGRATE_STATEMENT_TIMEOUT`]): the cleanup waits
/// for the advisory lock of the role script, which parallel test runs
/// share.
async fn admin() -> PgConnection {
    let options = connect_options(
        &test_url(),
        &Schema::new("public").unwrap(),
        MIGRATE_STATEMENT_TIMEOUT,
    )
    .unwrap();
    let mut attempt = 1;
    let mut conn = loop {
        match PgConnection::connect_with(&options).await {
            Ok(conn) => break conn,
            Err(error) if attempt < ADMIN_CONNECT_ATTEMPTS => {
                eprintln!("admin connect attempt {attempt} failed, retrying: {error}");
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => panic!("admin connect failed {ADMIN_CONNECT_ATTEMPTS} times: {error}"),
        }
    };
    let raw: bool = sqlx::query_scalar("SELECT to_regnamespace('raw') IS NOT NULL")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    if raw {
        conn.close().await.unwrap();
        panic!(
            "the test database has a schema 'raw'. Production has it; the test branch must \
             not. The tests refuse this database."
        );
    }
    conn
}

/// A password from the random source of the server: 122 random bits.
pub async fn random_password(conn: &mut PgConnection) -> String {
    sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(conn)
        .await
        .unwrap()
}

pub async fn execute(conn: &mut PgConnection, sql: &str) {
    sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(conn)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// Removes the schema and the roles of one test with a new admin
/// connection, and fails the test when a step fails.
async fn cleanup(prefix: &str) {
    let mut conn = admin().await;
    let failures = drop_prefix(&mut conn, prefix).await;
    conn.close().await.unwrap();
    assert!(
        failures.is_empty(),
        "cleanup of {prefix} failed: {failures:#?}"
    );
}

/// Removes the schema and the roles of one prefix. Every step runs, also
/// after a failed step. Returns the failed steps and the objects that still
/// depend on a role.
async fn drop_prefix(conn: &mut PgConnection, prefix: &str) -> Vec<String> {
    let mut failures = Vec::new();
    let mut step = async |conn: &mut PgConnection, sql: String| {
        if let Err(error) = sqlx::raw_sql(AssertSqlSafe(sql.clone()))
            .execute(conn)
            .await
        {
            let detail = error
                .as_database_error()
                .and_then(|error| error.try_downcast_ref::<sqlx::postgres::PgDatabaseError>())
                .and_then(sqlx::postgres::PgDatabaseError::detail)
                .unwrap_or("")
                .to_owned();
            failures.push(format!("{sql}: {error} {detail}"));
        }
    };
    // The same lock as `db/roles.sql`: a REVOKE on the database while
    // another test grants on it would fail with "tuple concurrently
    // updated".
    step(
        conn,
        "SELECT pg_advisory_lock(hashtext('telemetry.roles'))".to_owned(),
    )
    .await;
    for role in [
        format!("{prefix}_migrator"),
        format!("{prefix}_ingest"),
        format!("{prefix}_web"),
    ] {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT FROM pg_roles WHERE rolname = $1)")
                .bind(&role)
                .fetch_one(&mut *conn)
                .await
                .unwrap_or(true);
        if exists {
            // The owner role created the role, so it has the ADMIN option
            // and can give itself the SET option.
            step(
                conn,
                format!("GRANT {role} TO CURRENT_USER WITH INHERIT FALSE, SET TRUE"),
            )
            .await;
            step(conn, format!("SET ROLE {role}")).await;
            step(conn, format!("DROP OWNED BY {role} CASCADE")).await;
            step(conn, "RESET ROLE".to_owned()).await;
            // DROP OWNED, run as the role, cannot take back the CONNECT
            // privilege that the owner role granted; the grantor can.
            step(
                conn,
                format!(
                    "DO $$ BEGIN EXECUTE format('REVOKE ALL ON DATABASE %I FROM {role}', \
                     current_database()); END $$"
                ),
            )
            .await;
            step(conn, format!("DROP ROLE {role}")).await;
        }
    }
    step(
        conn,
        "SELECT pg_advisory_unlock(hashtext('telemetry.roles'))".to_owned(),
    )
    .await;
    step(conn, format!("DROP SCHEMA IF EXISTS {prefix} CASCADE")).await;
    if !failures.is_empty() {
        let dependencies: Vec<String> = sqlx::query_scalar(
            "SELECT r.rolname || ': ' || coalesce(pg_describe_object(d.classid, d.objid, d.objsubid), '?')
                    || ' (' || d.deptype || ', database ' || d.dbid || ')'
             FROM pg_shdepend d JOIN pg_roles r ON r.oid = d.refobjid
             WHERE r.rolname LIKE $1",
        )
        .bind(format!("{prefix}\\_%"))
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default();
        failures.extend(dependencies);
    }
    failures
}

/// The advisory lock of the sweep: one test process sweeps at a time.
const SWEEP_LOCK: i64 = 4_242_424_242;

/// Removes test schemas, test roles, and metadata databases of earlier runs
/// that are older than [`STALE_AFTER`]. Only the process that gets the
/// sweep lock sweeps; the others skip it. A failed sweep step does not fail
/// the test: the next sweep tries again.
async fn sweep(conn: &mut PgConnection) {
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(SWEEP_LOCK)
        .fetch_one(&mut *conn)
        .await
        .unwrap_or(false);
    if !locked {
        return;
    }
    let failures = sweep_locked(conn).await;
    if !failures.is_empty() {
        eprintln!("sweep of old test objects failed: {failures:#?}");
    }
    let _unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(SWEEP_LOCK)
        .execute(&mut *conn)
        .await;
}

async fn sweep_locked(conn: &mut PgConnection) -> Vec<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let stale = |name: &str, prefix: &str| -> bool {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.split('_').next())
            .and_then(|seconds| seconds.parse::<u64>().ok())
            .is_some_and(|seconds| now.saturating_sub(seconds) > STALE_AFTER.as_secs())
    };
    // One round trip for the three lists.
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT nspname::text FROM pg_namespace WHERE nspname LIKE 'itest\\_%'
         UNION
         SELECT rolname::text FROM pg_roles WHERE rolname LIKE 'itest\\_%'
         UNION
         SELECT datname::text FROM pg_database WHERE datname LIKE 'sqlx\\_meta\\_%'",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap_or_default();
    let mut prefixes: Vec<String> = names
        .iter()
        .filter(|name| stale(name, "itest_"))
        .map(|name| name.split('_').take(3).collect::<Vec<_>>().join("_"))
        .collect();
    prefixes.sort();
    prefixes.dedup();
    let mut failures = Vec::new();
    for prefix in prefixes {
        failures.extend(drop_prefix(conn, &prefix).await);
    }
    for name in names.iter().filter(|name| name.starts_with("sqlx_meta_")) {
        let numbered = name.strip_prefix("sqlx_meta_").is_some_and(|rest| {
            rest.split('_')
                .all(|part| part.bytes().all(|b| b.is_ascii_digit()))
        });
        if numbered && stale(name, "sqlx_meta_") {
            let sql = format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)");
            if let Err(error) = sqlx::raw_sql(AssertSqlSafe(sql.clone()))
                .execute(&mut *conn)
                .await
            {
                failures.push(format!("{sql}: {error}"));
            }
        }
    }
    failures
}

/// The time `seconds` after the epoch, with `nanos` nanoseconds.
pub fn at(seconds: i64, nanos: i32) -> Timestamp {
    Timestamp::new(seconds, nanos).unwrap()
}

/// A base time for fixtures: 2026-11-01T00:00:00Z.
pub const BASE: i64 = 1_793_491_200;

/// A `V` record with the given signals, created at `BASE + created`.
pub fn vehicle_data(created: i64, signals: Vec<(i32, SignalValue)>) -> Decoded {
    Decoded::VehicleData(VehicleData {
        vin: Vin::new(VIN.to_owned()),
        created_at: at(BASE + created, 0),
        received_at: at(BASE + created + 2, 0),
        is_resend: false,
        signals: signals
            .into_iter()
            .map(|(field, value)| Signal { field, value })
            .collect(),
        unknown_value_variants: 0,
        unknown_fields: false,
    })
}

/// The same record with another VIN.
pub fn with_vin(decoded: Decoded, vin: &str) -> Decoded {
    match decoded {
        Decoded::VehicleData(mut record) => {
            record.vin = Vin::new(vin.to_owned());
            Decoded::VehicleData(record)
        }
        Decoded::Alerts(mut record) => {
            record.vin = Vin::new(vin.to_owned());
            Decoded::Alerts(record)
        }
        Decoded::Errors(mut record) => {
            record.vin = Vin::new(vin.to_owned());
            Decoded::Errors(record)
        }
        Decoded::Connectivity(mut record) => {
            record.vin = Vin::new(vin.to_owned());
            Decoded::Connectivity(record)
        }
        other @ Decoded::Unprocessed(_) => other,
    }
}

pub fn alerts(created: i64, alerts: Vec<Alert>) -> Decoded {
    Decoded::Alerts(Alerts {
        vin: Vin::new(VIN.to_owned()),
        created_at: at(BASE + created, 0),
        received_at: at(BASE + created + 2, 0),
        alerts,
        unknown_fields: false,
    })
}

pub fn alert(name: &str, audiences: Vec<i32>, started: Option<i64>, ended: Option<i64>) -> Alert {
    Alert {
        name: name.to_owned(),
        audiences,
        started_at: started.map(|seconds| at(BASE + seconds, 0)),
        ended_at: ended.map(|seconds| at(BASE + seconds, 0)),
    }
}

pub fn errors(created: i64, errors: Vec<VehicleError>) -> Decoded {
    Decoded::Errors(Errors {
        vin: Vin::new(VIN.to_owned()),
        created_at: at(BASE + created, 0),
        received_at: at(BASE + created + 2, 0),
        errors,
        unknown_fields: false,
    })
}

pub fn error_item(
    created: Option<i64>,
    name: &str,
    tags: &[(&str, &str)],
    body: &str,
) -> VehicleError {
    VehicleError {
        created_at: created.map(|seconds| at(BASE + seconds, 0)),
        name: name.to_owned(),
        tags: tags
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        body: body.to_owned(),
    }
}

pub fn connectivity(created: i64, connection: u128, status: i32, interface: &str) -> Decoded {
    Decoded::Connectivity(Connectivity {
        vin: Vin::new(VIN.to_owned()),
        connection_id: uuid::Uuid::from_u128(connection),
        status,
        network_interface: interface.to_owned(),
        created_at: at(BASE + created, 0),
        received_at: at(BASE + created + 2, 0),
        unknown_fields: false,
    })
}

pub fn unprocessed(reason: UnprocessedReason, received: i64) -> Decoded {
    Decoded::Unprocessed(Unprocessed {
        reason,
        received_at: at(BASE + received, 0),
    })
}

/// An enum value as the decode makes it. Only the decode can make an
/// enum value, so the helper encodes a one-datum `Payload` by hand and
/// decodes it: field 9, the variant with field number `value_type`, and
/// the enum number `number`.
pub fn enum_value(value_type: u64, number: u64) -> SignalValue {
    fn varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push(u8::try_from(value & 0x7f).unwrap() | 0x80);
            value >>= 7;
        }
        out.push(u8::try_from(value).unwrap());
    }
    fn field(number: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint(number << 3 | 2, out);
        varint(u64::try_from(bytes.len()).unwrap(), out);
        out.extend_from_slice(bytes);
    }
    let mut value = Vec::new();
    varint(value_type << 3, &mut value);
    varint(number, &mut value);
    let mut datum = vec![0x08, 0x09];
    field(2, &value, &mut datum);
    let mut created = vec![0x08];
    varint(u64::try_from(BASE).unwrap(), &mut created);
    let mut payload = Vec::new();
    field(1, &datum, &mut payload);
    field(2, &created, &mut payload);
    field(3, VIN.as_bytes(), &mut payload);
    match ingest::decode::decode(b"tesla_telemetry_V", &payload, at(BASE, 0)) {
        Decoded::VehicleData(record) => record.signals[0].value.clone(),
        other => panic!("not a V record: {other:?}"),
    }
}

/// A queued message with invented frames. The payload is not a real
/// protobuf message; the writer stores it only as bytes.
pub struct Message {
    pub topic: Vec<u8>,
    pub payload: Vec<u8>,
    pub decoded: Decoded,
}

impl Message {
    pub fn new(decoded: Decoded) -> Self {
        let topic = match &decoded {
            Decoded::VehicleData(_) => "tesla_telemetry_V",
            Decoded::Alerts(_) => "tesla_telemetry_alerts",
            Decoded::Errors(_) => "tesla_telemetry_errors",
            Decoded::Connectivity(_) => "tesla_telemetry_connectivity",
            Decoded::Unprocessed(_) => "tesla_telemetry_unknown",
        };
        // A payload that differs for each record: the debug text of the
        // structure, which holds no values.
        let payload = format!("{decoded:?}").into_bytes();
        Self {
            topic: topic.as_bytes().to_vec(),
            payload,
            decoded,
        }
    }

    pub fn with_payload(mut self, payload: &[u8]) -> Self {
        self.payload = payload.to_vec();
        self
    }

    pub fn with_topic(mut self, topic: &[u8]) -> Self {
        self.topic = topic.to_vec();
        self
    }

    pub fn queued(&self) -> QueuedRecord<'_> {
        QueuedRecord {
            topic: &self.topic,
            payload: &self.payload,
            decoded: &self.decoded,
        }
    }
}

pub fn queued(messages: &[Message]) -> Vec<QueuedRecord<'_>> {
    messages.iter().map(Message::queued).collect()
}

/// The number of rows of a table of the schema.
pub async fn count(conn: &mut PgConnection, table: &str) -> i64 {
    sqlx::query(AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(conn)
        .await
        .unwrap()
        .get(0)
}

/// The SQLSTATE of a failed statement, or `None` when it succeeds.
pub async fn sqlstate(conn: &mut PgConnection, sql: &str) -> Option<String> {
    match sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(conn)
        .await
    {
        Ok(_) => None,
        Err(sqlx::Error::Database(error)) => error.code().map(std::borrow::Cow::into_owned),
        Err(error) => panic!("{sql}: {error}"),
    }
}

/// Settings with certificate verification turned off, to show the
/// difference in the TLS test.
pub fn without_verification(options: PgConnectOptions) -> PgConnectOptions {
    options.ssl_mode(PgSslMode::Require)
}

#[test]
fn the_guard_refuses_the_production_endpoint() {
    for url in [
        "postgresql://u:p@ep-young-dawn-bgyaj83l.c-9.eu-central-1.aws.neon.tech/neondb",
        "postgresql://u:p@ep-young-dawn-bgyaj83l-pooler.c-9.eu-central-1.aws.neon.tech/neondb",
        "postgresql://u:p@EP-YOUNG-DAWN-BGYAJ83L.c-9.eu-central-1.aws.neon.tech/neondb",
        "postgresql://u:p@10.0.0.1/neondb?options=endpoint%3Dep-young-dawn-bgyaj83l",
        "postgresql://u:p@other.invalid/neondb?host=ep-young-dawn-bgyaj83l.aws.neon.tech",
    ] {
        assert_eq!(guard(url), Err("production endpoint"), "{url}");
    }
    assert_eq!(
        guard("postgresql://u:p@ep-other-test-12345.c-9.eu-central-1.aws.neon.tech/neondb"),
        Ok(())
    );
    assert_eq!(guard("not a url"), Err("not a PostgreSQL URL"));
}
