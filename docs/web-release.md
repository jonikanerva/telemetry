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
3. Set the service variables without a deployment. The values are the
   rows of the table "Railway settings of the service `telemetry-web`"
   whose column "Where the lead sets it" names a variable. Use
   `railway variable set --skip-deploys <NAME>=<value>`. Do not set
   `WEB_STATIC_DIR`: the image sets it, and a service variable replaces
   the image value.
4. Set the service settings: the rows of the same table whose column
   "Where the lead sets it" names the service settings (the watch paths
   one pattern each line). Apply them without a deployment, as in
   `docs/ingest-release.md`, section 1, step 4.
5. Read the settings back (`railway service` or the Railway MCP tool
   `get-service-config`) and compare each value with the table.

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
| No secret in the log   | `railway logs --since 1h`                                                                   | No line holds a variable value. Record only the count of such lines.                                        |
| Public checks (S5)     | After the custom domain exists: `curl -sI https://tesla.calmdonut.com/` and `/health`        | TLS, the security headers of ADR 0010, `200` for `/health`, and the SPA. Pending until the domain exists.     |

## 4. Recovery

Use a rollback in the Railway deployment view. Railway restores the image
and the custom variables of the selected deployment
(<https://docs.railway.com/deployments/deployment-actions>). The web
service holds no data and runs no migration, so a rollback has no schema
step. After a rollback, compare the variables and the settings with
STACK.md section 15 and repeat steps 3 to 5 of section 1 before the next
merge.

A failed web release does not affect the ingest service: the services
have separate images, deployments, and watch paths. The ingest service
keeps the vehicle connection.
