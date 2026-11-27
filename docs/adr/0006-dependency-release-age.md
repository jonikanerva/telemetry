# ADR 0006: Minimum release age of dependencies

- Status: Accepted. Decided by: owner, 27 November 2026.
- Scope: when a new crate release is due in the dependency freshness check
  of `STACK.md` §3.

## Context

- `DOCTRINE.md` P7 requires the latest stable releases, and it requires
  respect for the declared minimum release age, except for security fixes.
- The profile sets a minimum release age for pnpm (`minimumReleaseAge`, for
  example 1440 minutes).
- `cargo update` and `mise run deps` do not print the publish time of a
  crate release.

## Decision

- Cargo has a minimum release age of 1440 minutes (24 hours). A crate
  release that is younger is not due in the dependency freshness check.
- A security fix is the exception: it is due at once.
- The lead reads the publish time on crates.io (`created_at` of the
  version).

## Key alternatives

Not recorded with the decision.

## Consequences

- A new crate release waits at least one day before a dependency PR adopts
  it, except a security fix.
- The age check is a manual step of the lead.

## Cost of reversal

Low. The rule is a step of the freshness check.

## Reassessment

No condition was recorded with the decision.
