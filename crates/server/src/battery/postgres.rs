//! The PostgreSQL source of the battery view (ADR 0013).
//!
//! Each request opens one connection as the role `telemetry_web`, reads
//! everything in one `REPEATABLE READ READ ONLY` transaction (one
//! snapshot), and closes the connection. No connection stays open between
//! requests and no pool exists (ADR 0001, "Delivery path", step 6: Neon
//! scales to zero only without an open connection).
//!
//! Bounds:
//!
//! - at most [`MAX_CONCURRENT`] reads at a time; a request waits at most
//!   [`PERMIT_WAIT`] for a slot, else `service_busy`;
//! - the connect has [`CONNECT_TIMEOUT`], long enough for a Neon cold
//!   start;
//! - each statement has [`STATEMENT_TIMEOUT`] on the server;
//! - the whole read has [`READ_DEADLINE`] on the client; after it the
//!   connection is dropped, which closes the socket, and the server rolls
//!   the transaction back.
//!
//! [`READ_DEADLINE`] plus [`PERMIT_WAIT`] is less than the request timeout
//! of `crate::app` (30 s), so the problem of this module reaches the
//! client first.
//!
//! The SQL rules equal the reference rules of `telemetry_web_domain`; the
//! database tests compare both on the same rows.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff_sqlx::ToSqlx;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{Connection, PgConnection};
use telemetry_web_domain::battery::gaps::{
    EventSnapshot, Limited, MAX_GAPS, MAX_RESTARTS, loss_order,
};
use telemetry_web_domain::battery::{
    ConnectivityRow, ContractViolation, EventKind, EventRow, ExcludedCounts, Field, PerField,
    Point, RowTimes, Sample, SeriesPair, SeriesSnapshot, Snapshot, TileSnapshot, VehicleCount,
    Window,
};
use tokio::sync::Semaphore;

use super::source::{BatterySource, SnapshotFuture, SourceError, SourceRequest};

/// The largest number of reads at the same time. The role has CONNECTION
/// LIMIT 4: two reads, plus two for a deploy overlap.
pub const MAX_CONCURRENT: usize = 2;

/// The longest wait for a read slot.
pub const PERMIT_WAIT: Duration = Duration::from_secs(5);

/// The time to open a connection, including a Neon cold start.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The statement that opens the transaction of a read: one snapshot for
/// every statement, and no write.
pub const READ_TRANSACTION: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY";

/// The deadline of one statement on the server. The role sets the same
/// value; the connection sets it too, so it does not depend on the role.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// The client deadline of the whole read, from the connect to the commit.
pub const READ_DEADLINE: Duration = Duration::from_secs(24);

/// The longest graceful close of a connection.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// The connection settings of the web role. The `Debug` output holds no
/// value, because the settings hold the password.
#[derive(Clone)]
pub struct DatabaseOptions(PgConnectOptions);

impl fmt::Debug for DatabaseOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseOptions([redacted])")
    }
}

impl DatabaseOptions {
    /// Parses a database URL and forces the settings of this module:
    ///
    /// - `sslmode=verify-full`, whatever the URL or `PGSSLMODE` says
    ///   (`STACK.md` §5);
    /// - `search_path` `raw`, `statement_timeout` [`STATEMENT_TIMEOUT`],
    ///   and `client_min_messages` `error` (the server sends no notice
    ///   text that `SQLx` would log).
    ///
    /// # Errors
    ///
    /// Returns `None` when the URL does not parse. The caller reports the
    /// variable name, never the value.
    #[must_use]
    pub fn from_url(url: &str) -> Option<Self> {
        Self::with_schema(url, "raw")
    }

    /// As [`Self::from_url`], with another schema (the database tests).
    #[must_use]
    pub fn with_schema(url: &str, schema: &str) -> Option<Self> {
        let options = PgConnectOptions::from_str(url).ok()?;
        Some(Self(
            options
                .ssl_mode(PgSslMode::VerifyFull)
                .application_name("telemetry-web")
                .options([
                    ("search_path", schema.to_owned()),
                    (
                        "statement_timeout",
                        format!("{}ms", STATEMENT_TIMEOUT.as_millis()),
                    ),
                    ("client_min_messages", "error".to_owned()),
                ]),
        ))
    }

