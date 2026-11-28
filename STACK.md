# STACK.md — Rust ingest service and the stack profile

This file states the current technology, commands, values, and rules. Use it
with `DOCTRINE.md` and `CLAUDE.md`; MUST, SHOULD, and MAY have the doctrine
meanings. Text with an ADR citation changes only through that ADR. The
profile is `STACK-RUST.md` of agent-setup `05e0480`; a profile rule that no
code uses yet becomes binding when an application PR implements it.

**Web profile.** `docs/stack-web-profile.md` holds the web parts and the
full profile text of each section; this file states the rules that apply
now in short form. Read the web profile before the first web PR and with
each web change. It is binding from the first web PR. `CLAUDE.md` does not
import it.

---

## 0. Project shape

- **Project risk rationale:** one person, the owner, depends on the system. The data contains the vehicle location history and the VIN; a leak shows where the owner is and when the owner is away. A collection failure loses history that cannot be collected again. Usage above the Tesla billing limit removes the vehicle telemetry configuration and suspends the Tesla API for the owner's Home Assistant. No other system depends on this product. The product sends no commands to the vehicle.
- **Change risk:** `DOCTRINE.md` (introduction) decides the risk class of each changed surface. These paths are examples of the high class here: `crates/ingest`, `ingest/`, `db/`, `migrations/`, each database role and query, `crates/server/src/auth/`, the session guard and security headers of `crates/server/src/app.rs`, the configuration and secrets of the web server (`crates/server/src/config.rs`, `WEB_PASSWORD_HASH`), the log redaction of both services (`logging.rs`), `config/telemetry_config.json`, the Railway settings of both services, the CI workflow, `scripts/`, the check tasks of `mise.toml`, `deny.toml`, `docs/security/`, and every other input of an image (the watch paths of §15) or of a CI gate. Any other code that handles a secret, access control, the session, or location or the VIN is also high class. Light class: the SPA views, the display rules of `crates/domain`, and read-only API responses, unless they expose location or the VIN. A class does not waive a required check.
- **Phase scope (ADR 0009):** the ingest path and the raw storage are live. The web phase has started: these parts of `docs/stack-web-profile.md` are binding: §1 Frontend, §2.1, §2.4, §2.5, the web rows of §4, §11, and the pnpm, TypeScript, Oxlint, Vitest, and Playwright parts of §3, §6, and §14. The derived periods (drives, charges, parked) come later. The general text of the other sections in `docs/stack-web-profile.md` binds all code wherever the code has that concern; read the section when a change touches its topic.
- **Shape:** a monorepo with two Railway services. The service `telemetry` runs one image with Tesla's `fleet-telemetry` server (upstream Go binary) and the Rust ingest process, which supervises it and writes the records to PostgreSQL (ADR 0001). The service `telemetry-web` runs one image with the Rust web server and the built SPA on one origin (ADR 0010). The application server uses Rust (ADR 0009).

### Repository layout and boundaries

Two crates SHOULD be the starting point when the product has domain rules.
A pass-through service without such rules MAY use one crate. The ingest
process is such a service: it stores the source values without
interpretation, so `crates/ingest` is one crate. Do not invent concepts to
populate a layer. Further splits need a current ownership, build-time, or
maintenance benefit.

```text
repo/
  ingest/                # Dockerfile, entrypoint, and fleet-telemetry config (ADR 0001)
  crates/ingest/src/     # Ingest process: gate, supervisor, ZMQ reader, PostgreSQL writer
  crates/ingest/proto/   # Vendored upstream .proto files of the deployed fleet-telemetry tag
  crates/ingest/.sqlx/   # Committed query metadata of the SQLx macros of ingest
  crates/domain/         # Package telemetry-web-domain: pure web domain rules (battery view, ADR 0013)
  crates/server/         # Package telemetry-web-server: HTTP server, problem mapping, static files, contract export (ADR 0010)
  contract/openapi.json  # Committed OpenAPI 3.1 export of the server
  packages/api-client/   # Package telemetry-web-api-client: generated client and Zod validators, handwritten facade
  web/                   # Package telemetry-web-spa: the SPA (Vite, React, TanStack Router and Query)
  e2e/                   # Package telemetry-web-e2e: Playwright journeys against the test server
  webapp/                # Dockerfile of the web image (ADR 0010)
  migrations/            # SQLx migrations (ADR 0002)
  db/roles.sql           # Roles, schema, and privileges (ADR 0002, ADR 0013); run by the owner role, again after a migration
  scripts/               # Check scripts: SQLx metadata, image check, contract check, bundle size, test PostgreSQL server, Gitleaks guard
  Cargo.toml  Cargo.lock  rust-toolchain.toml  clippy.toml  deny.toml  mise.toml
  package.json  pnpm-workspace.yaml  pnpm-lock.yaml  .npmrc  tsconfig*.json
```

Boundaries of the web parts (ADR 0010): `crates/domain` depends on no
HTTP, runtime, or database crate. `crates/server` depends on `domain` and
shares no code with `crates/ingest`. `web` reaches the API only through
`telemetry-web-api-client`; Oxlint `no-restricted-imports` enforces this.
Nothing edits `packages/api-client/src/generated/` by hand. Each
first-party npm package is private, and its name starts with
`telemetry-`, for the same reason as the Cargo package names below.
Each crate with SQLx query macros keeps its metadata in
`crates/<crate>/.sqlx/`. The ingest image reads only `crates/ingest` in
full and the `Cargo.toml` of each other member, with a stub target (§15),
so a change of the web sources does not release ingest. Each first-party
package name other than `ingest` starts with `telemetry-`: osv-scanner
reads a `Cargo.lock` package by its name as a crates.io package, so a
crates.io name gets the advisories of that crate.

---

## 1. Language & Runtime

### Backend

- Rust 1.100.0, edition 2024, pinned in `rust-toolchain.toml` with rustfmt and Clippy. Check the dependency MSRVs with each toolchain change. `rust-version` is in the workspace package metadata and each crate inherits it; lowering it needs owner approval.
- A native Linux binary in the container. A local macOS build does not validate the Linux artifact.
- Cargo with workspace dependency inheritance and a committed `Cargo.lock`; `--locked` on each Cargo operation that supports it.
- Every first-party crate inherits the workspace lints; Clippy runs with `-D warnings`. The unsafe rule applies to first-party code only.

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

Use a narrow `#[expect(clippy::lint_name, reason = "...")]` for a justified
lint limitation; a stale expectation fails verification, and it cannot
waive an acceptance criterion. Do not ban lossless numeric casts as a
class.

### Frontend

- TypeScript 7.0.2 (native compiler) for every type check. The profile
  flags of `docs/stack-web-profile.md` §1 are in `tsconfig.base.json`;
  each package extends it. Exceptions (§13): TypeScript 6.0.3 resolves only
  for the generator in `packages/api-client`, and the generated directory
  has its own `tsconfig.generated.json` that turns off only
  `exactOptionalPropertyTypes`.
- Node.js 24.22.3 (LTS) and pnpm 12.23.0, pinned in `mise.toml`. Node is a
  build-time runtime only.
- pnpm workspace (`pnpm-workspace.yaml`): exact versions, committed
  `pnpm-lock.yaml`, `--frozen-lockfile` installs, `minimumReleaseAge: 1440`
  (strict), `strictDepBuilds: true`. pnpm 12 reads only the registry, auth,
  TLS, and proxy settings from `.npmrc`.
- Oxlint 1.95.0 with `oxlint-tsgolint` 7.0.2009, type-aware, with
  `--deny-warnings` (`.oxlintrc.json`). It enables every rule of
  typescript-eslint `strict-type-checked`, the React hook rules, and
  `no-restricted-imports` for the import boundaries (§0). Gap: a computed
  `import(x)` is not checked; review covers it.
- Prettier 3.10.1 for the TypeScript packages and their config files.
- Zod 4 runs with `jitless: true` (`packages/api-client/src/zod-config.ts`),
  because its `new Function` probe is a CSP `script-src` violation.

### Provisioning

`rust-toolchain.toml` owns the Rust version; mise owns the other tool pins.
Do not keep conflicting Rust pins. `mise run setup` MUST provision or
verify the pinned tools and install the locked dependencies. A pinned
package version alone does not prove a reproducible tool build; verify the
installation method and feature set of each tool.

---

## 2. Frameworks

The framework table and §2.1 to §2.5 (API contract, errors, outbound
sources, frontend state, and authentication with the password login of ADR
0003) are in `docs/stack-web-profile.md`.

---

## 3. Build & verify commands

Mise tasks are the single source of truth for commits, agents, and CI.

| Variable      | Command           |
| ------------- | ----------------- |
| `$FORMAT_CMD` | `mise run format` |
| `$LINT_CMD`   | `mise run lint`   |
| `$BUILD_CMD`  | `mise run build`  |
| `$TEST_CMD`   | `mise run test`   |
| `$VERIFY_CMD` | `mise run verify` |

`mise.toml` defines the tasks and pins the tools. In a fresh checkout,
install mise and rustup, then run `mise trust` and `mise run setup`. mise
reads the Rust version from `rust-toolchain.toml`
(`idiomatic_version_file_enable_tools`).

