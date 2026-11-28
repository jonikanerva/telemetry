//! `db/roles.sql`: the roles, the schema, and the privileges of ADR 0002.
//! The roles log in with their passwords, as in production.

use ingest::config::SecretString;
use ingest::events::{EventKind, IngestEvent};
use ingest::record::{SignalValue, UnprocessedReason};
use ingest::storage::{
    FailureClass, RoleSetup, Writer, apply_roles, check_migrations, migrate, write_events,
};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use crate::support::{
    Given, Message, TestRoles, alert, alerts, at, connectivity, error_item, errors, execute,
    queued, random_password, sqlstate, unprocessed, vehicle_data, with_roles, with_roles_given,
};

/// The statements that the ingest role must not run.
const DENIED: [&str; 12] = [
    "UPDATE vehicle_signal SET is_resend = true",
    "UPDATE vehicle SET vin = vin",
    "DELETE FROM vehicle_signal",
    "DELETE FROM unprocessed_record",
    "TRUNCATE ingest_event",
    "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
     VALUES (9999, 'x', true, '\\x00', 1)",
    "DELETE FROM _sqlx_migrations",
    "CREATE TABLE x (a integer)",
    "ALTER TABLE vehicle ADD COLUMN x integer",
    "DROP TABLE vehicle_alert",
    "CREATE INDEX x ON vehicle (first_seen_at)",
    "SELECT nextval(pg_get_serial_sequence('vehicle', 'id'))",
];

async fn role_settings(conn: &mut PgConnection, role: &str) -> (i32, Vec<String>, bool) {
    sqlx::query_as(
        "SELECT r.rolconnlimit,
                coalesce((SELECT setconfig FROM pg_db_role_setting
                          WHERE setrole = r.oid AND setdatabase = 0), '{}'),
                pg_has_role(r.oid, 'neon_superuser', 'MEMBER')
         FROM pg_roles r WHERE r.rolname = $1",
    )
    .bind(role)
    .fetch_one(conn)
    .await
    .unwrap()
}

/// The role attributes, the settings, and the owner of the schema.
async fn assert_roles_and_schema(owner: &mut PgConnection, roles: &TestRoles, schema: &str) {
    let (limit, config, superuser) = role_settings(owner, &roles.setup.ingest_role).await;
    assert_eq!(limit, 5);
    assert_eq!(
        config,
        [
            format!("search_path={schema}"),
            "statement_timeout=30s".to_owned()
        ]
    );
    assert!(
        !superuser,
        "the ingest role is not a member of neon_superuser"
    );
    let (_, config, superuser) = role_settings(owner, &roles.setup.migrator_role).await;
    assert_eq!(config, [format!("search_path={schema}")]);
    assert!(!superuser);
    let (_, _, superuser) = role_settings(owner, &roles.setup.web_role).await;
    assert!(!superuser, "the web role is not a member of neon_superuser");
    let schema_owner: String =
        sqlx::query_scalar("SELECT nspowner::regrole::text FROM pg_namespace WHERE nspname = $1")
            .bind(schema)
            .fetch_one(&mut *owner)
            .await
            .unwrap();
    assert_eq!(schema_owner, roles.setup.migrator_role);
    // The script takes its SET option on the migrator role back.
    let member: bool = sqlx::query_scalar("SELECT pg_has_role(current_user, $1, 'SET')")
        .bind(&roles.setup.migrator_role)
        .fetch_one(&mut *owner)
        .await
        .unwrap();
    assert!(!member);
}