    /// The settings.
    #[must_use]
    pub const fn options(&self) -> &PgConnectOptions {
        &self.0
    }
}

/// The clock of the source: the system clock in production.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// The PostgreSQL source.
pub struct PostgresSource {
    options: DatabaseOptions,
    permits: Semaphore,
    clock: Clock,
}

impl fmt::Debug for PostgresSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresSource").finish_non_exhaustive()
    }
}

impl PostgresSource {
    /// A source with the system clock.
    #[must_use]
    pub fn new(options: DatabaseOptions) -> Self {
        Self::with_clock(options, Arc::new(Timestamp::now))
    }

    /// A source with a given clock (tests).
    #[must_use]
    pub fn with_clock(options: DatabaseOptions, clock: Clock) -> Self {
        Self {
            options,
            permits: Semaphore::new(MAX_CONCURRENT),
            clock,
        }
    }

    async fn read(&self, request: &SourceRequest) -> Result<Snapshot, SourceError> {
        self.read_with(request, |window| self.read_window(window))
            .await
    }

    /// The bounds of a read around `reader`: the read slot, the clock, and
    /// the deadline [`READ_DEADLINE`] of the whole read. The battery and
    /// the consumption reads share the read slots.
    pub(crate) async fn read_with<T, F, R>(
        &self,
        request: &SourceRequest,
        reader: F,
    ) -> Result<T, SourceError>
    where
        F: FnOnce(Window) -> R,
        R: Future<Output = Result<T, SourceError>>,
    {
        let _permit = tokio::time::timeout(PERMIT_WAIT, self.permits.acquire())
            .await
            .map_err(|_| SourceError::Busy)?
            .map_err(|_| SourceError::Internal("semaphore_closed"))?;
        let now = crate::wire::Instant::truncated((self.clock)()).timestamp();
        let window = Window::ending_at(request.key, now)
            .map_err(|_| SourceError::Internal("clock_out_of_range"))?;
        tokio::time::timeout(READ_DEADLINE, reader(window))
            .await
            .map_err(|_| SourceError::Timeout("read_deadline"))?
    }

    async fn read_window(&self, window: Window) -> Result<Snapshot, SourceError> {
        let mut conn = self.connect().await?;
        let result = read_snapshot(&mut conn, window).await;
        close(conn, result.is_ok()).await;
        result
    }

    /// Opens the one connection of a read, within [`CONNECT_TIMEOUT`].
    pub(crate) async fn connect(&self) -> Result<PgConnection, SourceError> {
        tokio::time::timeout(
            CONNECT_TIMEOUT,
            PgConnection::connect_with(self.options.options()),
        )
        .await
        .map_err(|_| SourceError::Unavailable("connect_timeout"))?
        .map_err(|error| classify(Phase::Connect, &error))
    }
}

/// Closes the connection of a read. After a failed read the drop closes the
/// socket, and the server rolls the transaction back.
pub(crate) async fn close(conn: PgConnection, read_ok: bool) {
    if read_ok {
        let _closed = tokio::time::timeout(CLOSE_TIMEOUT, conn.close()).await;
    }
}

impl BatterySource for PostgresSource {
    fn snapshot<'a>(&'a self, request: &'a SourceRequest) -> SnapshotFuture<'a> {
        Box::pin(self.read(request))
    }
}

/// The phase of a database error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The connect and the start-up of the session.
    Connect,
    /// A statement of an open session.
    Statement,
}

