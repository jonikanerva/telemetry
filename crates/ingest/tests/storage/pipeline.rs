//! End-to-end tests of `ingest run` against the Neon branch `test`: the
//! real binary, the fake `fleet-telemetry` (`examples/fake_fleet_telemetry.rs`),
//! the roles and the migrations of the production procedure, and a TCP
//! proxy that the test controls.
//!
//! - **Schema.** Each test has a schema and roles of its own
//!   (`support::with_roles`); the migrator role applies the migrations, and
//!   the binary connects as the ingest role. A debug build reads
//!   `INGEST_TEST_SCHEMA` (`ingest::config`, "Test overrides").
//! - **Outage.** The binary connects to the proxy on the loopback
//!   interface: the URL has the Neon host name, the port of the proxy, and
//!   `hostaddr=127.0.0.1`. `SQLx` sends the host name for SNI and checks
//!   the certificate against it (`sslmode=verify-full`), so the proxy only
//!   copies the TLS bytes. [`Proxy::cut`] closes every connection and
//!   refuses new ones: for the binary, the database gives no answer.
//! - **Secrets.** The URL holds the password of the test role. No assertion
//!   prints a log line or the URL; the log check names a secret by its
//!   index only.

use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ingest::storage::migrate;
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use crate::harness::{Ingest, VEHICLE_SENTINELS, WAIT};
use crate::support::{TestDb, TestRoles, count, test_url, with_roles};

