# Release of the ingest image

This runbook is for the lead. It releases the image of ADR 0001
(`ingest/Dockerfile`) to the Railway service `telemetry`, checks the
release, and recovers from a failed release. STACK.md section 15 holds the
Railway settings and the release rules. ADR 0001, "To verify at first
deployment", holds the checks that the first release must answer.

Rules:

- Do not put a VIN, a location, a signal value, a password, or a database
  URL into a PR, an issue, or a commit. Record counts, field names, and
  yes or no answers.
- Run SQL read-only, for example in the Neon SQL Editor, on the branch
  `production` of the project `telemetry`.
- `railway` commands use the service `telemetry` in the environment
  `production` (`-s telemetry -e production`). The commands below leave
  out these two flags.

## 1. Before the merge

1. Confirm that the PR has all acceptance evidence: the local
   `mise run verify`, green `ci`, `vuln`, and `image-check` jobs of the
   current PR head, and the PASS of the independent review.
2. Create the database roles on the branch `production`: README.md,
   "Database roles", steps 1 to 4. Expected: one JSON line
   `roles_applied`. Then read the server version read-only:
   `SHOW server_version;`. Expected: PostgreSQL 18 (`18.x`), the major
   version of the branch `test` (18.7). Another major version stops the
   release: the migrations and the query metadata are tested on 18
   (STACK.md section 5). Record the value in STACK.md section 15 in the
   next change.