/// Maps a `SQLx` error to a [`SourceError`] with a static code. The error
/// text of the database is never kept: it can hold a value.
///
/// - The connect phase: every failure is `Unavailable` (a Neon cold
///   start, a suspended compute, a network fault), except SQLSTATE class
///   28 (a wrong password or role), which a retry does not fix.
/// - A TLS failure is `Internal`: `sslmode=verify-full` and the fixed
///   roots make it a certificate or configuration fault that a retry does
///   not fix. A network fault during the handshake is an I/O error, so it
///   stays `Unavailable`.
/// - A statement: classes 08 (connection), 53 (resources), 57P (server
///   shutdown) are `Unavailable`; 57014 (the statement timeout) is
///   `Timeout`; every other answer is `Internal` (for example 42501, a
///   missing privilege, or 42P01, a missing table).
#[must_use]
pub fn classify(phase: Phase, error: &sqlx::Error) -> SourceError {
    match error {
        sqlx::Error::Database(database) => {
            let code = database.code();
            let code = code.as_deref().unwrap_or("");
            match (phase, code) {
                (_, code) if code.starts_with("28") => SourceError::Internal("authentication"),
                (Phase::Connect, _) => SourceError::Unavailable("connect_refused"),
                (Phase::Statement, "57014") => SourceError::Timeout("statement_timeout"),
                (Phase::Statement, code)
                    if code.starts_with("08")
                        || code.starts_with("53")
                        || code.starts_with("57P") =>
                {
                    SourceError::Unavailable("server_unavailable")
                }
                (Phase::Statement, _) => SourceError::Internal("statement_failed"),
            }
        }
        sqlx::Error::Tls(_) => SourceError::Internal("tls"),
        sqlx::Error::Io(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => SourceError::Unavailable("connection_failed"),
        sqlx::Error::Configuration(_) => SourceError::Internal("configuration"),
        _ => match phase {
            Phase::Connect => SourceError::Unavailable("connect_failed"),
            Phase::Statement => SourceError::Internal("decode"),
        },
    }
}

pub(crate) fn statement(error: &sqlx::Error) -> SourceError {
    classify(Phase::Statement, error)
}

fn ts(value: jiff_sqlx::Timestamp) -> Timestamp {
    value.to_jiff()
}

fn opt_ts(value: Option<jiff_sqlx::Timestamp>) -> Option<Timestamp> {
    value.map(ts)
}

fn sample(at: Option<jiff_sqlx::Timestamp>, value: Option<f64>) -> Option<Sample> {
    Some(Sample {
        at: ts(at?),
        value: value?,
    })
}

fn count(value: i64) -> Result<u32, SourceError> {
    u32::try_from(value).map_err(|_| SourceError::Contract(ContractViolation::CountTooLarge))
}

/// Reads the snapshot in one read-only transaction.
///
/// # Errors
///
/// Returns the classified [`SourceError`].
pub async fn read_snapshot(
    conn: &mut PgConnection,
    window: Window,
) -> Result<Snapshot, SourceError> {
    read_snapshot_with(conn, window, async || {}).await
}

/// [`read_snapshot`] with a step after the first statement of the
/// transaction. A test writes rows there to show that the later statements
/// read the snapshot of the first one.
///
/// # Errors
///
/// Returns the classified [`SourceError`].
pub async fn read_snapshot_with(
    conn: &mut PgConnection,
    window: Window,
    after_first: impl AsyncFnOnce(),
) -> Result<Snapshot, SourceError> {
    // `begin_with` sends the statement in the simple query protocol: one
    // round trip, no prepare.
    let mut tx = conn
        .begin_with(READ_TRANSACTION)
        .await
        .map_err(|error| statement(&error))?;

    let (vehicles, connectivity) = read_header(&mut tx, window).await?;
    after_first().await;
    let VehicleCount::One(vehicle_id) = vehicles else {
        commit(tx).await?;
        return Ok(Snapshot {
            window,
            vehicles,
            tiles: PerField::default(),
            series: SeriesPair::default(),
            events: EventSnapshot::default(),
            connectivity: None,
        });
    };

    let (tiles, mut series) = read_tiles(&mut tx, vehicle_id, window).await?;
    read_buckets(&mut tx, vehicle_id, window, &mut series).await?;
    let events = read_events(&mut tx, window).await?;
    commit(tx).await?;
    Ok(Snapshot {
        window,
        vehicles,
        tiles,
        series,
        events,
        connectivity,
    })
}

/// The vehicles and the newest CONNECTED or DISCONNECTED row of the
/// vehicle (`connection::newest_connectivity`), in one statement. The
/// consumption read uses it too.
pub(crate) async fn read_header(
    conn: &mut PgConnection,
    window: Window,
) -> Result<(VehicleCount, Option<ConnectivityRow>), SourceError> {
    // The vehicles and the newest CONNECTED or DISCONNECTED row of the
    // vehicle (`connection::newest_connectivity`), in one statement.
    let header = sqlx::query!(
        r#"
        SELECT
            ARRAY(SELECT id FROM vehicle ORDER BY id LIMIT 2) AS "vehicles!",
            c.connection_id AS "connection_id?",
            c.created_at AS "created_at?: jiff_sqlx::Timestamp",
            c.status AS "status?"
        FROM (SELECT 1) AS one
        LEFT JOIN LATERAL (
            SELECT connection_id, created_at, status
            FROM connectivity_event
            WHERE vehicle_id = (SELECT min(id) FROM vehicle)
              AND created_at < $1 AND status IN (1, 2)
            ORDER BY created_at DESC, connection_id DESC, status DESC
            LIMIT 1
        ) AS c ON true
        "#,
        window.to().to_sqlx() as _,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| statement(&error))?;
    let vehicles = match header.vehicles.as_slice() {
        [] => VehicleCount::None,
        [id] => VehicleCount::One(*id),
        _ => VehicleCount::Many,
    };
    let connectivity = match (header.connection_id, header.created_at, header.status) {
        (Some(id), Some(created_at), Some(status)) => Some(ConnectivityRow {
            connection_id: id.as_u128(),
            created_at: ts(created_at),
            status,
        }),
        _ => None,
    };
    Ok((vehicles, connectivity))
}

pub(crate) async fn commit(tx: sqlx::Transaction<'_, sqlx::Postgres>) -> Result<(), SourceError> {
    tx.commit().await.map_err(|error| statement(&error))
}

#[expect(
    clippy::too_many_lines,
    reason = "one checked query with four lookups; the SQL text is most of the lines"
)]
pub(crate) async fn read_tiles(
    conn: &mut PgConnection,
    vehicle_id: i16,
    window: Window,
) -> Result<(PerField<TileSnapshot>, SeriesPair<SeriesSnapshot>), SourceError> {
    let fields: Vec<i32> = Field::ALL.iter().map(|field| field.number()).collect();
    let from = window.from().to_sqlx();
    let to = window.to().to_sqlx();
    // The usable value of a row (`telemetry_web_domain::battery::value::extract`).
    let rows = sqlx::query!(
        r#"
        SELECT
            f.field AS "field!",
            latest.created_at AS "latest_created_at?: jiff_sqlx::Timestamp",
            latest.received_at AS "latest_received_at?: jiff_sqlx::Timestamp",
            usable.created_at AS "usable_at?: jiff_sqlx::Timestamp",
            usable.v AS "usable_value?",
            carry.created_at AS "carry_at?: jiff_sqlx::Timestamp",
            carry.v AS "carry_value?",
            first.created_at AS "first_at?: jiff_sqlx::Timestamp",
            first.v AS "first_value?"
        FROM unnest($2::integer[]) AS f (field)
        LEFT JOIN LATERAL (
            SELECT s.created_at, s.received_at
            FROM vehicle_signal AS s
            WHERE s.vehicle_id = $1 AND s.field = f.field AND s.created_at < $4
            ORDER BY s.created_at DESC
            LIMIT 1
        ) AS latest ON true
        LEFT JOIN LATERAL (
            SELECT x.created_at, x.v FROM (
                SELECT s.created_at, CASE
                    WHEN s.value_type IN (4, 5) AND s.float_value NOT IN ('NaN', 'Infinity', '-Infinity') THEN s.float_value
                    WHEN s.value_type IN (2, 3) AND s.int_value BETWEEN -9007199254740992 AND 9007199254740992 THEN s.int_value::double precision
                    WHEN s.value_type = 1 AND length(s.text_value) <= 32 AND s.text_value ~ '^-?[0-9]+(\.[0-9]+)?$' THEN s.text_value::double precision
                END AS v
                FROM vehicle_signal AS s
                WHERE s.vehicle_id = $1 AND s.field = f.field AND s.created_at < $4
                ORDER BY s.created_at DESC
            ) AS x
            WHERE x.v IS NOT NULL
            LIMIT 1
        ) AS usable ON true
        LEFT JOIN LATERAL (
            SELECT x.created_at, x.v FROM (
                SELECT s.created_at, CASE
                    WHEN s.value_type IN (4, 5) AND s.float_value NOT IN ('NaN', 'Infinity', '-Infinity') THEN s.float_value
                    WHEN s.value_type IN (2, 3) AND s.int_value BETWEEN -9007199254740992 AND 9007199254740992 THEN s.int_value::double precision
                    WHEN s.value_type = 1 AND length(s.text_value) <= 32 AND s.text_value ~ '^-?[0-9]+(\.[0-9]+)?$' THEN s.text_value::double precision
                END AS v
                FROM vehicle_signal AS s
                WHERE s.vehicle_id = $1 AND s.field = f.field AND s.created_at < $3
                ORDER BY s.created_at DESC
            ) AS x
            WHERE x.v IS NOT NULL
            LIMIT 1
        ) AS carry ON true
        LEFT JOIN LATERAL (
            SELECT x.created_at, x.v FROM (
                SELECT s.created_at, CASE
                    WHEN s.value_type IN (4, 5) AND s.float_value NOT IN ('NaN', 'Infinity', '-Infinity') THEN s.float_value
                    WHEN s.value_type IN (2, 3) AND s.int_value BETWEEN -9007199254740992 AND 9007199254740992 THEN s.int_value::double precision
                    WHEN s.value_type = 1 AND length(s.text_value) <= 32 AND s.text_value ~ '^-?[0-9]+(\.[0-9]+)?$' THEN s.text_value::double precision
                END AS v
                FROM vehicle_signal AS s
                WHERE s.vehicle_id = $1 AND s.field = f.field AND s.created_at >= $3 AND s.created_at < $4
                ORDER BY s.created_at ASC
            ) AS x
            WHERE x.v IS NOT NULL
            LIMIT 1
        ) AS first ON true
        "#,
        vehicle_id,
        &fields,
        from as _,
        to as _,
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|error| statement(&error))?;

    let mut tiles = PerField::<TileSnapshot>::default();
    let mut series = SeriesPair::<SeriesSnapshot>::default();
    for row in rows {
        let Some(field) = Field::from_number(row.field) else {
            return Err(SourceError::Internal("unexpected_field"));
        };
        let latest_row = match (row.latest_created_at, row.latest_received_at) {
            (Some(created_at), Some(received_at)) => Some(RowTimes {
                created_at: ts(created_at),
                received_at: ts(received_at),
            }),
            _ => None,
        };
        *tiles.get_mut(field) = TileSnapshot {
            latest_row,
            latest_usable: sample(row.usable_at, row.usable_value),
        };
        let target = match field {
            Field::Soc => &mut series.soc,
            Field::NominalFullPackEnergy => &mut series.nominal_full_pack_energy,
            Field::EnergyRemaining | Field::OutsideTemp => continue,
        };
        target.carry_in = sample(row.carry_at, row.carry_value);
        target.first_in_range = sample(row.first_at, row.first_value);
    }
    Ok((tiles, series))
}

