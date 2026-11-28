# Telemetry

A private service that collects the telemetry of one Tesla vehicle and turns
it into a history of drives, charging sessions, and parked periods.

The vehicle streams its data with Tesla
[Fleet Telemetry](https://github.com/teslamotors/fleet-telemetry). This
repository contains the ingest service that receives the stream and
stores it, and the web application that shows it.

## Status

| Part                | State                                                             |
| ------------------- | ----------------------------------------------------------------- |
| Ingest service      | Deployed: the image of ADR 0001. The vehicle connects. `config/telemetry_config.json` selects 201 fields ([usage estimate](docs/telemetry-usage-estimate.md)); the vehicle streams the config that the owner sent last. No record reaches the Railway log. |
| Rust ingest process | Live. `ingest/Dockerfile` builds the image of ADR 0001: `ingest` starts `fleet-telemetry`, receives the records over ZMQ, and writes them to PostgreSQL. On Railway an init is PID 1 and `ingest` is PID 2. A merge to `main` that changes a watch path releases it; [STACK.md](STACK.md) section 15 has the release steps and the post-release checks. |
| Storage             | Live on the Neon branch `production` (PostgreSQL 18): schema `raw`, migrations, and writer in `crates/ingest` (ADR 0002). `ingest migrate` runs as the Railway pre-deploy command. |
| Application and web | Live: the web server (`crates/server`), the SPA (`web/`) with the password login (ADR 0012) and the battery view (ADR 0013), the generated API client, and the image `webapp/Dockerfile` (ADR 0010). The Railway service `telemetry-web` serves the custom domain `tesla.calmdonut.com`. See [VISION.md](VISION.md) for the product direction. |

## How it works

```text
Tesla vehicle
   │  mTLS WebSocket, port 57438
   ▼
telemetry.calmdonut.com  (DNS only, CNAME to the Railway TCP proxy)
   │  raw TCP; the proxy does not terminate TLS
   ▼
ingest                   (Rust: the gate on port 4443)
   │  127.0.0.1:4444
   ▼
fleet-telemetry          (Tesla's upstream server, child of ingest)
   │  ZMQ on 127.0.0.1:5284
   ▼
ingest → PostgreSQL      (Neon; planned: application and web)
```

- The vehicle authenticates with a client certificate from Tesla. The ingest
  service terminates mTLS itself.
- The vehicle trusts the server certificate through the CA in its telemetry
  config. The service uses a Cloudflare Origin CA certificate.
- A telemetry config tells the vehicle where to connect and which signals to
  send. The config is signed with the Tesla application private key.

### The Rust ingest process

The image of ADR 0001 runs the Rust process `ingest`. It starts
`fleet-telemetry` as its child and owns the public port. On Railway an init
that Railway adds is PID 1, and `ingest` is PID 2; under Docker (the CI job
`image-check`) `ingest` is PID 1 (ADR 0001, "Process tree on Railway"):

```text
vehicle ──TCP──▶ gate (ingest, port 4443) ──▶ fleet-telemetry (127.0.0.1:4444)
                                                  │ ZMQ PUB 127.0.0.1:5284
                                                  ▼
                      reader ──▶ queue (64 MiB) ──▶ writer ──▶ PostgreSQL (Neon)
                                                  ▲
                      shortfall check ◀── /metrics 127.0.0.1:9090
```

- The **gate** copies the bytes of each vehicle connection to
  `fleet-telemetry`. It is open only when the child runs, the ZMQ
  subscription has settled, the database check has passed (or degraded
  mode applies), and the queue is below its high mark. A closed gate makes
  the vehicle keep its data in its own buffer.
- The **reader** decodes each ZMQ message and puts it into the **queue**
  with its original bytes. A full queue drops the newest message and
  records the gap.
- The **writer** checks the migrations, then writes the queue every 15
  minutes, at 5,000 signal rows, or when the queue reaches 75 %. It retries
  a failed write with a growing delay and never gives up while the process
  runs.
- The **shortfall check** compares the `dispatch_total` counter of
  `fleet-telemetry` with the received messages every 15 s and records a
  gap of the ZMQ path. When the child dispatches records and the reader
  receives none, the reader replaces its ZMQ socket (ADR 0001, "SUB
  socket").
- On SIGTERM the process closes the gate, drains the ZMQ socket, stops the
  child, and writes the queue and the `stopped` event within 26 s.
- `/health` on `PORT` answers 200 only when the gate is open and the
  database check has passed.
- `raw.ingest_event` records the start, the stop, the gate changes, the
  child exits, the drops, the shortfalls, and degraded mode. The log never
  holds a VIN, a location, a value, a payload, or a database URL.

## Repository layout

| Path                           | Content                                                        |
| ------------------------------ | -------------------------------------------------------------- |
| `ingest/Dockerfile`            | Image of the service: `ingest` and `fleet-telemetry` (ADR 0001) |
| `ingest/entrypoint.sh`         | Start command: writes the TLS files, then starts `ingest run`  |
| `ingest/server_config.json`    | Configuration of `fleet-telemetry`                             |
| `.dockerignore`                | Allowlist of the build inputs of `ingest/Dockerfile` and `webapp/Dockerfile` |
| `config/telemetry_config.json` | Telemetry config for the vehicle: host, port, CA, and signals  |
| `crates/ingest/`               | Rust ingest process                                            |
| `crates/ingest/.sqlx/`         | Query metadata of the SQLx macros of `ingest`                  |
| `crates/domain/`               | Package `telemetry-web-domain`: web domain rules; empty now   |
| `crates/server/`               | Package `telemetry-web-server`: HTTP server, problem mapping, static files, contract export (ADR 0010) |
| `contract/openapi.json`        | Committed OpenAPI 3.1 export of the server                     |
| `packages/api-client/`         | Generated client and Zod schemas (`src/generated/`, never edited) and the facade |
| `web/`                         | The SPA: Vite, React, TanStack Router and Query                |
| `e2e/`                         | Playwright journeys against the test server                    |
| `webapp/Dockerfile`            | Image of the web service: server and built SPA (ADR 0010)      |
| `package.json`, `pnpm-workspace.yaml`, `pnpm-lock.yaml` | pnpm workspace and locked npm packages |
| `migrations/`                  | SQLx migrations of the raw schema (ADR 0002)                   |
| `db/roles.sql`                 | Database roles, schema, and privileges (ADR 0002)              |
| `scripts/sqlx-metadata.sh`     | Checks or writes `crates/<crate>/.sqlx/` against a test database |
| `scripts/image-check.sh`, `scripts/image-check-web.sh` | Build the images and check the containers (CI job `image-check`) |
| `scripts/contract-check.sh`    | Compares the committed contract and client with a new export   |
| `scripts/bundle-size.sh`       | Checks the gzip size of the initial JS and CSS of the SPA      |
| `Cargo.toml`, `Cargo.lock`     | Rust workspace, lints, and locked dependencies                 |
| `rust-toolchain.toml`          | Rust toolchain version                                         |
| `mise.toml`                    | Tool pins and the project commands                             |
| `clippy.toml`, `deny.toml`     | Clippy and cargo-deny settings                                 |
| `VISION.md`                    | Product intent, non-goals, and privacy posture                 |
| `STACK.md`                     | Technology, commands, infrastructure, and release rules        |
| `DOCTRINE.md`                  | Quality principles P1–P9                                       |
| `CLAUDE.md`                    | Operating contract for Claude Code                             |

## Requirements

- A Tesla developer application with a public key hosted on its domain, and
  the matching virtual key paired with the vehicle.
- A host that forwards raw TCP to the container. An HTTP proxy that
  terminates TLS does not work.
- A DNS name for the ingest service and a TLS certificate for that name.

## Configuration

The start command `ingest/entrypoint.sh` reads two variables. It writes
them to files with mode 0600 for `fleet-telemetry`, removes them from the
environment, and starts `ingest run`.

| Variable   | Content                               |
| ---------- | ------------------------------------- |
| `TLS_CERT` | Server certificate chain, PEM format  |
| `TLS_KEY`  | Server private key, PEM format        |

`ingest run` reads these variables and stops at the start when one is
missing or not valid (`crates/ingest/src/config.rs`). The `ENV` lines of
`ingest/Dockerfile` set the fixed values. The Railway service variables
hold the other values. Do not set an image value as a Railway variable:
a service variable replaces the value of the image.

| Variable                        | Content                                                        | Source           |
| ------------------------------- | -------------------------------------------------------------- | ---------------- |
| `PORT`                          | Port of `/health`: 8080                                        | Railway variable |
| `INGEST_GATE_PORT`              | Public port of the gate: 4443                                  | Image            |
| `INGEST_UPSTREAM_PORT`          | Loopback port of `fleet-telemetry`: 4444                       | Image            |
| `INGEST_ZMQ_PORT`               | Loopback port of the ZMQ PUB socket of `fleet-telemetry`: 5284 | Image            |
| `INGEST_METRICS_PORT`           | Loopback port of its Prometheus metrics: 9090                  | Image            |
| `INGEST_CHILD_PROGRAM`          | Absolute path of the `fleet-telemetry` program                 | Image            |
| `INGEST_CHILD_CONFIG`           | Absolute path of its configuration file                        | Image            |
| `DATABASE_URL`                  | URL of the role `telemetry_ingest`; a secret                   | Railway variable |
| `INGEST_FLUSH_INTERVAL_SECONDS` | Optional: the flush interval, 1 to 900, default 900            | Not set          |

`ingest migrate` reads `MIGRATION_DATABASE_URL`, the URL of the role
`telemetry_migrator` (a Railway variable). Variables that start with
`INGEST_TEST_` shorten limits for the end-to-end tests. Only a debug build
reads them, and the image holds a release build.

Local files that must never be committed. `.gitignore` excludes them.

| Path                     | Content                                               |
| ------------------------ | ----------------------------------------------------- |
| `.env`                   | Short-lived Tesla OAuth access token                  |
| `.env.test`              | `TEST_DATABASE_URL` of the Neon branch `test`         |
| `secrets/tesla_fleet.key`| Tesla application private key                         |
| `secrets/tls.*`          | Server certificate and key                            |
| `.bin/`                  | Locally built Tesla tools                             |

## Web password

One password protects the web service `telemetry-web` (ADR 0003, ADR
0012). The service holds only an Argon2id hash of the password in the
Railway variable `WEB_PASSWORD_HASH`. The owner makes the hash on the
owner's computer; the password and the hash never pass through an agent,
a PR, an issue, or a log.

1. Make a long random password in the password manager. Use 20 or more
   characters. The command accepts 16 to 1024 bytes.
2. Copy the password to the clipboard. In the repository on macOS, run
   these commands. They make the hash and set the variable without a
   deployment. Neither the password nor the hash appears on the screen,
   in the shell history, or in the process list. `&&` stops the second
   step when `hash-password` fails; its error goes to standard error:

   ```sh
   HASH="$(pbpaste | cargo run --quiet --locked -p telemetry-web-server -- hash-password)" \
     && printf '%s' "$HASH" \
     | railway variable set -s telemetry-web -e production --skip-deploys --stdin WEB_PASSWORD_HASH
   unset HASH
   ```

   On another system, replace `pbpaste` with `(IFS= read -rs PW &&
   printf '%s' "$PW")`: `read -s` reads without an echo, and `printf` is a
   shell builtin.
3. Clear the clipboard.
4. The new value applies at the next deployment of the service. A
   deployment ends every session, so log in again afterwards.

`hash-password` removes one line end at the end of the input. Every other
character, also a space, is part of the password. Its output is one line
that starts with `$argon2id$v=19$m=65536,t=3,p=1$`. The shell command
substitution `$(...)` removes its line end, and `printf '%s'` adds none.

The server stops at the start when the hash is missing or outside the
accepted parameters (`crates/server/src/auth/password.rs`). The error
names the variable and the reason, never the value.

To change the password, repeat the steps. To end every session without a
password change, redeploy or restart the service.

## Deployment

Railway builds `ingest/Dockerfile` from `main` and deploys it. A merge to
`main` that changes a watch path is a release. The pre-deploy command
`ingest migrate` applies the migrations first; a failure stops the
deployment. A redeploy disconnects the vehicle for a short time. The
vehicle reconnects by itself and buffers its data meanwhile.

[STACK.md](STACK.md) section 15 lists the infrastructure, the Railway
settings, the release steps, the post-release checks, and the recovery
path.

## Change the vehicle telemetry config

The vehicle accepts a config only when the Tesla application private key
signs it. Tesla's
[`tesla-http-proxy`](https://github.com/teslamotors/vehicle-command) signs
the request. The owner does every step on the owner's computer. The
private key and the OAuth token stay there. Do not put a token, a key, or
the VIN in a file of this repository, a PR, or an issue.

Each signal that the vehicle sends counts against the Tesla usage of the
application, together with Home Assistant. When usage exceeds the billing
limit, Tesla removes the config from the vehicle, does not restore it, and
suspends the API also for Home Assistant (ADR 0018).

### Before the first send of a larger config

1. Open the usage page of the Tesla developer dashboard
   (`/dashboard/usage`). Read the usage of the current and the last month.
   The usage is Home Assistant plus the config that the vehicle streams
   now.
2. Add a payment method: "Manage Payment" on the same page. The default
   billing limit is 0, and Tesla allows a higher limit only with a payment
   method.
3. Set the billing limit: "Update Limit". The limit is 20 EUR each
   month. Tesla does not say whether the limit counts the usage before or
   after the monthly discount of 10 USD. The limit stays far above the
   highest modelled month in both readings: the noise case of the extreme
   month in [the usage estimate](docs/telemetry-usage-estimate.md) plus
   Home Assistant is about 11.9 USD of usage. Keep the limit when the
   dashboard shows the usage (ADR 0018).

### Send the config

1. Get a new Tesla OAuth access token. It expires in about 8 hours. Put
   it in `.env` (see "Configuration", local files). Load it with
   `set -a; . ./.env; set +a`, and give the commands the variable, for
   example `-H "Authorization: Bearer $<variable of .env>"`. Never type
   the token on a command line, so that it stays out of the shell
   history.
2. Start `tesla-http-proxy` locally with the application private key.
3. Send `POST /api/1/vehicles/fleet_telemetry_config` through the proxy
   with the VIN and `config/telemetry_config.json` of the merged `main`.
   The car does not need to be awake.
4. Wake the car once with the Tesla phone app, for example by opening the
   app. The vehicle adopts the config when it next connects. Do not use
   the API `wake_up` endpoint: Tesla bills each wake.
5. Read `GET /api/1/vehicles/{vin}/fleet_telemetry_config` until `synced`
   is `true`.
6. Read `GET /api/1/vehicles/{vin}/fleet_telemetry_errors`. The list must
   be empty.
7. Check that rows with new field numbers arrive in `raw.vehicle_signal`.

### Roll back

Send the previous config from Git with the same steps, for example
`git show <commit>:config/telemetry_config.json`. Do not use
`DELETE /api/1/vehicles/{vin}/fleet_telemetry_config`: it stops all
collection.

### Checks after the send

- After two days: read the usage on the dashboard. The usage since the
  send must stay near the planning month of the usage estimate
  (3.33 USD for a full month). If it grows much faster, roll back and
  report it.
- After one week: compare the awake hours of the week before and the week
  after the send (the connect and disconnect times in
  `raw.connectivity_event`). The car must sleep as before (`VISION.md`,
  "Success Definition"). If it sleeps less, roll back and report it.
- After the first full month: compare the dashboard usage with the
  planning month of the usage estimate, and read the bill. Report a
  difference to the lead, so that the estimate gets the measured value.

## Verification

The project commands are mise tasks. Install
[mise](https://mise.jdx.dev/) and [rustup](https://rustup.rs/), then run:

```sh
mise trust
mise run setup
mise run verify
```

`mise run verify` runs the database tests on a new local PostgreSQL
server in [Apple `container`](https://github.com/apple/container) (version
1 or later, with its service started: `container system start`). It needs
no database URL. `mise run setup` checks the tool and pulls the image.

`mise run verify` is the single local gate. During development, run
`mise run light`: the fast checks and the tests without a database.
[STACK.md](STACK.md) section 3 lists the tasks and what each one checks. `mise run setup` also installs
Node.js and pnpm (pinned in `mise.toml`), the npm packages, and the
Playwright browser (Chromium).

After a change of an HTTP type or route, export the contract and generate
the client again, and commit both:

```sh
mise run contract
```

The web server reads `PORT`, `WEB_STATIC_DIR` (the image sets it), and the
optional `RAILWAY_GIT_COMMIT_SHA` (the version). The test server serves
HTTPS and also reads `WEB_TEST_TLS_CERT` and `WEB_TEST_TLS_KEY`: the PEM
files of its certificate and key. `e2e/tls-cert.sh` makes a self-signed
certificate for `localhost` that is valid for 2 days, in the ignored
directory `e2e/.tls/`. To run the test server with the built SPA locally:

```sh
cargo build --locked -p telemetry-web-server --features test-server
pnpm run build:web
sh e2e/tls-cert.sh e2e/.tls
PORT=4173 WEB_STATIC_DIR=web/dist WEB_TEST_TLS_CERT=e2e/.tls/cert.pem \
  WEB_TEST_TLS_KEY=e2e/.tls/key.pem target/debug/telemetry-web-server serve-test
```

Then open <https://localhost:4173> and accept the self-signed certificate.

The GitHub Actions workflow `.github/workflows/ci.yml` also runs
`mise run ci` on linux/amd64 for each pull request that is not a draft and
each push to `main`. Its job `image-check` builds both images and checks
the running containers (`scripts/image-check.sh`,
`scripts/image-check-web.sh`) with Docker, and its job `vuln` runs
`mise run vuln`. On a pull request, these two jobs run only when one of
their inputs changes (`scripts/ci-select.sh`). CI does not replace the
local gate.

`scripts/test-postgres.sh` starts the test server, locally and in CI
(STACK.md section 3, ADR 0017). Each run gets a server of its own with a
new test CA, and every connection verifies the server certificate
(`sslmode=verify-full`).

Two role tests need Neon. Run them with `mise run test-neon` after a
change of the storage code, `db/`, `migrations/`, or a TLS crate, and
before an ingest release. The task needs `NEON_TEST_DATABASE_URL`: the URL
of the role `neondb_owner` on the Neon branch `test` of the project
`telemetry`. Keep it in `.env.test` or another local file; never commit
it:

```sh
set -a; . ./.env.test; set +a   # exports NEON_TEST_DATABASE_URL; the value must be in quotes
mise run test-neon
```

The tests refuse the production endpoint. Each test works in a schema of
its own and removes it, so two runs can share the branch.

After a change of a query or a migration, write the query metadata again
and commit the `.sqlx/` directory of each crate with queries (for example
`crates/ingest/.sqlx/`):

```sh
mise run sqlx-prepare
```

## Database roles

The ingest process uses two database roles (ADR 0002, "Roles and
privileges"): `telemetry_migrator` owns the schema `raw` and runs
`ingest migrate`; `telemetry_ingest` runs `ingest run` and can only read
and insert. The web service uses a third role, `telemetry_web`. It can
only read, and only the columns that the web uses. Each grant is a
`SELECT` on a list of columns, never on a whole table. The array
`web_columns` in `db/roles.sql` holds the list.

The web role cannot read the location (`latitude`, `longitude`,
`json_value` of `vehicle_signal`), the VIN, `unprocessed_record`,
`vehicle_alert`, or `vehicle_error`. A column that a migration adds
stays unreadable for the web role.
`db/roles.sql` creates the three roles and the schema. Run it once
on each Neon branch that runs the service, before the first deployment
that runs `ingest migrate`. The script is idempotent. A run changes the
password and the LOGIN attribute of a role only when the run gives the
password of that role. A role without a given password keeps its
password and its LOGIN attribute. A role that a run creates without a
password cannot log in until a later run gives its password.

The script grants the web role its table privileges only on tables that
exist. On a new branch, the tables exist only after `ingest migrate`.
Run the script again after the first `ingest migrate`. Each run
removes every other privilege of the web role in the schema and grants
the exact set again, in one transaction.

A new column or table in a query of the web needs a new grant: add it
to `web_columns` in `db/roles.sql`, and run the script again after the
migration that creates it. A later view that shows the route reads the
location. That view needs its own grant and its own review.

The procedure needs no `psql`. It runs the script through the same Rust
code that the tests use:

1. Read the owner URL of the branch with the Neon CLI (`neon`, the global
   mise tool):

   ```sh
   export OWNER_DATABASE_URL="$(neon connection-string production \
     --project-id <project-id> --role-name neondb_owner \
     --database-name neondb --ssl verify-full)"
   ```

   Then generate a password only for each role that needs a new
   password. Each variable that you set changes the password of its
   role, and the service that uses the role cannot connect until its
   Railway variable has the new password (step 3).

   - The first run on a new branch: all three roles.

     ```sh
     export TELEMETRY_INGEST_PASSWORD="$(openssl rand -hex 32)"
     export TELEMETRY_MIGRATOR_PASSWORD="$(openssl rand -hex 32)"
     export TELEMETRY_WEB_PASSWORD="$(openssl rand -hex 32)"
     ```

   - The web role only, for example its first run on a branch where the
     ingest roles exist: set only `TELEMETRY_WEB_PASSWORD`.

     ```sh
     export TELEMETRY_WEB_PASSWORD="$(openssl rand -hex 32)"
     ```

   - A run after a migration: no password variable. The run needs only
     `OWNER_DATABASE_URL`.

   A password must have 32 to 128 letters and digits, so it needs no
   escape in a URL; the program refuses other passwords. An unset
   variable keeps the password and the LOGIN attribute of its role.

2. Run the script:

   ```sh
   cargo run --locked -p ingest --example setup_roles
   ```

   The program logs one JSON line: `roles_applied`, or `roles_failed` with
   a static class and code. It never logs the URL or a password. The
   passwords reach the server as plain text, because Neon accepts no
   other form. They are bound values of a `set_config()` statement and
   then part of an `ALTER ROLE` statement, so a Neon server log can show
   them (ADR 0002). After a suspected leak, repeat the procedure with new
   passwords.

3. Set the Railway variable of each role whose password changed in
   step 1, with the host of the owner URL. A role without a new
   password keeps its variable. Use the direct host, not the `-pooler`
   host: the connections set the `search_path` at start, and
   `ingest migrate` takes a session lock. Set the variables with
   `railway variable set --skip-deploys`, so that no deployment starts
   (`docs/ingest-release.md`, section 1).

   - Service `telemetry`, ingest role:
     `DATABASE_URL=postgresql://telemetry_ingest:<ingest password>@<host>/neondb?sslmode=verify-full`
   - Service `telemetry`, migrator role:
     `MIGRATION_DATABASE_URL=postgresql://telemetry_migrator:<migrator password>@<host>/neondb?sslmode=verify-full`
   - Service `telemetry-web`, web role:
     `DATABASE_URL=postgresql://telemetry_web:<web password>@<host>/neondb?sslmode=verify-full`

4. Unset the variables of step 1 and close the shell.

## Contributing

This is a personal project. Work follows the contracts in this repository:

- Each change starts from a GitHub issue or a direct task.
- Each change uses a `feat|fix|chore|docs/<topic>` branch and a pull
  request. Nothing is pushed directly to `main`.
- Commits use [Conventional Commits](https://www.conventionalcommits.org/).
- Material changes get an independent review before merge.
- Pull requests merge with a merge commit.

[CLAUDE.md](CLAUDE.md) and [DOCTRINE.md](DOCTRINE.md) contain the full rules.

## License

No license is granted. All rights reserved.
