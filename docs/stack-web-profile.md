# Web profile of STACK.md

This file holds the stack profile text for the web phase: the browser
application, the HTTP API, and the general profile rules in their full
form. `STACK.md` holds the rules that apply to the ingest service now, in
short form, and the project facts.

- The web parts are binding from the first web PR: §1 Frontend, §2.1, §2.4, §2.5, the web rows of §4, §11, and the pnpm, TypeScript, Oxlint, Vitest, and Playwright parts of §3, §6, and §14. Read this file
  before the first web PR and with each web change.
- The general text of the other sections binds the ingest code now,
  wherever the code has that concern. Read the section when a change
  touches its topic.
- The section numbers are those of `STACK.md`. A section reference (§) to a
  section that this file does not hold points to `STACK.md`.
- When a rule here and a rule of `STACK.md` cover the same topic, both
  apply.

## Profile summary

> Web application with a Rust backend and a strict TypeScript SPA. Axum,
> Tokio, and Tower provide HTTP and concurrency. Utoipa exports OpenAPI 3.1;
> Hey API generates a TypeScript client and Zod validators. React, Vite,
> TanStack Router, and TanStack Query provide the browser application. Jiff
> represents time. PostgreSQL with SQLx is the default when durable data is
> needed. One immutable image contains the server and built SPA files.

Use with `DOCTRINE.md` (P1–P9) and the selected host contract. This is an
example profile, not evidence that a project has implemented its checks.
MUST, MUST NOT, SHOULD, and MAY have the meanings in the doctrine. On adoption,
confirm scope, pin a compatible version set, implement the five commands,
and complete §14–15. Record exclusions and enforcement gaps. This profile
is self-contained and does not require another stack profile to be copied.
It does not grant authority beyond the adopted project contract.

---

## 0. Project shape: web parts

- **Critical execution paths:** server request decode, domain conversion, use case, and response; browser route, query/client boundary, view model, and render. Blocking work must not stall async I/O or the browser main thread.
- **Applicable UI states:** the states of `VISION.md` (Product Shape), and denied access (an expired session, 401).
- **Intended use:** interactive web applications. Durable data, accounts, background jobs, and real-time connections depend on `VISION.md`; they are not mandatory product features.
- **Reassessment:** SEO-first, SSR-required, or offline-first products need an adapted design. A small integration or CRUD product may cost less to maintain as one TypeScript system; record the product reason for Rust.

### Repository layout and boundaries: web crates

- `domain` MUST NOT depend on Axum, Tokio, SQLx, reqwest, server code, or storage adapters. It receives time, identifiers, and other external values explicitly. It MUST NOT read the environment, filesystem, ambient clock, or random sources.
- `server` owns effects, HTTP DTOs, use cases, persistence adapters, public error mapping, and process wiring. Handlers decode input, call a use case, and return its mapped result.
- Separate DTOs, database rows, and domain values when their meanings differ. Use validating constructors or `TryFrom`. Serde derives on domain values MAY be used when representations agree and deserialization cannot bypass invariants.
- `packages/api-client` owns generated API artifacts and a small handwritten facade for runtime configuration, validation failures, and application-facing errors. Generated files MUST NOT be edited manually.
- `web` accesses the API through that package. It MUST NOT import server implementations or maintain duplicate handwritten wire contracts.

Cargo enforces declared crate dependencies, not purity. Changes to dependency
manifests need boundary review. Standard-library I/O and ambient time remain
possible without new dependencies. Use selected Clippy disallowed-method
rules where practical and record review-only gaps. Global cargo-deny bans
are not per-crate architecture checks. Enforce frontend import directions
with Oxlint `no-restricted-imports` rules and inspect dynamic-loading gaps.

---

## 1. Language & Runtime

### Backend: profile text

`dbg!` stays denied in tests, because it is a debugging leftover.

Use narrow `#[expect(clippy::lint_name, reason = "...")]` for a justified
lint limitation. A stale expectation fails verification. An expectation
cannot waive an acceptance criterion or replace §13. Test-only assertion and
fixture code has a different failure contract from production request paths.
Do not ban lossless numeric casts as a class; pedantic conversion findings
and domain bounds address lossy conversions. Add targeted checks where needed.

### Frontend

- **Language baseline:** TypeScript 7 with the native compiler. Pin the exact release. TypeScript 7.0 has no programmatic compiler API. A tool that needs the compiler API gets a scoped exception (§2.1). Reassess the exceptions when a TypeScript 7 release provides the new API.
- **Build-time runtime:** Node.js 24 LTS, exact supported patch pinned in mise. Node is not a production server requirement for this SPA. Reassess later LTS lines after release and a tooling trial.
- **Package manager:** pnpm workspaces, exact pnpm version pinned, committed `pnpm-lock.yaml`, and frozen-lockfile installs. Do not use `latest` in manifests.
- **Lint:** Oxlint with its type-aware tsgolint backend (`oxlint-tsgolint`). typescript-eslint does not run on TypeScript 7. Enable the type-aware rules that correspond to typescript-eslint `strict-type-checked`, the React hook rules, and the import restrictions. Deny warnings. Record each strict rule that Oxlint does not implement and the review that covers it.

