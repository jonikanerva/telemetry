# STACK.md — Rust / Axum backend + TypeScript / React / Vite frontend profile

> Web application with a Rust backend and a strict TypeScript SPA. Axum,
> Tokio, and Tower provide HTTP and concurrency. Utoipa exports OpenAPI 3.1;
> Hey API generates a TypeScript client and Zod validators. React, Vite,
> TanStack Router, and TanStack Query provide the browser application. Jiff
> represents time. PostgreSQL with SQLx is the default when durable data is
> needed. One immutable image contains the server and built SPA files.

**Adoption status (26 November 2026):** copied from agent-setup `05e0480`,
profile `STACK-RUST.md`. Sections 0, 1, 3, 5, 6, 13, and 15 contain
project facts. The first application PR (#9) adds the Rust workspace and
the ingest process skeleton. The other sections are the profile baseline
and become binding when an application PR implements them.
§13 lists the open gaps.

Use with `DOCTRINE.md` (P1–P9) and the selected host contract. This is an
example profile, not evidence that a project has implemented its checks.
MUST, MUST NOT, SHOULD, and MAY have the meanings in the doctrine. On adoption,
confirm scope, pin a compatible version set, implement the five commands,
and complete §14–15. Record exclusions and enforcement gaps. This profile
is self-contained and does not require another stack profile to be copied.
It does not grant authority beyond the adopted project contract.

---

## 0. Project shape

- **Project risk rationale:** one person, the owner, depends on the system. The data contains the vehicle location history and the VIN; a leak shows where the owner is and when the owner is away. A collection failure loses history that cannot be collected again. Usage above the Tesla billing limit removes the vehicle telemetry configuration and suspends the Tesla API for the owner's Home Assistant. No other system depends on this product. The product sends no commands to the vehicle.
- **Change risk:** assess each changed surface against that rationale. A risk label does not waive required evidence or review. Changes to the ingest service, the vehicle telemetry configuration, access control, and stored location data are material.
- **Phase scope (owner, 26 November 2026):** the first phase builds the ingest path and the raw storage. The web application and the derived periods (drives, charges, parked) come later. Until the first web PR, these parts of the profile do not apply, because the phase has no browser code: §1 Frontend, §2.1, §2.4, §2.5, the web rows of §4, §11, and the pnpm, TypeScript, Oxlint, Vitest, and Playwright parts of §3, §6, and §14. They become binding with the first web PR.
- **Shape:** a monorepo. The first phase deploys one Railway service, `telemetry`. Its image runs Tesla's `fleet-telemetry` reference server (Go, upstream binary, not rewritten) and the Rust ingest process, which supervises the server and writes the records to PostgreSQL (`docs/adr/0001-ingestion-architecture.md`). The web application comes later as a second service: one Rust service and one browser SPA, served from one origin.
- **Reason for Rust:** the owner selected Rust for the application server. The product has domain rules (units, periods, energy calculations) and a long-running ingestion path where strict types and explicit failure handling earn their cost.
- **Critical execution paths:** server request decode, domain conversion, use case, and response; browser route, query/client boundary, view model, and render. Blocking work must not stall async I/O or the browser main thread.
- **Applicable UI states:** initial loading, success, empty, stale usable data with refresh failure, unavailable data, offline, denied access, and product-specific states.
- **Intended use:** interactive web applications. Durable data, accounts, background jobs, and real-time connections depend on `VISION.md`; they are not mandatory product features.
- **Reassessment:** SEO-first, SSR-required, or offline-first products need an adapted design. A small integration or CRUD product may cost less to maintain as one TypeScript system; record the product reason for Rust.

### Repository layout and boundaries

Two crates SHOULD be the starting point when the product has domain rules.
A pass-through service without such rules MAY use one crate. The ingest
process is such a service: it stores the source values without
interpretation, so `crates/ingest` is one crate. Do not invent
concepts to populate a layer. Further splits need a current ownership,
build-time, or maintenance benefit.

```text
repo/
  ingest/                # Image, entrypoint, server config, railway.json of the ingest service (ADR 0001; now at the repo root)
  crates/
    ingest/src/          # Ingest process: gate, supervisor, ZMQ reader, PostgreSQL writer (ADR 0001)
    ingest/proto/        # Vendored upstream .proto files of the deployed fleet-telemetry tag
    domain/src/          # Types, invariants, and rules (web phase)
    server/src/          # HTTP, use cases, adapters, configuration, lifecycle (web phase)
  packages/api-client/   # Generated SDK/validators and a small handwritten facade
  web/src/               # Routes, features, components, view models
  contract/openapi.json  # Generated and committed
  migrations/            # SQLx migrations; raw storage model in ADR 0002
  db/roles.sql           # Roles, schema, and privileges of ADR 0002; run once by the owner role
  scripts/               # sqlx-metadata.sh: the SQLx metadata check against a test database
  .sqlx/                 # Committed query metadata of the SQLx macros
  e2e/                   # Browser journeys against the built service
  Cargo.toml
  Cargo.lock
  rust-toolchain.toml
  clippy.toml
  deny.toml
  mise.toml
  pnpm-workspace.yaml
  pnpm-lock.yaml
  STACK.md  VISION.md  DOCTRINE.md  CLAUDE.md / AGENTS.md
```

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

### Backend

- **Language baseline:** Rust 1.100, edition 2024 (pinned 1.100.0, §15). Confirm the stable release and dependency MSRVs on adoption; pin the exact supported toolchain in `rust-toolchain.toml`, including rustfmt and Clippy.
- **Minimum Rust version:** declare `rust-version` in workspace package metadata and inherit it in each crate. It must cover selected dependency MSRVs; lowering an adopted floor needs owner approval.
- **Runtime:** native Linux binary in the declared container target. A local macOS build does not validate the Linux artifact or its CPU architecture.
- **Package manager:** Cargo with workspace dependency inheritance. Commit `Cargo.lock`; use `--locked` on Cargo operations that support it and resolve/build/test dependencies.
- **Strictness:** inherit workspace lints in every first-party crate. Run Clippy with `-D warnings`. The unsafe restriction applies to first-party code, not a claim about dependency internals.

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
unused_must_use = "deny"
unfulfilled_lint_expectations = "deny"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
dbg_macro = "deny"
print_stdout = "deny"
print_stderr = "deny"
allow_attributes = "deny"
allow_attributes_without_reason = "deny"
let_underscore_must_use = "deny"
```

```toml
# clippy.toml
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-print-in-tests = true
```

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

### Provisioning

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
5. Enable runtime validation explicitly in the executed client path. Test rejection of an invalid success response and separately test error-body validation. A generated unused schema is not protection.
6. Test real responses from changed operations against the declared contract: status, content type, body, and failure paths. Keep expected outcomes traced to requirements, not only to the same generator.
7. Define handling of optional/null fields, unknown fields and enum values, tagged unions, numeric bounds, and temporal precision. Do not silently coerce or invent facts.
8. Preserve a declared client/server compatibility window. An old browser tab remains an old client after a combined deployment. Define safe upgrade behaviour that preserves unsaved input.

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
Scope query keys/data to authorization context. Remove or isolate cached data
on logout and tenant changes. Forward cancellation signals where supported;
client cancellation does not prove server-side rollback. Avoid copying query
data into another global store. Use typed view states rather than unrelated
optional flags. A failed fetch MUST NOT become an empty successful result.

### 2.5 Authentication, when accounts are required

Use an approved OIDC provider and opaque backend-owned cookie sessions.
Evaluate openidconnect and a maintained session implementation with a
compatible storage/version tuple. Backend authorization applies per resource
and tenant, regardless of browser visibility or cached claims.

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

---

## 3. Build & verify commands

Mise tasks are the single source of truth. Implement them in the adopting
project; this profile ships no application scripts. Commits, agent workflows,
and any CI invoke these entry points rather than separate tool pipelines.

| Variable      | Command           |
| ------------- | ----------------- |
| `$FORMAT_CMD` | `mise run format` |
| `$LINT_CMD`   | `mise run lint`   |
| `$BUILD_CMD`  | `mise run build`  |
| `$TEST_CMD`   | `mise run test`   |
| `$VERIFY_CMD` | `mise run verify` |

**Project commands.** `mise.toml` defines the tasks and pins the auxiliary
tools. In a fresh checkout, install mise and rustup, then run `mise trust`
and `mise run setup`. mise reads the Rust version from `rust-toolchain.toml`
(setting `idiomatic_version_file_enable_tools`), so a global mise `rust`
entry cannot replace it.

| Task     | Content                                                                                                                              |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `setup`  | Install the pinned tools of `mise.toml` (not the global mise tools) and the Rust toolchain, fetch the locked crates. The database tests need `TEST_DATABASE_URL` (below). |
| `format` | `cargo fmt --all`                                                                                                                    |
| `lint`   | `cargo clippy --workspace --all-targets --locked -- -D warnings` for the host. In CI, the host is linux/amd64.                      |
| `build`  | `cargo build --workspace --release --locked`                                                                                         |
| `test`   | Fails at once when `TEST_DATABASE_URL` is not set. Then `cargo nextest run --workspace --locked` (all tests, also the database tests) and `cargo test --doc --workspace --locked` (task `doctest`) |
| `test-without-database` | For CI only. `cargo nextest run --workspace --locked -E 'not binary_id(ingest::storage)'`: every test except the database tests (the test binary `storage`), and `doctest`. |
| `verify` | Fails at once when `TEST_DATABASE_URL` is not set. Then the static checks (task `checks`): format check without edits, `lint`, `cargo deny --locked check`, `cargo shear`, Gitleaks, the JSON syntax of `config/`, the shell syntax of `entrypoint.sh` and `scripts/sqlx-metadata.sh`, no tracked `.env*`, `secrets/`, or `.bin/` path. Then `sqlx-check`, `build`, and `test`. Stops at the first failure. |
| `ci`     | The task of the GitHub Actions workflow (below): `checks`, `build`, and `test-without-database`. No `TEST_DATABASE_URL` and no `sqlx-check`. Stops at the first failure. `ci` does not replace `verify`. |
| `sqlx-check` | `scripts/sqlx-metadata.sh check`: `cargo sqlx prepare --workspace --check` against a new database on the Neon branch `test`, migrated with `migrations/`, then dropped. |
| `sqlx-prepare` | `scripts/sqlx-metadata.sh prepare`: writes `.sqlx/` the same way after a change of a query or a migration. |
| `deps`   | The dependency freshness check below: the crates, the mise tools, the Rust toolchain, the `FROM` images of the `Dockerfile` (newest `tesla/fleet-telemetry` release tag, current Debian stable release), and the CI pins of `.github/workflows/ci.yml` (actions, mise version, runner label). |
| `vuln`   | `cargo deny --locked check advisories`, `osv-scanner scan source -r .`, and `osv-scanner scan image --archive` for each `FROM` image of the `Dockerfile`. crane downloads each image for `linux/amd64` as a Docker archive, with `DOCKER_CONFIG` set to a temporary directory that holds an empty `config.json`, so the pulls are anonymous and no local credential file is read. No container runtime is needed. Each image scan reads the reviewed ignore file `docs/security/osv-scanner-<repository, / as ->.toml` (`--config`); an image without a file fails. First checks the premises of the ignore files. Dockerfile: a strict allowlist; after removal of empty lines and `#` lines, the lines must equal the expected lines of this project, and only the two image tags may change (printable ASCII only, no parser directive); the expected lines copy only `/fleet-telemetry` from the upstream image and the config only from `config/server_config.json`. `entrypoint.sh`: the server starts with `-config /etc/fleet-telemetry/config.json`, and no other `-config` exists. `config/server_config.json`: exactly one JSON value, the same leaf paths in the `jq --stream` token stream and in the parsed value (no repeated key, also no repeated object), lowercase keys in the root, `monitoring`, and `tls` objects, dispatchers `logger` or `zmq` only, no extra client CA, no status port or profiler, metrics on loopback. Runs every scan; fails when a premise breaks, a scan fails, a scan finds a vulnerability that no entry covers, or the task did not scan the two images of the allowlist. A Dockerfile change needs an allowlist change in the same PR. |

Pinned tools: cargo-nextest 0.9.152, cargo-deny 0.22.1, cargo-shear 1.14.1,
Gitleaks 8.30.1, osv-scanner 2.7.1, SQLx CLI 0.9.1 (features `postgres` and
`rustls`, built from source, both programs `sqlx` and `cargo-sqlx`), jq
1.8.2, and crane 0.22.1.

**Database tests and the SQLx metadata check.** Both need
`TEST_DATABASE_URL`, the URL of the role `neondb_owner` on the Neon branch
`test` (§5). Export it before the command, for example
`set -a; . ./.env.test; set +a; mise run verify`, with the value in
quotes in the file (a Neon URL has `?` and `&`); mise does not read the
file. The tests and the script refuse the production endpoint, and the
tests refuse a database with a schema `raw`. Every connection uses
`sslmode=verify-full`, whatever the URL says. The query macros read
`.sqlx/` (`SQLX_OFFLINE=true` in `.cargo/config.toml`), so `build` and
`lint` never connect to a database. Every `sqlx` call uses `--no-dotenv`,
so no `.env` file of a parent directory is read.

Scope and gaps of `vuln`:

- The image scan covers the two `FROM` images, not the image that Railway
  builds. Only `/fleet-telemetry` comes from `tesla/fleet-telemetry`, so
  the Debian 12 package findings of that image are not in production. The
  packages that `apt-get` installs or upgrades during the Railway build are
  not scanned (§13).
- `linux/amd64` is an assumption about the Railway host. It is not
  verified.
- The Rust toolchain and the mise tools have no scanner (§13). The static
  C libraries inside `/fleet-telemetry` (libzmq with its tweetnacl copy,
  librdkafka with its bundled libraries) have no scanner either (§13).
- The triage `docs/security/image-advisories.md` records a decision for
  each image advisory. The ignore files contain only advisories with a
  recorded decision, each with a reason and an expiry date. A new
  advisory, an expired entry, or a broken premise fails `vuln`. Then the
  lead triages the finding in a `chore/deps-<topic>` PR. A "needs action"
  finding gets no entry.
- `vuln` does not read the CISA KEV catalog. At each vulnerability check,
  compare the CVE IDs of the ignored advisories with the catalog.
- `verify` does not include `vuln`. Reassessed with issue #15: with the
  ignore files, `vuln` passes, but new gating image advisories arrived on
  9 days between 30 October and 25 November 2026 (evidence in
  `docs/security/image-advisories.md`). In `verify`, each would block every
  open change until a reviewed triage merges. `vuln` therefore stays a
  separate required check at the points of DOCTRINE.md P7. `verify` keeps
  `cargo deny --locked check`, which covers the advisories of the Rust
  dependencies. Reassess when a scheduled CI run can execute `vuln`, or
  when the gating advisory rate falls below one each month over 60 days.
- The CI workflow (below) runs `vuln` in a separate job for each pull
  request and each push to `main`; no scheduled run exists yet. The job
  does not change the points of DOCTRINE.md P7 above: the lead still runs
  `mise run vuln` at these points. The result of the job:
  - A green `vuln` job is a merge condition of a PR that changes an input
    of the job: `Cargo.lock`, each `Cargo.toml`, `deny.toml`, the `vuln`
    task or the scanner pins in `[tools]` of `mise.toml` (cargo-deny,
    osv-scanner, crane, jq), the `vuln` job of `.github/workflows/ci.yml`,
    any lockfile that `osv-scanner scan source -r .` reads, any
    `osv-scanner.toml` (in the root or next to a lockfile), the
    `Dockerfile`, `entrypoint.sh`, `config/`, or the ignore files in
    `docs/security/`.
  - For any other PR, a red `vuln` job of any cause (a new advisory, an
    expired ignore entry, a broken premise, or a failed image download)
    starts a triage in a `chore/deps-<topic>` PR. It does not block that
    other PR. Before that other PR merges with a red `vuln` job, the lead
    compares the IDs of the new findings with the CISA KEV catalog. The
    comparison is manual, because `vuln` does not read the catalog (above).
  - When a finding is a known exploited vulnerability (for example in the
    CISA KEV catalog) or a malicious package version, DOCTRINE.md P7
    applies: its fix is due before other work.

Scope and gaps of `verify`:

- Gitleaks scans the committed history (`gitleaks git --redact`). It does
  not read uncommitted or ignored files, so it never reads `.env` or
  `secrets/`. Commit the change before the final `verify`.
- `cargo deny` reads the RustSec advisory database from the network.
- `verify` does not build or test the ingest image. §13 records this gap.
- `verify` runs on the development machine (macOS arm64). The Linux evidence comes from the CI workflow below, not from `verify`.
- No web code exists yet. Prettier, Oxlint, `tsc`, Vitest, Playwright, and
  `pnpm audit` join `verify` with the first web change.
- The database tests run on the Neon branch `test` (PostgreSQL 18.7),
  from this machine with a round-trip time of about 240 ms. A full run of
  the 36 database tests takes about 2.5 minutes, at most 8 at the same
  time. The ignored test `measure_the_size_of_a_signal_row` writes
  100,000 rows; it runs only on request (ADR 0002, "Expected size").
- A database test needs the network and the Neon compute. When Neon is
  down or the CU-hours of the project are used up, `verify` fails; that
  is a gap, not a pass.

**CI (GitHub Actions).** `.github/workflows/ci.yml` has two jobs on the
GitHub-hosted runner `ubuntu-26.04` (x86_64, linux/amd64, the production
platform of §1). The job `ci` runs `mise run setup` and `mise run ci`. The
job `vuln` runs `mise run setup` and `mise run vuln` (see the scope of
`vuln` above). The workflow runs for each pull request and for each push
to `main`.

- Each PR has one concurrency group: a new push to the PR cancels the old
  run of that PR. Each push to `main` has a group of its own (`github.sha`
  of the tip commit), so a later push cancels neither its running nor its
  pending run. There is one run for each push to `main`, for the tip
  commit of that push. With merge commits only, that is one run for each
  merge.
- The runner installs the tools of `mise.toml` and the Rust toolchain of
  `rust-toolchain.toml` through mise, so CI and the development machine
  use the same versions. The mise version is pinned in the workflow.
  §15 records the CI pins, and `mise run deps` compares them with their
  newest releases.
- The run compiles, links, and runs the tests on Linux. It is the Linux
  evidence of §13. A `pull_request` run tests the merge of the PR head with
  the base branch, not the head alone.
- The token of the workflow can only read the repository contents. The
  workflow uses no repository secret, no database, and no
  `pull_request_target` trigger. A pull request from a fork therefore runs
  without a secret and without write access. The checkout stores no
  credential. Each third-party action is pinned to a full commit SHA.
- The checkout reads the full history (`fetch-depth: 0`), because Gitleaks
  scans all commits.
- The tests that need a database run only locally, in `verify`. They need
  the URL of the test database, and CI would need it as a repository
  secret. A repository secret is an owner setting (issue #20, "Out of
  scope"). `test-without-database` excludes them with the nextest filter
  `not binary_id(ingest::storage)`; the SQLx metadata check (`sqlx-check`)
  also needs the database and is not in `ci`. The storage of PR #19 therefore has a Linux gap until the deployment checks: CI compiles and links `aws-lc-sys` (C, `cc` build) and SQLx on linux/amd64 and runs the storage unit tests that need no database (failure classes, rows, chunks, connect options, deadlines), but the 36 database tests and the SQLx and TLS runtime path (a rustls connection with `sslmode=verify-full` to Neon, the migrations, the writer, the role script) run only on macOS.
- Local verification stays mandatory (DOCTRINE.md P6): `mise run verify`
  runs before merge on the version to be merged, also when CI is green.
  A green `ci` job of the PR is also required before merge, because it is
  the Linux evidence. It does not replace `verify`. The green run must be
  for the current head of the PR. A `pull_request` run tests the merge
  with the base at the time of the run, and it does not start again when
  `main` moves. When the base changes after the run, update the PR branch,
  so that a new run tests the merge with the current base.
- The workflow does not change the repository settings. Branch protection
  is an owner decision, and this workflow does not add it. A red run
  therefore does not block a merge technically; the acceptance conditions
  of CLAUDE.md block it.

- **format:** rustfmt and Prettier in write mode.
- **lint:** `cargo clippy --workspace --all-targets --locked -- -D warnings`, type-aware Oxlint, and TypeScript 7 `tsc --noEmit` for web, client, and test code. Declare supported feature combinations; do not blindly enable incompatible features.
- **build:** export/generate in the declared order, build the SPA, and build the release server with locked dependencies and offline SQLx metadata. Verify the intended Linux image before claiming release readiness.
- **test:** nextest workspace tests, `cargo test --doc --workspace --locked`, Vitest, required real-database integration tests, and Playwright against the built service. Required browser tests are part of the complete test command, not an optional omitted suffix.
- **verify:** format check without edits; isolated service setup and migrations where needed; lint/type checks; complete-tree contract drift check; SQLx metadata check; security/dependency checks; build; all required tests. Stop on required failures, retain useful diagnostics, and clean up only owned temporary resources.

`cargo sqlx prepare --workspace --check` MUST use an isolated migrated
database with the relevant targets/features. The release compilation uses
`SQLX_OFFLINE=true`; offline compilation does not replace the schema check.
Initial query/migration work may generate metadata against the isolated database
before verification. Checks never silently regenerate missing expected evidence.

Use `cargo deny check`, `pnpm audit`, and Gitleaks with the policies in §14.
Cargo-shear SHOULD check unused Rust dependencies. Quick tasks MAY exist but
do not replace full verification. A missing required browser, database, or
scanner is a gap, not a pass. Validate test connection configuration and
isolation before creating, migrating, or dropping any database.

**Dependency freshness check:** `cargo update --dry-run --verbose`, `pnpm outdated --recursive`, and `mise outdated --bump`. `cargo update` stays within SemVer-compatible versions, so also compare each direct crate requirement with its latest crates.io release. In this project, `cargo update --dry-run --verbose` (Cargo 1.100) also lists each locked crate that is behind its latest crates.io release across major versions, as `Unchanged <crate> <version> (available: <latest>)`. Checked on 26 November 2026 with a probe crate that requires `thiserror = "1"`: the output was `Unchanged thiserror v1.0.69 (available: v2.0.21)`. Compare `rust-toolchain.toml` with the latest stable Rust release. Compare each infrastructure version in §15, for example the database server, the container base image, and the source builder, with its latest stable release.

In this project, `mise run deps` also reads the CI pins of `.github/workflows/ci.yml` (§15) and prints each pin with the newest release tag: each action (and it fails when the pinned SHA is not the commit of the tag in the comment), the mise version, and the runner label with the newest Ubuntu runner image that has a tag in `actions/runner-images`. It reads the tags with `git ls-remote` and fails when it cannot read a tag list. Before it reads the tags, it checks the workflow against a strict allowlist, like the Dockerfile check of `vuln`. (1) `.github/` holds only `workflows/` and `pull_request_template.md`, and `.github/workflows/` holds only `ci.yml`; any other entry fails with its name, for example a second workflow file (`.yml` or `.yaml`) or a composite action in `.github/actions/`, and a PR that adds such an entry extends the check in the same PR. (2) In `ci.yml`, each line with a `uses`, `version`, or `runs-on` key (also quoted, also without `- `) must be `- uses: <owner>/<repo>@<40 lowercase hex> # vX.Y.Z` (a full release tag), `version: X.Y.Z` directly under the `with:` of each mise action step, or `runs-on: ubuntu-NN.04`; each line with a `container`, `services`, or `image` key fails, because no container image is allowed yet; each job has one `runs-on`; YAML anchors and aliases are not allowed. Any other line fails the task with its line number, and the check changes together with the workflow. Limits: the check searches lines and does not parse YAML, so a key that a line search cannot see (a key written with an escape sequence, an explicit `? key`, or an anchor after a quoted `#`) is not checked; it does not read what a pinned action runs inside (other actions or downloads), the inputs of a pinned action other than the mise `version`, or the commands of `run:` steps, which can download and run code; the tools that the mise tasks run are pinned in `mise.toml` and compared by `mise outdated`. A newer runner image can be a beta: before a change of the runner label, check in the README of `actions/runner-images` that the image is generally available.

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

---

## 5. Persistence shape

- **Storage:** PostgreSQL on Neon (Free plan) through SQLx. `VISION.md` requires durable data. Read on 26 November 2026: 1 GB storage for each project, 100 CU-hours each month, scale to zero after 5 minutes (cannot be disabled), 6 hours of restore history, computes up to 2 CU, 10 branches for each project. Source: <https://neon.com/docs/introduction/plans>. Neon suspends the compute when the CU-hours are used, and rejects writes when the storage is full. Batch the writes and bound the raw signal volume so that the compute can scale to zero. The Neon project is `telemetry` (§15). The ingest process writes a batch at the latest 15 minutes after the oldest queued record and keeps no idle connection, so that the compute can scale to zero (ADR 0001). The raw tables, value representation, idempotency keys, roles, and size estimate are in `docs/adr/0002-raw-storage-model.md`.
- **Test databases:** tests that need PostgreSQL use the Neon branch `test` of the project. It has the production major version and no production data. Never reset it from its parent; that copies production data. The credentials stay outside the repository. The branch counts against the 1 GB of the project, and its compute uses the same 100 CU-hours: budget at most 20 active hours (5 CU-hours) each month. Tests close their connections when they end, so the compute scales to zero after 5 minutes. Isolation (`crates/ingest/tests/storage/support.rs`): each test works in a schema of its own, `itest_<unix seconds>_<random>`, and a test that needs roles creates roles with the same prefix, because roles are shared by every database of the branch. The SQL of the storage names no schema; the connection sets the `search_path` (production: `raw`), so the schema name is a test parameter and production code has no test branch. The harness removes the schema and the roles after the test, also after a failure, and removes objects of earlier runs that are older than 6 hours. It refuses the production endpoint `ep-young-dawn-bgyaj83l` in any part of the URL before it connects, and refuses a database that has a schema `raw`. At most 8 database tests run at the same time (`.config/nextest.toml`); a hung test stops after 4 minutes. `db/roles.sql` and the cleanup of test roles take one advisory lock, because two changes of the privileges of the database at the same time fail with SQLSTATE XX000 ("tuple concurrently updated"; seen in 4 of 4 parallel runs without the lock). One full run keeps the compute active for about 3 minutes.
- **Entities and retention:** defined by `VISION.md`; technical browser caches require the same purpose and privacy assessment.
- **Queries:** prefer SQLx checked query macros for static SQL. Commit `.sqlx` metadata and verify it against the migrated schema. Dynamic SQL needs bound parameters, explicit structural constraints, and integration tests; it does not acquire the same compile-time guarantee. In this project every statement of the storage is a query macro (`.sqlx/`, `mise run sqlx-check`), except the role script and the statements of the SQLx migrator itself. The macros know no `jiff` type, so a bind of a `jiff_sqlx::Timestamp` or of an array with NULL elements uses the override `as _`, and a time column uses the override `AS "name: jiff_sqlx::Timestamp"`. The database tests execute every statement against the migrated schema. The role script `db/roles.sql` is plain SQL that the tests and the lead run through `apply_roles`.
- **Domain conversion:** validate rows when storage types permit values the domain forbids. A query type check does not establish authorization, isolation, or meaning.
- **Constraints:** use applicable NOT NULL, CHECK, foreign keys, unique keys, and exclusion constraints. Test rejection and competing writes; read-before-write alone cannot enforce concurrent uniqueness.
- **Atomicity:** use transactions for atomic operations. Define isolation, locking, or optimistic concurrency from the invariant. Do not publish partial success.
- **Repeated requests:** define idempotency and outcome reconciliation, including a committed write followed by a lost response. Use keys and uniqueness where needed.
- **Migrations:** numbered, reviewed migrations. Run SQLx migration tooling or a subcommand using SQLx's own migration support as a separate release step, not an implicit startup effect. Use expand-and-contract compatibility with running prior releases. In this project, `ingest migrate` runs as the Railway pre-deploy command with the role `telemetry_migrator` (ADR 0002; lead decision in §15). `ingest run` only checks that its embedded migrations are applied (ADR 0002). A migration names no role and no schema: the default privileges of `db/roles.sql` give the ingest role its privileges on each new table. A new reason of `raw.unprocessed_record` needs a migration in the same release (ADR 0002).
- **Recovery:** test empty-database setup, upgrade from supported prior schemas, and restore/roll-forward paths. Application rollback does not restore deleted data. Follow the authorized policy for destructive changes.
- **Server version:** run the isolated test server on the same PostgreSQL major version as production. Read the production version on adoption and after each platform upgrade (`SHOW server_version`, read-only, or the platform's database image tag) and pin the test server to that major version. Record both versions. A migration test, query metadata check, or catalog snapshot from another major version is not evidence for production. A newer stable major version follows the `DOCTRINE.md` P7 infrastructure rule.
- **Connection TLS:** enable one SQLx TLS feature that uses the same rustls crypto provider as the rest of the binary. With reqwest `rustls`, that is `tls-rustls-aws-lc-rs` (§7). The default `sslmode=prefer` uses TLS when the server offers it, checks no certificate chain, and fails the connection on a failed handshake instead of falling back to plain text. `sslmode=require` fails without a TLS feature. SQLx also reads `PGSSLMODE` when the URL has no `sslmode`. rustls rejects a server certificate that is not version 3, although `psql` accepts it. Before the first release, check the production `sslmode`, `PGSSLMODE`, and certificate version read-only. In this project `storage::connect_options` sets `sslmode=verify-full` after it parses the URL, so neither the URL nor `PGSSLMODE` can turn the check off. SQLx 0.9.1 builds the rustls configuration with an explicit aws-lc-rs provider and the Mozilla roots of `webpki-roots`, so it does not panic and needs no system certificates. It ignores the libpq parameter `channel_binding` (Neon URLs have `channel_binding=require`) with a warning; it does not use channel binding. A test connects to the test branch by its IP address: the name check fails the handshake, and SQLx returns that error as an I/O error. Neon terminates TLS in its proxy, so `pg_stat_ssl` shows no TLS for the session.

SQLite backup needs its supported backup API, VACUUM INTO, or a proven
quiescent snapshot with a tested restore. Do not assume a live file copy is
consistent. Redis, event sourcing, repository traits, and durable queues need
a current reason; they are not baseline dependencies.

---

## 6. Approved dependencies

These are profile baseline lines, not a tested application lockfile. They
reflect the 13 October 2026 selection. On adoption, verify support, features,
licences, advisories, and compatibility; pin exact resolved versions and tools.
Do not install unused conditional packages. New dependencies need a PR rationale
for necessity, provenance, maintenance, licence, transitive cost, and replacement
cost. Significant durable choices need short ADRs in the adopting project.
Routine libraries remain delegated choices; providers, external data transfers,
added cost, and material lock-in follow the doctrine's escalation rules.

| Dependency                                     | Baseline line             | Why it earns its place                                                           | Approver  | Date       |
| ---------------------------------------------- | ------------------------- | -------------------------------------------------------------------------------- | --------- | ---------- |
| axum / tokio                                   | 0.8 / 1                   | HTTP and async runtime                                                           | (default) | (template) |
| tower / tower-http                             | 0.5 / 0.7                 | Service middleware and static-file delivery                                      | (default) | (template) |
| serde / serde_json / thiserror                 | 1 / 1 / 2                 | Wire types and typed errors                                                      | (default) | (template) |
| jiff                                           | 1                         | Semantic time types; the first workspace PR pins the exact resolved version      | Lead      | 2026-11-26 |
| utoipa / utoipa-axum                           | 6 / 0.3                   | OpenAPI routes and schemas                                                       | (default) | (template) |
| tracing / tracing-subscriber                   | 0.1 / 0.3                 | Structured backend diagnostics                                                   | (default) | (template) |
| sqlx / jiff-sqlx, conditional                  | 0.9 / 1                   | Durable data and Jiff database wrappers; TLS feature `tls-rustls-aws-lc-rs` (§5) | Lead      | 2026-11-26 |
| uuid / rust_decimal, conditional               | 1 / 1                     | Typed identities and exact decimal quantities                                    | (default) | (template) |
| reqwest, conditional                           | 0.13                      | External HTTP calls; feature `rustls` (aws-lc-rs provider)                       | (default) | (template) |
| @hey-api/openapi-ts                            | 0.99, pin exact release   | Local client and validator generation                                            | (default) | (template) |
| zod                                            | 4                         | Executed browser boundary validation                                             | (default) | (template) |
| react / react-dom / vite                       | 19 / 19 / 8               | SPA and build                                                                    | (default) | (template) |
| @tanstack/react-router / @tanstack/react-query | 1 / 5                     | Routing and server-state cache                                                   | (default) | (template) |
| typescript / oxlint / oxlint-tsgolint          | 7 / pin exact / pin exact | Native compiler and type-aware linting (§1)                                      | (default) | (template) |
| typescript, API-client generator only          | 6, pin exact              | Compiler API for the generator until it supports TypeScript 7 (§2.1)             | (default) | (template) |
| prettier / vitest                              | 3 / 5                     | Format and frontend unit tests                                                   | (default) | (template) |
| @playwright/test / @axe-core/playwright        | 1 / 4                     | Browser journeys and accessibility checks                                        | (default) | (template) |
| proptest, dev                                  | 1                         | Targeted invariant tests                                                         | (default) | (template) |
| tailwindcss, optional                          | 4                         | Styling when selected by the product                                             | (default) | (template) |

Jiff SQLx values use adapter wrapper types; configure query type overrides
or annotations deliberately and test their round trips. Utoipa's `uuid`
and decimal-related features must agree with actual serialization.

Known conflict (lead, 26 November 2026): the baseline names `jiff` 1, but
utoipa 6.0.0, the newest release, has only the feature `jiff_0_2`. The
ingest phase has no utoipa dependency, so the conflict does not affect it.
The first web PR must resolve the utoipa and `jiff` pairing with
compatibility evidence: a utoipa release with `jiff` 1 support, explicit
schema overrides for the time types, or a reviewed §13 exception. Reassess
when utoipa publishes `jiff` 1 support or when the first web PR starts.
Pin selected Cargo feature sets; avoid enabling everything by convenience.
Authentication packages and their compatible store remain conditional §2.5
adoption decisions. Snapshot, mutation, and coverage tools are optional when
they address a named risk. Their versions belong in the adopting tool pins.

### Pinned direct dependencies

`Cargo.lock` pins the resolved versions. Each version below was the latest
stable release on crates.io on 26 November 2026. Default features are off;
the listed features are the complete set. `src/main.rs` of `crates/ingest`
is the only reader of the environment and the command line.

| Crate              | Version | Features                                                     | Why it earns its place                                                   | Licence           |
| ------------------ | ------- | ------------------------------------------------------------ | ------------------------------------------------------------------------ | ----------------- |
| axum               | 0.8.9   | none                                                         | Router and handler of `GET /health`                                      | MIT               |
| hyper              | 1.13.0  | `http1`, `server`                                            | HTTP/1 connection with a header read timeout (`axum::serve` has none)    | MIT               |
| hyper-util         | 0.1.21  | `service`, `tokio`                                           | Tokio I/O and timer adapters, router-to-hyper service adapter            | MIT               |
| socket2            | 0.6.5   | `all`                                                        | TCP keep-alive with idle time, interval, and probe count on accepted gate sockets (ADR 0001, gate rule 3); `with_retries` needs `all` | MIT OR Apache-2.0 |
| tokio              | 1.54.1  | `rt-multi-thread`, `net`, `signal`, `time`, `sync`, `macros`, `io-util`, `process` | Runtime, listeners, SIGTERM and SIGINT, connection limits, deadlines, the gate copy (`io-util`), the child process (`process`) | MIT               |
| tracing            | 0.1.44  | `std`                                                        | Structured log events                                                    | MIT               |
| tracing-subscriber | 0.3.23  | `fmt`, `std`                                                 | Subscriber, level filter, and writer for the JSON formatter              | MIT               |
| serde_json         | 1.0.154 | `std`                                                        | Correct JSON encoding of each log line                                   | MIT OR Apache-2.0 |
| thiserror          | 2.0.21  | `std`                                                        | Error types whose text holds static words only                           | MIT OR Apache-2.0 |
| jiff               | 1.0.3   | `std`                                                        | `Timestamp` of the record times and of the `ingest_event` records (§10)  | Unlicense OR MIT  |
| prost              | 0.14.6  | `derive`, `std`                                              | Protobuf decode of the `fleet-telemetry` payloads (ADR 0001)             | Apache-2.0        |
| prost-types, also build | 0.14.6 | `std`                                                   | `google.protobuf.Timestamp` of the generated types; `build.rs` reads the schema descriptors with it | Apache-2.0 |
| uuid               | 1.29.3  | `std`                                                        | Typed connection ID of a `connectivity` record (ADR 0002)                | Apache-2.0 OR MIT |
| prost-build, build | 0.14.6  | none                                                         | Generates the Rust types of `crates/ingest/proto/` in `build.rs`         | Apache-2.0        |
| protox, build and dev | 0.10.0 | none                                                       | Parses the `.proto` files in Rust, so no `protoc` binary is needed; the tests read the schema with it | MIT OR Apache-2.0 |
| proptest, dev      | 1.12.0  | `std`                                                        | Property tests of the decode with arbitrary bytes                        | MIT OR Apache-2.0 |
| sqlx               | 0.9.1   | `postgres`, `runtime-tokio`, `tls-rustls-aws-lc-rs`, `migrate`, `macros`, `uuid`, `json` | PostgreSQL pool, checked query macros, embedded migrations, TLS with certificate verification (ADR 0001, ADR 0002) | MIT OR Apache-2.0 |
| jiff-sqlx          | 1.0.0   | `postgres`                                                   | `timestamptz` and `timestamptz[]` binds of `jiff::Timestamp` (§10)        | Unlicense OR MIT  |

hyper and hyper-util replace the axum features `http1` and `tokio`, which
pulled in the same two crates. `Cargo.lock` has 56 packages, as before
the change. The other baseline lines that the table above does not list
are not dependencies yet. The first change that uses one pins its exact
version here.

Protobuf decode (issue #10): `Cargo.lock` now has 118 packages. Most of
the new ones are build-only or dev-only: `prost-build` and `protox` run
only in `build.rs` and in the tests; they are not part of the binary. No
crate of the tree builds C code, and no `protoc` binary is needed
(`cargo tree -i cc` finds no package). `deny.toml` has one licence
exception: `foldhash` (Zlib), which `prost-build` needs through
`petgraph`. Two duplicate versions are new, at the warning level of
`cargo deny`: `hashbrown` 0.15 (`petgraph`) next to 0.17, and `syn` 2
(`prost-derive`) next to 3.

Gate and child supervision (issue #11): `socket2` and the tokio features
`io-util` and `process` are new; the tests also use the tokio feature
`test-util` (a paused clock). tokio already pulled `socket2` 0.6.5 with
`all` through its feature `net`, so `Cargo.lock` gets no new package; the
direct dependency keeps `all` when that transitive path changes.
`Cargo.lock` has 118 packages, as after the decode change.

Storage (issue #12): `Cargo.lock` now has 238 packages. sqlx brings the
rustls stack with the aws-lc-rs provider, the pool, and the macros.
`Cargo.lock` also lists `sqlx-mysql`, `sqlx-sqlite`, `libsqlite3-sys`, and
`ring`: optional dependencies of `sqlx-macros-core` that Cargo records but
does not build (`cargo tree -i ring --target all` and `cargo tree -i
libsqlite3-sys --target all` find no package). osv-scanner reads
`Cargo.lock`, so it can report these crates; they are not in the binary.
`cargo tree -i aws-lc-rs` shows the one provider, through rustls.

C code: `aws-lc-sys` 0.47.0 compiles the AWS-LC C library with the `cc`
crate and links it statically. On macOS (aarch64) and on Linux x86_64 and
aarch64 the crate has pre-generated bindings, so the build needs a C
compiler only: no CMake, Go, Perl, NASM, or `bindgen` (checked on macOS
with the Xcode command line tools; the build output shows the `cc` build
and `link_kind=static`). The Linux image build needs a C compiler in its
builder stage, for example `gcc` of the official Rust image; the runtime
image needs no TLS library. The CI workflow (§3) compiles and links
`aws-lc-sys` on linux/amd64, on the GitHub Actions runner. The build in the
builder stage of the image is not verified yet (§13, the image build gap).
The crate looks for an installed AWS-LC first; none is installed here.

Licences: `deny.toml` has one exception for each new crate with a licence
outside the allow list: ISC (`aws-lc-rs`, `aws-lc-sys`, `rustls-webpki`,
`untrusted`) and CDLA-Permissive-2.0 (`webpki-roots`, the Mozilla root
certificates as data). `foldhash` 0.2 (Zlib) is now part of the binary
through the statement cache of sqlx; the existing `foldhash` exception
covers it.

Retained older transitive version: matchit 0.8.4 (0.8.6 exists), because
axum 0.8.9 requires `=0.8.4`. Reassess with the next axum release.

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
- useEffect for server-data fetching, duplicate global query stores without need, or auth-scoped cache reuse across users/tenants.
- Host-local timezone arithmetic, invented instants for date-only values, or manual UTC-offset calculations (§10).

Scope tests and tooling according to their contracts. Supported platform APIs
and justified lossless operations do not need blanket bans. New frontend
state frameworks require a current reason rather than an automatic prohibition.

---

## 8. Logging & privacy

Use tracing with tracing-subscriber JSON output in production and a readable
development formatter. Carry safe request correlation through async spans.
Allowlist fields. Apply the allowlist to span fields as well as event fields,
because request correlation puts data on spans. Avoid automatic Debug output of complete arguments, DTOs,
configuration, headers, and payloads. Redact secret types and test redaction.
Create the request identifier inside the panic containment of the request: a
random identifier generator can panic when the operating system gives no
random bytes. Work that several requests share has no single request
identifier; record that gap instead of borrowing the first caller's span.

Define diagnostic access, retention, cardinality, and resource limits. Select
a browser diagnostic mechanism explicitly; the Rust logger does not cover
browser failures. No external crash reporter or telemetry exporter is enabled
by default. Assess new data flows and obtain required authority before adding
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

---

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
- **Thresholds:** exercise text contrast at 4.5:1 for normal text and 3:1 for large text; pointer targets at the WCAG 2.5.8 24 by 24 CSS pixel threshold or a documented applicable exception; visible keyboard focus on interactive elements.
- **Inputs:** full keyboard operability, sensible DOM/focus order, accessible names, and announced validation/error states.
- **Browser evidence:** Playwright journeys against the release service, axe checks on critical pages/states, and keyboard-only operation. Declare supported browsers and viewports.
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

## 13. Scoped exceptions

Do not weaken a rule or check to make a change pass. The responsible lead
resolves exceptions within project authority, with an independent reviewer.
The change author must not approve their own weaker acceptance or checks.
Escalate changes beyond authority and unresolved material risks to the owner.
Record significant durable choices in short docs/adr records in the adopting
project; do not require an ADR for every ordinary default. Backlog and history
remain in GitHub.

| Rule / scope | Reason and consequences | Compensating evidence | Responsible lead / reviewer / approval | Expiry or reassessment |
| ------------ | ----------------------- | --------------------- | -------------------------------------- | ---------------------- |
| §3, §14: vulnerability check of the built ingest image layer, the Rust toolchain, and the mise tools | The earlier exception for all freshness and vulnerability checks ends with this first application PR. `mise run vuln` now scans the crates in `Cargo.lock` and both `FROM` images of the `Dockerfile` as archives (crane and `osv-scanner scan image --archive`, no container runtime). Three surfaces stay without a scanner. (1) The image that Railway builds: it stays in the Railway registry, and no local container runtime can build it. The packages that `apt-get` adds or upgrades during the build (`ca-certificates`, `libstdc++6`, and the `apt-get upgrade` results) are therefore not scanned. (2) The Rust toolchain and the mise tools: no advisory database that osv-scanner or cargo-deny reads has a record for the Rust toolchain, and no scanner reads the installed tool binaries. (3) The C code that is linked statically into `/fleet-telemetry`: libzmq 4.3.4 (upstream Dockerfile) with its own copies of tweetnacl and SHA-1, and the librdkafka 2.3.0 bundle of `confluent-kafka-go/v2` v2.3.0 with OpenSSL 3.0.11, libcurl 7.86.0, zlib 1.2.13, zstd 1.5.2, and lz4 1.9.3. libsodium is not linked: the upstream Dockerfile builds libsodium 1.0.19, but the libzmq build uses its tweetnacl copy (no `--with-libsodium`). Cyrus SASL is not linked. At each start, in every configuration, the ELF `_init` runs the OpenSSL CPU feature detection `OPENSSL_cpuid_setup` (reads the environment variable `OPENSSL_ia32cap` and the `CPUID` instruction), and the Go package `zmq4` creates a libzmq context (allocation, process ID, one `eventfd`; no random source, no socket, no I/O thread); neither takes network input. All other code of the bundle components runs only behind the `kafka` dispatcher, which the `vuln` premise check rejects (`docs/security/image-advisories.md`, "Static C libraries"). Consequence: a known vulnerability in these surfaces is found only by the manual comparison. | In each PR that runs the vulnerability check, record: the Rust toolchain against the GitHub security advisories of `rust-lang/rust`; each mise tool against OSV (crates.io or Go package) and the GitHub security advisories of its repository; the `apt-get` packages against OSV for `Debian:13`; each static C component of `/fleet-telemetry` against OSV by release commit, or the reason why a component is not compared one by one (`docs/security/image-advisories.md`, "Static C libraries"). Compare the OpenSSL and libcurl records one by one before a change enables the `kafka` dispatcher. | Responsible lead: Claude Code (orchestrating lead). Approving independent reviewer: qa-enforcer, [PR #9](https://github.com/jonikanerva/telemetry/pull/9); surface (3): qa-enforcer, [PR #17](https://github.com/jonikanerva/telemetry/pull/17). | Reassess when the repository gets a container build check, when the built image can be downloaded, or when a scanner covers the toolchain, the tools, or the static C libraries. |
| §3: reviewed ignore files of the image scan | `osv-scanner scan image` reports advisories that do not affect this deployment, or that have no fix. The ignore files `docs/security/osv-scanner-tesla-fleet-telemetry.toml` (33 Go groups and the Debian 12 packages of the upstream image) and `docs/security/osv-scanner-debian.toml` (69 Debian 13 groups) hide them, so that `vuln` fails only on new or undecided findings. Consequence: an ignored advisory that later gets a Debian fix or a KEV listing is not shown until its entry expires. | Each entry names its decision in `docs/security/image-advisories.md` and has an expiry date. `vuln` checks the premises of the entries before the scan (the Dockerfile against a strict allowlist; the `-config` path of `entrypoint.sh`; config key case, exactly one JSON value, no repeated key or object, dispatchers, client CAs, listeners) and fails when it did not scan the two images of the allowlist. A build that runs the `apt-get` step installs the Debian fixes of the build date. A cached layer of that step skips the upgrade, and the Railway cache behaviour is not verified; the post-release check of §15 reads the installed package versions from the build log and detects a skipped step. The KEV comparison stays manual. | Responsible lead: Claude Code (orchestrating lead). Approving independent reviewer: qa-enforcer, [PR #17](https://github.com/jonikanerva/telemetry/pull/17). | Go entries expire on 2027-02-24, Debian 13 entries on 2027-01-25. Reassess earlier with a new upstream release, a base image change, a broken premise, or a KEV listing. |
| §8: readable development formatter | The process has only the JSON formatter. One formatter gives one redaction path: the field allowlist exists once, and a second formatter cannot leak a field that the JSON formatter redacts. Nothing needs readable logs yet. Consequence: developers read the logs with `jq`. | The JSON formatter and its redaction tests in `crates/ingest/src/logging.rs`. | Responsible lead: Claude Code (orchestrating lead). Approving independent reviewer: qa-enforcer, [PR #9](https://github.com/jonikanerva/telemetry/pull/9). | Reassess when a developer workflow needs readable logs, and with the first web PR. |
| §1, §14: runtime tests on linux/amd64 before merge | The development machine is macOS arm64. The CI workflow (§3) runs the tests that need no database on linux/amd64, but not in the container: `ingest` is not PID 1 there, no Railway proxy is in front of the gate, and the tests that need a database do not run there. The ingest process has platform-dependent code: TCP keep-alive options, PID 1 signal handling and child reaping, and the child process. Consequence: a Linux-only runtime fault that shows only in the container, as PID 1, behind the Railway proxy, or in a database test shows only after deployment. The storage of PR #19 therefore has a Linux gap until the deployment checks: CI compiles and links `aws-lc-sys` (C, `cc` build) and SQLx on linux/amd64 and runs the storage unit tests that need no database (failure classes, rows, chunks, connect options, deadlines), but the 36 database tests and the SQLx and TLS runtime path (a rustls connection with `sslmode=verify-full` to Neon, the migrations, the writer, the role script) run only on macOS. Scope: until the first deployment that starts `ingest`. Until then the image does not contain the binary, the gate stays closed, and `/health` answers 503. | Before merge: a green run of `mise run ci` on linux/amd64 for the PR (§3): Clippy with `-D warnings`, the locked release build, and the nextest suite and the doctests without the tests that need a database. Check 1 below is now produced by CI for each PR, for the tests that need no database; it replaces the earlier Clippy step for `x86_64-unknown-linux-gnu` in `mise run lint`. The tests that need a database run only locally, on macOS (§3). Required evidence of the deployment PR, before the first deployment that starts `ingest` (also in ADR 0001, "To verify at first deployment"): (1) `cargo nextest run --workspace --locked` passes on linux/amd64: CI produces this for the tests that need no database, and the deployment PR links the green CI run of its head. (2) `ingest` is PID 1. On a redeploy, the log of the old deployment shows `shutdown_started` with reason `SIGTERM`, `gate_stopped`, `child_killed` with `exit_signal` 9, the `stopped` event, and `shutdown_complete` within `drainingSeconds`, and no `task_stop_timeout`. (3) After at least three child exits and restarts, the process table of the container has no zombie (`ps` state `Z`). (4) After a forced end of PID 1 (a watchdog exit or SIGKILL of `ingest`), the container ends, Railway restarts it, a new `started` event follows, and no `fleet-telemetry` process survives without `ingest`. (5) Every `gate_connection_closed` event of a normal connection has `socket_options` `set`, `keepalive` `15/15/9`, and `nodelay` `true` (none has `failed`), and the idle dead-peer case closes in about 150 s; if the Railway proxy terminates TCP, `ss -tio` in the container shows the keep-alive timers, or check 1 covers them. (6) `fleet-telemetry` starts with an empty environment, and `/proc/<child pid>/fd` has no listener of `ingest`. | Lead: Claude Code. Approving independent reviewer: qa-enforcer, [PR #18](https://github.com/jonikanerva/telemetry/pull/18); CI as the source of check 1: qa-enforcer, [PR #21](https://github.com/jonikanerva/telemetry/pull/21). | Ends when the deployment PR records checks 1 to 6. Reassess when the repository gets a container build check, or when CI can run the tests that need a database. |
| §14: build and test of the ingest image before merge | No local container runtime is available to the lead. Railway builds the image after merge. | Post-release check: the service starts, the TLS certificate verifies, and the vehicle reconnects. | Lead: Claude Code. Approval: owner, with the adoption PR. | Reassess when the repository gets a container build check. |
| §15: one image for server and SPA | The ingest service is a second image, because it is Tesla's upstream server. | The application still ships as one image. | Lead: Claude Code. Approval: owner, with the adoption PR. | Permanent while the upstream server is used. |

---

## 14. Applicability and evidence

Complete local environments, command wiring, scanner policies, and existing
required CI checks on adoption. Run required tests and the full `$VERIFY_CMD`
locally on the version to be merged against the current integration base.
Missing or failed mandatory local evidence blocks merge. Existing required
CI must also pass; CI itself is optional under P6. Never bypass existing gates.

In this project, the local `mise run verify` stays mandatory before merge.
The GitHub Actions workflow of §3 adds the Linux evidence: a green `ci` job
(`mise run ci` on linux/amd64) for the current head of the PR is required
before merge (§13, §3). The CI
run does not replace the local run, and the local run does not replace the
CI run. The tests that need a database run only in the local `verify`,
because CI has no database secret (§3). The CI result is an acceptance
condition of this contract, not a branch protection rule; branch
protection is an owner decision.

Assign each check to before merge or after release. Missing pre-merge evidence
blocks merge; missing post-release evidence prevents claiming a successful
release. A future production deployment is not required to approve its PR.
For material changes, the independent reviewer MUST run full verification and
required tests on the PR head integrated with the current base in an isolated
checkout. Record head, base, and tested integration commit/tree. Missing or
failed independent execution is FAIL, as P9 requires. This adds no mandatory
review to low-impact changes that do not otherwise require one.

Retain commands, safe fixtures or reconstruction steps, expected outcomes and
their original sources, versions, environments, and results. Challenge material
assumptions and transformations beyond supplied examples. A test of a generated
assumption is not proof of that assumption. Regression tests should detect the
original failure. Investigate flakes rather than treating a rerun as an
explanation. Apply P6 evidence reuse without waiving P9's independent run.

| Doctrine / applicability      | Required evidence                                                                                                                       | Environment / gap                                                                                 |
| ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| P1, P6: each task             | Criteria, failures, assumptions, and check mapping                                                                                      | Lead records; independent review for material changes                                             |
| P2–P4: types and dependencies | Strict checks, dependency rationale, crate/import boundaries                                                                            | Manifest edits and ambient effects need review                                                    |
| P3, P5: HTTP contract         | Complete-tree drift check; real response tests; executed success/error validation                                                       | Annotations do not prove handler behaviour                                                        |
| P5–P6: data meaning           | Conversion and property/challenge cases; partial versus atomic failure tests                                                            | Preserve missing/null/zero, units, precision, and uncertainty                                     |
| P5–P7: storage, when used     | Migrated-schema metadata check; constraints, competing writes, upgrade, and restore tests; recorded production and test server versions | Isolated PostgreSQL of the production major version required; offline build alone is insufficient. Here: `mise run sqlx-check` and `crates/ingest/tests/storage/` on the Neon branch `test` (PostgreSQL 18.7); production version not read yet (§15). These tests run on macOS only; CI on linux/amd64 builds the storage and runs its tests without a database, so the database tests and the SQLx and TLS runtime path are not exercised on Linux before the deployment checks (§13). Restore is Neon's 6-hour history (ADR 0002), not tested. |
| P3, P7: security              | Named scanner results and auth/privacy checks                                                                                           | Resolve session tuple if accounts are used; scanners do not prove authorization                   |
| P5–P7: browser                | Critical Playwright journeys, degraded/error states, keyboard and axe                                                                   | Record unsupported browsers and manual accessibility gaps                                         |
| P7: release                   | Before merge: target build and recovery readiness; after release: identity, health, and smoke evidence                                  | Local build is not a verified deployment                                                          |
| P6, P7: production platform   | Green `ci` job (`mise run ci` on linux/amd64, GitHub Actions, §3) for the PR, together with the local `mise run verify`                  | CI runs no database test and no container (§13); it is not branch protection                     |
| P8–P9: material changes       | Current docs, significant decisions, independent integrated verification                                                                | Separate reviewer context; exact tested revisions                                                 |

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
Do not accept snapshots without reviewing their expected meaning.

### Adoption trial

Before calling the application profile validated, retain evidence for:

1. A browser request through the generated client, Rust conversion, and a real domain rule.
2. SQLx persistence where applicable, including data constraints and query checks.
3. Invalid input rejected through the public problem contract.
4. A deliberately invalid success response and an invalid error body rejected safely by the client.
5. Concurrent changes, repeated writes, cancellation, and shutdown behaviour relevant to the product.
6. Migration from a supported prior schema and a tested recovery path where persistence applies.
7. Full verification from a clean checkout with isolated services and pinned tools.
8. The actual deployment-target artifact and a critical browser smoke journey.

An item may be not applicable with a reason tied to product purpose. An
unsupported tool is not an applicability exemption. Identify what static
checks, the trial, and agent reviews still do not prove.

### Live checks of external sources

A live check against an external source asserts on a stable control sample.
When the source can legitimately lack data for recent items, report the hit
rate of such a sample as a measurement, not as a pass condition. Never run a
live check with a user's credential unless the owner runs it.

---

## 15. Release, recovery, and maintenance

- **Release:** one immutable image contains the Rust binary and built SPA files. ServeDir is the default asset mechanism; embedding is optional for a demonstrated single-executable need. Tag with the commit SHA and retain the image digest. Declare target architecture, environments, commands, permissions, and whether main deploys automatically.
- **Source builders:** a platform builder that detects the language from the repository root (for example Railpack) selects the Rust provider when the root has `Cargo.toml`, and then does not build the SPA. Declare the build in the builder's config file: the Rust and Node toolchains, the locked SPA build, the locked release build, and an image that contains only the binary and the SPA files. Pin the builder version where the platform allows it. A builder can give every service variable to every build command, including the build scripts of dependencies; record this with its compensating controls. A builder can also read crate manifests with its own parser: Railpack 0.40.1 cannot decode inherited package fields such as `edition.workspace = true` in the binary crate and then plans a failing build. Check the builder's plan after each manifest change, and repeat the needed package fields in the binary crate when the builder requires it.
- **Asset routing:** keep API failures out of the SPA fallback. Test deep links, missing assets, MIME types, cache headers, and relevant security headers. Separate static hosting MAY preserve one origin through routing; CORS is not an automatic requirement.
- **Configuration:** validate settings at startup, with safe diagnostics on failure. Secrets come from the declared secret mechanism, not the image or frontend bundle.
- **Migrations:** run the declared SQLx migration step before routing traffic to code that needs it. Maintain compatibility with still-running versions. Perform destructive contract steps only under approved policy after compatibility is no longer required.
- **Observe:** expose a safe version identifier, liveness, and dependency-aware readiness. Verify expected deployed revision/digest, health, and one critical journey. Define observation duration, diagnostic source, latency/error/resource signals, and readiness behaviour during dependency failure.
- **Compatibility:** declare the old-client support window and a safe upgrade path. An automatic reload alone does not preserve unsaved input or prove compatibility. Confirm Jiff timezone data source and update behaviour.
- **Recover:** keep a known working image and use a declared rollback or roll-forward command. Confirm schema compatibility before rolling back code. Test data restoration; a backup file or a down migration alone is not recovery evidence.
- **Data:** document each store's purpose, retention, deletion policy, credentials, and access boundary. Cache removal is not recovery for durable data.
- **Maintenance:** apply `DOCTRINE.md` P7. Record each infrastructure component, for example a database, cache, message broker, object storage, or container base image, with its version in each environment. Keep test and production on the same major version. Set pnpm `minimumReleaseAge` (for example 1440 minutes). Exclude a version from it only for a security fix. Review toolchain and dependency upgrades with the same evidence rules. Utoipa and Hey API need integration checks, not mandatory simultaneous upgrades. Recheck session adapters and pre-1.0 library compatibility when affected.

### Infrastructure and environments

There is one deployment environment: production. The CI workflow of §3 is
a verification environment; its pins are the four rows that start with
"CI". Read on 26 November 2026.

| Component                 | Version or value                                         | Notes                                                                                   |
| ------------------------- | -------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| Railway                   | Hobby plan, project `calmdonut`, service `telemetry`     | Builds from `main` of `jonikanerva/telemetry`. A merge to `main` is a release.          |
| Ingest server             | `tesla/fleet-telemetry:v0.10.1`                          | Upstream binary, copied into the image in `Dockerfile`. Go 1.27.4, dynamically linked against the glibc and libstdc++ of the base image. Advisory triage: `docs/security/image-advisories.md`. |
| Ingest base image         | `debian:trixie-slim`                                     | Debian 13 (trixie), the current stable release. Point release 13.8 on 26 November 2026. Same digest as `trixie-20261123-slim`. The build runs `apt-get upgrade`, so the packages depend on the build date. |
| Rust toolchain            | 1.100.0, `rust-toolchain.toml`                           | Latest stable on 26 November 2026. Builds `crates/ingest`. The deployed image does not contain the Rust binary yet. |
| Railway TCP proxy         | `zephyr.proxy.rlwy.net:57438` to container port 4443     | The vehicle's signed config contains host `telemetry.calmdonut.com` and port 57438.     |
| DNS                       | Cloudflare, `telemetry` CNAME, DNS only                  | A proxied record breaks the vehicle mTLS connection.                                    |
| Ingest TLS certificate    | Cloudflare Origin CA, ECC, valid until 22 November 2041  | Railway variables `TLS_CERT` and `TLS_KEY`. The Origin CA ECC root expires 15 August 2029. |
| Vehicle telemetry client  | 1.3.0, vehicle firmware 2026.39.100                      | Read from Tesla `fleet_status`.                                                         |
| Tesla Fleet API           | Region `na`                                              | The owner's account is in the NA region although the vehicle is in Europe.              |
| PostgreSQL                | 18, Neon Free plan                                       | Project `telemetry`, region `aws-eu-central-1`, database `neondb`, branch `production`. Values from the owner. The branch `test` reports 18.7 (`SELECT version()`, 26 November 2026, storage change); production was not contacted. No tables yet; the lead creates the roles with `db/roles.sql` (README.md, "Database roles"). |
| Neon production compute   | Fixed 0.25 CU (autoscaling minimum and maximum 0.25)     | Lead decision (ADR 0001). Applied on 26 November 2026 through the Neon API on endpoint `ep-young-dawn-bgyaj83l`; the response showed 0.25 and 0.25, and `suspend_timeout_seconds` 0 (plan default, 5 minutes). Check the effective size and the consumption after the first week of ingest. |
| PostgreSQL test database  | Neon branch `test`, fixed 0.25 CU                        | Created on 26 November 2026 from `production`, before it held vehicle data (§5). Shares the 100 CU-hours of the project; budget at most 5 CU-hours each month (ADR 0001). |
| CI runner image           | GitHub-hosted `ubuntu-26.04`, x86_64                     | `.github/workflows/ci.yml`. Ubuntu 26.04.1. GitHub updates the image within the label, and two jobs of one run can get different image versions (for example 20261122.9.1, 20261116.202.1, and 20261116.88.1 on 26 November 2026); the log of each job shows its version. `ubuntu-latest` pointed to `ubuntu-26.04` on 26 November 2026. |
| CI mise                   | 2026.11.14                                               | `version` input of the mise action in `.github/workflows/ci.yml`. Latest release on 26 November 2026; the same version as on the development machine. |
| CI action: checkout       | `actions/checkout` v7.0.1, commit `3d3c42e5aac5ba805825da76410c181273ba90b1` | Pinned by the full commit SHA. Latest release on 26 November 2026. |
| CI action: mise           | `jdx/mise-action` v5.3.0, commit `924e7c586e33d7d4bae9c14e26c5d22e2beb6e78` | Pinned by the full commit SHA. Latest release on 26 November 2026. |

Project-specific release rules:

- The ingest service must terminate mTLS itself. It uses the Railway TCP
  proxy, never a Railway HTTP domain.
- A redeploy of the ingest service disconnects the vehicle for a short time.
  The vehicle reconnects by itself. Give each Railway service its own root
  directory and watch patterns, so that an application merge does not
  redeploy the ingest service.
- Target layout (ADR 0001, applied by the deployment PR): the service
  `telemetry` runs one image with the Rust ingest process as PID 1 and
  `fleet-telemetry` as its child. The ingest process owns port 4443 and
  forwards the vehicle connections only while it can take records.
  `/ingest/railway.json` sets the Dockerfile path, the watch patterns, the
  pre-deploy migration command, the healthcheck, 30 draining seconds, and
  the restart policy `ALWAYS`.
- Railway documentation does not state that the TCP proxy port is permanent.
  The port stayed the same over one redeploy. If the port changes, sign and
  send a new vehicle telemetry config (`config/telemetry_config.json`).
- A change to the vehicle telemetry config needs the Tesla application
  private key and a Tesla OAuth token. Both stay on the owner's computer.
- Post-release check for the ingest service: the deployment status is
  `SUCCESS`, `openssl s_client` verifies the certificate against the Origin
  CA ECC root on port 57438, and the log shows a vehicle connection after the
  vehicle is next awake. After each build of the ingest image, also read the
  Railway build log: the `apt-get` step must have run, not come from a cached
  layer, and the log must show the installed versions of the fixed packages,
  for example `libzstd1` 1.5.7+dfsg-1+deb13u1 and `passwd` 1:4.17.4-2+deb13u1
  on 26 November 2026. If the step came from the cache, start a build
  without the cache. How Railway caches Dockerfile layers is not verified.
- Recovery: redeploy the previous successful Railway deployment.
- Image advisories: `mise run vuln` reads the reviewed ignore files in
  `docs/security/` (§3, §13). When an entry expires or a new advisory
  appears, triage it in a `chore/deps-<topic>` PR and update
  `docs/security/image-advisories.md` and the ignore file together. When
  Debian publishes a fix for a library that the server loads (glibc,
  gcc-14), start a rebuild of the ingest image, because `apt-get upgrade`
  applies fixes only at build time. A build can reuse a cached layer of the
  `apt-get` step; the build log must show that the step ran and installed
  the fix.
- The `vuln` premise check does not read the `zmq` bind address. The ADR
  0001 deployment change enables the `zmq` dispatcher, so libzmq 4.3.4
  starts to listen. That change must add a premise that `zmq.addr` binds
  the loopback interface. It must also update the Dockerfile allowlist
  and the paths of the check, because it moves the Dockerfile and the
  server config; the strict allowlist fails on any other Dockerfile.

Owner decisions of 26 November 2026 (lead authority):

- The lead may change and send the vehicle telemetry config without a
  separate owner approval when the estimated Tesla usage of the new config
  stays below 3 USD each month. Record the estimate in the PR. A config with
  a higher estimate needs the owner's approval.
- The lead may create a second Railway service in project `calmdonut` when
  the architecture needs it. The expected cost is about 1 to 2 USD each
  month.
- The lead may create Neon branches of project `telemetry` as isolated test
  databases, and an application role with limited privileges.

Lead decisions of 26 November 2026, within delegated authority (ADR 0001,
section "Lead decisions"):

- Neon roles: besides the application role `telemetry_ingest`, a second
  least-privilege role `telemetry_migrator` owns schema `raw` and runs the
  migrations. It serves the owner's intent and keeps `neondb_owner` out of
  Railway. The lead reports this decision to the owner. Accepted risk: the
  migrator URL is a service variable, so the running container can read it.
  Reassess when Railway can limit a variable to the pre-deploy command, when
  the migration step can run outside the service, or after a security
  finding in the running container.
- The production compute has a fixed size of 0.25 CU. Applied on
  26 November 2026 (table above). Verify the effective size after the first
  week of ingest.
- The ingest flush interval defaults to 900 s. Reassess after 30 days of
  measured awake hours.
- The vehicle config does not set `delivery_policy` for now. Reassess with
  evidence from the stored `is_resend` data or from Tesla documentation.
- Accepted gate risk (ADR 0001, lead decision 6): the ingest gate does not
  read TLS. A live peer that stalls inside the TLS handshake holds a
  connection until it closes it. A flood that fills the bound of 1024
  connections blocks the vehicle while it lasts, plus the dead-peer bound:
  about 150 s for an idle gone peer (TCP keep-alive), about 925 to 1,045 s
  for a gone peer with unacknowledged data (retransmission timeout with the
  default `tcp_retries2` of 15). Both bounds equal the present upstream
  server and are below the vehicle buffer of at least 2,500 s, on which
  the vehicle then relies. `TCP_USER_TIMEOUT` was considered and not
  adopted (ADR 0001). The lead reports this risk to the owner. Reassess at
  the first `gate_full` event or on evidence of handshake-stalling peers.

Record a purpose-based reason for each inapplicable obligation. The responsible
lead verifies the integrated result within task authority. Main remains
production-ready. Added cost, new providers or external data transfers,
material lock-in or product changes, and irreversible data changes outside
approved policy require owner approval. Required owner tests block merge.
On release failure, use only an authorized recovery path. Stop at the agreed
outcome and report unverified work explicitly.
