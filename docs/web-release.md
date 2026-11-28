# Release of the web image

This runbook is for the lead. It releases the image of ADR 0010
(`webapp/Dockerfile`) to the Railway service `telemetry-web`, checks the
release, and recovers from a failed release. STACK.md section 15 holds the
Railway settings in the table "Railway settings of the service
`telemetry-web`". This runbook does not repeat the values of that table;
the deployment test of the server (`crates/server/tests/deployment.rs`)
checks the table against the image and the code.

Rules:

- Do not put a password, a password hash, a session token, a database URL,
  a VIN, a location, or a signal value into a PR, an issue, or a commit.
- `railway` commands use the service `telemetry-web` in the environment
  `production` (`-s telemetry-web -e production`). The commands below
  leave out these two flags.
- The web service has no TCP proxy and no `up.railway.app` domain. Its
  only public address is the custom domain `tesla.calmdonut.com`, which
  the lead attaches when the login exists (slice S5). Until then, the
  service is not reachable from the internet.
- The web service reads the database as the role `telemetry_web`
  (ADR 0013). It runs no migration. A migration that the web
  needs ships in an ingest release first (`docs/ingest-release.md`).
- A merge to `main` that changes a watch path of the service is a web
  release. A merge that changes `Cargo.lock` or a member `Cargo.toml` is
  also an ingest release (`docs/ingest-release.md`).

## 1. Before the merge

1. Confirm that the PR has all acceptance evidence: the local
   `mise run verify`, green `ci`, `vuln`, and `image-check` jobs of the
   current PR head, and the PASS of the independent review.
2. If the service does not exist: create the service `telemetry-web` in
   the project `calmdonut` from the GitHub repository, branch `main`
   (ADR 0004, grant 2). Do not generate a Railway domain. Do not add a
   TCP proxy.
3. Before the first release that reads the database: create the role
   `telemetry_web` on the branch `production`. Use the procedure of
   README.md, "Database roles", and set only `TELEMETRY_WEB_PASSWORD`
   (the web-only run). The script then does not change the password or
   the LOGIN attribute of `telemetry_ingest` and `telemetry_migrator`, so
   the ingest service can still log in. Expected: one JSON line
   `roles_applied`. Run the script once, from a checkout of the head of
   the PR to merge. Then check `pg_roles`: `telemetry_web` has LOGIN, and
   the ingest roles keep their LOGIN. Do not run the first-run case of
   `docs/ingest-release.md`, section 1, step 2 for this release: it
   rotates the ingest passwords. The script is idempotent. Run it again
   without a password variable after a migration that adds a table or a
   column that the web reads.
4. Set the service variables without a deployment. The values are the
   rows of the table "Railway settings of the service `telemetry-web`"
   whose column "Where the lead sets it" names a variable. Use
   `railway variable set --skip-deploys <NAME>=<value>`. Do not set
   `WEB_STATIC_DIR`: the image sets it, and a service variable replaces
   the image value.
   - The server stops at the start without a valid `WEB_PUBLIC_ORIGIN`
     and `WEB_PASSWORD_HASH` (ADR 0012). Both must exist before the merge
     of a version with the login.
   - The owner sets `WEB_PASSWORD_HASH` (README, "Web password"). The
     lead does not ask for the value and does not read it back. To check
     that the variable exists, list only the names:
     `railway variable list --json | jq -r 'keys[]'`.
   - `DATABASE_URL` is a secret:
     `postgresql://telemetry_web:<web password>@<host>/neondb?sslmode=verify-full`,
     with the direct host of the owner URL of step 3, not the `-pooler`
     host. Do not write the value into a document, a PR, or a log. Set it
     from standard input, so that the value does not reach the shell
     history: `railway variable set --skip-deploys --stdin DATABASE_URL`.
   - The release build does not start without `DATABASE_URL`. The log
     has the event `invalid_configuration` with the field `name`
     `DATABASE_URL` and the field `reason` `missing`. The healthcheck
     then fails, and Railway keeps the old deployment.