```json
{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "exactOptionalPropertyTypes": true,
    "noImplicitOverride": true,
    "noImplicitReturns": true,
    "noFallthroughCasesInSwitch": true,
    "noPropertyAccessFromIndexSignature": true,
    "useUnknownInCatchVariables": true,
    "verbatimModuleSyntax": true,
    "moduleDetection": "force"
  }
}
```

### Provisioning: profile text

Mise tasks are the common bootstrap and command interface. Declare how mise
and rustup are installed on a fresh machine. `rust-toolchain.toml` owns the
Rust version; mise owns Node, pnpm, and auxiliary tool pins. Do not maintain
conflicting Rust pins. Use the selected toolchain, not an incidental global one.

`mise run setup` MUST provision or verify the pinned tools, install locked
Cargo/pnpm dependencies, install required browser binaries, and explain how
to start isolated test services. Pin nextest, cargo-deny, cargo-shear, Gitleaks,
and SQLx CLI when selected. Verify each tool's installation method and feature
set; pinning its package version alone does not prove reproducible tool builds.

---

## 2. Frameworks

| Concern                  | Technology                                                | Role                                                           |
| ------------------------ | --------------------------------------------------------- | -------------------------------------------------------------- |
| HTTP                     | Axum, Tower, tower-http                                   | Routing, extraction, request limits, tracing, and static files |
| Concurrency              | Tokio                                                     | Async I/O and owned tasks; explicit shutdown policy            |
| Domain errors            | Result, enums, thiserror                                  | Meaningful expected failures                                   |
| Wire representation      | Serde and serde_json                                      | Validated transport boundaries                                 |
| HTTP contract            | utoipa and utoipa-axum                                    | OpenAPI 3.1 tied to registered routes                          |
| Client generation        | Hey API with Zod                                          | SDK, types, and executed runtime validators                    |
| Time                     | Jiff                                                      | Instant, civil, zoned, elapsed, and calendar values            |
| Persistence, if needed   | PostgreSQL, SQLx, jiff-sqlx                               | Explicit SQL, constraints, transactions, and time adapters     |
| Outbound HTTP, if needed | reqwest                                                   | Shared configured client with bounded work                     |
| Logging                  | tracing and tracing-subscriber                            | Structured events and async spans                              |
| UI and build             | React, React DOM, Vite                                    | SPA without a second production runtime                        |
| Routing and server state | TanStack Router and Query                                 | URL state and query cache ownership                            |
| Styling                  | CSS; Tailwind optional                                    | Product-specific styling choice                                |
| Tests                    | nextest, Cargo doctests, proptest, Vitest, Playwright/axe | Behaviour, contracts, and critical journeys                    |
| Formatting and lint      | rustfmt, Clippy, Prettier, Oxlint, tsc                    | Named checks through mise                                      |

### 2.1 API contract and generation

1. Register documented routes through utoipa-axum. Export OpenAPI 3.1 from Rust HTTP types and route declarations. Do not claim that annotations prove actual handler responses.
2. Export to an explicit file path without production configuration, a database connection, or pre-built SPA assets. SQLx macro compilation uses committed offline metadata where applicable.
3. Generate the SDK, wire types, and Zod validators from the local specification into `packages/api-client`. No hosted generation service is required.
4. Commit the OpenAPI document and generated client artifacts. Pin the generator and plugins. `contract:check` regenerates in scratch space and compares complete trees, including added/deleted files. It MUST NOT repair the working tree. A plain tracked-file diff is insufficient.
5. For a high-class route (`DOCTRINE.md`, introduction) and for problem bodies, enable runtime validation in the executed client path. Test rejection of an invalid success response and separately test error-body validation. A generated unused schema is not protection. A read-only data response MAY rely on the generated types only.
6. Test real responses from changed high-class operations against the declared contract: status, content type, body, and failure paths. Keep expected outcomes traced to requirements, not only to the same generator.
7. Define handling of optional/null fields, unknown fields and enum values, tagged unions, numeric bounds, and temporal precision. Do not silently coerce or invent facts.

Generator configuration, checked with Hey API 0.99:

- Make the output independent of where it is written. The generator reads the first `tsconfig*.json` above its install directory to choose the import extension; set the extension and turn the lookup off (`output.module.extension`, `output.tsConfigPath: null`), or `contract:check` in scratch space can differ from the committed tree.
- The generator uses the TypeScript compiler API, which TypeScript 7.0 does not provide. Pin a TypeScript 6 release as a devDependency of the API-client package only, so that only the generator resolves it. Run every type check with TypeScript 7. Record this dependency as a §13 exception. Remove the exception when the generator supports TypeScript 7.
- The client runtime that the generator copies into the output can fail `exactOptionalPropertyTypes`. Never edit generated files. Type-check the generated directory with its own `tsconfig` that turns off only this flag, keep the flag for the facade and the application, and record a scoped exception.
- A generator dependency can pin a vulnerable package version outside the reach of an ordinary update. Use a pnpm override for that exact parent version, prove with `contract:check` that the output does not change, and record the override with its removal condition.

