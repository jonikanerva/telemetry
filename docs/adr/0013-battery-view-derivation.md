# ADR 0013: Battery view derivation

- Status: Accepted.
- Date: 28 November 2026.
- Deciders: the owner (the simplification, 28 November 2026); the lead of
  slice S3 (the rest, within ADR 0004).

## Context

`GET /api/v1/battery` shows the battery history (`VISION.md`, view 1).
It reads the raw tables of ADR 0002; no derived periods exist yet. The
view must tell apart: loading, data, no data yet, a gap, asleep, stale,
and unavailable. The product has one user.

## Decision

The view shows stored facts only. It infers nothing from the order of
ingest events.

1. **Values.** A row is usable when it holds a finite number; the invalid
   marker and other values are counted by reason, never zero. Series use
   fixed buckets (60 s to 43,200 s from 2000-01-01Z), a carry-in sample
   before the range, and the capacity change from the carry-in or the
   first value. Units come from the Tesla documentation; `OutsideTemp`
   is assumed to be in °C.
2. **Connection.** The newest CONNECTED or DISCONNECTED row of
   `connectivity_event`, as it is: `connected` or `disconnected` with its
   time, or `none` when no row exists. Asleep and offline are one state.
3. **Freshness.** `current` when the newest signal row was received at
   most 1200 s ago (the 900 s flush plus 300 s); else `overdue` when the
   connection is `connected`, else `not_expected` (asleep).
4. **Gaps.** Only rows in which ingest records a loss: `records_dropped`
   (`queue_full`, `database_refused`) and `zmq_shortfall`, whose receive
   interval meets the range, with their count and times. `gate_full` is
   no gap: the vehicle keeps refused data. A new drop reason needs a
   change here in the same release. Gaps use the ingest clock and points
   the vehicle clock, so the split of a line at a gap is approximate.
5. **Restart markers.** The `started` times in the range. A marker says
   that the ingest process started, not that data was lost.
6. **Limits.** At most 500 gaps and 500 markers, each list with a
   `truncated` flag. Every read is bounded by the range, except the
   newest connectivity row and the newest value of each field.
7. **Access.** The role `telemetry_web` reads only the columns of the
   queries. One connection and one `REPEATABLE READ READ ONLY`
   transaction per request, no pool; at most 2 reads at a time (5 s wait,
   then 503 `service_busy`); connect 15 s, statement 10 s, read 24 s. A
   connect failure, SQLSTATE class 08 or 53, or 57P gives 503
   `database_unavailable`; a timeout gives 503 `timeout`; class 28, a TLS
   failure, and other errors give 500. A failed read is never an empty
   view.

Experiment E3 (Neon branch `test`, 27 November 2026, 90 days at the
planning rate): server work under 0.1 s for every range; the read time is
mostly round trips, about 4.7 s from a client with a 240 ms round trip.
The post-release check measures the read on Railway
(`docs/web-release.md`, "Battery API read time").

## Key alternative

**Infer run liveness from the ingest events** (pair `started` and
`stopped` rows, wait for proofs, show crashes as estimated gaps, and
derive the connection from the runs). Rejected by the owner: three
review rounds found false results in each version of the rule, and the
complexity does not fit a one-user product.

## Consequences

- An unclean crash shows as a restart marker and, while the car is
  connected, as stale data; it is not a shaded gap.
- A process that ends with an open connection leaves `connected`; the
  view then shows `connected` at T and stale data, nothing more.
- Asleep and offline are one state.

## Cost of reversal

Low: the rules are in `crates/domain/src/battery/` and
`crates/server/src/battery/postgres.rs`.

## Reassessment

When the owner wants shaded gaps for crashes, or when the derived periods
exist.