| Task     | Content |
| -------- | ------- |
| `setup`  | The pinned tools of `mise.toml`, the Rust toolchain, the locked crates, `pnpm install --frozen-lockfile`, and the Playwright browser chromium (in CI with `--with-deps`). Outside CI also `scripts/test-postgres.sh setup`: it checks Apple `container` (version 1 or later) and its service, and pulls the test image for `linux/arm64`. It never installs the tool or starts the service. |
| `format` | `cargo fmt --all` and Prettier in write mode over the TypeScript packages and their config files. |
| `lint`   | `cargo clippy --workspace --all-targets --locked -- -D warnings` for the host (linux/amd64 in CI), Clippy of `telemetry-web-server` with the feature `test-server`, type-aware Oxlint with `--deny-warnings`, and `tsc` of TypeScript 7 for `web`, `packages/api-client`, and `e2e`. |
| `build`  | `cargo build --workspace --release --locked`, `vite build` of the SPA into `web/dist` with gzip copies, and the bundle-size check (`scripts/bundle-size.sh`). The build uses the committed client; it does not export or generate it. |
| `light`  | The fast checks of the development loop: the checks of `checks` that need no network and no container (format checks, `lint`, `contract-check`, `cargo shear`, the Gitleaks guard, the syntax checks, and the script tests), `cargo nextest` without the test binaries that need a database (`ingest::storage`, `telemetry-web-server::battery_postgres`), the server tests with the feature `test-server`, the doctests, and Vitest. It removes `TEST_DATABASE_URL` from the environment, so a database test that is not on the list fails. It leaves out `cargo deny`, `pnpm audit`, the Gitleaks history scan, `build`, the SQLx check, the database tests, and the Playwright journeys. |
| `contract` | Writes `contract/openapi.json` (`telemetry-web-server export-openapi`) and generates `packages/api-client/src/generated/` from it with Hey API. Run it after a change of an HTTP type or route, and commit both. |
| `contract-check` | `scripts/contract-check.sh`: exports and generates in a scratch directory and compares the complete trees with `diff -r` (also added and deleted files). It never writes the working tree. |
| `test`   | `scripts/test-postgres.sh local -- mise run test-db`: a new local test server (below), then task `test-db`. |
| `test-db` | Hidden. `cargo nextest run --workspace --locked --no-fail-fast` without the tests that need Neon (below), the server tests with the feature `test-server`, `cargo test --doc --workspace --locked` (task `doctest`), and task `test-web`: Vitest, then the Playwright journeys in chromium with a desktop viewport against the debug test server (`telemetry-web-server serve-test`) with the real router, middleware, and built SPA, in one worker (the session store and the login throttle of the test server are global, ADR 0012). |
| `test-neon` | The tests that need Neon (below), against the Neon branch `test` (`NEON_TEST_DATABASE_URL`). Not part of `verify`. Run it after a change of `crates/ingest/src/storage/`, `db/`, `migrations/`, or a TLS crate in `Cargo.lock`, and before an ingest release (ADR 0017). |
| `verify` | The static checks of task `checks` (Rust and Prettier format checks, `lint`, `contract-check`, `cargo deny --locked check`, `pnpm audit --prod=false`, `cargo shear`, Gitleaks, the syntax of the config files and scripts, the script tests, no tracked `.env*`, `secrets/`, `.bin/`, or `node_modules/` path), then `build`, then `sqlx-check` and `test` on one local test server. |
| `ci`     | The CI job `ci`: `checks`, the SQLx check, `build`, and `test-db` against the server that `scripts/test-postgres.sh ci` starts on the runner. It does not replace `verify`. |
| `sqlx-check` | `scripts/sqlx-metadata.sh check` on a local test server: `cargo sqlx prepare --check` in each crate with a SQLx dependency, against a new database on the server of `TEST_DATABASE_URL` (the local or the CI test server), migrated with `migrations/`, then dropped. Each crate has its metadata in `crates/<crate>/.sqlx/`; a `.sqlx/` at the root fails the check. |
| `sqlx-prepare` | `scripts/sqlx-metadata.sh prepare` on a local test server: writes `crates/<crate>/.sqlx/` the same way after a change of a query or a migration. |
| `deps`   | The dependency freshness check (below). |
| `vuln`   | The vulnerability check (below). |
| `image-check` | CI only. `scripts/image-check.sh` builds `ingest/Dockerfile` and checks the running container without a database, without an init, with the init of Docker (`--init`, as on Railway), and as user 10001 (must fail). `scripts/image-check-web.sh` builds `webapp/Dockerfile` and checks the web container: user 10001, `/health`, the security headers, the problem document of an unknown API path, a missing asset (404, never `index.html`), the version in `index.html`, gzip assets, SIGTERM, the image size, and no secret in the log. Each script header lists its checks. |

Tests that need Neon: the two tests of `tests/storage/roles.rs` that read the membership of `neon_superuser` (`the_roles_get_the_privileges_of_adr_0002`, `the_role_script_is_idempotent`). They run only in `test-neon` (ADR 0017). The filter of `test-db` and the filter of `test-neon` are complements.

Pinned tools: cargo-nextest 0.9.152, cargo-deny 0.22.1, cargo-shear 1.14.1,
Gitleaks 8.30.1, osv-scanner 2.7.1, SQLx CLI 0.9.1 (features `postgres` and
`rustls`, built from source, both programs `sqlx` and `cargo-sqlx`), jq
1.8.2, crane 0.22.1, Node.js 24.22.3, and pnpm 12.23.0. `package.json` and
`pnpm-lock.yaml` pin the npm tools (§6).

**Database tests (ADR 0017).** `scripts/test-postgres.sh` starts the test
server of PostgreSQL 18: Apple `container` locally, Docker in CI. Each has
TLS with a new test CA, the role `neondb_owner` (not a superuser), and
`TEST_DATABASE_URL` with `sslrootcert`. A local run gets its own server
(data in tmpfs, `127.0.0.1` with a random port in 10000-18999), which goes
away at the end of the run. Each start removes the servers whose owner
process is gone; at most 4 run at the same time. A nested call uses the
server of the outer call. A delete of the script acts only on a container
with the label of this project and the owner of the run;
`scripts/test-postgres-test.sh` (in `checks`) tests these rules with stub
programs. The passwords go to the tools through the environment or
standard input, not on the command line. `verify`, `test`, `sqlx-check`, and
`sqlx-prepare` need no URL and make no Neon connection. A missing Apple
`container` or a stopped service fails the task; an agent sandbox that
blocks its XPC service fails it the same way, so run these tasks outside
the sandbox. `test-neon` needs `NEON_TEST_DATABASE_URL`: the URL of the
role `neondb_owner` on the Neon branch `test` (§5). A missing URL is a
failure, not a skipped check. Export it before the command
(`set -a; . ./.env.test; set +a; mise run test-neon`, value in quotes); mise
does not read the file. The query macros read `crates/<crate>/.sqlx/`
(`SQLX_OFFLINE=true`), so `build` and `lint` never connect to a database;
every `sqlx` call uses `--no-dotenv`. The SQLx check MUST use an isolated
migrated database. Checks never silently regenerate missing evidence. A
missing required database or scanner is a gap, not a pass.

Scope and gaps of `verify`:

- Gitleaks scans the committed history, not uncommitted or ignored files:
  commit before the final `verify`.
- `cargo deny` needs the network. The database tests need Apple
  `container`; a Neon outage or used-up CU-hours fail only `test-neon`.
- The local test server is not Neon: the Neon proxy TLS path, the role
  `neon_superuser`, the Neon build, and the cold start stay unverified in
  `verify` (ADR 0017). `test-neon` covers the role, and partly the TLS
  path.
- `verify` runs on macOS arm64 and builds no image; CI gives the Linux and
  image evidence (ADR 0008). `crates/ingest/tests/deployment.rs` checks
  that the image, the server configuration, the Railway settings, and the
  entrypoint agree.
- The end-to-end tests need a debug build, because only a debug build
  reads the test overrides (ADR 0001). The ignored test
  `measure_the_size_of_a_signal_row` runs only on request (ADR 0002).
- Each task stops at the first failing step. Inside a step, `test-db` runs
  every test (`--no-fail-fast`) before it reports the failures.
- The journeys need the host tool `openssl` (LibreSSL on macOS, OpenSSL on
  the runner) for the test certificate (`e2e/tls-cert.sh`). It is not
  pinned; a missing tool fails `test-web`.
- The Playwright journeys run against `serve-test` of a debug build with
  the feature `test-server`, which the release image does not build.
  `pnpm audit` sends the package names and versions of `pnpm-lock.yaml` to
  the audit endpoint of the npm registry; it sends no product data.
- Supported browser: Chrome on a desktop viewport (ADR 0010). No engine
  evidence for a phone browser (every iOS browser uses WebKit). Reassess
  when the owner starts to use the product on a phone.
- Prettier formats the TypeScript packages and their config files, not the
  Markdown documents.