The build dependency order is Rust contract-export compilation, OpenAPI
export, client generation, frontend build, and image assembly. Export and
ordinary server compilation MUST NOT depend on frontend assets. If embedding
is selected, keep export independent so it cannot create a generation cycle.

### 2.2 Errors and failure containment

Use thiserror enums for expected domain and use-case errors. Use RFC 9457
Problem Details with `application/problem+json` for public failures and a
stable application `code` extension. Preserve the standard meanings of
`type`, `title`, `status`, `detail`, and `instance` when supplied.

Map known server errors exhaustively in one place. Cover extractor rejections,
invalid JSON, body limits, timeouts, authentication failures, and route
fallbacks too. The client handles known codes exhaustively and unknown codes,
invalid bodies, and intermediary responses safely. Do not expose SQL,
internal traces, secrets, or personal data. Programmer defects remain defects;
define and test the generic public failure behaviour without treating a
broken invariant as a valid business outcome.

A retryable classification does not authorize repeating a write. Define
idempotency separately. An invalid independent item may be excluded only if
the contract permits partial success; expose its safe reason. Invalid
whole-operation input and atomic failures MUST NOT publish partial success.

### 2.3 Outbound sources

Decode each external source at its adapter and narrow it into domain types
before other code sees it.

- Serde's derived `Deserialize` for a struct also accepts a JSON array in field order, and a repeated key fails the whole value. When the source contract requires an object or accepts the last repeated key, check the JSON type or decode through `serde_json::Value`, which keeps the last repeated key. Test both cases.
- Give each attempt a deadline that includes the response body, and limit the body size. A per-attempt timeout that stops at the headers leaves the body read unbounded.
- The text of a transport error can contain a URL with a query or a token. Map transport failures to fixed messages before they reach a log, an error value, or a response. In reqwest, `Error::without_url` removes the URL.
- Turn off redirects for a client that sends a credential in a custom header or in the query. reqwest removes only standard credential headers, such as `Authorization` and `Cookie`, on a redirect to another host.
- Set the `User-Agent` explicitly. reqwest sends none by default.

### 2.4 Frontend state

| State                                     | Owner                           |
| ----------------------------------------- | ------------------------------- |
| Server data and client cache              | TanStack Query                  |
| Shareable filters, search, and pagination | Router and validated URL values |
| Local interaction state                   | React                           |
| Incomplete input                          | Form state                      |
| Durable business data                     | Backend and selected storage    |

Set stale times, refetch triggers, retries, and cache lifetimes explicitly.
Remove cached data on logout. Forward cancellation signals where supported;
client cancellation does not prove server-side rollback. Avoid copying query
data into another global store. Use typed view states rather than unrelated
optional flags. A failed fetch MUST NOT become an empty successful result.

### 2.5 Authentication, when accounts are required

Use an approved OIDC provider and opaque backend-owned cookie sessions.
Evaluate openidconnect and a maintained session implementation with a
compatible storage/version tuple. Backend authorization applies per resource
regardless of browser visibility or cached claims.

Define Secure, HttpOnly, and suitable SameSite cookie settings, CSRF protection,
rotation, fixation prevention, expiry, revocation, logout, and concurrent
session behaviour. Verify the login/callback flow and failure paths. Avoid
persistent browser bearer-token storage as the default design.

The inspected tower-sessions-sqlx-store 0.15.0 uses SQLx 0.8 and
tower-sessions-core 0.14, not the SQLx 0.9 baseline below. Recheck this at
adoption. Do not assume pools or session types are interchangeable, or add
two incompatible pools as a workaround. A first-party session system requires
a demonstrated need, threat model, independent security review, and end-to-end
evidence; it is not the default fix for adapter lag. Until the authentication
trial passes, report that capability as unvalidated.

**Project note (ADR 0003):** this project
has no accounts and no OIDC provider. One password protects the web
application: a Railway service variable holds an Argon2id hash, a login
gives a session cookie with an expiry and a logout, and the server slows
down repeated login attempts globally. The login, the session, and the
throttle are a first-party session system. The requirements above for
such a system apply with the first web PR: a threat model, an independent
security review, and end-to-end evidence. The cookie, CSRF, rotation,
fixation, expiry, revocation, and logout rules above also apply. ADR 0003
records the demonstrated need.

---

## 3. Build & verify commands: profile text

Mise tasks are the single source of truth. Implement them in the adopting
project; this profile ships no application scripts. Commits, agent workflows,
and any CI invoke these entry points rather than separate tool pipelines.