5. Set the service settings: the rows of the same table whose column
   "Where the lead sets it" names the service settings (the watch paths
   one pattern each line). Apply them without a deployment, as in
   `docs/ingest-release.md`, section 1, step 4.
6. Read the settings back (`railway service` or the Railway MCP tool
   `get-service-config`) and compare each value with the table. For
   `DATABASE_URL`, confirm only that the variable exists. Do not print
   its value.

Order of the steps:

- Steps 3 and 4 come before the merge of the first version that reads
  the database. Without `DATABASE_URL`, the new deployment fails at
  start-up, and Railway keeps the old deployment.
- With `DATABASE_URL`, `GET /api/v1/battery` answers only a request with
  a valid session (the session guard of ADR 0012); without a session it
  answers 401 and reads no database.
- The service gets no public domain before the login works (STACK.md
  section 15).

## 2. The merge

Merge the PR with a merge commit. Watch the deployment:

| Phase       | Command                                | Expected                                                                                                                                             |
| ----------- | -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Build       | `railway logs --build <deployment id>` | The `apt-get upgrade` step runs (not `CACHED`) and lists the installed versions. A `CACHED` step means that the variable `NO_CACHE` of the table is missing or wrong: set it and build again (STACK.md section 15, "Build cache"). `pnpm run build:web` and `cargo build --release --locked -p telemetry-web-server` end without error. |
| Healthcheck | Railway deployment view                | The healthcheck on `/health` passes; the status is `SUCCESS`.                                                                                        |
| Start       | `railway logs <deployment id>`         | One JSON line `listening` with the port. The version is the value of `/health` (section 3).                                                         |

If the healthcheck fails, Railway keeps the old deployment. The first
deployment has no old deployment: the service then has no running
deployment, which affects no other service. Read the deploy log, fix the
cause in a new PR, and merge again.

## 3. Post-release checks

Record each result in the PR. A check that cannot run yet is "pending"
with its reason.

| Check                  | Command                                                                                     | Expected                                                                                                     |
| ---------------------- | ------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Deployed revision      | `railway ssh -- sh -c 'echo $RAILWAY_GIT_COMMIT_SHA'`, or the deployment view               | The merge commit SHA.                                                                                        |
| Health and version     | `railway ssh -- perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new("127.0.0.1:$ENV{PORT}") or die; print $s "GET /health HTTP/1.0\r\n\r\n"; print <$s>'` | `200` and `{"status":"ok","version":"<merge commit SHA>"}`. Without a public domain, the check runs inside the container. |
| User of the process    | `railway ssh -- grep -e ^Uid -e ^Gid /proc/1/status /proc/[0-9]*/status`                    | The server process has `Uid` and `Gid` 10001.                                                                |
| No secret in the log   | `railway logs --since 1h`                                                                   | No line holds a variable value, a password, a hash, or a cookie. Record only the count of such lines.       |
| API without a session  | `railway ssh -- perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new("127.0.0.1:$ENV{PORT}") or die; print $s "GET /api/v1/session HTTP/1.0\r\n\r\n"; print <$s>'` | `401` and a problem with the code `unauthenticated`. No database query runs. |
| Battery API without a session | `railway ssh -- perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new("127.0.0.1:$ENV{PORT}") or die; print $s "GET /api/v1/battery HTTP/1.0\r\n\r\n"; print <$s>'` | `401` and a problem with the code `unauthenticated`. The session guard answers before the battery handler, so no database query runs. |
| Back-forward cache (owner, S5) | After the login, the owner opens another site in the same tab, logs out in a second tab, and goes Back in the first tab | The login page, not the earlier data (the `pageshow` reload of ADR 0012, decision 14; no browser test covers this path). Pending until the domain exists. |
| Login (owner, S5)      | After the custom domain exists, the owner opens `https://tesla.calmdonut.com/`, logs in, sees the start page (the battery view `/battery`), and logs out | The login page, the battery view after the login, and the login page after the logout. The log has one `login_succeeded` and one `logout` line. Pending until the domain exists; the release verification stays pending until the owner reports the result. |
| Public checks (S5)     | After the custom domain exists: `curl -sI https://tesla.calmdonut.com/` and `/health`        | TLS, the security headers of ADR 0010, `200` for `/health`, and the SPA. Pending until the domain exists.     |
| Battery API read time  | Section 4                                                                                   | The read times are in the budgets of STACK.md section 4. Pending until the domain and the login exist.        |