**CI (GitHub Actions).** `.github/workflows/ci.yml` runs the jobs `ci`
(with `postgres:18.7-trixie` on the runner, TLS, a test CA, and
`sslmode=verify-full`), `vuln`, and `image-check` on `ubuntu-26.04`
(linux/amd64) for each pull request that is not a draft and each push to
`main`. A draft PR gets a run with skipped jobs. A daily schedule (04:17
UTC) runs only `vuln`; GitHub disables a schedule after 60
days without activity. The token can only read the contents; there is no
repository secret and no `pull_request_target` trigger; each third-party
action is pinned to a full commit SHA. `image-check` pushes no image and
has no secret and no database. A `pull_request` run tests the merge with
the base. On a pull request, the first job `select` runs
`scripts/ci-select.sh` on the changed files (the diff of the merge commit
to the base). `ci` always runs. `image-check` runs only when an image
input changes: a watch path of either service (§15) or
`scripts/image-check*.sh`. `vuln` runs only when a `vuln` input (below)
changes. A change of `ci.yml`, `mise.toml`, or `scripts/ci-select.sh`
runs every job. A push to `main` runs every job. The selection fails safe:
an unknown event, a failed or empty diff, a quoted path, or a failed
`select` job runs every job. `scripts/ci-select-test.sh` (in `checks`) tests the decisions
and checks that each watch path of §15 selects `image-check`. Gaps: it
does not check the reverse direction, and it does not compare the
patterns with the `vuln` input list of this section; a pattern can match
more paths than the documented input.

Merge conditions (`CLAUDE.md`, ADR 0008, ADR 0016):

- One local `mise run verify` on the version to be merged, integrated with
  the current base: by the reviewer for a change with a high-class part,
  otherwise by the lead (`DOCTRINE.md` P9). Before review, the implementer runs the fast
  checks and the tests of the changed parts (`mise run light`). For a change of database
  code, migrations, queries, roles, or `.sqlx/`, the implementer also runs
  `mise run sqlx-check` and `mise run test` (both on a local test server),
  and `mise run test-neon` for a change that the `test-neon` row names.
- A green `ci` job for the current head. When the base changes after the
  run, update the PR branch so that a new run tests the current base.
- A green `image-check` job of the current head for a change of an image
  input (the watch paths of §15) or of the job.
- A green `vuln` job for a change of a `vuln` input (below).
- A green job is a job that ran and passed. A `skipped` job is not green:
  for a change of an image or `vuln` input, the job must run.
- The workflow adds no branch protection (an owner decision); the
  acceptance conditions of `CLAUDE.md` block a merge with a red run.

**Dependency freshness check** (`mise run deps`): `cargo update --dry-run
--verbose` (also lists crates behind across major versions), `mise outdated
--bump`, `pnpm outdated --recursive`, and the newest releases of
`rust-toolchain.toml`, Node.js (LTS lines), pnpm, the `FROM` images of
`ingest/Dockerfile` and `webapp/Dockerfile`, and the CI pins. pnpm applies
`minimumReleaseAge` (1440 minutes) to each install; `deps` prints the
newest npm releases without that filter. Compare the other §15 versions by
hand. A crate release younger than 1440 minutes is not due, except a
security fix (ADR 0006). `deps` checks the workflow against a strict line
allowlist (`mise.toml`, task `deps`) that changes together with the
workflow. Limits: it does not parse YAML, and it does not read what a
pinned action runs, the action inputs other than the mise `version`, or the
`run:` commands, which can download and run code. Before a change of the
runner label, check in the README of `actions/runner-images` that the image
is generally available.

**Vulnerability check** (`mise run vuln`, ADR 0005): `cargo deny --locked
check advisories`, `pnpm audit --prod=false`, `osv-scanner scan source -r .`
(`Cargo.lock` and `pnpm-lock.yaml`, without `.claude/worktrees/`; the
workspace links of `pnpm-lock.yaml` and the ignored `node_modules/` are not
read), and `osv-scanner scan image --archive` of each runtime `FROM` image
of `ingest/Dockerfile` and `webapp/Dockerfile` (every stage except the
builder stages), downloaded anonymously with crane for `linux/amd64`. The
task scans the runtime images of each Dockerfile, so an image that both
Dockerfiles name is scanned once for each. Each image scan reads the
reviewed ignore file of its Dockerfile:
`docs/security/osv-scanner-<repository, / as ->.toml` for
`ingest/Dockerfile` and `docs/security/osv-scanner-web-<repository, / as ->.toml`
for `webapp/Dockerfile`; a missing file fails the task. First the task
checks the three deployment facts that no other check holds
(`docs/security/image-advisories.md`, "Ignore files and deployment facts").
It fails when a fact breaks, a scan fails or finds an uncovered
vulnerability, or the scanned runtime images of a Dockerfile differ from
its expected number (two for `ingest/Dockerfile`, one for
`webapp/Dockerfile`). The `COPY --from` lines of each Dockerfile must equal
an allowlist in the task. Compare the Rust toolchain and each §15
version with its vendor advisories.

Scope and gaps of `vuln`:

- §13 lists the surfaces without a scanner. The Debian 12 packages of the
  upstream image are not in production; only `/fleet-telemetry` comes from
  it. `linux/amd64` is an unverified assumption about the Railway host.
- Each ignore entry needs a recorded decision in
  `docs/security/image-advisories.md`, a reason, and an expiry date. A red
  `vuln` gets a triage in a `chore/deps-<topic>` PR; a "needs action"
  finding gets no entry.
- `vuln` does not read the CISA KEV catalog. The lead does the manual
  comparisons of ADR 0005 (decision 6) in each PR that changes an input of
  `vuln`, and in each triage of a red `vuln` job.
- `verify` does not include `vuln`; it keeps `cargo deny --locked check`
  (ADR 0005).
- The CI job `vuln` gives the runs of `DOCTRINE.md` P7. Result rules (ADR
  0005):
  - A green `vuln` job is a merge condition of a PR that changes an input:
    `Cargo.lock`, each `Cargo.toml`, `deny.toml`, the `vuln` task or the
    scanner pins in `[tools]` of `mise.toml` (cargo-deny, osv-scanner,
    crane, jq, pnpm), the `vuln` job of `.github/workflows/ci.yml`, any
    lockfile that `osv-scanner scan source -r .` reads (also
    `pnpm-lock.yaml`), each `package.json`, `pnpm-workspace.yaml`, any
    `osv-scanner.toml`, `ingest/Dockerfile`, `webapp/Dockerfile`,
    `ingest/entrypoint.sh`,
    `ingest/server_config.json`, or the ignore files in `docs/security/`.
  - A red `vuln` job of a push to `main` or of the schedule starts a
    triage in a `chore/deps-<topic>` PR and does not block a PR that
    changes no input. Before such a PR merges while `vuln` on `main` is
    red, the lead compares the IDs of the new findings with the CISA KEV
    catalog (ADR 0016).
  - A known exploited vulnerability or a malicious package version makes
    its fix due before other work (`DOCTRINE.md` P7).

---

## 4. Performance budgets

The ingest service has no adopted budget table. The web budgets below are
targets (ADR 0010). A row is a gate only where a check enforces it; the
other rows name the slice that can measure them.

| Budget | Target and conditions | Measured | Enforcement |
| ------ | --------------------- | -------- | ----------- |
| Initial bundle | JS ≤ 200 KB and CSS ≤ 30 KB, gzip, the files that `index.html` references | JS 131,619 B, CSS 1,009 B (S1 shell, macOS arm64, 27 November 2026) | `scripts/bundle-size.sh` in `build` |
| Web image | ≤ 100 MB compressed (`docker save` with gzip) | Printed by `image-check-web.sh` in CI | `image-check-web.sh` fails above the budget |
| Startup to `/health` | ≤ 1 s, release image on linux/amd64 | Printed in whole seconds (container start included) by `image-check-web.sh` in CI | Not a gate: the script waits at most 30 s |
| Battery API, cold Neon | ≤ 8 s total | Not measured: the shared test compute did not suspend during experiment E3 (ADR 0013) | Post-release check on Railway (`docs/web-release.md`, "Battery API read time") |
| Resident memory | steady ≤ 128 MiB, peak ≤ 256 MiB | Not measured | Railway metrics after the release |
| Concurrency | Argon2 ≤ 1 | Not applicable in S1 | Owned by S2 |
| Lab experience | LCP ≤ 2.5 s, CLS ≤ 0.1 (Chromium, desktop viewport, 4× CPU slowdown); no field data | Not measured | Owned by S4 |
| Web use of Neon | ≤ 10 CU-hours each month | Not measured: each request wakes the compute only for its read; no background polling | Neon usage after the first week of web use (ADR 0013, reassessment) |

---

## 5. Persistence shape

