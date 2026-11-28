# ADR 0008: Verification environments and their gaps

- Status: Accepted.
  - Linux evidence from CI, and the exception for runtime tests on
    linux/amd64: decided by the lead, 26 and 27 November 2026, with
    approval by an independent reviewer (qa-enforcer) in PR #18, PR #21,
    and PR #23.
  - Exception for the build and test of the ingest image before merge:
    approved by the owner with the adoption PR (26 November 2026). The
    change of its compensating evidence to the CI job `image-check`: lead,
    27 November 2026, approved by qa-enforcer in PR #23.
  - Decision 1 superseded in part by ADR 0016 (28 November 2026): one local
    `mise run verify` runs for each version to be merged, by the reviewer
    for a change with a high-class part, otherwise by the lead. The CI job `ci` stays.
  - Partly superseded by ADR 0017 (28 November 2026): the statements that
    the development machine has no container runtime, and that the
    end-to-end tests of `tests/storage/pipeline.rs` need Neon.
- Scope: where the required checks run, and what each environment does not
  prove. `STACK.md` §3, §13, and §14 state the current rules.

## Context

- The development machine is macOS arm64 and has no container runtime.
  (Superseded by ADR 0017: the machine has Apple `container` for the
  local test server; it builds no image.)
- Production is a Linux container on Railway (linux/amd64 is an
  assumption about the Railway host). Railway builds the image with its own
  builder after the merge.
- The ingest process has platform-dependent code: TCP keep-alive options,
  signal handling and child reaping as PID 1, and the child process.
- Some database tests need Neon: the end-to-end tests of
  `tests/storage/pipeline.rs` give the binary a new URL and an empty
  environment, so it trusts only the Mozilla roots (superseded by ADR
  0017: the URL now holds the test CA); two tests of
  `tests/storage/roles.rs` read the membership of `neon_superuser`.

## Decision

1. **Two required runs before merge.** The local `mise run verify`
   (`DOCTRINE.md` P6) and a green CI job `ci` (`mise run ci` on
   linux/amd64) for the current head of the PR. Neither replaces the
   other. ADR 0016 sets who runs the local `verify`, and how often.
2. **Exception: runtime tests on linux/amd64 before merge.** CI runs the
   tests on linux/amd64 against PostgreSQL on the runner, not in the
   container: `ingest` is not PID 1 there, no Railway proxy is in front of
   the gate, and the tests that need Neon do not run there.
   - Consequence: a Linux-only runtime fault that shows only in the
     container, as PID 1, behind the Railway proxy, or in a test that needs
     Neon shows only after deployment. The Neon TLS path and the tests that
     need Neon run only on macOS.
   - Compensating evidence: the Linux checks 1 to 6 of ADR 0001 ("To
     verify at first deployment"). Check 1 comes from CI. The CI job
     `image-check` runs the container part of checks 2, 3, and 6 under
     Docker without an init (`ingest` is PID 1) and under the init of
     Docker (`docker run --init`, tini; `ingest` is PID 2). tini is not the
     init of Railway, so checks 2 to 5 stay Railway checks after the
     release (`docs/ingest-release.md`). Check 6 has CI evidence only: a
     `railway ssh` session (UID 0) cannot read the environment of a process
     of UID 10001.
3. **Exception: build and test of the ingest image before merge.** The CI
   job `image-check` builds `ingest/Dockerfile` with Docker on linux/amd64
   and checks the running container. The Railway build stays unverified
   before the merge: another builder and cache, the Railway settings, the
   TCP proxy, and the healthcheck run only after the merge.
   - Compensating evidence: a green `image-check` job of the PR head before
     the merge; after the release, the checks and the recovery path of
     `docs/ingest-release.md`.

## Key alternatives

- **A local container runtime.** Not available on the development
  machine. (Superseded by ADR 0017 for the database tests; the image
  checks stay in CI.)
- Other alternatives were not recorded with the decisions.

## Consequences

- A merge needs a local run and a CI run of the same head.
- Some faults appear only after a release. The post-release checks of
  `docs/ingest-release.md` look for them.

## Cost of reversal

Low. The decisions are rules for the checks, not code.

## Reassessment

- Exception 2 ends when the release records checks 1 to 5
  (`docs/ingest-release.md`) and check 6 has its CI evidence. Reassess on
  2027-02-28 if it is still open, and when CI can run the tests that need
  Neon.
- Exception 3: when Railway documents its Dockerfile build, or when the
  repository can build with the Railway builder before the merge.