## 4. Battery API read time

This check is the evidence for the row "Battery API, cold Neon" of
STACK.md section 4. Run it after each release that changes the battery
read, and after a change of the Railway region or the Neon region.

Privacy rules of the measurement:

- Use only the request log lines of the server and the HTTP status. A
  request log line holds the fields `method`, `route` (the route
  template), `status`, and `latency_ms`.
- Never record a response body, a signal value, a VIN, or a location.
- The record of the measurement holds only the range key, the status,
  and the times.

Who and when: the owner runs the measurement, because only the owner
can log in with the owner password. The service holds a placeholder hash
until the owner sets the real `WEB_PASSWORD_HASH` (README, "Web
password"). Run the measurement after the owner has set the hash, the
custom domain works, and the first release that reads the database is
live. The lead prepares the commands and records the result.

Make each request in one of two ways:

- In the browser, while logged in: open the battery view and select the
  range.
- With `curl` and the session cookie of a login:

  ```sh
  curl -s -o /dev/null -w '%{http_code} %{time_total}\n' \
    -b '__Host-telemetry_session=<session token>' \
    'https://tesla.calmdonut.com/api/v1/battery?range=<range key>'
  ```

  `__Host-telemetry_session` is the session cookie of ADR 0012. Copy
  the token from the browser of the logged-in owner. Do not put the token
  into a document, a PR, or a log.

Read the server times with `railway logs --since 1h`. Select the lines
with the route `/api/v1/battery` and read `status` and `latency_ms`.

Cold read:

1. Wait until the Neon production compute is suspended. The Neon console
   shows the compute as idle and suspended. The car must be asleep, so
   that the ingest service does not wake the compute.
2. Make one request with the range key `30d`.
3. The pass condition is a status of `200` and a time of at most 8 s.
   Use the `time_total` of `curl` if available, else `latency_ms`.

When a check fails:

1. Record the range keys, the statuses, and the times in the release PR
   or in an issue. Do not record a value of the data.
2. Report the result to the owner.
3. Keep the service running. The read stays correct; only the time is
   longer than the budget.
4. The next step is fewer round trips (ADR 0013), not an
   index.

## 5. Recovery

Use a rollback in the Railway deployment view. Railway restores the image
and the custom variables of the selected deployment
(<https://docs.railway.com/deployments/deployment-actions>). The web
service holds no data and runs no migration, so a rollback has no schema
step. After a rollback, compare the variables and the settings with
STACK.md section 15 and repeat steps 4 to 6 of section 1 before the next
merge.

- **To an image that does not read the database.** Roll back.
  `DATABASE_URL` can stay: that image ignores the variable.
- **To stop all database reads.** Remove the variable `DATABASE_URL`.
  The next deployment then fails at start-up, and the old deployment
  keeps running. The running deployment keeps the variable in its
  process, so it continues to read. To stop its reads at once, use the
  next item, or roll back to an image that does not read the database.
- **To stop all web reads at once, without a deployment.** Run
  `ALTER ROLE telemetry_web NOLOGIN` as the owner role `neondb_owner` on
  the branch `production`, for example in the Neon SQL Editor. The
  server opens one connection for each request, so each later request
  fails at the connect and answers a problem, never data. Expected: 500
  `internal_error`, because PostgreSQL refuses the login with an
  SQLSTATE of class 28. A read that runs already ends with its
  transaction. The ingest roles do not change. To undo it, run
  `ALTER ROLE telemetry_web LOGIN`, or run the web-only procedure of
  section 1, step 3, which also sets a new password.
- **The role `telemetry_web`.** The role stays after a rollback. It can
  only read, so it does no harm. Only a decision of the lead removes it.

A failed web release does not affect the ingest service: the services
have separate images, deployments, and watch paths. The ingest service
keeps the vehicle connection.