/// The ingest role checks the migrations and writes every table, through
/// the identity column of `vehicle` without a privilege on its sequence.
async fn assert_ingest_writes(roles: &TestRoles) {
    let options = roles.ingest.clone();
    assert_eq!(check_migrations(&options).await, Ok(()));
    let batch = [
        Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(1))])),
        Message::new(vehicle_data(0, vec![(8, SignalValue::Int32(2))])),
        Message::new(alerts(0, vec![alert("TestAlert", vec![1], None, None)])),
        Message::new(errors(0, vec![error_item(None, "test_error", &[], "")])),
        Message::new(connectivity(0, 1, 1, "wifi")),
        Message::new(unprocessed(UnprocessedReason::UnknownTopic, 0)),
    ];
    let mut writer = Writer::new(options.clone());
    let written = writer.write(&queued(&batch), &mut || {}).await.unwrap();
    assert_eq!(written.rows_inserted, 4);
    assert_eq!(
        written.conflicting_records, 1,
        "the stored row was read back"
    );
    let events = [IngestEvent {
        occurred_at: at(0, 0),
        kind: EventKind::Started,
    }];
    let outcome = write_events(&options, Uuid::from_u128(1), &events, &mut || {})
        .await
        .unwrap();
    assert_eq!(outcome.inserted, 1);
}