3. Set the service variables without a deployment. The values are the
   rows of the table "Railway settings of the service `telemetry`" in
   STACK.md section 15 whose column "Where the lead sets it" names a
   variable, plus `DATABASE_URL` and `MIGRATION_DATABASE_URL` (README.md,
   "Database roles", step 3). This runbook does not repeat the values;
   the deployment test checks the table. Keep `TLS_CERT` and `TLS_KEY`.
   For each variable, use
   `railway variable set --skip-deploys <NAME>=<value>` (for a secret, use
   `--stdin` with the single name, so that the value is not in the
   shell history). The CLI reference describes `--skip-deploys` as "Skip
   triggering deploys" (<https://docs.railway.com/cli/variable>); the API
   field is `skipDeploys` of `variableUpsert`, "Don't trigger a redeploy
   after change"
   (<https://docs.railway.com/integrations/api/manage-variables>).
   Make sure that no variable starts with `INGEST_`. A service variable
   replaces the value of the image.
4. Set the service settings: the rows of the same table whose column
   "Where the lead sets it" names the service settings (the watch paths
   one pattern each line). Do not set a config file path. Two ways work
   without a deployment:
   - In the dashboard. The dashboard stages these changes. Apply them with
     Alt and "Deploy": this "lets you commit the changes without
     triggering a redeploy"
     (<https://docs.railway.com/deployments/staged-changes>). Do not click
     "Deploy" without Alt.
   - Through the Railway API without staging, for example the tool
     `update-service` of the Railway MCP server; its answer says "Config
     changes apply on the service's next deployment". The release of
     27 November 2026 used this way: no deployment started.
5. Check that nothing was deployed: the deployment list shows no new
   deployment, and the old deployment is still active. Check that the
   vehicle is still connected while it is awake. Before the first release
   (the image before ADR 0001), the old image logged each record, so
   `railway logs --since 5m --lines 500 | wc -l` gave a count above 0
   (count only; the lines held the VIN and the location). With the image
   of ADR 0001, no record reaches the log; while the vehicle is awake,
   `railway logs --since 20m --filter "flushed"` shows a `flushed` line
   with `records` above 0 at least every 15 minutes. No such line while
   the vehicle is asleep is expected; check again when it is awake. Read the settings back in the dashboard, and the variable names
   with `railway variable list` (names only; do not copy values into a
   PR). Expected: every value of the STACK.md section 15 table.

Why the merge deployment uses these values: a deployment reads the
service variables and settings when it starts. The Railway guide for a
secret rotation uses the same order: "Use `variableUpsert` with
`skipDeploys: true` for all services, then trigger deployments manually
when ready"
(<https://docs.railway.com/integrations/api/manage-variables>). The
build log of the merge shows the result: it must build
`ingest/Dockerfile` (section 2).

If a deployment of the current `main` starts by mistake (for example
"Deploy" without Alt), it cannot replace the active deployment. A
deployment becomes active only after its healthcheck passes: "Only then
will the new deployment be made active and the previous deployment
inactive" (<https://docs.railway.com/deployments/healthchecks>), and the
old deployment gets SIGTERM only "once the new deployment is active"
(<https://docs.railway.com/deployments/deployment-teardown>). Each of
three steps of that deployment fails: the build (`main` has no
`ingest/Dockerfile`), the pre-deploy command (the old image has no
`/usr/local/bin/ingest`; "If your command fails, it will not be retried
and the deployment will not proceed",
<https://docs.railway.com/deployments/pre-deploy-command>), and the
healthcheck (the old image has no `/health` on `PORT`). If Railway
gives the pre-deploy command to the old `ENTRYPOINT` as arguments, the
old entrypoint starts the old server and the pre-deploy step does not
end; cancel the deployment, or the pre-deploy timeout ends it. In all
cases the old deployment stays active and keeps the vehicle connection.
Record the result.

What Railway builds at each step: steps 2 to 5 build nothing. The merge
builds the merge commit with `ingest/Dockerfile`. A merge before step 3
builds the Rust workspace with the Railway builder (Railpack), because the
root has no `Dockerfile` and has a `Cargo.toml`. That build does not run
the image of ADR 0001. Without the pre-deploy command and the healthcheck of
step 4, nothing stops it from going live. So never merge before step 5
has passed.

## 2. The merge

Merge the PR with a merge commit. The merge changes watch paths, so
Railway builds the image. Watch the deployment:

| Phase | Command | Expected |
| ----- | ------- | -------- |
| Build | `railway logs --build <deployment id>` | The `apt-get upgrade` step runs (not `CACHED`) and lists the installed versions. `cargo build --release --locked -p ingest` ends with `Finished`. |
| Pre-deploy | `railway logs <deployment id> --filter "migrated OR migrate_failed"` | One line `migrated` with `applied` 1 and `embedded` 1 on the first release. |
| Healthcheck | Railway deployment view | The healthcheck on `/health` passes; the status is `SUCCESS`. |
| Start | `railway logs <deployment id> --filter "started OR gate_opened OR subscriber_settled OR database_check_passed"` | `subscriber_settled`, `database_check_passed`, and an `ingest_event` `gate_opened` within a few seconds. |

If the healthcheck fails, Railway keeps the old deployment.

Address family of the healthcheck (ADR 0001, "To verify at first
deployment"): `/health` binds `0.0.0.0:PORT`, IPv4 only. The Railway
guide says: "Your web server should bind to the host `0.0.0.0` and
listen on the port specified by the `PORT` environment variable"
(<https://docs.railway.com/guides/fixing-common-errors>). The healthcheck
page does not name the address family. Failure mode: if the healthcheck
came over IPv6 only, it would never reach `/health`, the deployment
would fail after the healthcheck timeout, and the old deployment would
stay active (fail safe). The deploy log then shows `listening` and
`gate_opened`, but the healthcheck never passes. Fix: bind `/health` on
`[::]` with dual stack in `crates/ingest/src/lifecycle.rs` in a new
change, and release again. Read the
deploy log, fix the cause in a new PR, and merge again.

## 3. Post-release checks

Record each result in the PR. A check that cannot run yet is "pending"
with its reason. Section 3.1 runs at once; section 3.2 needs the vehicle
awake; section 3.3 runs after one week. The first release (27 November
2026) has its record on PR #23; ADR 0001, "To verify at first
deployment", has the status of each point.

Notes on `railway ssh` (observed at the first release):

- On Railway, PID 1 is an init that Railway adds
  (`/run/podman-init -- /entrypoint.sh`), and `ingest` is PID 2 (ADR 0001,
  "Process tree on Railway"). Do not read `/proc/1/...` for `ingest`; find
  its PID first:
  `railway ssh -- sh -c 'for d in /proc/[0-9]*; do case "$(tr "\0" " " < "$d/cmdline" 2>/dev/null)" in "/usr/local/bin/ingest run ") echo "${d#/proc/}";; esac; done'`.
  The commands below write `<ingest pid>` for that PID.
- The session runs as UID 0, but it cannot read `/proc/<pid>/environ` of a
  process of UID 10001 ("Permission denied"). So no check here reads an
  environment; the CI job `image-check` is the evidence for the
  environment of `ingest` and of `fleet-telemetry`. Untested alternative:
  run the read as UID 10001, for example
  `railway ssh -- setpriv --reuid=10001 --regid=10001 --clear-groups sh -c 'tr "\0" "\n" < /proc/<ingest pid>/environ > /tmp/names; sed "s/=.*//" /tmp/names; rm /tmp/names'`
  (names only).
- `railway ssh` uses the local SSH agent. It can ask the owner for other,
  unrelated keys of the agent.

### 3.1 At once

| Check (ADR 0001) | Command | Expected |
| ---------------- | ------- | -------- |
| TLS on the public port | `openssl s_client -connect telemetry.calmdonut.com:57438 -CAfile <Origin CA ECC root> -verify_return_error < /dev/null` | `verify return:1` for the chain. The server can end with an alert, because it asks for a client certificate. |
| Process tree through the image `CMD` | `railway ssh -- sh -c 'for p in /proc/[0-9]*; do printf "%s " "${p#/proc/}"; tr "\0" " " < "$p/cmdline"; echo; done'`, then `railway ssh -- grep -e ^Uid -e ^PPid /proc/<ingest pid>/status /proc/1/status` | PID 1 is the init of Railway (`/run/podman-init -- /entrypoint.sh`); `/usr/local/bin/ingest run` has `PPid` 1 and `Uid` 10001; `fleet-telemetry` has the PID of `ingest` as `PPid`. Record the `Uid` of PID 1: if it is 10001, the child can read the full service environment in `/proc/1/environ` (ADR 0001, "Process tree on Railway"). |
| Open-file limit | `railway ssh -- grep "open files" /proc/<ingest pid>/limits` | Soft limit equals hard limit. Both are at least 4096: 2048 descriptors for 1024 gate connections, plus the descriptors of the child and a margin. Record the values. |
| Child environment and descriptors (Linux check 6) | The child PID: `railway ssh -- sh -c 'for d in /proc/[0-9]*; do case "$(tr "\0" " " < "$d/cmdline" 2>/dev/null)" in "/fleet-telemetry -config "*) echo "${d#/proc/}";; esac; done'`. Then `railway ssh -- sh -c 'grep PPid /proc/<pid>/status; ls -l /proc/<pid>/fd'` and `railway ssh -- cat /proc/net/tcp` | `PPid` is the PID of `ingest`. No `socket:[<inode>]` of the child is a listening socket (state `0A`) of `ingest`: the gate port 4443 (hex `115B`) or the `PORT` of STACK.md section 15 (in hex). If the session cannot read `/proc/<pid>/fd` ("Permission denied"), record that; the CI job `image-check` is then the evidence. The environments of the child (empty) and of `ingest` (`DATABASE_URL`, no `TLS_CERT`, `TLS_KEY`, or `MIGRATION_DATABASE_URL`) come from the CI job `image-check`, because the session cannot read them (notes above). |
| TLS files | `railway ssh -- stat -c "%a %u %n" /etc/fleet-telemetry/tls /etc/fleet-telemetry/tls/tls.crt /etc/fleet-telemetry/tls/tls.key` | `700 10001`, `600 10001`, `600 10001`. |
| Kernel values | `railway ssh -- sh -c 'uname -r; cat /proc/sys/net/ipv4/tcp_retries2; cat /proc/sys/kernel/yama/ptrace_scope'` | Record all three. ADR 0001 assumes `tcp_retries2` 15. `ptrace_scope` 1 or higher keeps the child from reading the memory of `ingest` (STACK.md section 15, accepted risks); a missing file means that Yama is off, so record it as 0. |
| No record in the log | `railway logs --since 1h --filter "<the VIN>"` on the lead's computer | No line. Record only the count. Before this release, every record had a line. |
| `dispatch_total` on the real binary (ADR 0001) | After the first records: `railway ssh -- perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new("127.0.0.1:9090") or die; print $s "GET /metrics HTTP/1.0\r\n\r\n"; print grep { /^dispatch_total/ } <$s>'` | Lines `dispatch_total{record_type="V"}` and others. The program prints only these lines; the other metric `vin_signal_count` holds the VIN. `perl-base` is part of every Debian image. |
| Linux check 1 | The PR | Links to the green `ci` and `image-check` runs of the merged head. |
| Release build: no `test_overrides` line (ADR 0001, "Test overrides") | `railway logs <deployment id> --filter "test_overrides"` | No line. The CI job `image-check` also starts the image with two `INGEST_TEST_*` variables and fails on this line. |
| `V`, `alerts`, and `errors` go to `zmq` (ADR 0001, "SUB socket defences") | `railway ssh -- cat /etc/fleet-telemetry/config.json` | `records` sends each of `V`, `alerts`, `errors`, and `connectivity` to `["zmq"]` only. The deployment test checks the same file before merge. |

### 3.2 While the vehicle is awake

| Check (ADR 0001) | Command | Expected |
| ---------------- | ------- | -------- |
| Records reach PostgreSQL | After 15 minutes: `SELECT count(*), max(received_at) FROM raw.vehicle_signal;` | A count above 0, and a time of the last flush. |
| Settle time of 250 ms | `SELECT count(*) FROM raw.ingest_event WHERE kind = 'zmq_shortfall';` after the first vehicle connection of a start | 0. A row means that records were lost at the start; record its count and window. |
| Shortfall check reports zero | `railway logs --since 1d --filter "shortfall_read_failed OR metrics_unreadable OR zmq_shortfall"` | No line. The shortfall task reads the metrics each 15 s; two failed reads in a row close the gate with `metrics_unreadable`. |
| Client address through the proxy | `railway logs --filter "gate_connection_closed"`, field `peer_class` | Record the class (`private`, `shared`, or `other`). `other` means that the container sees public client addresses. |
| Gate rule 1 | The same lines, field `first_byte_ms`, for each line with `duration_ms` above 60000 | The maximum over the first week is far below 10000, for example below 2000. Record the maximum. |
| Socket options (Linux check 5) | The same lines | Every line has `socket_options` `set`, `keepalive` `15/15/9`, and `nodelay` `true`. No line has `failed`. |
| Quiet connection of 30 minutes | During AC charging: `SELECT status, created_at FROM raw.connectivity_event WHERE created_at > now() - interval '45 minutes' ORDER BY created_at;` | One row with status 1 (`CONNECTED`) and no row with status 2 (`DISCONNECTED`) for at least 30 minutes. |
| Child exits (Linux check 3) and the end of the SUB socket through libzmq (ADR 0001, "SUB socket defences") | Three times: `railway ssh -- sh -c 'kill -9 <child pid>'` (the image has no `kill` program; `kill` is a builtin of `sh`), wait 40 s, and read the new child PID. Then `railway ssh -- sh -c 'grep -l ") Z " /proc/[0-9]*/stat'`, and `railway logs --since 10m --filter "child_exited OR subscriber_disconnected OR subscriber_rebuilt OR subscriber_settled OR gate_opened"` | Three `child_exited` events and three new child PIDs. Not tested: whether the session (UID 0, which cannot read the environment of UID 10001) may signal a process of UID 10001; if `kill` fails, record it, and the CI job `image-check` stays the evidence. The `grep` prints no file: no process is a zombie. After each exit: `subscriber_disconnected`, or `subscriber_rebuilt` with the reason `child_stopped`, then `subscriber_settled` and an `ingest_event` `gate_opened`. Record how many exits gave `subscriber_disconnected`. Each exit closes the gate for some seconds, and the vehicle buffers. The CI job `image-check` runs the same sequence against the real binary without a vehicle. |
| Shutdown on a redeploy (Linux check 2) | `railway redeploy`, then `railway logs <old deployment id> --filter "shutdown_started OR gate_stopped OR child_killed OR ingest_event OR shutdown_complete OR task_stop_timeout OR gate_connections_open OR flush_incomplete"` | `shutdown_started` with `reason` `SIGTERM`, `gate_stopped`, `child_killed` with `exit_signal` 9, an `ingest_event` `stopped`, and `shutdown_complete` within the draining time of STACK.md section 15 after `shutdown_started`. No `task_stop_timeout`, no `gate_connections_open`, and no `flush_incomplete`. |
| Dead-peer bound, idle case | From a computer that the lead can cut from the network: `openssl s_client -connect telemetry.calmdonut.com:57438` until the server certificate shows, then cut the network of that computer (no FIN and no RST). | A `gate_connection_closed` line with `duration_ms` of about 150000 plus the time before the cut. If the proxy terminates TCP, the line can come later or never: record the result. |
| Dead-peer bound, unacknowledged data | Cut the network of the client directly after the ClientHello, before the server flight arrives. | A `gate_connection_closed` line within about 1,045 s. This case is hard to produce by hand; if it cannot be produced, report it as pending. |
| Container memory at 1024 connections | Optional, only while the vehicle is asleep: open 1030 TLS connections that stall in the handshake, then read `railway ssh -- cat /sys/fs/cgroup/memory.current` and the `gate_full` event. | Record the memory. The vehicle cannot connect while the bound is full. |

### 3.3 After one week

| Check (ADR 0001) | Command | Expected |
| ---------------- | ------- | -------- |
| `unknown_fields` false positives | `SELECT id, received_at, length(payload) FROM raw.unprocessed_record WHERE reason = 'unknown_fields';` and, for each row, a decode of `payload` that lists the field numbers | Each row holds a field that the vendored schema does not know. Record the count; a row without such a field is a defect of the decode. |
| False positives of the stall rule (ADR 0001, "SUB socket defences") | `SELECT occurred_at, reason FROM raw.ingest_event WHERE kind = 'gate_closed' AND reason = 'subscriber_stalled' ORDER BY occurred_at;` and `railway logs --since 7d --filter "subscriber_stalled OR subscriber_rebuilt OR zmq_shortfall"` | No row and no `subscriber_rebuilt` with the reason `stalled`. Record each one with the `zmq_shortfall` lines and counts around it. |
| A loss at the high water mark is followed by the older queued records (ADR 0001, "SUB socket defences") | `SELECT occurred_at, dropped_count FROM raw.ingest_event WHERE kind = 'zmq_shortfall' ORDER BY occurred_at;` and the same log query | For each `zmq_shortfall` event, no `subscriber_stalled` line follows within 60 s: the socket stayed live after the loss. With no `zmq_shortfall` event, record "not observed"; the premise then stays unverified. |
| Neon compute | Neon console or API: effective compute size of `production` and `test`, and the CU-hours of the week | 0.25 CU for both. The consumption fits the budget of ADR 0001, "Why 15 minutes". |
| A failed healthcheck keeps the old deployment | No test on purpose: it needs a failing release while the vehicle is connected. The healthcheck documentation states it ("Only then will the new deployment be made active and the previous deployment inactive"). | Record "documented, not observed". A first failed release, or a deployment of `main` by mistake in section 1, gives the observation. |
| Forced end of `ingest` (Linux check 4) | On Railway, `ingest` is not PID 1, so the session can send it SIGKILL: `railway ssh -- sh -c 'kill -9 <ingest pid>'`. Run it only while the vehicle is asleep and after a `flushed` line, because SIGKILL loses the records that wait for the next flush. Then `railway logs --since 10m --filter "started"` and the process tree of section 3.1. If the session cannot signal the process, wait for the first watchdog exit or crash instead. | The init ends with `ingest`, the container ends, and the restart policy starts it again: a new `started` event follows, and the process tree has a new `ingest` and a new `fleet-telemetry`, and no `fleet-telemetry` without `ingest`. Not run at the first release (ADR 0001, "To verify at first deployment"). |

## 4. Recovery

Use a rollback in the Railway deployment view. Railway restores the image
and the custom variables of the selected deployment
(<https://docs.railway.com/deployments/deployment-actions>); the plans
page adds the settings: "Rolling back a removed deployment within the
retention policy will restore the previous image, settings, and all
variables with a new deployment"
(<https://docs.railway.com/pricing/plans#image-retention-policy>).

Limit: "Deployments older than your plan's retention policy cannot be
restored via rollback". The image retention of the Hobby plan is 72
hours after a deployment is removed. After that, only "Redeploy" is
left, which "will rebuild the image from the original source code with
the deployment's original variables". So the image before ADR 0001 can
be restored by rollback only within 72 hours after the release removes
it; later, a redeploy rebuilds it from the old commit.

- **To an earlier deployment of the ADR 0001 image.** Roll back. The
  migration of this release only adds the schema `raw`, so an earlier
  image of the same schema version works.
- **To the image before ADR 0001** (the first release failed after it went
  live). That image has no `/usr/local/bin/ingest` and no `/health`.
  First clear the pre-deploy command and the healthcheck path, then roll
  back. The migration is compatible: the old image does not use the
  database, and the tables stay. Warning: the old image writes every
  record, with the VIN and the location, to the Railway log again. Fix
  forward as soon as possible.
- **After any rollback.** The rollback restores the custom variables
  (and, for a removed deployment, the settings) of the selected
  deployment. So the Dockerfile path setting (`dockerfilePath`), `PORT`,
  `RAILWAY_DEPLOYMENT_DRAINING_SECONDS`, `RAILWAY_DEPLOYMENT_OVERLAP_SECONDS`,
  and the database URLs can be gone or old, and `main` has no root
  `Dockerfile`. Before the next merge, compare the variables and the
  settings with STACK.md section 15 and repeat steps 3 to 5 of section 1.
- **Data.** A rollback does not delete stored rows. The records that the
  vehicle sent while the service was down stay in the vehicle buffer (at
  least 2,500 s) and arrive after the reconnect.
