# ADR 0009: Scope of the stack profile

- Status: Accepted.
  - Phase scope: decided by the owner, 26 November 2026.
  - Rust for the application server: decided by the owner. The earlier
    `STACK.md` text did not record the date.
  - One JSON log formatter (exception to `STACK.md` §8): decided by the
    lead, 26 November 2026, with approval by an independent reviewer
    (qa-enforcer) in PR #9.
  - The extension of the JSON-only rule to the web server is proposed in
    ADR 0010 (decision 8); it needs the approval of its own reviewer.
  - A second image for the ingest service (exception to the one-image
    rule of `docs/stack-web-profile.md` §15): decided by the lead,
    approved by the owner with the adoption PR (26 November 2026).
- Scope: which parts of the stack profile apply now, and two scoped
  exceptions to it.

## Context

- The stack profile (`STACK.md` and `docs/stack-web-profile.md`) is a
  template for a web application with a Rust backend and a TypeScript SPA.
- The product starts with the collection of the vehicle telemetry
  (`VISION.md`). The web application comes later.
- The ingest service runs Tesla's upstream `fleet-telemetry` server
  (ADR 0001).

## Decision

1. **Phase scope.** The first phase builds the ingest path and the raw
   storage. The web application and the derived periods (drives, charges,
   parked) come later. These parts of the profile apply from the first web PR: §1 Frontend, §2.1, §2.4, §2.5, the web rows of §4, §11, and the pnpm, TypeScript, Oxlint, Vitest, and Playwright parts of §3, §6, and §14. The
   general text of the other sections applies now, wherever the code has
   that concern.
2. **Rust.** The application server uses Rust. The product has domain
   rules (units, periods, energy calculations) and a long-running ingestion
   path, where strict types and explicit failure handling earn their cost.
3. **One log formatter.** The process has only the JSON formatter. One
   formatter gives one redaction path: the field allowlist exists once,
   and a second formatter cannot leak a field that the JSON formatter
   redacts. Consequence: developers read the logs with `jq`. Compensating
   evidence: the JSON formatter and its redaction tests in
   `crates/ingest/src/logging.rs`.
4. **A second image.** The ingest service is an image of its own, because
   it runs Tesla's upstream server. The application still ships as one
   image.

## Key alternatives

- For decision 2: not recorded with the decision.
- **A readable development formatter.** The profile default. Not chosen
  while no workflow needs it (decision 3).

## Consequences

- `CLAUDE.md` imports `STACK.md` only. The web profile is read before the
  first web PR and with each web change.
- The ingest image does not follow the one-image rule.

## Cost of reversal

Low for decisions 1, 3, and 4. High for decision 2: the code is Rust.

## Reassessment

- Decision 3: when a developer workflow needs readable logs, and with the
  first web PR.
- Decision 4: permanent while the upstream server is used.
- Decisions 1 and 2: no condition was recorded with the decisions.