/// Each call closes its connection before it returns, also after a
/// success: no session of the ingest role remains. The role belongs to
/// this test only, so other tests do not count. The server removes a
/// closed session from `pg_stat_activity` within moments, so the check
/// waits up to 5 s.
async fn assert_no_session_remains(owner: &mut PgConnection, roles: &TestRoles) {
    let role = &roles.setup.ingest_role;
    let mut sessions: i64 = -1;
    for _ in 0..50 {
        sessions = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE usename = $1")
            .bind(role)
            .fetch_one(&mut *owner)
            .await
            .unwrap();
        if sessions == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{sessions} sessions of the ingest role remain after the calls");
}

/// The ingest role cannot change or delete rows, write the migration
/// history, or run DDL.
async fn assert_denied(roles: &TestRoles, schema: &str) {
    let mut ingest = PgConnection::connect_with(&roles.ingest).await.unwrap();
    let other = [
        format!("CREATE SCHEMA {schema}_x"),
        "CREATE TABLE public.x (a integer)".to_owned(),
    ];
    for sql in DENIED
        .iter()
        .copied()
        .chain(other.iter().map(String::as_str))
    {
        assert_eq!(
            sqlstate(&mut ingest, sql).await.as_deref(),
            Some("42501"),
            "{sql}"
        );
    }
    let readable = "SELECT version, checksum FROM _sqlx_migrations";
    assert_eq!(sqlstate(&mut ingest, readable).await, None);
    ingest.close().await.unwrap();
}

#[tokio::test]
async fn the_roles_get_the_privileges_of_adr_0002() {
    with_roles(|db, roles: TestRoles| async move {
        let schema = db.schema.as_str().to_owned();
        let mut owner = db.owner().await;
        assert_roles_and_schema(&mut owner, &roles, &schema).await;

        // The migrator applies the migrations and owns every object.
        let outcome = migrate(&roles.migrator, &db.schema).await.unwrap();
        assert_eq!(outcome.applied, 1);
        let owners: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT relowner::regrole::text FROM pg_class
             WHERE relnamespace = $1::regnamespace",
        )
        .bind(&schema)
        .fetch_all(&mut owner)
        .await
        .unwrap();
        assert_eq!(owners, std::slice::from_ref(&roles.setup.migrator_role));

        assert_ingest_writes(&roles).await;
        assert_no_session_remains(&mut owner, &roles).await;
        assert_denied(&roles, &schema).await;
        owner.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn the_role_script_is_idempotent() {
    with_roles(|db, roles: TestRoles| async move {
        let schema = db.schema.as_str().to_owned();
        migrate(&roles.migrator, &db.schema).await.unwrap();
        // Two more runs without passwords keep the passwords and the
        // privileges, also on the tables that exist now.
        let mut owner = db.owner().await;
        let setup = RoleSetup {
            ingest_password: None,
            migrator_password: None,
            web_password: None,
            ..roles.setup
        };
        apply_roles(&mut owner, &setup).await.unwrap();
        apply_roles(&mut owner, &setup).await.unwrap();
        let roles = TestRoles { setup, ..roles };
        assert_roles_and_schema(&mut owner, &roles, &schema).await;
        assert_ingest_writes(&roles).await;
        assert_no_session_remains(&mut owner, &roles).await;
        assert_denied(&roles, &schema).await;
        let migrator = PgConnection::connect_with(&roles.migrator).await.unwrap();
        migrator.close().await.unwrap();
        owner.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_wrong_password_is_an_answered_failure() {
    with_roles(|db, roles: TestRoles| async move {
        migrate(&roles.migrator, &db.schema).await.unwrap();
        // The gate keeps itself closed on an authentication error, and goes
        // to degraded mode only when no session answers (ADR 0001).
        let options = roles
            .ingest
            .clone()
            .password("wrong0password0wrong0password0x");
        let error = check_migrations(&options).await.unwrap_err();
        assert_eq!(error.class, FailureClass::Answered, "{error}");
        assert_eq!(error.code.to_string(), "28P01");
    })
    .await;
}

/// The setup of the roles of `roles` with only the given passwords.
fn setup_with(
    roles: &TestRoles,
    ingest_password: Option<&str>,
    web_password: Option<&str>,
) -> RoleSetup {
    let secret = |password: Option<&str>| password.map(|text| SecretString::new(text.to_owned()));
    RoleSetup {
        ingest_role: roles.setup.ingest_role.clone(),
        migrator_role: roles.setup.migrator_role.clone(),
        web_role: roles.setup.web_role.clone(),
        schema: roles.setup.schema.clone(),
        ingest_password: secret(ingest_password),
        migrator_password: None,
        web_password: secret(web_password),
    }
}

/// Logs in with `options` and closes the session.
async fn log_in(options: &PgConnectOptions) {
    PgConnection::connect_with(options)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

/// The SQLSTATE of a failed login with `options`.
async fn refused_login(options: &PgConnectOptions) -> String {
    let Err(error) = PgConnection::connect_with(options).await else {
        panic!("the login succeeded");
    };
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map_or_else(
            || panic!("not a database error: {error}"),
            std::borrow::Cow::into_owned,
        )
}

/// The LOGIN attribute of each role: ingest, migrator, web.
async fn can_log_in(owner: &mut PgConnection, roles: &TestRoles) -> (bool, bool, bool) {
    let mut login = Vec::new();
    for role in [
        &roles.setup.ingest_role,
        &roles.setup.migrator_role,
        &roles.setup.web_role,
    ] {
        let value: bool = sqlx::query_scalar("SELECT rolcanlogin FROM pg_roles WHERE rolname = $1")
            .bind(role)
            .fetch_one(&mut *owner)
            .await
            .unwrap();
        login.push(value);
    }
    (login[0], login[1], login[2])
}

/// A run with only the web password changes only the web role: the
/// ingest and migrator roles keep their passwords and their LOGIN
/// attribute (README, "Database roles": a web-only run sets only
/// `TELEMETRY_WEB_PASSWORD`).
#[tokio::test]
async fn a_run_with_only_the_web_password_keeps_the_other_logins() {
    with_roles(|db, roles: TestRoles| async move {
        let mut owner = db.owner().await;
        let web_password = random_password(&mut owner).await;
        apply_roles(&mut owner, &setup_with(&roles, None, Some(&web_password)))
            .await
            .unwrap();
        // The ingest and migrator roles log in with the passwords of the
        // first run; the web role logs in only with its new password.
        log_in(&roles.ingest).await;
        log_in(&roles.migrator).await;
        log_in(&roles.web.clone().password(&web_password)).await;
        assert_eq!(refused_login(&roles.web).await, "28P01");

        // A role without LOGIN keeps NOLOGIN after a web-only run, so the
        // run does not touch the attribute.
        for role in [&roles.setup.ingest_role, &roles.setup.migrator_role] {
            execute(&mut owner, &format!("ALTER ROLE {role} NOLOGIN")).await;
        }
        apply_roles(&mut owner, &setup_with(&roles, None, Some(&web_password)))
            .await
            .unwrap();
        assert_eq!(can_log_in(&mut owner, &roles).await, (false, false, true));
        // NOLOGIN refuses the login also with the right password, with an
        // SQLSTATE of class 28 (`docs/web-release.md`, "Recovery").
        let code = refused_login(&roles.ingest).await;
        assert!(code.starts_with("28"), "{code}");

        // A run with the password of a role gives it LOGIN again.
        let ingest_password = roles.ingest_password.expose().to_owned();
        apply_roles(
            &mut owner,
            &setup_with(&roles, Some(&ingest_password), None),
        )
        .await
        .unwrap();
        assert_eq!(can_log_in(&mut owner, &roles).await, (true, false, true));
        log_in(&roles.ingest).await;
        owner.close().await.unwrap();
    })
    .await;
}

/// A role that the run creates without a password has NOLOGIN and no
/// password. A later run with its password gives it LOGIN.
#[tokio::test]
async fn a_new_role_without_a_password_cannot_log_in() {
    let given = Given {
        ingest: false,
        migrator: true,
        web: true,
    };
    with_roles_given(given, |db, roles: TestRoles| async move {
        let mut owner = db.owner().await;
        assert_eq!(can_log_in(&mut owner, &roles).await, (false, true, true));
        let code = refused_login(&roles.ingest).await;
        assert!(code.starts_with("28"), "{code}");
        log_in(&roles.migrator).await;
        log_in(&roles.web).await;

        let ingest_password = roles.ingest_password.expose().to_owned();
        apply_roles(
            &mut owner,
            &setup_with(&roles, Some(&ingest_password), None),
        )
        .await
        .unwrap();
        assert_eq!(can_log_in(&mut owner, &roles).await, (true, true, true));
        log_in(&roles.ingest).await;
        owner.close().await.unwrap();
    })
    .await;
}

/// The tables that the web role reads, each with one of its time columns.
const WEB_TABLES: [(&str, &str); 3] = [
    ("vehicle_signal", "received_at"),
    ("connectivity_event", "created_at"),
    ("ingest_event", "occurred_at"),
];

/// The columns that the web role reads. Each is a column privilege; the
/// web role has no table privilege.
const WEB_COLUMNS: [(&str, &[&str]); 4] = [
    (
        "vehicle_signal",
        &[
            "vehicle_id",
            "field",
            "created_at",
            "received_at",
            "value_type",
            "int_value",
            "float_value",
            "text_value",
        ],
    ),
    (
        "connectivity_event",
        &["connection_id", "vehicle_id", "created_at", "status"],
    ),
    (
        "ingest_event",
        &[
            "run_id",
            "occurred_at",
            "kind",
            "reason",
            "dropped_count",
            "first_received_at",
            "last_received_at",
        ],
    ),
    ("vehicle", &["id"]),
];

/// The reads that the web role must not run: the location, the VIN, the
/// other columns, and the tables that the web does not read.
const WEB_DENIED_READS: [&str; 17] = [
    "SELECT latitude FROM vehicle_signal",
    "SELECT longitude FROM vehicle_signal",
    "SELECT json_value FROM vehicle_signal",
    "SELECT bool_value FROM vehicle_signal",
    "SELECT is_resend FROM vehicle_signal",
    "SELECT * FROM vehicle_signal",
    "SELECT count(*) FROM vehicle_signal WHERE latitude IS NOT NULL",
    "SELECT network_interface FROM connectivity_event",
    "SELECT * FROM connectivity_event",
    "SELECT vin FROM vehicle",
    "SELECT * FROM vehicle",
    "SELECT first_seen_at FROM vehicle",
    "SELECT * FROM unprocessed_record",
    "SELECT * FROM vehicle_alert",
    "SELECT * FROM vehicle_error",
    "SELECT * FROM _sqlx_migrations",
    "SELECT nextval(pg_get_serial_sequence('ingest_event', 'id'))",
];

/// The privileges of the web role in the schema after `db/roles.sql` ran
/// on the migrated schema, sorted: one entry for each column privilege.
fn web_privilege_list() -> Vec<String> {
    let mut list: Vec<String> = WEB_COLUMNS
        .iter()
        .flat_map(|(table, columns)| {
            columns
                .iter()
                .map(move |column| format!("{table}.{column}:SELECT"))
        })
        .collect();
    list.sort();
    list
}

/// Runs `db/roles.sql` again without new passwords, as the procedure does
/// after a migration. The roles keep their passwords: the web checks after
/// a rerun log in with the password of the first run.
async fn rerun_roles(owner: &mut PgConnection, roles: &TestRoles) {
    let setup = RoleSetup {
        ingest_role: roles.setup.ingest_role.clone(),
        migrator_role: roles.setup.migrator_role.clone(),
        web_role: roles.setup.web_role.clone(),
        schema: roles.setup.schema.clone(),
        ingest_password: None,
        migrator_password: None,
        web_password: None,
    };
    apply_roles(owner, &setup).await.unwrap();
}

/// Every table, column, and sequence privilege of the web role in the
/// schema, sorted, and the count of its default privileges.
async fn web_privileges(owner: &mut PgConnection, roles: &TestRoles) -> (Vec<String>, i64) {
    let mut privileges: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname || ':' || a.privilege_type
         FROM pg_class c, aclexplode(c.relacl) a
         WHERE c.relnamespace = $1::regnamespace AND a.grantee = $2::regrole
         UNION ALL
         SELECT c.relname || '.' || att.attname || ':' || a.privilege_type
         FROM pg_class c JOIN pg_attribute att ON att.attrelid = c.oid,
              aclexplode(att.attacl) a
         WHERE c.relnamespace = $1::regnamespace AND a.grantee = $2::regrole",
    )
    .bind(roles.setup.schema.as_str())
    .bind(&roles.setup.web_role)
    .fetch_all(&mut *owner)
    .await
    .unwrap();
    privileges.sort();
    let defaults: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_default_acl d, aclexplode(d.defaclacl) a
         WHERE a.grantee = $1::regrole",
    )
    .bind(&roles.setup.web_role)
    .fetch_one(&mut *owner)
    .await
    .unwrap();
    (privileges, defaults)
}

/// The web role has exactly the privileges of [`WEB_COLUMNS`], USAGE
/// but not CREATE on the schema, CONNECT on the database, and no default
/// privileges.
async fn assert_web_privileges(owner: &mut PgConnection, roles: &TestRoles) {
    let (privileges, defaults) = web_privileges(owner, roles).await;
    assert_eq!(privileges, web_privilege_list());
    assert_eq!(defaults, 0, "the web role has no default privileges");
    let (usage, create, connect): (bool, bool, bool) = sqlx::query_as(
        "SELECT has_schema_privilege($1::name, $2::text, 'USAGE'),
                has_schema_privilege($1::name, $2::text, 'CREATE'),
                has_database_privilege($1::name, current_database()::text, 'CONNECT')",
    )
    .bind(&roles.setup.web_role)
    .bind(roles.setup.schema.as_str())
    .fetch_one(&mut *owner)
    .await
    .unwrap();
    assert!(usage && !create && connect);
}

/// The role attributes and the role settings of the web role. The query
/// does not read `neon_superuser`, so the test runs on every server.
async fn assert_web_settings(owner: &mut PgConnection, roles: &TestRoles) {
    let (limit, login, create_db, create_role, superuser, mut config, memberships): (
        i32,
        bool,
        bool,
        bool,
        bool,
        Vec<String>,
        i64,
    ) = sqlx::query_as(
        "SELECT r.rolconnlimit, r.rolcanlogin, r.rolcreatedb, r.rolcreaterole, r.rolsuper,
                coalesce((SELECT setconfig FROM pg_db_role_setting
                          WHERE setrole = r.oid AND setdatabase = 0), '{}'),
                (SELECT count(*) FROM pg_auth_members m WHERE m.member = r.oid)
         FROM pg_roles r WHERE r.rolname = $1",
    )
    .bind(&roles.setup.web_role)
    .fetch_one(&mut *owner)
    .await
    .unwrap();
    assert_eq!(limit, 4);
    assert!(login && !create_db && !create_role && !superuser);
    assert_eq!(memberships, 0, "the web role is a member of no role");
    config.sort();
    assert_eq!(
        config,
        [
            "default_transaction_read_only=on".to_owned(),
            "idle_in_transaction_session_timeout=15s".to_owned(),
            format!("search_path={}", roles.setup.schema.as_str()),
            "statement_timeout=10s".to_owned(),
        ]
    );
    let other = per_database_settings(owner, &roles.setup.web_role).await;
    assert_eq!(other, 0, "the web role has no settings for one database");
}

async fn per_database_settings(owner: &mut PgConnection, role: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_db_role_setting
         WHERE setrole = $1::regrole AND setdatabase <> 0",
    )
    .bind(role)
    .fetch_one(owner)
    .await
    .unwrap()
}

/// The statements that change data or the schema, for each table that the
/// web role reads, and DDL in the schema.
fn web_writes(schema: &str) -> Vec<String> {
    let mut writes = vec![
        "INSERT INTO vehicle (vin, first_seen_at) VALUES ('TESTVIN0000000009', now())".to_owned(),
        "UPDATE vehicle SET first_seen_at = now()".to_owned(),
        "DELETE FROM vehicle".to_owned(),
        "TRUNCATE vehicle".to_owned(),
        "INSERT INTO ingest_event (run_id, occurred_at, kind)
         VALUES (gen_random_uuid(), now(), 'started')"
            .to_owned(),
        "INSERT INTO connectivity_event
         (connection_id, created_at, received_at, status, vehicle_id, network_interface)
         VALUES (gen_random_uuid(), now(), now(), 1, 1, 'wifi')"
            .to_owned(),
        "INSERT INTO vehicle_signal
         (created_at, received_at, field, vehicle_id, value_type, is_resend, int_value)
         VALUES (now(), now(), 8, 1, 2, false, 1)"
            .to_owned(),
        format!("CREATE TABLE {schema}.x (a integer)"),
        "CREATE INDEX x ON vehicle_signal (received_at)".to_owned(),
        "ALTER TABLE ingest_event ADD COLUMN x integer".to_owned(),
    ];
    for (table, column) in WEB_TABLES {
        writes.push(format!("UPDATE {table} SET {column} = {column}"));
        writes.push(format!("DELETE FROM {table}"));
        writes.push(format!("TRUNCATE {table}"));
    }
    writes
}

/// The web role reads what it may read and nothing else, and every write
/// fails: with the read-only default (25006), and without it (42501), so
/// the privileges alone block the write.
async fn assert_web_access(roles: &TestRoles, schema: &str) {
    let mut web = PgConnection::connect_with(&roles.web).await.unwrap();
    let read_only: String = sqlx::query_scalar("SHOW default_transaction_read_only")
        .fetch_one(&mut web)
        .await
        .unwrap();
    assert_eq!(read_only, "on");
    for (table, columns) in WEB_COLUMNS {
        let sql = format!("SELECT {} FROM {table}", columns.join(", "));
        assert_eq!(sqlstate(&mut web, &sql).await, None, "{sql}");
    }
    for sql in WEB_DENIED_READS {
        assert_eq!(
            sqlstate(&mut web, sql).await.as_deref(),
            Some("42501"),
            "{sql}"
        );
    }
    let writes = web_writes(schema);
    for sql in &writes {
        assert_eq!(
            sqlstate(&mut web, sql).await.as_deref(),
            Some("25006"),
            "read-only default: {sql}"
        );
    }
    execute(&mut web, "SET default_transaction_read_only = off").await;
    for sql in &writes {
        assert_eq!(
            sqlstate(&mut web, sql).await.as_deref(),
            Some("42501"),
            "privileges: {sql}"
        );
    }
    web.close().await.unwrap();
}

#[tokio::test]
async fn the_web_role_reads_only_the_allowed_tables_and_columns() {
    with_roles(|db, roles: TestRoles| async move {
        let schema = db.schema.as_str().to_owned();
        let mut owner = db.owner().await;
        // Before the migration, the schema has no table: the role gets
        // USAGE on the schema only.
        let (privileges, defaults) = web_privileges(&mut owner, &roles).await;
        assert_eq!((privileges, defaults), (Vec::<String>::new(), 0));
        assert_web_settings(&mut owner, &roles).await;

        // The procedure: run the script again after the migration.
        migrate(&roles.migrator, &db.schema).await.unwrap();
        let (privileges, _) = web_privileges(&mut owner, &roles).await;
        assert!(
            privileges.is_empty(),
            "no default privileges: {privileges:?}"
        );
        rerun_roles(&mut owner, &roles).await;
        assert_web_privileges(&mut owner, &roles).await;
        assert_web_settings(&mut owner, &roles).await;
        assert_web_access(&roles, &schema).await;
        owner.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_second_run_removes_a_wrong_grant_to_the_web_role() {
    with_roles(|db, roles: TestRoles| async move {
        let schema = db.schema.as_str().to_owned();
        migrate(&roles.migrator, &db.schema).await.unwrap();
        let mut owner = db.owner().await;
        rerun_roles(&mut owner, &roles).await;
        assert_web_privileges(&mut owner, &roles).await;

        // Wrong grants of the owner of the tables, and a wrong setting.
        let web = &roles.setup.web_role;
        let mut migrator = PgConnection::connect_with(&roles.migrator).await.unwrap();
        for sql in [
            format!("GRANT SELECT ON {schema}.vehicle TO {web}"),
            format!("GRANT SELECT ON {schema}.vehicle_signal TO {web}"),
            format!("GRANT SELECT ON {schema}.ingest_event TO {web}"),
            format!("GRANT SELECT (network_interface) ON {schema}.connectivity_event TO {web}"),
            format!("GRANT SELECT, INSERT ON {schema}.unprocessed_record TO {web}"),
            format!("GRANT UPDATE (vin) ON {schema}.vehicle TO {web}"),
            format!("GRANT DELETE ON {schema}.ingest_event TO {web}"),
            format!("GRANT USAGE ON SEQUENCE {schema}.ingest_event_id_seq TO {web}"),
            format!("GRANT CREATE ON SCHEMA {schema} TO {web}"),
            format!("ALTER DEFAULT PRIVILEGES IN SCHEMA {schema} GRANT SELECT ON TABLES TO {web}"),
        ] {
            execute(&mut migrator, &sql).await;
        }
        migrator.close().await.unwrap();
        execute(
            &mut owner,
            &format!("ALTER ROLE {web} SET work_mem = '1MB'"),
        )
        .await;
        execute(&mut owner, &format!("ALTER ROLE {web} CONNECTION LIMIT 50")).await;
        // The wrong grants are in effect before the second run, so the
        // checks below can fail.
        let mut session = PgConnection::connect_with(&roles.web).await.unwrap();
        for sql in [
            "SELECT vin FROM vehicle",
            "SELECT latitude, longitude FROM vehicle_signal",
            "SELECT * FROM vehicle_signal",
            "SELECT network_interface FROM connectivity_event",
        ] {
            assert_eq!(sqlstate(&mut session, sql).await, None, "{sql}");
        }
        session.close().await.unwrap();
        let (privileges, defaults) = web_privileges(&mut owner, &roles).await;
        assert!(privileges.contains(&"vehicle_signal:SELECT".to_owned()));
        assert_ne!(privileges, web_privilege_list());
        assert_ne!(defaults, 0);

        rerun_roles(&mut owner, &roles).await;
        assert_web_privileges(&mut owner, &roles).await;
        rerun_roles(&mut owner, &roles).await;
        assert_web_privileges(&mut owner, &roles).await;
        assert_web_settings(&mut owner, &roles).await;
        assert_web_access(&roles, &schema).await;
        owner.close().await.unwrap();
    })
    .await;
}