/// A TCP proxy to the Neon endpoint that the test can cut.
struct Proxy {
    port: u16,
    open: Arc<AtomicBool>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    cut: Arc<tokio::sync::Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Proxy {
    /// Starts the proxy on its own thread and runtime, so that the blocking
    /// waits of the test cannot stall it.
    fn start(host: String, port: u16, open: bool) -> Self {
        let open = Arc::new(AtomicBool::new(open));
        let cut = Arc::new(tokio::sync::Notify::new());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, listening) = std::sync::mpsc::channel();
        let thread = {
            let (open, cut) = (Arc::clone(&open), Arc::clone(&cut));
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                    ready.send(listener.local_addr().unwrap().port()).unwrap();
                    let mut connections = JoinSet::new();
                    tokio::pin!(stopped);
                    loop {
                        tokio::select! {
                            _ = &mut stopped => break,
                            () = cut.notified() => connections.abort_all(),
                            accepted = listener.accept() => {
                                let Ok((mut client, _)) = accepted else { continue };
                                if !open.load(Ordering::SeqCst) {
                                    continue;
                                }
                                let target = (host.clone(), port);
                                connections.spawn(async move {
                                    if let Ok(mut server) = TcpStream::connect(target).await {
                                        tokio::io::copy_bidirectional(&mut client, &mut server).await.ok();
                                    }
                                });
                            }
                        }
                    }
                    connections.abort_all();
                });
            })
        };
        Self {
            port: listening.recv().unwrap(),
            open,
            stop: Some(stop),
            cut,
            thread: Some(thread),
        }
    }

    /// The database gives no answer: every connection closes, and new ones
    /// close at once.
    fn cut(&self) {
        self.open.store(false, Ordering::SeqCst);
        self.cut.notify_one();
    }

    fn restore(&self) {
        self.open.store(true, Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// One run of `ingest run` on the schema of a test.
struct Run {
    ingest: Ingest,
    proxy: Proxy,
    /// Parts of the URL that the log must not hold.
    secrets: Vec<String>,
}

/// Applies the migrations as the migrator role, starts the proxy, and
/// starts `ingest run` as the ingest role with the given overrides.
async fn start(db: &TestDb, roles: &TestRoles, open: bool, extra: &[(&str, &str)]) -> Run {
    migrate(&roles.migrator, &db.schema).await.unwrap();
    // `test_url` applies the production guard before anything is derived.
    let neon = PgConnectOptions::from_str(test_url().expose()).unwrap();
    let host = neon.get_host().to_owned();
    let database = neon.get_database().unwrap_or("neondb").to_owned();
    let proxy = Proxy::start(host.clone(), neon.get_port(), open);
    let password = roles.ingest_password.expose().to_owned();
    let role = roles.setup.ingest_role.clone();
    let url = format!(
        "postgresql://{role}:{password}@{host}:{}/{database}?hostaddr=127.0.0.1",
        proxy.port
    );
    let mut vars = vec![
        ("INGEST_TEST_SCHEMA", db.schema.as_str().to_owned()),
        ("INGEST_TEST_RETRY_MAX_MS", "300".to_owned()),
        ("INGEST_TEST_SHORTFALL_PERIOD_MS", "300".to_owned()),
    ];
    for (name, value) in extra {
        vars.retain(|(existing, _)| existing != name);
        vars.push((name, (*value).to_owned()));
    }
    let ingest = tokio::task::block_in_place(|| Ingest::start(&url, &vars));
    Run {
        ingest,
        proxy,
        secrets: vec![
            password,
            role,
            host,
            "postgresql://".to_owned(),
            // The text of the database error of the record with a NUL byte
            // (SQLSTATE 22021): `invalid byte sequence for encoding
            // "UTF8": 0x00`.
            "invalid byte sequence".to_owned(),
            "0x00".to_owned(),
        ],
    }
}

impl Run {
    /// SIGTERM, then the exit within the shutdown budget, with code 0.
    fn stop(&mut self) {
        let started = Instant::now();
        self.ingest.signal("TERM");
        let status = tokio::task::block_in_place(|| self.ingest.wait());
        assert_eq!(status.code(), Some(0), "{:?}", self.ingest.messages());
        // ADR 0001: the sequence ends within its budget of 26 s.
        assert!(started.elapsed() < ingest::lifecycle::BUDGET.total());
        let mut secrets: Vec<&str> = VEHICLE_SENTINELS.to_vec();
        secrets.extend(self.secrets.iter().map(String::as_str));
        self.ingest.assert_log_holds_none_of(&secrets);
    }
}

/// Waits until `query` returns `expected`.
async fn wait_for(conn: &mut PgConnection, query: &str, expected: i64) {
    let started = Instant::now();
    loop {
        let value: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query.to_owned()))
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        if value == expected {
            return;
        }
        assert!(
            started.elapsed() < WAIT,
            "{query}: {value}, expected {expected}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The kinds of the stored events, in the order of their times.
async fn stored_kinds(conn: &mut PgConnection) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM ingest_event ORDER BY occurred_at")
        .fetch_all(conn)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn records_reach_the_tables_once_and_sigterm_stores_the_stopped_event() {
    with_roles(|db, roles| async move {
        let mut run = start(&db, &roles, true, &[("INGEST_FLUSH_INTERVAL_SECONDS", "1")]).await;
        tokio::task::block_in_place(|| {
            run.ingest.wait_for_health(200);
            let mut vehicle = run.ingest.wait_for_vehicle();
            assert_eq!(vehicle.command("publish V 0 10"), "ok 10");
            // A resend and a second copy of the same records.
            assert_eq!(vehicle.command("resend V 0 10"), "ok 10");
            assert_eq!(vehicle.command("publish V 0 10"), "ok 10");
            for command in [
                "publish alerts 1",
                "publish alerts 1",
                "publish errors 1",
                "publish connectivity 1",
                "publish nul 1",
            ] {
                assert_eq!(vehicle.command(command), "ok 1");
            }
            assert_eq!(vehicle.command("junk 1 64"), "ok 1");
        });
        let mut conn = db.owner().await;
        // The flush interval of 1 s writes them while the process runs.
        wait_for(&mut conn, "SELECT count(*) FROM vehicle_signal", 30).await;
        wait_for(&mut conn, "SELECT count(*) FROM unprocessed_record", 2).await;
        run.stop();
        assert_eq!(count(&mut conn, "vehicle").await, 1);
        assert_eq!(count(&mut conn, "vehicle_signal").await, 30);
        assert_eq!(count(&mut conn, "vehicle_alert").await, 1);
        assert_eq!(count(&mut conn, "vehicle_error").await, 1);
        assert_eq!(count(&mut conn, "connectivity_event").await, 1);
        let rejected: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT reason, sqlstate FROM unprocessed_record ORDER BY reason")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        // The record with the NUL byte, refused by PostgreSQL, and the junk
        // message, kept as bytes.
        assert_eq!(rejected.len(), 2);
        assert_eq!(
            rejected[0],
            ("database_rejected".to_owned(), Some("22021".to_owned()))
        );
        let kinds = stored_kinds(&mut conn).await;
        assert_eq!(kinds.first().map(String::as_str), Some("started"));
        assert_eq!(kinds.last().map(String::as_str), Some("stopped"));
        assert!(kinds.contains(&"gate_opened".to_owned()), "{kinds:?}");
        let runs: i64 = sqlx::query_scalar("SELECT count(DISTINCT run_id) FROM ingest_event")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(runs, 1);
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_outage_closes_the_gate_at_the_high_mark_and_the_recovery_writes_every_record() {
    with_roles(|db, roles| async move {
        let mut run = start(&db, &roles, true, &[("INGEST_TEST_QUEUE_BYTES", "16384")]).await;
        let published = tokio::task::block_in_place(|| {
            run.ingest.wait_for_health(200);
            let mut vehicle = run.ingest.wait_for_vehicle();
            run.proxy.cut();
            // One record at a time until the queue closes the gate.
            let mut published = 0_u64;
            loop {
                assert_eq!(
                    run.ingest.control(&format!("publish V {published} 1")),
                    "ok 1"
                );
                published += 1;
                std::thread::sleep(Duration::from_millis(20));
                run.ingest.poll();
                if run.ingest.with_message("ingest_event").iter().any(|event| {
                    event["kind"] == "gate_closed" && event["reason"] == "queue_high_mark"
                }) {
                    break;
                }
                assert!(published < 200, "the gate did not close");
            }
            // Records in flight after the close fit into the last 25 %.
            assert_eq!(
                run.ingest.control(&format!("publish V {published} 2")),
                "ok 2"
            );
            published += 2;
            // The closed gate ends the forwarded connection and refuses new
            // ones; the vehicle keeps its data in its own buffer.
            assert!(vehicle.is_closed());
            assert!(run.ingest.vehicle().is_none(), "the gate is closed");
            run.ingest
                .read_until(|event| event["message"] == "write_failed", "write_failed");
            run.proxy.restore();
            // The writer stores the queue, and the gate opens again.
            let before = run.ingest.events.len();
            run.ingest.read_after(
                before,
                |event| event["message"] == "ingest_event" && event["kind"] == "gate_opened",
                "the gate opened again",
            );
            run.ingest.wait_for_vehicle();
            published
        });
        run.stop();
        assert!(
            run.ingest
                .kinds()
                .iter()
                .all(|kind| *kind != "records_dropped")
        );
        let mut conn = db.owner().await;
        assert_eq!(
            count(&mut conn, "vehicle_signal").await,
            i64::try_from(published * 3).unwrap()
        );
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_during_an_outage_enters_degraded_mode_and_stores_its_events_after_recovery() {
    with_roles(|db, roles| async move {
        let mut run = start(
            &db,
            &roles,
            false,
            &[("INGEST_TEST_DEGRADED_AFTER_MS", "1500")],
        )
        .await;
        tokio::task::block_in_place(|| {
            run.ingest.read_event("degraded_mode");
            let mut vehicle = run.ingest.wait_for_vehicle();
            assert_eq!(run.ingest.health(), 503);
            assert_eq!(vehicle.command("publish V 0 5"), "ok 5");
            run.proxy.restore();
            run.ingest.wait_for_health(200);
        });
        let mut conn = db.owner().await;
        wait_for(&mut conn, "SELECT count(*) FROM vehicle_signal", 15).await;
        run.stop();
        let kinds = stored_kinds(&mut conn).await;
        for kind in ["started", "degraded_mode", "gate_opened", "stopped"] {
            assert!(kinds.contains(&kind.to_owned()), "{kind} in {kinds:?}");
        }
        conn.close().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_queue_and_a_shortfall_are_stored_as_gaps() {
    with_roles(|db, roles| async move {
        let mut run = start(&db, &roles, true, &[("INGEST_TEST_QUEUE_BYTES", "16384")]).await;
        tokio::task::block_in_place(|| {
            run.ingest.wait_for_health(200);
            // A drop on the ZMQ path: the counter grows without messages.
            assert_eq!(run.ingest.control("lose V 3"), "ok 0");
            let shortfall = run.ingest.read_event("zmq_shortfall");
            assert_eq!(shortfall["count"], 3);
            run.proxy.cut();
            assert_eq!(run.ingest.control("junk 60 512"), "ok 60");
            run.ingest.read_event("records_dropped");
            run.proxy.restore();
            run.ingest.wait_for_vehicle();
        });
        run.stop();
        let mut conn = db.owner().await;
        let dropped: Vec<(String, i64)> = sqlx::query_as(
            "SELECT kind, dropped_count FROM ingest_event WHERE dropped_count IS NOT NULL ORDER BY kind",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        let sum = |kind: &str| -> i64 {
            dropped
                .iter()
                .filter(|(stored, _)| stored == kind)
                .map(|(_, count)| count)
                .sum()
        };
        // The shortfall is stored once.
        assert_eq!(sum("zmq_shortfall"), 3);
        assert_eq!(
            dropped.iter().filter(|(kind, _)| kind == "zmq_shortfall").count(),
            1
        );
        // Every junk message is stored as bytes or counted as dropped.
        let stored = count(&mut conn, "unprocessed_record").await;
        assert!(sum("records_dropped") > 0);
        assert_eq!(stored + sum("records_dropped"), 60);
        conn.close().await.unwrap();
    })
    .await;
}
