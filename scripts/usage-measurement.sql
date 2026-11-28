-- Measurement of the Tesla usage and the storage growth from the stored
-- records (docs/telemetry-usage-estimate.md, "Measure the usage").
--
-- Each statement is one plain SELECT. Run the statements one by one in
-- any SQL client. They read only. They return counts, days, field
-- numbers, kinds, and sizes, never an identity, a place, or a value.
--
-- Privacy: a row for one day shows when the car was awake or driven, so
-- it shows when the owner was away. A public record (an issue, a PR, or
-- a document) gets only totals of the whole measured period. It never
-- gets a value of a single day or a time of day. The rows of each day
-- go to the owner in the private report of the lead.
-- crates/ingest/tests/storage/usage.rs runs every statement of this file.
--
-- Rules of the file: each statement ends with a semicolon at the end of
-- a line, and no comment holds a semicolon, because the test splits the
-- file there. Every table name starts with the schema "raw.", because the
-- test replaces that prefix with its own schema.
--
-- Day: the UTC day of received_at. received_at is the time when the
-- ingest process received the record. created_at is the clock of the
-- car, and a record that the car sends late keeps its old created_at.
-- Tesla bills a signal when it streams, so the receive time is nearer to
-- the billing. AT TIME ZONE 'UTC' makes the day independent of the time
-- zone of the session.
--
-- Price: 150,000 signals for 1 USD. One stored row of vehicle_signal is
-- one signal (docs/telemetry-usage-estimate.md names the differences).

-- 1. Signals for each day, with the cost before the monthly discount.
SELECT (received_at AT TIME ZONE 'UTC')::date AS day,
       count(*) AS signals,
       round(count(*) / 150000.0, 6) AS usd
FROM raw.vehicle_signal
GROUP BY day
ORDER BY day;

-- 2. Signals for each day and field number, the largest fields first.
SELECT (received_at AT TIME ZONE 'UTC')::date AS day,
       field,
       count(*) AS signals,
       round(100.0 * count(*) / sum(count(*)) OVER (PARTITION BY (received_at AT TIME ZONE 'UTC')::date), 2) AS percent_of_day
FROM raw.vehicle_signal
GROUP BY day, field
ORDER BY day, signals DESC, field;

-- 3. The month of the latest signal: signals to date, and a projection to
-- the full month at the same rate. The elapsed time ends at the latest
-- signal, not at now(), so that the result depends only on the stored
-- rows. The projection is NULL until at least 3 UTC days of the month
-- have signals, because a few hours give a misleading rate. A projection
-- is not a measurement.
WITH latest AS (
    SELECT max(received_at) AT TIME ZONE 'UTC' AS at FROM raw.vehicle_signal
), month AS (
    SELECT date_trunc('month', at) AS start, at,
           extract(epoch FROM at - date_trunc('month', at)) AS elapsed,
           extract(epoch FROM date_trunc('month', at) + interval '1 month' - date_trunc('month', at)) AS length
    FROM latest
    WHERE at IS NOT NULL
), counted AS (
    SELECT month.start, month.elapsed, month.length, count(*) AS signals,
           count(DISTINCT (signal.received_at AT TIME ZONE 'UTC')::date) AS days
    FROM month, raw.vehicle_signal AS signal
    WHERE signal.received_at AT TIME ZONE 'UTC' >= month.start
    GROUP BY month.start, month.elapsed, month.length
)
SELECT start::date AS month,
       signals AS signals_to_date,
       round(signals / 150000.0, 6) AS usd_to_date,
       round(elapsed / 86400, 2) AS elapsed_days,
       round(length / 86400) AS days_in_month,
       days AS days_with_signals,
       CASE WHEN days >= 3 THEN round(signals * length / NULLIF(elapsed, 0)) END AS projected_signals,
       CASE WHEN days >= 3 THEN round(signals * length / NULLIF(elapsed, 0) / 150000.0, 6) END AS projected_usd
FROM counted;

-- 4. Connections for each day. The server takes connection_id from the
-- X-TXID header of the car, and no source says that the car never reuses
-- it (ADR 0002, "Idempotency"). So a connection is a connect event
-- (status 1) and the next event of the same connection_id when that is
-- a disconnect event (status 2). A disconnect without a connect before
-- it is a connection of its own. The day is the UTC day of the connect,
-- or of the disconnect when the connect is missing. An open connection
-- has no disconnect yet, or the server stopped before it wrote one.
-- The lengths count closed connections only. Many short connections show
-- as a high count with a short median.
WITH event AS (
    SELECT connection_id, status, created_at,
           lag(status) OVER same AS previous_status,
           lead(status) OVER same AS next_status,
           lead(created_at) OVER same AS next_at
    FROM raw.connectivity_event
    WHERE status IN (1, 2)
    WINDOW same AS (PARTITION BY connection_id ORDER BY created_at, status)
), connection AS (
    SELECT created_at AS connected,
           CASE WHEN next_status = 2 THEN next_at END AS disconnected
    FROM event
    WHERE status = 1
    UNION ALL
    SELECT NULL, created_at
    FROM event
    WHERE status = 2 AND previous_status IS DISTINCT FROM 1
), length AS (
    SELECT coalesce(connected, disconnected) AS first_event,
           connected, disconnected,
           extract(epoch FROM disconnected - connected) AS seconds
    FROM connection
)
SELECT (first_event AT TIME ZONE 'UTC')::date AS day,
       count(*) AS connections,
       count(*) FILTER (WHERE connected IS NOT NULL AND disconnected IS NOT NULL) AS closed,
       count(*) FILTER (WHERE disconnected IS NULL) AS open,
       count(*) FILTER (WHERE connected IS NULL) AS without_connect,
       round(percentile_cont(0.5) WITHIN GROUP (ORDER BY seconds)::numeric, 1) AS median_seconds,
       round(min(seconds), 1) AS shortest_seconds,
       round(max(seconds), 1) AS longest_seconds