- **format:** rustfmt and Prettier in write mode.
- **lint:** `cargo clippy --workspace --all-targets --locked -- -D warnings`, type-aware Oxlint, and TypeScript 7 `tsc --noEmit` for web, client, and test code. Declare supported feature combinations; do not blindly enable incompatible features.
- **build:** export/generate in the declared order, build the SPA, and build the release server with locked dependencies and offline SQLx metadata. Verify the intended Linux image before claiming release readiness.
- **test:** nextest workspace tests, `cargo test --doc --workspace --locked`, Vitest, required real-database integration tests, and Playwright against the built service. Required browser tests are part of the complete test command, not an optional omitted suffix.
- **verify:** format check without edits; isolated service setup and migrations where needed; lint/type checks; complete-tree contract drift check; SQLx metadata check; security/dependency checks; build; all required tests. Stop on required failures, retain useful diagnostics, and clean up only owned temporary resources.

`cargo sqlx prepare --check`, run once in each crate with SQLx query macros
so that each crate keeps its metadata in its own `.sqlx/` (`STACK.md` §3,
task `sqlx-check`), MUST use an isolated migrated database with the
relevant targets/features. The release compilation uses
`SQLX_OFFLINE=true`; offline compilation does not replace the schema check.
Initial query/migration work may generate metadata against the isolated database
before verification. Checks never silently regenerate missing expected evidence.

Use `cargo deny check`, `pnpm audit`, and Gitleaks with the policies in §14.
Cargo-shear SHOULD check unused Rust dependencies. Quick tasks MAY exist but
do not replace full verification. A missing required browser, database, or
scanner is a gap, not a pass. Validate test connection configuration and
isolation before creating, migrating, or dropping any database.

**Vulnerability check:** `cargo deny check advisories`, `pnpm audit`, and `osv-scanner scan source -r .` for the lockfiles. `osv-scanner scan image <image>` for each container image. Compare the Rust toolchain and each infrastructure version in §15 with its vendor security advisories. Record each surface without scanner coverage.

---

## 4. Performance budgets

Complete this table at adoption from product requirements. Specify conditions
and measure the result. Unvalidated example numbers are not acceptance gates.
Do not raise an adopted budget only to fit a poor implementation result.

The illustration column shows an order of magnitude for a typical interactive
web application. It is not a gate, a promise, or a measured result.

| Budget                         | Required adoption value and measurement conditions                                                       | Illustration (not a gate)                                              |
| ------------------------------ | -------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| API latency and errors         | Per-route class p50/p99 and error-rate limit; workload, concurrency, data volume, and upstream inclusion | p50 10 ms, p99 50 ms per simple route, excluding upstream calls        |
| Startup                        | Startup-to-ready limit; target hardware and cold/warm conditions                                         | < 1 s to ready, excluding migrations                                   |
| Memory and resource saturation | Resident-memory limit, database pool, queue, and concurrency ceilings under declared load                | < 256 MB resident for the server container                             |
| Release size                   | Image/binary and initial compressed JS/CSS budgets where they affect product fitness                     | Image < 100 MB compressed; initial JS < 200 KB and CSS < 50 KB gzipped |
| Web experience                 | LCP, INP, and CLS targets with devices, network, sample conditions, and measurement method               | At the 75th percentile: LCP ≤ 2.5 s, INP ≤ 200 ms, CLS ≤ 0.1           |

Distinguish field percentile evidence from laboratory approximations. New
analytics collection needs the normal privacy and authority assessment.
Include TLS, diagnostics, database access, and concurrent work in service
measurements. A microbenchmark is not production capacity evidence.

## 5. Persistence shape: profile text

