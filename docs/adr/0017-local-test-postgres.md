# ADR 0017: Local PostgreSQL server for the database tests

- Status: Accepted, 28 November 2026. Decided by the owner. The lead
  implemented the decision in PR #53.
- Scope: where the database tests and the SQLx check run, locally and in
  CI. Supersedes the test location of ADR 0002, "Test databases", and the
  statements of ADR 0008 that the development machine has no container
  runtime and that the pipeline tests need Neon.

## Context

- The local `verify` ran the database tests and the SQLx check on the Neon
  branch `test`. That branch used 5.39 CU-hours in November 2026, over its
  budget of 5 CU-hours each month (ADR 0002).
- The 44 database tests took about 233 s against Neon. A short Neon fault
  failed the run.
- CI already ran the same tests against PostgreSQL 18 in Docker on the
  runner, with TLS and the Neon-like role `neondb_owner`.
- The development machine has Apple `container` 1.7.1. The owner uses the
  same model in the project `spot-price` (its ADR 0008).

## Decision

1. `scripts/test-postgres.sh` starts the test server. It has one set-up
   and two back ends: Docker on the CI runner, and Apple `container` on the
   Mac. Both use the image of the `postgres_image` line of
   `.github/workflows/ci.yml`, a new test CA for each run,
   `sslmode=verify-full`, and the role `neondb_owner` (not a superuser).
2. The local back end starts a new server for each run, with data in
   tmpfs, on 127.0.0.1 with a random port in 10000-18999. The server goes
   away when the run ends. Each start removes the servers whose owner
   process is gone. At most 4 servers run at the same time.
3. `verify` uses one local server for `sqlx-check` and `test`. It needs no
   Neon URL and makes no Neon connection.
4. The end-to-end tests of `tests/storage/pipeline.rs` give the binary the
   test CA in the URL (`sslrootcert`) and read the tables as the migrator
   role. They run locally and in CI.
5. The two role tests that read the membership of `neon_superuser` run
   only in `mise run test-neon`, against the Neon branch `test`
   (`NEON_TEST_DATABASE_URL`). Run it after a change of
   `crates/ingest/src/storage/`, `db/`, `migrations/`, or a TLS crate in
   `Cargo.lock`, and before an ingest release. The local server has no
   `neon_superuser` role, and the tests create no copy of it.

## Key alternatives

- **Keep Neon for the local tests.** Rejected: over the CU-hour budget,
  slow, and a Neon fault fails a local run.
- **A shared long-lived local server.** Rejected: parallel runs from
  several worktrees would share state, and a stale server needs manual
  work.

## Consequences

- A local `verify` runs the database tests in about 18 s of test time
  and uses no CU-hours. It needs Apple `container` and its service; a
  missing tool or a stopped service fails `setup` and `verify`.
- Agents in a sandbox that blocks the XPC service of Apple `container`
  must run the database commands outside the sandbox.
- A local server cannot show these Neon facts. `test-neon` and the
  release checks of `docs/ingest-release.md` cover them in part:
  - the TLS path of the Neon proxy (SNI routing, the Neon certificate,
    the Mozilla roots without a test CA);
  - the role `neon_superuser` and its privileges;
  - the Neon build of PostgreSQL and its extensions or limits;
  - the cold start of a suspended Neon compute and its connect time.

## Cost of reversal

Low. Point `TEST_DATABASE_URL` back to the Neon branch and remove the
wrapper from the mise tasks. The tests do not depend on the local back
end.

## Reassessment

When Neon gives a test branch with no CU-hour cost, when Apple
`container` stops being maintained, or when a fault appears in production
that the local server did not show.
