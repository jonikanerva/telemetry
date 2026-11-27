# ADR 0004: Lead authority granted by the owner

- Status: Accepted. Decided by: owner, 26 November 2026.
- Scope: three kinds of change that the lead may make without a separate
  owner approval. `STACK.md` §15 states the current grants.

## Context

- `DOCTRINE.md` ("Authority and escalation") requires an owner approval for
  added spending, a new provider, and a change of the agreed goal.
- The owner's Home Assistant uses the same Tesla developer application. Both
  share one Tesla usage credit of 10 USD each month. Tesla removes the
  vehicle telemetry configuration when usage exceeds the billing limit
  (`VISION.md`).
- The product runs on Railway (Hobby plan) and Neon (Free plan)
  (`VISION.md`).

## Decision

1. The lead may change and send the vehicle telemetry config without a
   separate owner approval when the estimated Tesla usage of the new config
   stays below 3 USD each month. The PR records the estimate. A config with
   a higher estimate needs the owner's approval.
2. The lead may create a second Railway service in project `calmdonut` when
   the architecture needs it. The expected cost is about 1 to 2 USD each
   month.
3. The lead may create Neon branches of project `telemetry` as isolated
   test databases, and an application role with limited privileges.

The lead decision on the second database role `telemetry_migrator` uses
grant 3 (ADR 0001, lead decision 2; ADR 0002, "Roles and privileges").

## Key alternatives

Not recorded with the decision. Without this grant, each such change needs
an owner approval (`DOCTRINE.md`, "Authority and escalation").

## Consequences

- A telemetry config change below the bound goes through the normal PR
  path. The estimate in the PR is the evidence.
- A second Railway service adds a monthly cost inside the stated bound.
- Test branches share the storage and the CU-hours of the Neon project
  (ADR 0002, "Test databases").

## Cost of reversal

Low. The owner can withdraw a grant. Resources that the lead created stay
until a later change removes them.

## Reassessment

No condition was recorded with the decision. The owner can change a grant
at any time.