- **Storage:** PostgreSQL on Neon (Free plan: 1 GB storage, 100 CU-hours each month, scale to zero after 5 minutes, 6 hours of restore history, computes up to 2 CU, 10 branches; <https://neon.com/docs/introduction/plans>). Neon suspends the compute when the CU-hours are used, and rejects writes when the storage is full. The ingest process writes a batch at the latest 15 minutes after the oldest queued record and keeps no idle connection (ADR 0001). ADR 0002 holds the tables, values, idempotency keys, roles, and size estimate.
- **Test databases:** a local or CI test server of `scripts/test-postgres.sh` for `verify` and CI; the Neon branch `test` only for `test-neon` (ADR 0017; ADR 0002, "Test databases"); never reset the branch from its parent. The connection sets the `search_path` (production: `raw`), so the storage SQL names no schema. At most 8 database tests run at the same time (`.config/nextest.toml`); a hung test stops after 4 minutes. `db/roles.sql` and the cleanup of test roles take one advisory lock (parallel privilege changes fail with SQLSTATE XX000).
- **Queries:** every storage statement is a checked SQLx query macro, except `db/roles.sql` (run through `apply_roles`) and the SQLx migrator. A bind of a `jiff_sqlx::Timestamp` or of an array with NULL elements uses `as _`; a time column uses `AS "name: jiff_sqlx::Timestamp"`. The database tests execute every statement against the migrated schema. Dynamic SQL needs bound parameters, structural constraints, and integration tests.
- **Web access (ADR 0013):** the web server connects as the role `telemetry_web` (`db/roles.sql`): SELECT only on the columns that the web queries read (`web_columns` of the script: no `latitude`, `longitude`, or `json_value`); no VIN, no `unprocessed_record`, no write; a new query column needs a new grant; CONNECTION LIMIT 4, `statement_timeout` 10 s, `idle_in_transaction_session_timeout` 15 s, `default_transaction_read_only` on. Run `db/roles.sql` again after a migration that adds a table that the web reads. One connection and one `REPEATABLE READ READ ONLY` transaction for each request, no pool, at most 2 reads at a time (5 s wait, then `service_busy`), connect 15 s, the whole read 24 s. The web server never migrates; its queries are checked macros with metadata in `crates/server/.sqlx/`, and `crates/server/tests/battery_postgres.rs` compares them with the reference rules of `telemetry-web-domain` on the migrated schema as the web role. `crates/server/tests/battery_e3.rs` repeats experiment E3 on request.
- **Entities and retention:** `VISION.md`.
- **Constraints and atomicity:** use the database constraints that apply (NOT NULL, CHECK, foreign, unique, exclusion), and test rejection and competing writes; read-before-write alone cannot enforce concurrent uniqueness. Use transactions for atomic operations; never publish partial success. Idempotency: ADR 0002, "Idempotency".
- **Migrations:** numbered, reviewed SQLx migrations as a separate release step, never at process start: `ingest migrate` is the Railway pre-deploy command with the role `telemetry_migrator`, and `ingest run` only checks that its embedded migrations are applied (ADR 0002). Use expand-and-contract compatibility. A migration names no role and no schema (the default privileges of `db/roles.sql` apply). A new reason of `raw.unprocessed_record` needs a migration in the same release.
- **Recovery:** test empty-database setup and upgrade from supported prior schemas. Application rollback does not restore deleted data. Restore is Neon's 6-hour history, not tested.
- **Server version:** the test server has the production major version (§15). Read the production version after each platform upgrade (`SHOW server_version`). Evidence from another major version is not evidence for production.
- **Connection TLS:** one SQLx TLS feature, `tls-rustls-aws-lc-rs`, with the one rustls provider of the binary (§7). `storage::connect_options` sets `sslmode=verify-full`, so neither the URL nor `PGSSLMODE` can turn the check off. SQLx builds the rustls configuration with an explicit aws-lc-rs provider and the Mozilla roots of `webpki-roots`, so it needs no system certificates. It ignores the libpq parameter `channel_binding` with a warning; it does not use channel binding. rustls rejects a server certificate that is not version 3; check the production `sslmode`, `PGSSLMODE`, and certificate version read-only at a release (ADR 0002, "Not verified"). Neon terminates TLS in its proxy, so `pg_stat_ssl` shows no TLS.

---

## 6. Approved dependencies

A new dependency needs a PR rationale for necessity, provenance,
maintenance, licence, transitive cost, and replacement cost (`DOCTRINE.md`
P2). Pin the selected Cargo feature sets. A change of a dependency manifest
needs boundary review.

### Pinned direct dependencies

`Cargo.lock` pins the resolved versions. Default features are off; the
listed features are the complete set of the workspace entry.
`src/main.rs` of `crates/ingest` is the only reader of the environment and
the command line of ingest; `config` and `cli` of `crates/server` are those
of the web server. A feature that only the web server uses is in
`crates/server/Cargo.toml`, so the ingest binary does not change.

| Crate | Version | Features | Use | Licence |
| ----- | ------- | -------- | --- | ------- |
| axum | 0.8.9 | none | `GET /health` | MIT |
| hyper | 1.13.0 | `client`, `http1`, `server` | HTTP/1 with a header read timeout; client of the metrics read | MIT |
| hyper-util | 0.1.21 | `service`, `tokio` | Tokio and router adapters for hyper | MIT |
| socket2 | 0.6.5 | `all` | TCP keep-alive on gate sockets (ADR 0001, gate rule 3) | MIT OR Apache-2.0 |
| tokio | 1.54.1 | `rt-multi-thread`, `net`, `signal`, `time`, `sync`, `macros`, `io-util`, `process` | Runtime, listeners, signals, deadlines, gate copy, child process; tests add `test-util` | MIT |
| tracing | 0.1.44 | `std` | Structured log events | MIT |
| tracing-subscriber | 0.3.23 | `fmt`, `std` | Subscriber and writer of the JSON formatter | MIT |
| serde_json | 1.0.154 | `std` | JSON encoding of each log line | MIT OR Apache-2.0 |
| thiserror | 2.0.21 | `std` | Error types with static text only | MIT OR Apache-2.0 |
| jiff | 1.0.3 | `std` | `Timestamp` of records and events (§10) | Unlicense OR MIT |
| prost | 0.14.6 | `derive`, `std` | Protobuf decode (ADR 0001) | Apache-2.0 |
| prost-types, also build | 0.14.6 | `std` | `google.protobuf.Timestamp`; schema descriptors in `build.rs` | Apache-2.0 |
| uuid | 1.29.3 | `std` | Typed connection ID (ADR 0002); run and connection IDs of the web reads | Apache-2.0 OR MIT |
| prost-build, build | 0.14.6 | none | Rust types of `crates/ingest/proto/` | Apache-2.0 |
| protox, build and dev | 0.10.0 | none | `.proto` parser in Rust, no `protoc` | MIT OR Apache-2.0 |
| proptest, dev | 1.12.0 | `std` | Property tests of the decode | MIT OR Apache-2.0 |
| sqlx | 0.9.1 | `postgres`, `runtime-tokio`, `tls-rustls-aws-lc-rs`, `migrate`, `macros`, `uuid`, `json` | Connections without a pool, checked queries, migrations, verified TLS (§5); also the web reads | MIT OR Apache-2.0 |
| jiff-sqlx | 1.0.0 | `postgres` | `timestamptz` binds of `jiff::Timestamp` (§10), ingest and web | Unlicense OR MIT |
| zeromq | 0.7.0 | `tcp-transport`, `tokio-runtime` | ZMQ SUB socket (ADR 0001); pure Rust | MIT |
| futures-util | 0.3.34 | none | `StreamExt::next` on the `zeromq` monitor stream | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | none | Run ID and retry jitter without a panic | MIT OR Apache-2.0 |
| serde, web | 1.0.229 | `derive`, `std` | Wire types of the HTTP contract | MIT OR Apache-2.0 |
| utoipa, web | 6.0.0 | `macros` | OpenAPI 3.1 export of the HTTP types (no `jiff_0_2`: `wire::Instant` has an explicit schema) | MIT OR Apache-2.0 |
| utoipa-axum, web | 0.3.0 | none | Ties the documented routes to the registered routes | MIT OR Apache-2.0 |
| tower-http, web | 0.7.1 | server crate: `catch-panic`, `fs` | Panic containment; static files with precompressed gzip | MIT |
| tower, web dev | 0.5.3 | server tests: `util` | `oneshot` in the HTTP tests | MIT |
| rustls, web test server | 0.23.46 | server crate, optional (feature `test-server`): `aws_lc_rs`, `std`, `tls12` | TLS 1.2 and 1.3 of `serve-test` with the explicit aws-lc-rs provider; PEM parser of `rustls-pki-types` | Apache-2.0 OR ISC OR MIT |
| tokio-rustls, web test server | 0.26.6 | server crate, optional (feature `test-server`): none | Tokio TLS acceptor of `serve-test` | MIT OR Apache-2.0 |
| argon2, web | 0.6.0 | server crate: `alloc`, `password-hash` | Argon2id verification of the login and the command `hash-password` (ADR 0012); `opt-level = 3` in the dev profile | MIT OR Apache-2.0 |
| subtle, web | 2.6.1 | none | Constant-time compare of session tokens (ADR 0012) | BSD-3-Clause |

The web server adds the axum features `json` and `matched-path` and, in the
tests, the tokio feature `test-util`, in `crates/server/Cargo.toml`.
Only the feature `test-server` enables rustls and tokio-rustls, so the
release image has no TLS code; the Railway proxy ends TLS.

### Pinned npm packages

`package.json` of each workspace package pins exact versions;
`pnpm-lock.yaml` pins the tree. Each release was older than 1440 minutes
at the pin.

| Package | Version | Package of the workspace | Use | Licence |
| ------- | ------- | ------------------------ | --- | ------- |
| typescript | 7.0.2 | root | `tsc` for every type check (§1) | Apache-2.0 |
| typescript | 6.0.3 | `telemetry-web-api-client` (dev) | Compiler API for the generator only (§13) | Apache-2.0 |
| oxlint, oxlint-tsgolint | 1.95.0, 7.0.2009 | root | Type-aware lint | MIT |
| prettier | 3.10.1 | root | Format | MIT |
| vitest | 5.1.2 | root | Unit tests | MIT |
| vite | 8.4.0 | root, `telemetry-web-spa` | SPA build; the peer of Vitest | MIT |
| @vitejs/plugin-react | 6.3.0 | `telemetry-web-spa` (dev) | React transform | MIT |
| react, react-dom | 19.3.1 | `telemetry-web-spa` | UI | MIT |
| @tanstack/react-router | 1.170.46 | `telemetry-web-spa` | Code-based routes and URL state | MIT |
| @tanstack/react-query | 5.108.4 | `telemetry-web-spa` | Server state and cache | MIT |
| zod | 4.6.7 | `telemetry-web-api-client` | Executed validation of responses and problem bodies | MIT |
| @hey-api/openapi-ts | 0.99.0 | `telemetry-web-api-client` (dev) | Generation of the SDK, types, and Zod schemas | MIT |
| @playwright/test | 1.65.1 | root, `telemetry-web-e2e` | Browser journeys; ships Chromium build 1255 (`browsers.json` of `playwright-core`) | Apache-2.0 |
| @axe-core/playwright | 4.14.1 | `telemetry-web-e2e` | Accessibility checks in the journeys | MPL-2.0 (test only; the image holds no file of it) |
| @types/react, @types/react-dom, @types/node | 19.3.3, 19.3.0, 24.19.4 | dev | Types; `@types/node` follows the Node 24 runtime | MIT |

Override: `pnpm-workspace.yaml` sets `js-yaml` 4.3.2 for
`@hey-api/json-schema-ref-parser@1.4.4`, which pins js-yaml 4.2.0 with
three high advisories (GHSA-52cp-r559-cp3m, GHSA-5p4m-2wfm-xmqj,
GHSA-2883-xcg3-v3hh). `contract-check` proves that the generated client
does not change. Remove the override when a generator release depends on
js-yaml 4.3.2 or later.

Facts of the dependency tree:

- hyper and hyper-util replace the axum features `http1` and `tokio`.
  `prost-build` and `protox` run only at build time and in the tests.
- Only `aws-lc-sys` compiles C code (AWS-LC, linked statically). With its
  pre-generated bindings, the build needs only a C compiler (`gcc` in the
  builder stage); the runtime image needs no TLS library.
- One rustls provider (`cargo tree -i aws-lc-rs`). `Cargo.lock` also lists
  `sqlx-mysql`, `sqlx-sqlite`, `libsqlite3-sys`, and `ring`, which Cargo
  does not build; osv-scanner can report them.
- Licence exceptions are in `deny.toml`.
- matchit stays at 0.8.4, because axum 0.8.9 requires `=0.8.4`. Reassess
  with the next axum release.
- `zeromq` logs through the `log` crate; the process installs no `log`
  logger, so these records cannot reach the JSON log (§8). `zeromq` has no
  `unsafe` in its `src/`; its maps `scc`, `saa`, and `sdd` use `unsafe`
  inside. First-party code needs none.

---

## 7. Stack-specific reject-list additions

- First-party unsafe code; production unwrap, expect, panic, todo, or unimplemented for ordinary failures; unchecked indexing or arithmetic that can violate accepted input bounds.
- Lint suppression without a narrow reason; a check changed only to obtain a pass.
- Bare `String`, `Uuid`, or integer entity IDs across crate or API boundaries (use one newtype per entity); binary floating point for exact quantities.
- Deserialization that bypasses invariants; unchecked wire casts, silent loss of precision, or invented default facts.
- Broad untyped error signatures; anyhow only for startup or tooling context.
- println, eprintln, or dbg in shipped paths; diagnostics with secrets, personal data, or whole payloads.
- Ambient time, environment, filesystem, or random access in domain logic; unvalidated configuration across modules.
- Unbounded queues, retries, response bodies, or concurrency; outbound or database operations without deadlines.
- Blocking work on async paths; synchronous lock guards across await; unowned spawned tasks or silently discarded errors or futures.
- Unbound SQL interpolation or unchecked SQL safety assertions.
- Two rustls crypto providers in one binary (for example SQLx `tls-rustls-ring-webpki` with reqwest `rustls`): rustls then panics when `sslmode=verify-ca` or `verify-full` builds a verifier. Check `cargo tree -i ring` and `cargo tree -i aws-lc-rs` after a TLS feature change.
- Host-local timezone arithmetic, invented instants for date-only values, or manual UTC offsets (§10).

Scope tests and tooling by their contracts. Supported platform APIs and
justified lossless operations need no blanket ban.

---

## 8. Logging & privacy

tracing with the tracing-subscriber JSON formatter only (ADR 0009).
Allowlist the fields of events and spans. No automatic Debug output of
arguments, configuration, headers, or payloads. Redact secret types and
test the redaction. Carry safe correlation through spans. Create a request
identifier inside the panic containment of the request (a random generator
can panic); work that several requests share has no single identifier, so
record that gap. Define diagnostic access, retention, cardinality, and
resource limits. ADR 0001, "Logging", holds the log rules of the
ingest process. No external crash reporter or telemetry exporter is
enabled; a new data flow needs an assessment and the required authority.
Keep secrets out of source history, images, and generated files. Validate
configuration in one startup boundary.

---

## 9. Background & lifecycle

Every background task has an owner, an observable failure, and a shutdown
policy. Bound all external work. Define retry count, backoff, jitter, total
deadline, and operation safety; make time and randomness controllable in
tests. Dropping a future does not undo committed effects, and a timeout on
waiting does not prove that work stopped. Implement and test a shutdown
deadline; after it, end the process with `shutdown_background` or
`shutdown_timeout`, because a dropped Tokio runtime waits without a limit
for its blocking tasks. Treat a failed signal-handler set-up the same way
for every signal, and log it. At shutdown, stop accepting work, drain or
cancel owned operations, and define the outcome at the deadline. Work
promised to survive a restart needs durable ownership and recovery.

`ingest run` owns these tasks (`crates/ingest/src/lifecycle.rs`):

| Task | Watchdog limit | Stop |
| ---- | -------------- | ---- |
| Gate acceptor | 30 s | Shutdown step 1, bound 1 s |
| ZMQ reader | 30 s | Step 2: the drain, bound 3 s |
| Shortfall check | none | Step 3: the final read, bound 1 s |
| Child supervisor | none | Step 4: SIGKILL, bound 1 s |
| Writer | 120 s | Step 5: the final flush, bound 20 s |
| Log of the events | none | Aborted after the last log lines |
| Health server | none | At once, in parallel, deadline 3 s |

A task that ends by itself starts the shutdown, and the exit code is 1. ADR
0001 holds the retry cadence of the writer and its deadlines ("Delivery
path and bounded buffering", "Decisions of the pipeline change"). The tests
control the time with the paused Tokio clock, and the jitter with an
injected function.

The in-memory queue does not survive a restart. A restart during a database
outage longer than 15 minutes can lose the whole queue, at most 64 MiB.
This loss is accepted, and there is no durable spool (ADR 0001, "Key
alternatives").

---

## 10. Time & timezones

- **Instant:** Jiff `Timestamp`; PostgreSQL `timestamptz` when persisted; validated RFC 3339 UTC with `Z` on the wire. Define the supported precision and range.
- **Duration:** `SignedDuration` for elapsed quantities and `Span` for calendar arithmetic. Never silently treat calendar months or days as fixed seconds. Measure elapsed time with a monotonic clock.
- **Tests:** pass fixed time into the functions under test; use an injectable process clock where needed. Test the relevant precision, expiry, and retry cases. The Jiff SQLx wrapper conversion needs round-trip evidence.

Use one first-party time abstraction. Transitive dependencies may use other
libraries.

---

## 11. Design guidelines & UX thresholds

The design guidelines and UX thresholds are in `docs/stack-web-profile.md`, §11.

---

## 12. Best practices source

The list of documentation sources is in `docs/stack-web-profile.md`, §12.
Consult current official documentation for the pinned version (`DOCTRINE.md`
P2).

---

## 13. Scoped exceptions

Do not weaken a rule or check to make a change pass. The responsible lead
resolves exceptions within project authority, with an independent reviewer.
The change author must not approve their own weaker acceptance or checks.
Escalate changes beyond authority and unresolved material risks to the
owner. Each exception has an ADR; the ADR names the decider and the
approval.

| Rule / scope | Consequence | Compensating evidence | ADR | Reassessment |
| ------------ | ----------- | --------------------- | --- | ------------ |
| §3: no scanner for the `apt-get` packages of the built image, the Rust toolchain, the mise tools, and the static C code of `/fleet-telemetry` | A known vulnerability there is found only by a manual comparison. | The manual comparisons of ADR 0005, decision 6. Before a change enables the `kafka` dispatcher, compare the OpenSSL and libcurl records one by one. | ADR 0005 | The built image can be downloaded; a scanner covers these surfaces; another dispatcher or another libzmq address; or by 2027-01-25. |
| §3: reviewed ignore files of the image scan | An ignored advisory that later gets a Debian fix or a KEV listing stays hidden until its entry expires. | A recorded decision and an expiry for each entry; the deployment-fact checks; the build-log check of §15; manual KEV comparison. | ADR 0005 | Expiry: Go entries 2027-02-24, Debian 13 entries 2027-01-25. Earlier with a new upstream release, a base image change, a broken deployment fact, or a KEV listing. Due now, because the service `telemetry-web` has a public domain: the glibc and gcc-14 entries of `docs/security/osv-scanner-web-debian.toml` (W4 of `docs/security/image-advisories.md` no longer holds). |
| §3: no scan of the builder image `rust:<version>-slim-trixie` | A compiler or linker vulnerability that changes the binary is not found by a scanner. | The tag is pinned (`<channel of rust-toolchain.toml>-slim-<Debian codename of the final stage>`; no check holds this form); `deps` prints the newest tag; `image-check` runs the binary; manual KEV comparison. | ADR 0005 | A scanner of the built image in CI; a build-stage advisory in CISA KEV; a change of the builder image. |
| §3: no scan of the test image `postgres:18.7-trixie` (CI and the local test server) | A vulnerability in that image is not found by a scanner. | It runs only on the CI runner and in the local test server, with test data, no secret, and on loopback (the local server publishes only on `127.0.0.1`). | ADR 0005, ADR 0017 | The image is used outside the tests. |
| §3, §14: Gitleaks 8.30.1 contains the brotli decoder of CVE-2026-111497 (CISA KEV) | A scan into archives, or another Gitleaks version, can run the decoder on fork input: a denial of service of the scan. | The scan reads no archive content (depth 0); `scripts/gitleaks-guard.sh` in `checks` fails on the literal archive depth flag in any tracked file (also a binary one), on a `git grep` error, and on a version other than 8.30.1; ADR 0011 lists the known limits. | ADR 0011 | A Gitleaks release with brotli 1.2.7 or later; any Gitleaks version change; a need to scan archives; or by 2027-01-25. |
| §1, §14: runtime tests on linux/amd64 before merge | A Linux-only fault in the container, as PID 1, behind the Railway proxy, or in a test that needs Neon shows only after deployment. | The Linux checks 1 to 6 of ADR 0001 ("To verify at first deployment"): CI, `image-check`, and the Railway checks of `docs/ingest-release.md`. Open: check 3 on Railway, check 4, and check 5. | ADR 0008 | Ends when the release records checks 1 to 5 and check 6 has CI evidence. Reassess on 2027-02-28 if open, and when CI can run the tests that need Neon. |
| §14: build and test of the ingest image before merge | The Railway build, settings, proxy, and healthcheck are unverified before the merge. | A green `image-check` job of the PR head; the post-release checks and the recovery of `docs/ingest-release.md`. | ADR 0008 | Railway documents its Dockerfile build, or the Railway builder can run before the merge. |
| Web profile §2.1: TypeScript 6.0.3 for the generator | Two TypeScript versions in the lockfile; the generator runs on the older compiler API. | Only `telemetry-web-api-client` resolves it (dev dependency, `pnpm why`); every type check runs TypeScript 7.0.2; `contract-check` compares the output. | ADR 0010 | A Hey API release that supports TypeScript 7. |
| §1: `exactOptionalPropertyTypes` off for `packages/api-client/src/generated/` | The copied client runtime of Hey API is not checked with that one flag. | Nothing edits the directory; `contract-check`; every other strict flag stays on; the facade and the SPA keep the flag and read the generated code only as declarations. | ADR 0010 | A Hey API release whose runtime passes the flag. |
| §3: no scan of the web builder images `node:<version>-trixie-slim` and `rust:<version>-slim-trixie` | A vulnerability in Node, npm, pnpm, or a builder Debian package that changes the SPA files or the server binary is not found by a scanner. | The tags equal the pins of `mise.toml` and `rust-toolchain.toml` (`crates/server/tests/deployment.rs`); `deps` prints the newest tags; `pnpm audit` and the source scan cover the npm packages of the build; `vuln` allows only the binary and `web/dist` out of the builder stages; `image-check` runs the built image; manual KEV comparison. | ADR 0010 | A scanner of the built image in CI; a builder-stage advisory in CISA KEV; a change of a builder image. |
| §14: build and test of the web image before merge | The Railway build, settings, and healthcheck of `telemetry-web` are unverified before the merge. | A green `image-check` job of the PR head (`image-check-web.sh`); the post-release checks and the recovery of `docs/web-release.md`. | ADR 0008, ADR 0010 | Railway documents its Dockerfile build, or the Railway builder can run before the merge. |
| Web profile §15: one image for server and SPA | The ingest service is a second image (Tesla's upstream server). | The application still ships as one image. | ADR 0009 | Permanent while the upstream server is used. |

---

## 14. Applicability and evidence

The merge conditions are in §3. CI does not replace the local run, and the
local run does not replace CI; neither is a branch protection rule. Assign
each check to before merge or after release: missing pre-merge evidence
blocks the merge; missing post-release evidence blocks a claim of a
successful release. A future production deployment is not required to
approve its PR. `DOCTRINE.md` P9 states who runs the one full
verification.

| Doctrine / applicability | Required evidence | Environment / gap |
| ------------------------ | ----------------- | ----------------- |
| P1, P6: each task | Criteria, failures, assumptions, and check mapping | Lead records; independent review for material changes |
| P2–P4: types and dependencies | Strict checks, dependency rationale, crate boundaries | Manifest edits and ambient effects need review |
| P5–P6: data meaning | Conversion and property or challenge cases; partial versus atomic failure tests | Preserve missing, null, and zero, units, precision, and uncertainty |
| P5–P7: storage | Migrated-schema metadata check; constraint, competing-write, and upgrade tests; production and test server versions | §5. The two role tests that need Neon run only on macOS in `test-neon`; the Neon TLS path has no Linux evidence before deployment (§13, ADR 0017). Restore is not tested. |
| P3, P7: security | Named scanner results and privacy checks | Scanners do not prove authorization |
| P6, P7: release and platform | During development: the fast checks and the tests of the changed parts (`mise run light`). Before review: green `ci`. Before merge: one `verify` of the merged version on the current base (P9), green `ci`, and for an image input a green `image-check`; after release: identity, health, and smoke evidence | A local build is not a verified deployment (§13) |
| P8–P9: material changes | Current docs, ADRs, independent review; independent integrated verification where P9 requires it | Separate reviewer context; exact tested revisions |

### Check configuration

| Check | Tool | Acceptance condition |
| ----- | ---- | -------------------- |
| Format, types, lint | rustfmt check, Clippy `-D warnings` | No differences, no unreviewed warnings |
| Dead dependencies | cargo-shear, rustc unused checks | No unexplained findings |
| Boundaries | Cargo declarations; Clippy disallowed methods where practical | No forbidden edges; declared gaps reviewed |
| SQL metadata | SQLx prepare check | Matches the migrated isolated schema |
| Secrets | Gitleaks, redacted, over the committed history | No confirmed exposed secret |
| Rust dependencies | cargo-deny advisories, licences, bans, sources (`deny.toml`) | Findings triaged; allowlists and reviewed exceptions enforced |
| Behaviour | nextest, doctests, SQLx tests | All required cases pass; expected outcomes have independent support |

Pin the scanners and their configuration. Record advisory freshness and
unsupported surfaces. A new external audit endpoint needs a data-flow
assessment and authorization. An unavailable scanner is a gap, not a pass.

---

## 15. Release, recovery, and maintenance

- **Configuration:** validate settings at startup, with safe diagnostics. Secrets come from the service variables, never from the image.
- **Migrations:** run the migration step before traffic reaches code that needs it; keep compatibility with running versions; do destructive steps only under an approved policy.
- **Observe:** after a release, verify the deployed revision, the health, and the post-release checks of `docs/ingest-release.md`.
- **Data:** each store has a documented purpose, retention, deletion policy, credentials, and access boundary (`VISION.md`, ADR 0002).
- **Recover:** keep a known working image and a declared rollback or roll-forward. Confirm schema compatibility before a code rollback. A backup file or a down migration alone is not recovery evidence.
- **Maintenance:** apply `DOCTRINE.md` P7. The table below records each infrastructure component with its version. Keep test and production on the same major version. Minimum release age: 1440 minutes for crates (ADR 0006), and pnpm `minimumReleaseAge` with the first web PR, except a security fix.

### Infrastructure and environments

One deployment environment: production. CI (§3) is a verification
environment; its pins are the rows that start with "CI" and the row "Test
PostgreSQL image".

| Component | Version or value | Notes |
| --------- | ---------------- | ----- |
| Railway | Hobby plan, project `calmdonut`, service `telemetry` | Builds `ingest/Dockerfile` from `main`. A merge to `main` that changes a watch path is a release. PID 1 is the Railway init `/run/podman-init -- /entrypoint.sh` as UID 0; `ingest` is PID 2 as UID 10001 (ADR 0001, "Process tree on Railway"). |
| Ingest server | `tesla/fleet-telemetry:v0.10.1` | Upstream binary, Go 1.27.4, dynamically linked against the glibc and libstdc++ of the base image. Triage: `docs/security/image-advisories.md`. |
| Ingest base image | `debian:trixie-slim` | Debian 13. The build runs `apt-get upgrade`, so the packages depend on the build date. |
| Ingest builder image | `rust:1.100.0-slim-trixie` | Rust 1.100.0 (`--profile minimal`) with gcc and libc6-dev. Only the `ingest` binary leaves the stage (§13). |
| Railway web service | Hobby plan, project `calmdonut`, service `telemetry-web` | Builds `webapp/Dockerfile` from `main` (ADR 0010). Public address: the custom domain `tesla.calmdonut.com`; the Railway edge ends TLS with a Let's Encrypt certificate. No TCP proxy. Created by the lead before the first web release (ADR 0004, grant 2). |
| Web base image | `debian:trixie-slim` | The same image as the ingest base; the build runs `apt-get upgrade`. The final stage installs no package. |
| Web builder images | `node:24.22.3-trixie-slim` (SPA) and `rust:1.100.0-slim-trixie` (server) | Only `web/dist` and the server binary leave the stages (§13). |
| Node.js | 24.22.3 (LTS), `mise.toml` and the web builder image | Build-time only; not a production runtime. |
| pnpm | 12.23.0, `mise.toml`; `npm install -g` in the web builder image | `pnpm-workspace.yaml` sets `minimumReleaseAge: 1440`. |
| Rust toolchain | 1.100.0, `rust-toolchain.toml` | Local, CI, and Railway builds. |
| Railway TCP proxy | `zephyr.proxy.rlwy.net:57438` to container port 4443 | The vehicle's signed config names host `telemetry.calmdonut.com` and port 57438. |
| Railway container | Kernel `6.12.12+bpo-cloud-amd64`; `tcp_retries2` 15; Yama `ptrace_scope` 0; open files 1048576 soft and hard | Listeners `0.0.0.0:4443`, `0.0.0.0:8080`, `127.0.0.1:4444`, `127.0.0.1:5284`, `127.0.0.1:9090`. `railway ssh` (UID 0) cannot read `/proc/<pid>/environ` of UID 10001. Read again after a platform change. |
| DNS | Cloudflare, `telemetry` CNAME, DNS only | A proxied record breaks the vehicle mTLS connection. |
| DNS of the web service | Cloudflare, `tesla` CNAME to the Railway target and the Railway verification record, DNS only | A proxied record lets Cloudflare read the battery data and the session cookie (`VISION.md`, forbidden data flows). |
| Ingest TLS certificate | Cloudflare Origin CA, ECC, valid until 22 November 2041 | Variables `TLS_CERT` and `TLS_KEY`. The Origin CA ECC root expires 15 August 2029. |
| Vehicle telemetry client | 1.3.0, vehicle firmware 2026.39.100 | From Tesla `fleet_status`. |
| Tesla Fleet API | Region `na` | The owner's account is in the NA region. |
| PostgreSQL | 18, Neon Free plan | Project `telemetry`, region `aws-eu-central-1`, database `neondb`, branch `production`. The branch `test` runs 18.7. |
| Neon production compute | Fixed 0.25 CU (autoscaling minimum and maximum 0.25) | Endpoint `ep-young-dawn-bgyaj83l`; `suspend_timeout_seconds` 0 (5 minutes). No connection pool (ADR 0001, lead decision 3). |
| PostgreSQL test database | Neon branch `test`, fixed 0.25 CU | Only for `test-neon` (ADR 0017). Holds no production data. At most 5 CU-hours each month (ADR 0002). |
| CI runner image | GitHub-hosted `ubuntu-26.04`, x86_64 | GitHub updates the image within the label; the log of each job shows its version. |
| CI mise | 2026.11.14 | `version` input of the mise action; the same as on the development machine. |
| CI action: checkout | `actions/checkout` v7.0.1, commit `3d3c42e5aac5ba805825da76410c181273ba90b1` | Full commit SHA. |
| CI action: mise | `jdx/mise-action` v5.3.0, commit `924e7c586e33d7d4bae9c14e26c5d22e2beb6e78` | Full commit SHA. |
| Test PostgreSQL image | `postgres:18.7-trixie`, index digest `sha256:64c8b0d31e4ab6ea665e8426ea406a94bf52de3c05b3a4a8bf713704eefa9328` | The one pin: `postgres_image` of the job `ci`, which the local test server reads too (`scripts/test-postgres.sh`); `linux/amd64` in CI, `linux/arm64` locally. The version of the Neon branch `test`. Not scanned (§13). |
| Apple `container` | 1.7.1 on the development machine; version 1 or later required | Runs the local test server. mise cannot pin it; `setup` checks it, and `deps` prints the installed version and the newest release. |

### Release rules of the ingest service

- The ingest service terminates mTLS itself, behind the Railway TCP proxy,
  never a Railway HTTP domain. A redeploy disconnects the vehicle for a
  short time; it reconnects by itself. Each Railway service has its own
  root directory and watch patterns.
- Layout (ADR 0001): one image; `ingest` supervises `fleet-telemetry`, owns
  port 4443, and forwards the vehicle connections only while it can take
  records. The container starts as root (no `USER` line); the entrypoint
  drops to user and group 10001 with no capabilities (`setpriv`) before it
  starts `ingest`.
- Build isolation: the image build reads `crates/ingest` in full and only
  the `Cargo.toml` of each other workspace member, with an empty
  `src/lib.rs` as a stub target. A member other than `ingest` must not
  declare a target path or a build script path. The deployment test checks
  that the stub list equals the other workspace members. A change of
  `Cargo.lock` or of a member `Cargo.toml` still releases ingest, because
  it can change the ingest binary.
- The service has no `railway.json` and no config file in its place. The
  lead sets each value of the table below by hand. Infrastructure as Code
  (`.railway/railway.ts`) waits for a reassessment in December 2026 with a
  `railway config plan` dry run; adopt it only if the plan does not touch
  the TCP proxy port and starts no deployment (ADR 0001, lead decision 7).
- Release steps, post-release checks, and recovery:
  [`docs/ingest-release.md`](docs/ingest-release.md). The minimum check:
  the deployment status is `SUCCESS`, `openssl s_client` verifies the
  certificate against the Origin CA ECC root on port 57438, and the log
  shows a vehicle connection when the vehicle is next awake.
- Build cache: the service variable `NO_CACHE` is `1` (table below), so
  Railway builds each image without its layer cache and the `apt-get`
  step installs the Debian security fixes of the build date. The Railway
  documentation, "Disable build layer caching": "By default, Railway will
  cache build layers to provide faster build times." and "set the
  following environment variable in your service: `NO_CACHE=1`"
  (<https://docs.railway.com/builds/build-configuration>). Cost: a build
  without the cache takes longer; the duration is not measured
  (assumption from the quoted text). Evidence after each build: the
  Railway build log shows that the `apt-get` step ran (not a cached
  layer) and the installed versions of the fixed packages; else build
  again and check that `NO_CACHE` is set.
- If the TCP proxy port changes (Railway does not document it as
  permanent), sign and send a new vehicle telemetry config
  (`config/telemetry_config.json`). That needs the Tesla application private
  key and a Tesla OAuth token, which stay on the owner's computer.
- Recovery: roll back to the previous successful Railway deployment
  (image and custom variables). Before a rollback to the image before ADR
  0001, clear the pre-deploy command and the healthcheck path; that image
  writes the VIN and the location to the log (`docs/ingest-release.md`,
  "Recovery").
- Image advisories: triage an expired ignore entry or a new advisory in a
  `chore/deps-<topic>` PR, with `docs/security/image-advisories.md` and the
  affected ignore files (ingest and web) together. A Debian fix for a library that the server loads
  (glibc, gcc-14) needs a rebuild of the image.
- The deployment test requires `zmq.addr` `tcp://127.0.0.1:5284` for the
  libzmq 4.3.4 listener of the `zmq` dispatcher.

#### Railway settings of the service `telemetry`

This table is the one documented place of the values. The lead sets them
without a deployment before the merge (`docs/ingest-release.md`, section
1); the next deployment uses them. The deployment test
(`crates/ingest/tests/deployment.rs`, module `railway`) holds them as
constants, checks them against the image and the code, and checks that this
table holds the same values. Source: the Railway documentation
(<https://docs.railway.com>).

| Setting                 | Value | Where the lead sets it | Reason |
| ----------------------- | ----- | ---------------------- | ------ |
| Root directory          | `/` (unchanged) | Service settings | The build context holds `Cargo.toml`, `crates/ingest/`, the `Cargo.toml` of each other member, `db/`, and `migrations/`; the root `.dockerignore` also admits the inputs of the web image, which no `COPY` line of `ingest/Dockerfile` reads. |
| Dockerfile path         | `/ingest/Dockerfile` | Service settings (`dockerfilePath`, relative form `ingest/Dockerfile`), not `RAILWAY_DOCKERFILE_PATH` | Without it, Railpack builds the workspace and no image of ADR 0001. |
| Watch paths             | `/ingest/**`, `/crates/ingest/**`, `/crates/*/Cargo.toml`, `/db/**`, `/migrations/**`, `/.cargo/**`, `/Cargo.toml`, `/Cargo.lock`, `/rust-toolchain.toml`, `/.dockerignore` | Service settings, one pattern each line | Each input of the image; another change starts no deployment. |
| Custom start command    | None | Service settings | The image `CMD` runs `/entrypoint.sh`; a start command would replace the `ENTRYPOINT`. |
| Pre-deploy command      | `/usr/local/bin/ingest migrate` | Service settings | Migrations as a separate release step (ADR 0002); one string; runs as root in its own container; a failure stops the deployment. |
| Pre-deploy timeout      | 1200 s | Service settings | Longer than the 15-minute deadline of `ingest migrate`; ends a command that never ends. |
| Healthcheck path        | `/health` | Service settings | 200 only when the gate is open and the start-up check passed. |
| Healthcheck timeout     | 300 s (the default) | Variable `RAILWAY_HEALTHCHECK_TIMEOUT_SEC`, or unset | Includes the first Neon wake-up and the settle time. |
| `PORT`                  | `8080` | Variable `PORT` | The port of `/health` and the healthcheck, apart from 4443, 4444, 5284, and 9090. |
| Draining time           | 30 s | Variable `RAILWAY_DEPLOYMENT_DRAINING_SECONDS` | SIGTERM to SIGKILL; the shutdown budget is 26 s. |
| Overlap time            | 0 s | Variable `RAILWAY_DEPLOYMENT_OVERLAP_SECONDS` | The vehicle reconnects by itself. |
| Restart policy          | Always | Service settings | No limit of 10 restarts (assumption: Hobby counts as a paid plan). |
| `NO_CACHE`              | `1` | Variable `NO_CACHE` | Each build runs the `apt-get` step without the layer cache, so the image gets the Debian security fixes of the build date. |
| TCP proxy               | `zephyr.proxy.rlwy.net:57438` to port 4443 (unchanged) | Service settings | The vehicle config names port 57438. |

The service variables `DATABASE_URL`, `MIGRATION_DATABASE_URL`, `TLS_CERT`,
and `TLS_KEY` hold secrets. The image `ENV` lines hold the other settings
(README.md, "Configuration"). Do not set these names as service variables:
a service variable replaces the image value. `ingest/Dockerfile` has no `ARG` line, so no secret reaches a
build step.

### Release rules of the web service

- The service `telemetry-web` runs the image of `webapp/Dockerfile`: the
  server binary `telemetry-web-server` and the built SPA, as user 10001
  (ADR 0010). It holds no data.
- The service reads the database as the role `telemetry_web` (ADR 0013,
  §5). It runs no migration. A migration that the web needs ships in an
  ingest release first.
- The image has a `USER 10001:10001` line, unlike the ingest image: the
  server process holds its own service variables anyway, so
  `/proc/1/environ` exposes nothing more (ADR 0010).
- The web watch paths and the ingest watch paths overlap only in the
  Cargo files, `/.cargo/**`, `/rust-toolchain.toml`, and `/.dockerignore`.
  A change of one of these releases both services.
- The service has no TCP proxy and no `up.railway.app` domain. Its only
  public address is the custom domain `tesla.calmdonut.com`, behind the
  password login (ADR 0003, ADR 0012). A public URL without access
  control is a sign of drift (`VISION.md`).
- Release steps, post-release checks, and recovery:
  [`docs/web-release.md`](docs/web-release.md). The build-log check of
  "Build cache" applies to this image too.

#### Railway settings of the service `telemetry-web`

This table is the one documented place of the values. The deployment test
of the server (`crates/server/tests/deployment.rs`, module `railway`) holds
them as constants, checks them against `webapp/Dockerfile` and the code,
and checks that this table holds the same values.

| Setting                 | Value | Where the lead sets it | Reason |
| ----------------------- | ----- | ---------------------- | ------ |
| Root directory          | `/` | Service settings | The build context holds the inputs of `webapp/Dockerfile`. |
| Dockerfile path         | `/webapp/Dockerfile` | Service settings (`dockerfilePath`, relative form `webapp/Dockerfile`), not `RAILWAY_DOCKERFILE_PATH` | Without it, Railpack builds the workspace. |
| Watch paths             | `/webapp/**`, `/web/**`, `/packages/**`, `/crates/server/**`, `/crates/domain/**`, `/crates/*/Cargo.toml`, `/Cargo.toml`, `/Cargo.lock`, `/.cargo/**`, `/rust-toolchain.toml`, `/package.json`, `/pnpm-lock.yaml`, `/pnpm-workspace.yaml`, `/.npmrc`, `/tsconfig.base.json`, `/.dockerignore` | Service settings, one pattern each line | Each input of the image; another change starts no deployment. |
| Custom start command    | None | Service settings | The image `CMD` runs `telemetry-web-server serve`. |
| Pre-deploy command      | None | Service settings | The web service applies no migration. |
| Healthcheck path        | `/health` | Service settings | 200 without a database, so a deployment does not wake Neon. |
| Healthcheck timeout     | 300 s (the default) | Variable `RAILWAY_HEALTHCHECK_TIMEOUT_SEC`, or unset | The server answers within 1 s of the start. |
| `PORT`                  | `8080` | Variable `PORT` | The port of the server and of the healthcheck; the image `EXPOSE` line names it. |
| Draining time           | 15 s | Variable `RAILWAY_DEPLOYMENT_DRAINING_SECONDS` | SIGTERM to SIGKILL; the shutdown deadline is 10 s. |
| Overlap time            | 0 s | Variable `RAILWAY_DEPLOYMENT_OVERLAP_SECONDS` | One version of the server at a time. |
| Restart policy          | Always | Service settings | No limit of restarts. |
| `NO_CACHE`              | `1` | Variable `NO_CACHE` | Each build runs the `apt-get` step without the layer cache, so the image gets the Debian security fixes of the build date. |
| `DATABASE_URL`          | Secret: a URL of the role `telemetry_web`; never in a document | Variable `DATABASE_URL` | The battery API reads the database (§5). The release build does not start without it. |
| TCP proxy               | None | Service settings | No TCP port for the internet. |
| Custom domain           | `tesla.calmdonut.com` | Service settings (the Railway edge ends TLS) | The only public address; the password login protects the data (ADR 0003, ADR 0012). |
| `WEB_PUBLIC_ORIGIN`     | `https://tesla.calmdonut.com` | Variable `WEB_PUBLIC_ORIGIN` | The origin of the CSRF rule (ADR 0012); the server stops at the start without it. |
| `WEB_PASSWORD_HASH`     | The Argon2id PHC string of the owner password; a secret | Variable `WEB_PASSWORD_HASH`, set by the owner | The login checks the password against it (ADR 0003, ADR 0012); the server stops at the start without a valid hash. |

The service variable `DATABASE_URL` holds a secret: a URL of the role
`telemetry_web`, database `neondb`, on the branch `production`. The
variable `RAILWAY_GIT_COMMIT_SHA`, which Railway sets, gives the
version that `/health` reports. The image `ENV` line sets `WEB_STATIC_DIR`;
do not set it as a service variable: a service variable replaces the image
value. `webapp/Dockerfile` has no `ARG` line, so no secret reaches a build
step.
The owner makes `WEB_PASSWORD_HASH` with `telemetry-web-server
hash-password` on the owner's computer (README, "Web password"); the hash
never passes through an agent, a PR, or a log.

### Lead authority (ADR 0004, ADR 0007)

- The lead may change and send the vehicle telemetry config when the
  estimated Tesla usage stays below 3 USD each month; the PR records the
  estimate.
- The lead may create a second Railway service in project `calmdonut` when
  the architecture needs it.
- The lead may create Neon branches of project `telemetry` as isolated test
  databases, and an application role with limited privileges
  (`telemetry_ingest`; the migration role `telemetry_migrator`, ADR 0001,
  lead decision 2).
- The lead may ask Codex for a read-only second review of a single
  security-critical PR (ADR 0007).

### Operating values

The production compute is fixed at 0.25 CU without a connection pool, the
flush interval is 900 s, the vehicle config has no `delivery_policy`, and
`zeromq` stays at 0.7.0 with the stall detection (ADR 0001, "Lead
decisions" and "Decisions of the pipeline change").

### Accepted risks

- **Migrator URL as a service variable.** It stays readable in the
  pre-deploy container and to anything that reads the service variables
  (ADR 0002, "Roles and privileges").
- **Environment of the init.** The init runs as root; an image that starts
  as user 10001 exposes the service variables in `/proc/1/environ` again
  (ADR 0001, "Process tree on Railway").
- **Note N1.** `fleet-telemetry` can read `DATABASE_URL` in the environment
  of `ingest` (ADR 0001, "Process tree on Railway").
- **Entrypoint and pre-deploy command as root** (ADR 0001, "Process tree on
  Railway"). Open issue #31: a service variable named `cert` or `key` would
  reach the environment of `ingest`.
- **Gate.** No handshake deadline; a flood that fills the 1024 connections
  blocks the vehicle while it lasts plus the dead-peer bound, below the
  vehicle buffer (ADR 0001, lead decision 6).
- **Queue loss** at a restart during a long database outage (§9).
