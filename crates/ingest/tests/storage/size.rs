//! The measured size of a signal row (ADR 0002, "Expected size").
//!
//! The test is ignored in `mise run verify`, because it writes 100,000 rows.
//! Run it on its own:
//!
//! ```text
//! cargo nextest run --locked --test storage --run-ignored only --no-capture measure_the_size_of_a_signal_row
//! ```
//!
//! It writes 100 flushes of 1,000 signal rows each, as the writer does in
//! production, with a realistic mix of value kinds, and prints the sizes.

use ingest::record::{Location, SignalValue};
use ingest::storage::Writer;
use sqlx::{Connection, Row};

use crate::support::{Message, execute, queued, vehicle_data, with_migrated};

/// The value of datum `index` of a record: 70 % numbers, 10 % locations,
/// 10 % enums or booleans, 10 % short text, as the tuned vehicle config
/// streams mostly numbers. Every value is invented.
fn value(record: i64, index: i32) -> SignalValue {
    let step = f64::from(index) + f64::from(u32::try_from(record % 1_000).unwrap()) / 1_000.0;
    match index % 10 {
        0 => SignalValue::Location(Location {
            latitude: 10.0 + step / 1_000.0,
            longitude: 20.0 + step / 1_000.0,
        }),
        1 => SignalValue::Bool(record % 2 == 0),
        2 => SignalValue::Text(format!("{step:.5}")),
        3 | 4 => SignalValue::Int32(index + i32::try_from(record % 100).unwrap()),
        _ => SignalValue::Float64(step),
    }
}

#[tokio::test]
#[ignore = "writes 100,000 rows; run it on its own (module documentation)"]
async fn measure_the_size_of_a_signal_row() {
    with_migrated(|db| async move {
        let options = db.options.clone();
        let mut writer = Writer::new(options.clone());
        // 100 flushes. Each flush holds 20 records of 50 datums, one
        // record each 15 s, as a car that streams during a drive.
        for flush in 0..100_i64 {
            let messages: Vec<Message> = (0..20)
                .map(|record| {
                    let created = flush * 300 + record * 15;
                    Message::new(vehicle_data(
                        created,
                        (0..50)
                            .map(|index| (index, value(created, index)))
                            .collect(),
                    ))
                })
                .collect();
            writer.write(&queued(&messages), &mut || {}).await.unwrap();
        }

        let mut conn = db.owner().await;
        execute(&mut conn, "VACUUM ANALYZE vehicle_signal").await;
        let row = sqlx::query(
            "SELECT count(*),
                    avg(pg_column_size(s.*))::float8,
                    avg(pg_column_size(s.*)) FILTER (WHERE value_type IN (2, 5))::float8,
                    avg(pg_column_size(s.*)) FILTER (WHERE value_type = 7)::float8,
                    avg(pg_column_size(s.*)) FILTER (WHERE value_type = 1)::float8,
                    pg_relation_size('vehicle_signal'),
                    pg_relation_size('vehicle_signal_pkey'),
                    pg_relation_size('vehicle_signal_created_at_brin'),
                    pg_total_relation_size('vehicle_signal')
             FROM vehicle_signal AS s",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        let rows: i64 = row.get(0);
        let per_row = |bytes: i64| {
            f64::from(u32::try_from(bytes).unwrap()) / f64::from(u32::try_from(rows).unwrap())
        };
        println!("rows: {rows}");
        println!(
            "tuple (pg_column_size), all rows: {:.1} B",
            row.get::<f64, _>(1)
        );
        println!("tuple, numeric rows: {:.1} B", row.get::<f64, _>(2));
        println!("tuple, location rows: {:.1} B", row.get::<f64, _>(3));
        println!("tuple, text rows: {:.1} B", row.get::<f64, _>(4));
        println!("heap: {:.1} B per row", per_row(row.get(5)));
        println!("primary key: {:.1} B per row", per_row(row.get(6)));
        println!("BRIN: {:.2} B per row", per_row(row.get(7)));
        println!(
            "total (pg_total_relation_size): {:.1} B per row",
            per_row(row.get(8))
        );

        // The same rows with `field smallint` instead of `integer`: the cost
        // of the wider column.
        execute(
            &mut conn,
            "CREATE TABLE narrow (LIKE vehicle_signal);
             ALTER TABLE narrow ALTER COLUMN field TYPE smallint;
             INSERT INTO narrow SELECT * FROM vehicle_signal ORDER BY vehicle_id, field, created_at;
             CREATE TABLE wide (LIKE vehicle_signal);
             INSERT INTO wide SELECT * FROM vehicle_signal ORDER BY vehicle_id, field, created_at",
        )
        .await;
        // VACUUM cannot run inside the implicit transaction of a statement list.
        execute(&mut conn, "VACUUM ANALYZE narrow").await;
        execute(&mut conn, "VACUUM ANALYZE wide").await;
        let (narrow, wide): (i64, i64) =
            sqlx::query_as("SELECT pg_relation_size('narrow'), pg_relation_size('wide')")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        println!(
            "heap with field smallint: {:.1} B per row; with integer: {:.1} B per row",
            per_row(narrow),
            per_row(wide)
        );
        conn.close().await.unwrap();
    })
    .await;
}
