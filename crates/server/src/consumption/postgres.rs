//! The PostgreSQL read of the consumption view.
//!
//! One connection and one `REPEATABLE READ READ ONLY` transaction for each
//! request, with the read slots and deadlines of the battery source
//! (`crate::battery::postgres`). The SQL only selects rows; the rules are
//! in `telemetry_web_domain::consumption`. Two statements read the rows:
//!
//! 1. the Gear readings of the range, newest first, at most
//!    `MAX_GEAR_ROWS + 1`, and the newest reading before the range;
//! 2. the Odometer, `EnergyRemaining`, and `OutsideTemp` rows of the
//!    windows of `sample_windows` (one hour before each start to its end),
//!    newest first, at most `MAX_DRIVE_SAMPLES + 1`.
//!
//! Before these, the read runs the statements of the battery read for the
//! vehicles, the connection, the newest battery rows, and the losses.
//!
//! The columns are the columns that `db/roles.sql` grants to the web
//! role: `vehicle_id`, `field`, `created_at`, `value_type`, `int_value`,
//! `float_value`, and `text_value`.

use jiff_sqlx::ToSqlx;
use sqlx::{Connection, PgConnection};
use telemetry_web_domain::battery::{EventSnapshot, PerField, RawValue, VehicleCount, Window};
use telemetry_web_domain::consumption::{
    GEAR_FIELD, GearInput, MAX_DRIVE_SAMPLES, MAX_GEAR_ROWS, SampleInput, Snapshot, ValueField,
    gear, gear_input, periods, sample, sample_input, sample_windows,
};

use super::{ConsumptionFuture, ConsumptionSource};
use crate::battery::postgres::{
    PostgresSource, READ_TRANSACTION, close, commit, read_events, read_header, read_tiles,
    statement,
};
use crate::battery::source::{SourceError, SourceRequest};

impl PostgresSource {
    async fn read_consumption(&self, window: Window) -> Result<Snapshot, SourceError> {
        let mut conn = self.connect().await?;
        let result = read_consumption(&mut conn, window).await;
        close(conn, result.is_ok()).await;
        result
    }
}

impl ConsumptionSource for PostgresSource {
    fn consumption<'a>(&'a self, request: &'a SourceRequest) -> ConsumptionFuture<'a> {
        Box::pin(self.read_with(request, |window| self.read_consumption(window)))
    }
}

fn limit(max: usize) -> i64 {
    i64::try_from(max.saturating_add(1)).unwrap_or(i64::MAX)
}

/// Reads the snapshot in one read-only transaction.
///
/// # Errors
///
/// Returns the classified [`SourceError`].
pub async fn read_consumption(
    conn: &mut PgConnection,
    window: Window,
) -> Result<Snapshot, SourceError> {
    read_consumption_with(conn, window, MAX_DRIVE_SAMPLES).await
}

/// [`read_consumption`] with another limit of the value rows (the tests).
///
/// # Errors
///
/// Returns the classified [`SourceError`].
pub async fn read_consumption_with(
    conn: &mut PgConnection,
    window: Window,
    max_samples: usize,
) -> Result<Snapshot, SourceError> {
    let mut tx = conn
        .begin_with(READ_TRANSACTION)
        .await
        .map_err(|error| statement(&error))?;
    // The vehicles, the connection, the newest battery rows, and the
    // losses: the statements of the battery read.
    let (vehicles, connectivity) = read_header(&mut tx, window).await?;
    let VehicleCount::One(vehicle_id) = vehicles else {
        commit(tx).await?;
        return Ok(Snapshot {
            window,
            vehicles,
            gears: gear_input(None, Vec::new(), MAX_GEAR_ROWS),
            samples: SampleInput::default(),
            tiles: PerField::default(),
            connectivity: None,
            events: EventSnapshot::default(),
        });
    };
    let (tiles, _series) = read_tiles(&mut tx, vehicle_id, window).await?;
    let events = read_events(&mut tx, window).await?;
    let gears = read_gears(&mut tx, vehicle_id, window).await?;
    let samples = read_samples(&mut tx, vehicle_id, &gears, max_samples).await?;
    commit(tx).await?;
    Ok(Snapshot {
        window,
        vehicles,
        gears,
        samples,
        tiles,
        connectivity,
        events,
    })
}