- **Entities and retention:** defined by `VISION.md`; technical browser caches require the same purpose and privacy assessment.
- **Queries:** prefer SQLx checked query macros for static SQL. Commit `.sqlx` metadata and verify it against the migrated schema. Dynamic SQL needs bound parameters, explicit structural constraints, and integration tests; it does not acquire the same compile-time guarantee.
- **Domain conversion:** validate rows when storage types permit values the domain forbids. A query type check does not establish authorization, isolation, or meaning.
- **Constraints:** use applicable NOT NULL, CHECK, foreign keys, unique keys, and exclusion constraints. Test rejection and competing writes; read-before-write alone cannot enforce concurrent uniqueness.
- **Atomicity:** use transactions for atomic operations. Define isolation, locking, or optimistic concurrency from the invariant. Do not publish partial success.
- **Repeated requests:** define idempotency and outcome reconciliation, including a committed write followed by a lost response. Use keys and uniqueness where needed.
- **Migrations:** numbered, reviewed migrations. Run SQLx migration tooling or a subcommand using SQLx's own migration support as a separate release step, not an implicit startup effect. Use expand-and-contract compatibility with running prior releases.
- **Recovery:** test empty-database setup, upgrade from supported prior schemas, and restore/roll-forward paths. Application rollback does not restore deleted data. Follow the authorized policy for destructive changes.
- **Server version:** run the isolated test server on the same PostgreSQL major version as production. Read the production version on adoption and after each platform upgrade (`SHOW server_version`, read-only, or the platform's database image tag) and pin the test server to that major version. Record both versions. A migration test, query metadata check, or catalog snapshot from another major version is not evidence for production. A newer stable major version follows the `DOCTRINE.md` P7 infrastructure rule.
- **Connection TLS:** enable one SQLx TLS feature that uses the same rustls crypto provider as the rest of the binary. With reqwest `rustls`, that is `tls-rustls-aws-lc-rs` (§7). The default `sslmode=prefer` uses TLS when the server offers it, checks no certificate chain, and fails the connection on a failed handshake instead of falling back to plain text. `sslmode=require` fails without a TLS feature. SQLx also reads `PGSSLMODE` when the URL has no `sslmode`. rustls rejects a server certificate that is not version 3, although `psql` accepts it. Before the first release, check the production `sslmode`, `PGSSLMODE`, and certificate version read-only.

SQLite backup needs its supported backup API, VACUUM INTO, or a proven
quiescent snapshot with a tested restore. Do not assume a live file copy is
consistent. Redis, event sourcing, repository traits, and durable queues need
a current reason; they are not baseline dependencies.

---

## 6. Approved dependencies: profile baseline

These are profile baseline lines, not a tested application lockfile. On
adoption, verify support, features,
licences, advisories, and compatibility; pin exact resolved versions and tools.
Do not install unused conditional packages. New dependencies need a PR rationale
for necessity, provenance, maintenance, licence, transitive cost, and replacement
cost. Significant durable choices need short ADRs in the adopting project.
Routine libraries remain delegated choices; providers, external data transfers,
added cost, and material lock-in follow the doctrine's escalation rules.

| Dependency                                     | Baseline line             | Why it earns its place                                                           |
| ---------------------------------------------- | ------------------------- | -------------------------------------------------------------------------------- |
| axum / tokio                                   | 0.8 / 1                   | HTTP and async runtime                                                           |
| tower / tower-http                             | 0.5 / 0.7                 | Service middleware and static-file delivery                                      |
| serde / serde_json / thiserror                 | 1 / 1 / 2                 | Wire types and typed errors                                                      |
| jiff                                           | 1                         | Semantic time types                                                              |
| utoipa / utoipa-axum                           | 6 / 0.3                   | OpenAPI routes and schemas                                                       |
| tracing / tracing-subscriber                   | 0.1 / 0.3                 | Structured backend diagnostics                                                   |
| sqlx / jiff-sqlx, conditional                  | 0.9 / 1                   | Durable data and Jiff database wrappers; TLS feature `tls-rustls-aws-lc-rs` (§5) |
| uuid / rust_decimal, conditional               | 1 / 1                     | Typed identities and exact decimal quantities                                    |
| reqwest, conditional                           | 0.13                      | External HTTP calls; feature `rustls` (aws-lc-rs provider)                       |
| @hey-api/openapi-ts                            | 0.99, pin exact release   | Local client and validator generation                                            |
| zod                                            | 4                         | Executed browser boundary validation                                             |
| react / react-dom / vite                       | 19 / 19 / 8               | SPA and build                                                                    |
| @tanstack/react-router / @tanstack/react-query | 1 / 5                     | Routing and server-state cache                                                   |
| typescript / oxlint / oxlint-tsgolint          | 7 / pin exact / pin exact | Native compiler and type-aware linting (§1)                                      |
| typescript, API-client generator only          | 6, pin exact              | Compiler API for the generator until it supports TypeScript 7 (§2.1)             |
| prettier / vitest                              | 3 / 5                     | Format and frontend unit tests                                                   |
| @playwright/test / @axe-core/playwright        | 1 / 4                     | Browser journeys and accessibility checks                                        |
| proptest, dev                                  | 1                         | Targeted invariant tests                                                         |
| tailwindcss, optional                          | 4                         | Styling when selected by the product                                             |

Jiff SQLx values use adapter wrapper types; configure query type overrides
or annotations deliberately and test their round trips. Utoipa's `uuid`
and decimal-related features must agree with actual serialization.

Known conflict: the baseline names `jiff` 1, but
utoipa 6.0.0, the newest release, has only the feature `jiff_0_2`. The
project does not enable that feature. The time types of the HTTP contract
use an explicit schema override (`wire::Instant` in `crates/server`, ADR
0010) with round-trip tests and the executed Zod checks of experiment E1.
Reassess when utoipa publishes `jiff` 1 support.
Pin selected Cargo feature sets; avoid enabling everything by convenience.
Authentication packages and their compatible store remain conditional §2.5
adoption decisions. Snapshot, mutation, and coverage tools are optional when
they address a named risk. Their versions belong in the adopting tool pins.

---

## 7. Stack-specific reject-list additions

- First-party unsafe code outside a reviewed §13 exception.
- Production unwrap, expect, panic, todo, or unimplemented paths used for ordinary failures; unchecked indexing or arithmetic that can violate accepted input bounds.
- Lint suppression without a narrow reason; changing checks only to obtain a pass.
- Bare `String`, `Uuid`, or integer entity IDs across crate or API boundaries; use one newtype per entity. Binary floating-point values for exact quantities.
- Deserialization that bypasses domain invariants; unchecked wire-to-domain casts, silent loss of precision, or default facts without requirements.
- TypeScript any, double casts, or ts-ignore/ts-expect-error used to bypass required validation. A justified compiler limitation needs a precise reason and review.
- Raw API fetches or handwritten duplicate wire contracts outside the API-client boundary; manual edits to generated artifacts.
- Broad untyped domain error signatures that erase required error handling; anyhow may be used for startup/tooling context, not as the domain error model.
- println, eprintln, or dbg in shipped server paths; public diagnostics containing secrets, personal data, or whole payloads. Export tools write to an explicit file path.
- Ambient time, environment, filesystem, or random access in domain logic; unvalidated configuration scattered across modules.
- Unbounded queues, retries, response bodies, or concurrency; outbound/database operations without defined deadlines.
- Blocking I/O or heavy CPU work on async request execution paths; synchronous lock guards across await; unowned spawned tasks or silently discarded errors/futures.
- Unbound SQL interpolation or unchecked SQL safety assertions.
- Two rustls crypto providers in one binary, for example SQLx `tls-rustls-ring-webpki` together with reqwest `rustls` (aws-lc-rs). rustls panics when a certificate verifier is built without an explicit provider, so `sslmode=verify-ca` or `verify-full` stops the process. Check `cargo tree -i ring` and `cargo tree -i aws-lc-rs` after a TLS feature change.
- useEffect for server-data fetching, duplicate global query stores without need, or cached data kept after logout.
- Host-local timezone arithmetic, invented instants for date-only values, or manual UTC-offset calculations (§10).

Scope tests and tooling according to their contracts. Supported platform APIs
and justified lossless operations do not need blanket bans. New frontend
state frameworks require a current reason rather than an automatic prohibition.

---

## 8. Logging & privacy

Use tracing with tracing-subscriber JSON output. Carry safe request
correlation through async spans. Allowlist fields. Apply the allowlist to span fields as well as event fields,
because request correlation puts data on spans. Avoid automatic Debug output of complete arguments, DTOs,
configuration, headers, and payloads. Redact secret types and test redaction.
Create the request identifier inside the panic containment of the request: a
random identifier generator can panic when the operating system gives no
random bytes. Work that several requests share has no single request
identifier; record that gap instead of borrowing the first caller's span.

Define diagnostic access, retention, cardinality, and resource limits. No
external crash reporter or telemetry exporter is enabled by default. Assess new data flows and obtain required authority before adding
one. Keep secrets out of source history, images, generated files, and browser
bundles. Validate configuration in one startup boundary.

---

## 9. Background & lifecycle

Every background task needs an owner, observable failure, and shutdown policy.
Use JoinSet, tracked handles, or suitable cancellation mechanisms. Bound request,
upstream, database, and queue work. Define retry count, backoff, jitter, total
deadline, and operation safety; make time and randomness controllable in tests.

A client disconnect does not guarantee cancellation. Dropping a future does
not undo committed or external effects. Already-running spawn_blocking work
cannot normally be aborted; limit parallelism and define its completion or
cooperative stop policy. A timeout on waiting does not prove work stopped.

A cache or a request-coalescing map that runs one shared lookup per key as a
spawned task owns that task. Bound it with deadlines. Remove its entry with a
drop guard inside the task, so a panic after every caller has left cannot keep
the key in memory or cache the failure. Drive expiry from a clock that tests
can pause (`tokio::time::Instant`); a cache crate with its own clock source
makes expiry tests slow or nondeterministic. Choose and test the time to live
of a failure separately from the time to live of a success. The drop guard
runs on a panic only with `panic = "unwind"`.

Implement and test a shutdown deadline in addition to Axum graceful shutdown.
Dropping a Tokio runtime waits without a limit for its blocking tasks, so end
the process with `shutdown_background` or `shutdown_timeout` after the deadline.
Treat a failed signal-handler set-up the same way for every signal, and log it.
Stop accepting work, drain or cancel owned operations, and define the outcome
at the deadline. Periodic jobs and real-time connections are allowed for an
explicit product need with bounded resource use and authorization.

Work promised to survive a restart requires durable ownership and recovery.
An in-memory task is insufficient. Evaluate a database-backed job mechanism
before adding another service, but do not prescribe a custom queue without
assessing concurrency, retries, idempotency, and maintenance.

## 10. Time & timezones

- **Instant:** Jiff Timestamp; PostgreSQL timestamptz when persisted; validated RFC 3339 UTC with Z on the wire. Define supported precision and range.
- **Calendar value:** civil Date/Time and separate date-only/local-time wire fields. Store an IANA zone and rule for recurring local schedules. Use Zoned for zone-aware computation. Timestamptz alone does not preserve the original zone rule.
- **Duration:** SignedDuration for elapsed quantities and Span for calendar arithmetic. Define wire units/format separately; never silently treat calendar months or days as fixed seconds. Measure elapsed time with a monotonic clock.
- **Conversions:** use Jiff timezone APIs, declare the timezone database source/update policy, and specify DST gap/overlap handling. No manual offsets or host-local assumptions.
- **Browser:** validate wire values before use. Use Date only for appropriate instants and Intl with explicit display zones. Use Temporal only with verified target-browser support or an approved polyfill; retain date-only and calendar semantics otherwise.
- **Numbers:** exact decimals and integers outside JavaScript's safe integer range cross JSON as strings. A bounded small integer MAY remain numeric if the contract enforces its bounds. Validate DB/wire/domain range differences.
- **Tests:** pass fixed time into domain functions; use an injectable process clock where needed. Test relevant DST, precision, expiry, retry, and date-only cases. Jiff SQLx wrapper conversion needs round-trip evidence.

Use one first-party domain time abstraction by default. Transitive dependencies
may use other libraries; their presence does not require rewriting them.

---

## 11. Design guidelines & UX thresholds

- **Design authority:** WCAG 2.2 AA and native HTML semantics. Semantic elements first; ARIA only where needed.
- **Thresholds:** visible keyboard focus on interactive elements. The axe check of each page covers text contrast and target size; other contrast and target checks are optional.
- **Inputs:** full keyboard operability, sensible DOM/focus order, accessible names, and announced validation/error states.
- **Browser evidence:** Playwright journeys against the release service, one axe check for each page, and keyboard-only operation. Declare supported browsers and viewports; `STACK.md` §3 declares them.
- **View states:** test each state of `VISION.md` once for each view in Vitest, through the view model. Playwright covers one journey for each view: the data state and one failure state.
- **Deterministic journeys:** when the service depends on external sources, run the journeys in `verify` against a test server binary that mounts the real router, the real middleware, and the built SPA with fake adapters for the external sources. A journey against live external services is a separate live check, not part of `verify`.

A component or CSS library does not prove conformance. Automated checks miss
accessibility issues; identify remaining checks and evidence. An owner-only
test needed for safe release blocks merge, but seek an automated equivalent
instead of making owner testing routine. Preserve user input during upgrades,
retry, offline transitions, and recoverable errors where required by the product.

---

## 12. Best practices source

Consult current, version-relevant official documentation for uncertain APIs
and material decisions. Use the documentation tools available in the current
host. Cite the relevant sections in decisions or reviews; do not require every
role to repeat unchanged research.

- [Rust edition guide](https://doc.rust-lang.org/edition-guide/rust-2024/index.html) and [Clippy](https://rust-lang.github.io/rust-clippy/master/index.html).
- [Axum](https://docs.rs/axum/), [Tower HTTP](https://docs.rs/tower-http/), and [Tokio shutdown](https://tokio.rs/tokio/topics/shutdown).
- [SQLx](https://docs.rs/sqlx/), [Jiff SQLx](https://docs.rs/jiff-sqlx/), and [PostgreSQL](https://www.postgresql.org/docs/).
- [Serde](https://serde.rs/), [Jiff](https://docs.rs/jiff/), [utoipa](https://docs.rs/utoipa/), and [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457).
- [Hey API validators](https://heyapi.dev/docs/openapi/typescript/plugins/sdk#validators), [React](https://react.dev/), [Vite](https://vite.dev/), and [TanStack](https://tanstack.com/).
- [Nextest](https://nexte.st/), [Playwright accessibility](https://playwright.dev/docs/accessibility-testing), and [cargo-deny](https://embarkstudios.github.io/cargo-deny/checks/index.html).
- [OWASP CSRF guidance](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html) and [WCAG 2.2](https://www.w3.org/TR/WCAG22/).

Select the pinned version in documentation where available. Manifest support,
latest documentation, and a tested application combination are different facts.

---
## 13. Scoped exceptions: profile text

Do not weaken a rule or check to make a change pass. The responsible lead
resolves exceptions within project authority, with an independent reviewer.
The change author must not approve their own weaker acceptance or checks.
Escalate changes beyond authority and unresolved material risks to the owner.
Record significant durable choices in short docs/adr records in the adopting
project; do not require an ADR for every ordinary default. Backlog and history
remain in GitHub.

---

## 14. Applicability and evidence: profile text

Complete local environments, command wiring, scanner policies, and existing
required CI checks on adoption. Run the full `$VERIFY_CMD` locally once on
the version to be merged, integrated with the current base; `DOCTRINE.md` P9
names who runs it. Missing or failed mandatory local evidence blocks merge.
Existing required CI must also pass; CI itself is optional under P6. Never bypass existing gates.

Assign each check to before merge or after release. Missing pre-merge evidence
blocks merge; missing post-release evidence prevents claiming a successful
release. A future production deployment is not required to approve its PR.
DOCTRINE.md P9 states who runs the full verification and what the reviewer
records.

Retain commands, safe fixtures or reconstruction steps, expected outcomes and
their original sources, versions, environments, and results. Challenge material
assumptions and transformations beyond supplied examples. A test of a generated
assumption is not proof of that assumption. Regression tests should detect the
original failure. Investigate flakes rather than treating a rerun as an
explanation. Apply P6 evidence reuse without waiving an independent run that P9 requires.

| Doctrine / applicability      | Required evidence                                                                                                                       | Environment / gap                                                                                 |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| P3, P5: HTTP contract         | Complete-tree drift check; real response tests; executed success/error validation                                                       | Annotations do not prove handler behaviour                                                        |
| P5–P7: browser                | Critical Playwright journeys, degraded/error states, keyboard and axe                                                                   | Record unsupported browsers and manual accessibility gaps                                         |

### Check configuration

| Check             | Tool                                                                                     | Acceptance condition                                                                |
| ----------------- | ---------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| Format            | rustfmt and Prettier check modes                                                         | No formatting differences                                                           |
| Types/lint        | Clippy, tsc, type-aware Oxlint                                                           | No errors or unreviewed warnings under §1                                           |
| Dead dependencies | cargo-shear SHOULD; rustc unused checks; Knip optional                                   | No unexplained findings; configure generated/dynamic entry points                   |
| Boundaries        | Cargo declarations, Clippy disallowed methods where practical, Oxlint restricted imports | No forbidden edges; review declared gaps                                            |
| Contracts         | Scratch export/generation and actual HTTP/client tests                                   | No unexplained drift or invalid data accepted                                       |
| SQL metadata      | SQLx prepare check with declared targets/features                                        | Matches the migrated isolated schema                                                |
| Secrets           | Gitleaks with redacted output and declared source/history scope                          | No confirmed exposed secrets                                                        |
| Rust dependencies | cargo-deny advisories, licences, bans, sources                                           | Triage findings; enforce declared allowlists and reviewed exceptions                |
| JS dependencies   | pnpm audit                                                                               | Triage all findings; no unresolved high/critical findings without a valid exception |
| Behaviour         | nextest, doctests, SQLx tests, Vitest, Playwright                                        | All required cases pass; expected outcomes have independent support                 |

Pin scanner tools and configuration. Record advisory freshness and unsupported
surfaces. External audit endpoints may receive dependency metadata; assess the
data flow and obtain required authorization before using a new provider.
An unavailable scanner is a gap, not a pass. Property tests, mutation analysis,
snapshots, and coverage address named risks; scores do not replace correctness.
Mutation analysis is for high-class surfaces only.
Do not accept snapshots without reviewing their expected meaning.

### Live checks of external sources

A live check against an external source asserts on a stable control sample.
When the source can legitimately lack data for recent items, report the hit
rate of such a sample as a measurement, not as a pass condition. Never run a
live check with a user's credential unless the owner runs it.

## 15. Release, recovery, and maintenance: profile text

- **Release:** one immutable image contains the Rust binary and built SPA files. ServeDir is the default asset mechanism; embedding is optional for a demonstrated single-executable need. Tag with the commit SHA and retain the image digest. Declare target architecture, environments, commands, permissions, and whether main deploys automatically.
- **Source builders:** a platform builder that detects the language from the repository root (for example Railpack) selects the Rust provider when the root has `Cargo.toml`, and then does not build the SPA. Declare the build in the builder's config file: the Rust and Node toolchains, the locked SPA build, the locked release build, and an image that contains only the binary and the SPA files. Pin the builder version where the platform allows it. A builder can give every service variable to every build command, including the build scripts of dependencies; record this with its compensating controls. A builder can also read crate manifests with its own parser: Railpack 0.40.1 cannot decode inherited package fields such as `edition.workspace = true` in the binary crate and then plans a failing build. Check the builder's plan after each manifest change, and repeat the needed package fields in the binary crate when the builder requires it.
- **Asset routing:** keep API failures out of the SPA fallback. Test deep links, missing assets, MIME types, cache headers, and relevant security headers. Separate static hosting MAY preserve one origin through routing; CORS is not an automatic requirement.
- **Configuration:** validate settings at startup, with safe diagnostics on failure. Secrets come from the declared secret mechanism, not the image or frontend bundle.
- **Migrations:** run the declared SQLx migration step before routing traffic to code that needs it. Maintain compatibility with still-running versions. Perform destructive contract steps only under approved policy after compatibility is no longer required.
- **Observe:** expose a safe version identifier, liveness, and dependency-aware readiness. Verify expected deployed revision/digest, health, and one critical journey. Define observation duration, diagnostic source, latency/error/resource signals, and readiness behaviour during dependency failure.
- **Time zone data:** confirm the Jiff timezone data source and update behaviour.
- **Recover:** keep a known working image and use a declared rollback or roll-forward command. Confirm schema compatibility before rolling back code. Test data restoration; a backup file or a down migration alone is not recovery evidence.
- **Data:** document each store's purpose, retention, deletion policy, credentials, and access boundary. Cache removal is not recovery for durable data.
- **Maintenance:** apply `DOCTRINE.md` P7. Record each infrastructure component, for example a database, cache, message broker, object storage, or container base image, with its version in each environment. Keep test and production on the same major version. Set pnpm `minimumReleaseAge` (for example 1440 minutes). Exclude a version from it only for a security fix. Cargo has the same minimum release age of 1440 minutes (ADR 0006): a crate release that is younger is not due in the dependency freshness check (§3), except a security fix. Review toolchain and dependency upgrades with the same evidence rules. Utoipa and Hey API need integration checks, not mandatory simultaneous upgrades. Recheck session adapters and pre-1.0 library compatibility when affected.