async fn read_buckets(
    conn: &mut PgConnection,
    vehicle_id: i16,
    window: Window,
    series: &mut SeriesPair<SeriesSnapshot>,
) -> Result<(), SourceError> {
    let fields: Vec<i32> = Field::SERIES.iter().map(|field| field.number()).collect();
    let rows = sqlx::query!(
        r#"
        SELECT
            x.field AS "field!",
            max(x.created_at) FILTER (WHERE x.v IS NOT NULL) AS "at?: jiff_sqlx::Timestamp",
            (array_agg(x.v ORDER BY x.created_at DESC) FILTER (WHERE x.v IS NOT NULL))[1] AS "last?: f64",
            min(x.v) AS "min?",
            max(x.v) AS "max?",
            count(x.v) AS "usable!",
            count(*) FILTER (WHERE x.value_type = 10) AS "invalid!",
            count(*) AS "rows!"
        FROM (
            SELECT s.field, s.created_at, s.value_type, CASE
                WHEN s.value_type IN (4, 5) AND s.float_value NOT IN ('NaN', 'Infinity', '-Infinity') THEN s.float_value
                WHEN s.value_type IN (2, 3) AND s.int_value BETWEEN -9007199254740992 AND 9007199254740992 THEN s.int_value::double precision
                WHEN s.value_type = 1 AND length(s.text_value) <= 32 AND s.text_value ~ '^-?[0-9]+(\.[0-9]+)?$' THEN s.text_value::double precision
            END AS v
            FROM vehicle_signal AS s
            WHERE s.vehicle_id = $1 AND s.field = ANY ($2::integer[])
              AND s.created_at >= $3 AND s.created_at < $4
        ) AS x
        GROUP BY x.field, date_bin(make_interval(secs => $5), x.created_at, TIMESTAMPTZ '2000-01-01 00:00:00+00')
        ORDER BY 1, date_bin(make_interval(secs => $5), x.created_at, TIMESTAMPTZ '2000-01-01 00:00:00+00')
        "#,
        vehicle_id,
        &fields,
        window.from().to_sqlx() as _,
        window.to().to_sqlx() as _,
        f64::from(window.key().bucket_seconds()),
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|error| statement(&error))?;

    let mut totals: [(u64, u64); 2] = [(0, 0); 2];
    for row in rows {
        let index = match Field::from_number(row.field) {
            Some(Field::Soc) => 0,
            Some(Field::NominalFullPackEnergy) => 1,
            _ => return Err(SourceError::Internal("unexpected_field")),
        };
        let invalid = u64::try_from(row.invalid).map_err(|_| SourceError::Internal("count"))?;
        let all = u64::try_from(row.rows).map_err(|_| SourceError::Internal("count"))?;
        let usable = u64::try_from(row.usable).map_err(|_| SourceError::Internal("count"))?;
        let entry = &mut totals[index];
        entry.0 += invalid;
        entry.1 += all.saturating_sub(usable).saturating_sub(invalid);
        if let (Some(at), Some(last), Some(min), Some(max)) = (row.at, row.last, row.min, row.max) {
            let target = if index == 0 {
                &mut series.soc
            } else {
                &mut series.nominal_full_pack_energy
            };
            target.points.push(Point {
                at: ts(at),
                last,
                min,
                max,
                count: count(row.usable)?,
            });
        }
    }
    for (index, (invalid, not_numeric)) in totals.into_iter().enumerate() {
        let target = if index == 0 {
            &mut series.soc
        } else {
            &mut series.nominal_full_pack_energy
        };
        target.excluded = ExcludedCounts {
            invalid_marker: count(i64::try_from(invalid).unwrap_or(i64::MAX))?,
            not_numeric: count(i64::try_from(not_numeric).unwrap_or(i64::MAX))?,
        };
    }
    Ok(())
}