/// The Gear readings of the range and the newest reading before it.
async fn read_gears(
    conn: &mut PgConnection,
    vehicle_id: i16,
    window: Window,
) -> Result<GearInput, SourceError> {
    // The SQL filter equals `consumption::gear`: ShiftState P, R, N, D.
    let rows = sqlx::query!(
        r#"
        (
            SELECT true AS "before!", created_at AS "at!: jiff_sqlx::Timestamp",
                   value_type AS "value_type!", int_value
            FROM vehicle_signal
            WHERE vehicle_id = $1 AND field = $2::integer AND value_type = 9
              AND int_value IN (2, 3, 4, 5) AND created_at < $3
            ORDER BY created_at DESC
            LIMIT 1
        )
        UNION ALL
        (
            SELECT false, created_at, value_type, int_value
            FROM vehicle_signal
            WHERE vehicle_id = $1 AND field = $2::integer AND value_type = 9
              AND int_value IN (2, 3, 4, 5) AND created_at >= $3 AND created_at < $4
            ORDER BY created_at DESC
            LIMIT $5
        )
        "#,
        vehicle_id,
        GEAR_FIELD,
        window.from().to_sqlx() as _,
        window.to().to_sqlx() as _,
        limit(MAX_GEAR_ROWS),
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|error| statement(&error))?;
    let mut before = None;
    let mut newest_first = Vec::with_capacity(rows.len());
    for row in rows {
        let raw = RawValue {
            value_type: row.value_type,
            int_value: row.int_value,
            float_value: None,
            text_value: None,
            bool_value: None,
        };
        let Some(reading) = gear(&raw) else {
            return Err(SourceError::Internal("unexpected_gear"));
        };
        if row.before {
            before = Some(reading);
        } else {
            newest_first.push((row.at.to_jiff(), reading));
        }
    }
    // UNION ALL keeps no order between its parts; each part is ordered.
    newest_first.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    Ok(gear_input(before, newest_first, MAX_GEAR_ROWS))
}

/// The value rows inside the ended drives.
async fn read_samples(
    conn: &mut PgConnection,
    vehicle_id: i16,
    gears: &GearInput,
    max_samples: usize,
) -> Result<SampleInput, SourceError> {
    let windows = sample_windows(&periods(gears));
    if windows.is_empty() {
        return Ok(SampleInput::default());
    }
    let starts: Vec<jiff_sqlx::Timestamp> = windows.iter().map(|(s, _)| s.to_sqlx()).collect();
    let ends: Vec<jiff_sqlx::Timestamp> = windows.iter().map(|(_, e)| e.to_sqlx()).collect();
    let fields: Vec<i32> = ValueField::ALL.iter().map(|f| f.number()).collect();
    // The windows share no instant (`sample_windows`), so each row comes
    // once and the limit counts distinct rows.
    let rows = sqlx::query!(
        r#"
        SELECT s.field AS "field!", s.created_at AS "at!: jiff_sqlx::Timestamp",
               s.value_type AS "value_type!", s.int_value, s.float_value, s.text_value
        FROM unnest($2::timestamptz[], $3::timestamptz[]) AS d (start_at, end_at)
        JOIN vehicle_signal AS s
          ON s.vehicle_id = $1 AND s.field = ANY ($4::integer[])
         AND s.created_at >= d.start_at AND s.created_at <= d.end_at
        ORDER BY s.created_at DESC, s.field
        LIMIT $5
        "#,
        vehicle_id,
        starts as _,
        ends as _,
        &fields,
        limit(max_samples),
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|error| statement(&error))?;
    let mut newest_first = Vec::with_capacity(rows.len());
    for row in &rows {
        let Some(field) = ValueField::from_number(row.field) else {
            return Err(SourceError::Internal("unexpected_field"));
        };
        let at = row.at.to_jiff();
        let raw = RawValue {
            value_type: row.value_type,
            int_value: row.int_value,
            float_value: row.float_value,
            text_value: row.text_value.as_deref(),
            bool_value: None,
        };
        newest_first.push((at, sample(field, at, &raw)));
    }
    Ok(sample_input(newest_first, max_samples))
}