FROM length
GROUP BY day
ORDER BY day;

-- 5. Awake time for each day: the closed connections, with overlapping
-- connections merged, split at UTC midnight. An open connection does not
-- count, because its end is not known. The car can be awake without a
-- connection, so this is the connected time. The pairs are those of
-- statement 4.
WITH event AS (
    SELECT connection_id, status, created_at,
           lag(status) OVER same AS previous_status,
           lead(status) OVER same AS next_status,
           lead(created_at) OVER same AS next_at
    FROM raw.connectivity_event
    WHERE status IN (1, 2)
    WINDOW same AS (PARTITION BY connection_id ORDER BY created_at, status)
), connection AS (
    SELECT created_at AS connected,
           CASE WHEN next_status = 2 THEN next_at END AS disconnected
    FROM event
    WHERE status = 1
    UNION ALL
    SELECT NULL, created_at
    FROM event
    WHERE status = 2 AND previous_status IS DISTINCT FROM 1
), closed AS (
    SELECT connected, disconnected,
           max(disconnected) OVER (ORDER BY connected, disconnected ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS previous_end
    FROM connection
    WHERE connected IS NOT NULL AND disconnected > connected
), island AS (
    SELECT connected, disconnected,
           count(*) FILTER (WHERE previous_end IS NULL OR connected > previous_end) OVER (ORDER BY connected, disconnected) AS number
    FROM closed
), merged AS (
    SELECT min(connected) AT TIME ZONE 'UTC' AS start, max(disconnected) AT TIME ZONE 'UTC' AS stop
    FROM island
    GROUP BY number
), part AS (
    SELECT day::date AS day,
           extract(epoch FROM least(stop, day + interval '1 day') - greatest(start, day)) AS seconds
    FROM merged,
         generate_series(date_trunc('day', start), date_trunc('day', stop), interval '1 day') AS day
)
SELECT day,
       round(sum(seconds), 1) AS awake_seconds,
       round(sum(seconds) / 3600, 2) AS awake_hours
FROM part
WHERE seconds > 0
GROUP BY day
ORDER BY day;

-- 6. Share of the signal rows that the car marked as a resend.
SELECT count(*) AS signals,
       count(*) FILTER (WHERE is_resend) AS resend_signals,
       round(100.0 * count(*) FILTER (WHERE is_resend) / NULLIF(count(*), 0), 2) AS resend_percent
FROM raw.vehicle_signal;

-- 7. Records that the tables cannot hold, by reason.
SELECT reason, count(*) AS records
FROM raw.unprocessed_record
GROUP BY reason
ORDER BY records DESC, reason;

-- 8. Signals with the invalid marker (value_type 10), by field number. A
-- field whose every signal is the marker costs signals and gives no
-- data: it is a candidate to drop from the config.
SELECT field,
       count(*) FILTER (WHERE value_type = 10) AS invalid_signals,
       count(*) AS signals,
       count(*) FILTER (WHERE value_type = 10) = count(*) AS only_invalid
FROM raw.vehicle_signal
GROUP BY field
HAVING count(*) FILTER (WHERE value_type = 10) > 0
ORDER BY invalid_signals DESC, field;

-- 9. Messages that the ingest process lost before the database, for each
-- UTC day and kind. Tesla billed the signals of a lost V message, but no
-- row holds them. The counts are messages, not signals.
-- records_dropped: messages that the process received but could not
-- store (a full queue, or a database that refused even the unprocessed
-- row). zmq_shortfall: V, alerts, and errors messages that the server
-- sent but the process did not receive. An errors message is not billed.
-- gate_full counts refused connections, not messages, so it is left out.
SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day,
       kind,
       sum(dropped_count) AS records
FROM raw.ingest_event
WHERE kind IN ('records_dropped', 'zmq_shortfall')
GROUP BY day, kind
ORDER BY day, kind;

-- 10. Size of the signal table with its indexes, against the 1 GB of the
-- Neon Free plan (taken as 1,000,000,000 bytes).
SELECT count(*) AS signals,
       pg_total_relation_size('raw.vehicle_signal') AS total_bytes,
       round(pg_total_relation_size('raw.vehicle_signal')::numeric / NULLIF(count(*), 0), 1) AS bytes_per_signal,
       round(pg_total_relation_size('raw.vehicle_signal') / 1e7, 4) AS percent_of_1_gb
FROM raw.vehicle_signal;

-- 11. Growth of the signal table for each UTC day: the rows of the day
-- times the average size of a row today. An estimate, not a measurement.
WITH average AS (
    SELECT pg_total_relation_size('raw.vehicle_signal')::numeric / NULLIF(count(*), 0) AS bytes
    FROM raw.vehicle_signal
)
SELECT (received_at AT TIME ZONE 'UTC')::date AS day,
       count(*) AS signals,
       round(count(*) * min(average.bytes)) AS estimated_bytes
FROM raw.vehicle_signal, average
GROUP BY day
ORDER BY day;
