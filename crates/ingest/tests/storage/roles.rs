//! `db/roles.sql`: the roles, the schema, and the privileges of ADR 0002.
//! The roles log in with their passwords, as in production.

use ingest::events::{EventKind, IngestEvent};
use ingest::record::{SignalValue, UnprocessedReason};
use ingest::storage::{
    FailureClass, RoleSetup, Writer, apply_roles, check_migrations, migrate, write_events,
};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use crate::support::{
    Message, TestRoles, alert, alerts, at, connectivity, error_item, errors, queued, sqlstate,
    unprocessed, vehicle_data, with_roles,
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