/// The loss rows and the restart markers of the window
/// (`gaps::select_events`), in one statement. Each part reads one row more
/// than its limit, so the snapshot can say that the limit cut it.
pub(crate) async fn read_events(
    conn: &mut PgConnection,
    window: Window,
) -> Result<EventSnapshot, SourceError> {
    let gap_limit = i64::try_from(MAX_GAPS + 1).unwrap_or(i64::MAX);
    let restart_limit = i64::try_from(MAX_RESTARTS + 1).unwrap_or(i64::MAX);
    let rows = sqlx::query!(
        r#"
        (
            SELECT
                run_id AS "run_id!",
                occurred_at AS "occurred_at!: jiff_sqlx::Timestamp",
                kind AS "kind!",
                reason,
                dropped_count,
                first_received_at AS "first_received_at: jiff_sqlx::Timestamp",
                last_received_at AS "last_received_at: jiff_sqlx::Timestamp"
            FROM ingest_event
            WHERE (kind = 'zmq_shortfall'
                   OR (kind = 'records_dropped' AND reason IN ('queue_full', 'database_refused')))
              AND last_received_at >= $1 AND first_received_at < $2
            ORDER BY first_received_at, last_received_at, occurred_at, run_id, kind
            LIMIT $3
        )
        UNION ALL
        (
            SELECT run_id, occurred_at, kind, NULL, NULL, NULL, NULL
            FROM ingest_event
            WHERE kind = 'started' AND occurred_at >= $1 AND occurred_at < $2
            ORDER BY occurred_at, run_id
            LIMIT $4
        )
        "#,
        window.from().to_sqlx() as _,
        window.to().to_sqlx() as _,
        gap_limit,
        restart_limit,
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|error| statement(&error))?;
    let mut losses = Vec::new();
    let mut restarts = Vec::new();
    for row in rows {
        let kind = EventKind::parse(&row.kind).map_err(SourceError::Contract)?;
        if kind == EventKind::Started {
            restarts.push(ts(row.occurred_at));
        } else {
            losses.push(EventRow {
                run_id: row.run_id.as_u128(),
                occurred_at: ts(row.occurred_at),
                kind,
                reason: row.reason,
                dropped_count: row.dropped_count,
                first_received_at: opt_ts(row.first_received_at),
                last_received_at: opt_ts(row.last_received_at),
            });
        }
    }
    // UNION ALL keeps no order between its parts; the order of the
    // reference rules.
    losses.sort_by(|a, b| loss_order(a).cmp(&loss_order(b)));
    restarts.sort();
    Ok(EventSnapshot {
        losses: Limited::cut(losses, MAX_GAPS),
        restarts: Limited::cut(restarts, MAX_RESTARTS),
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::error::Error as StdError;
    use std::fmt;

    use sqlx::error::{DatabaseError, ErrorKind};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Debug)]
    struct Fake(&'static str);

    impl fmt::Display for Fake {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("value TESTVIN0000000001 is not valid")
        }
    }

    impl StdError for Fake {}

    impl DatabaseError for Fake {
        fn message(&self) -> &'static str {
            "value TESTVIN0000000001 is not valid"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn database(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(Fake(code)))
    }

    #[test]
    fn errors_map_to_static_codes() {
        use Phase::{Connect, Statement};
        let cases = [
            (
                Connect,
                database("28P01"),
                SourceError::Internal("authentication"),
            ),
            (
                Statement,
                database("28000"),
                SourceError::Internal("authentication"),
            ),
            (
                Connect,
                database("XX000"),
                SourceError::Unavailable("connect_refused"),
            ),
            (
                Connect,
                database("53300"),
                SourceError::Unavailable("connect_refused"),
            ),
            (
                Statement,
                database("57014"),
                SourceError::Timeout("statement_timeout"),
            ),
            (
                Statement,
                database("57P01"),
                SourceError::Unavailable("server_unavailable"),
            ),
            (
                Statement,
                database("08006"),
                SourceError::Unavailable("server_unavailable"),
            ),
            (
                Statement,
                database("53200"),
                SourceError::Unavailable("server_unavailable"),
            ),
            (
                Statement,
                database("42501"),
                SourceError::Internal("statement_failed"),
            ),
            (
                Statement,
                database("42P01"),
                SourceError::Internal("statement_failed"),
            ),
            (
                Statement,
                sqlx::Error::Io(std::io::Error::other("x")),
                SourceError::Unavailable("connection_failed"),
            ),
            (
                Statement,
                sqlx::Error::RowNotFound,
                SourceError::Internal("decode"),
            ),
            (
                Connect,
                sqlx::Error::Tls("certificate not valid".into()),
                SourceError::Internal("tls"),
            ),
            (
                Connect,
                sqlx::Error::Io(std::io::Error::other("reset")),
                SourceError::Unavailable("connection_failed"),
            ),
            (
                Statement,
                sqlx::Error::Protocol("unexpected message".to_owned()),
                SourceError::Unavailable("connection_failed"),
            ),
            (
                Connect,
                sqlx::Error::Configuration("bad url".into()),
                SourceError::Internal("configuration"),
            ),
            (
                Connect,
                sqlx::Error::RowNotFound,
                SourceError::Unavailable("connect_failed"),
            ),
        ];
        for (phase, error, expected) in cases {
            let got = classify(phase, &error);
            assert_eq!(got, expected, "{phase:?} {error}");
            assert!(!got.to_string().contains("TESTVIN"));
        }
    }

    /// The bounds of STACK.md §5 and ADR 0013, written out: a change of a
    /// value fails here and asks for a change of the documents.
    #[test]
    fn the_bounds_are_the_documented_values_and_end_before_the_request_timeout() {
        assert_eq!(MAX_CONCURRENT, 2);
        assert_eq!(PERMIT_WAIT, Duration::from_secs(5));
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(15));
        assert_eq!(STATEMENT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(READ_DEADLINE, Duration::from_secs(24));
        assert!(CONNECT_TIMEOUT < READ_DEADLINE);
        assert!(PERMIT_WAIT + READ_DEADLINE < crate::app::REQUEST_TIMEOUT);
        let stack = include_str!("../../../../STACK.md");
        assert!(stack.contains(
            "at most 2 reads at a time (5 s wait, then `service_busy`), connect 15 s, the whole read 24 s"
        ));
        assert!(stack.contains("`statement_timeout` 10 s"));
    }

    #[test]
    fn options_force_verification_and_hide_the_password() {
        for url in [
            "postgresql://u:hunter2@db.invalid/neondb",
            "postgresql://u:hunter2@db.invalid/neondb?sslmode=disable",
        ] {
            let options = DatabaseOptions::from_url(url).unwrap();
            assert!(matches!(
                options.options().get_ssl_mode(),
                PgSslMode::VerifyFull
            ));
            let startup = options.options().get_options().unwrap();
            assert!(startup.contains("-c search_path=raw"), "{startup}");
            assert!(
                startup.contains("-c statement_timeout=10000ms"),
                "{startup}"
            );
            assert!(
                startup.contains("-c client_min_messages=error"),
                "{startup}"
            );
            assert!(!format!("{options:?}").contains("hunter2"));
        }
        assert!(DatabaseOptions::from_url("not a url").is_none());
    }

    fn source_for(port: u16) -> PostgresSource {
        let options =
            DatabaseOptions::from_url(&format!("postgresql://u:p@127.0.0.1:{port}/neondb"))
                .unwrap();
        PostgresSource::with_clock(options, Arc::new(Timestamp::now))
    }

    fn request() -> SourceRequest {
        SourceRequest {
            key: telemetry_web_domain::battery::RangeKey::Day,
            scenario: None,
        }
    }

    /// A read that never ends: the deadline of the whole read ends it at
    /// 24 s, not earlier.
    #[tokio::test(start_paused = true)]
    async fn a_read_that_never_answers_ends_at_the_read_deadline() {
        let source = source_for(9);
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            source.read_with::<Snapshot, _, _>(&request(), |_| std::future::pending()),
        )
        .await
        .expect("the read deadline ends the read");
        assert_eq!(result.unwrap_err(), SourceError::Timeout("read_deadline"));
        assert_eq!(started.elapsed(), READ_DEADLINE);
    }

    /// A peer that accepts connections and never answers: a hung Neon
    /// proxy. The connect deadline ends the read.
    #[tokio::test(start_paused = true)]
    async fn a_silent_server_is_unavailable_after_the_connect_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _accept = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let started = tokio::time::Instant::now();
        let error = source_for(port).snapshot(&request()).await.unwrap_err();
        assert_eq!(error, SourceError::Unavailable("connect_timeout"));
        assert!(started.elapsed() >= CONNECT_TIMEOUT);
        assert!(started.elapsed() < READ_DEADLINE);
    }

    /// Two reads run at the same time; a third read waits exactly 5 s for
    /// a slot and is busy, and its read never starts.
    #[tokio::test(start_paused = true)]
    async fn a_third_read_is_busy() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let source = Arc::new(source_for(9));
        let running = Arc::new(AtomicUsize::new(0));
        let (release, released) = tokio::sync::watch::channel(false);
        let held = |source: Arc<PostgresSource>| {
            let running = Arc::clone(&running);
            let mut released = released.clone();
            tokio::spawn(async move {
                source
                    .read_with::<Snapshot, _, _>(&request(), |_| async move {
                        running.fetch_add(1, Ordering::SeqCst);
                        released.wait_for(|done| *done).await.ok();
                        Err(SourceError::Internal("released"))
                    })
                    .await
            })
        };
        let first = held(Arc::clone(&source));
        let second = held(Arc::clone(&source));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(running.load(Ordering::SeqCst), 2);
        let started = tokio::time::Instant::now();
        let third = source
            .read_with::<Snapshot, _, _>(&request(), |_| async {
                Err(SourceError::Internal("third read started"))
            })
            .await;
        assert_eq!(third.unwrap_err(), SourceError::Busy);
        assert_eq!(started.elapsed(), PERMIT_WAIT);
        release.send(true).unwrap();
        for read in [first, second] {
            assert_eq!(
                read.await.unwrap().unwrap_err(),
                SourceError::Internal("released")
            );
        }
    }
}
