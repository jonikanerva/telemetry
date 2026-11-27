# ADR 0001: Ingestion architecture

- Status: Accepted, 26 November 2026 (issue #4). Amended by issues #13, #14,
  and #29, and by the owner decisions of 27 November 2026.
- Scope: the path from the vehicle to PostgreSQL. ADR 0002 owns the tables.

## Context

Tesla's `fleet-telemetry` v0.10.1 terminates the vehicle mTLS WebSocket
behind a Railway TCP proxy (`telemetry.calmdonut.com:57438` to port 4443).
Its `logger` dispatcher wrote every record, with the VIN and the location,
to the Railway log. The goal is a Rust process that stores every record in
PostgreSQL on Neon (Free plan). The owner accepts a loss of up to 15
minutes of queued data at a crash, a deploy, or a Neon outage; each such
loss stays visible as a gap (`VISION.md`, 27 November 2026).

Facts. Upstream paths refer to tag `v0.10.1`, commit `6689adb` (Sources).

1. Without a connection, the vehicle buffers 5000 messages, "at least 2,500
   seconds of data", and delivers them after the reconnect, with backoff at
   most 30 s apart [T]. A resend of data without an ack is documented only
   for `delivery_policy: latest`, which the config does not set. The owner
   saw `IsResend` records, so some resend happens.
2. The server acks a record right after it hands the record to the
   dispatchers (`telemetry/serializer.go#L90`).
3. A reliable ack waits only until the producer call returns. PUB drops
   every message without a subscriber and at the high water mark (default
   1000) [Z1][Z2][Z3]; the record is acked and lost.
4. The ZMQ message has two frames: the topic `<namespace>_<record type>` and
   the payload, with no transaction ID and no receive time.
5. The server skips and does not ack a message over its rate limit.
6. The server binds the ZMQ socket and then listens on its TLS port at once.
   It has no signal handler; SIGTERM ends it at once.
7. Railway sends SIGTERM to the old deployment when the new one is active
   [R1]. A healthcheck runs only before a deployment becomes active [R3]. A
   failed pre-deploy command stops the deployment [R4].
8. When PID 1 of a PID namespace ends, the kernel kills all processes in the
   namespace [K].
9. Neon compute scales to zero after 5 minutes without activity. After 100
   CU-hours in a month Neon suspends the compute [N1].

## Decision

### Transport: ZMQ on the loopback interface

`fleet-telemetry` publishes all four record types with the `zmq` dispatcher
to `tcp://127.0.0.1:5284`. `ingest` subscribes with a SUB socket to the
prefix `tesla_telemetry_`. The `logger` dispatcher is removed.
`reliable_ack_sources` stays unset (fact 3). ZMQ is the only candidate
without a broker or a new provider that delivers the complete record. The
layout compensates for the missing guarantee.

### Layout: one Railway service, one image, Rust is the parent of the server

- `ingest` starts `fleet-telemetry` as its child. Under Docker `ingest` is
  PID 1; on Railway it is PID 2 under an init ("Process tree on Railway").
  In both cases fact 8 ends the child with `ingest`.
- `ingest` owns the public port `0.0.0.0:4443` and copies each accepted TCP
  connection byte for byte to `127.0.0.1:4444` without TLS. This is the
  **gate**. The service has a TCP proxy to port 4443 and no HTTP domain.
- The gate is open only when four gate conditions are true:
  1. the child runs;
  2. the SUB socket is connected and subscribed, and the settle time of
     250 ms has passed ("SUB socket");
  3. the start-up check passed (one round trip, and every embedded
     migration is applied, ADR 0002), or the database has given **no
     answer** for 120 s. **No answer**: no session starts (a network, TLS,
     or timeout error, or any error before authentication except an
     authentication error). **An answered failure**: an authentication
     error (SQLSTATE class 28) or a missing migration; the gate stays
     closed, `/health` fails, and the vehicle buffers. After 120 s of no
     answer, the gate opens in **degraded mode**: records go to the queue,
     and the writer runs the check before its first write; an answered
     failure then closes the gate. The check repeats on the cadence of
     "Delivery path", step 5;
  4. the queue did not close the gate ("Delivery path", step 2).
- A closed gate closes every forwarded connection, so the vehicle buffers
  (fact 1). It accepts and closes each new connection at once. The gate also
  closes the start race of fact 6.
- Gate rules. The gate cannot see inside TLS, so it does not guess which
  connection is the vehicle:
  1. **First answer within 10 s.** The gate closes a connection when
     `fleet-telemetry` sends no byte within 10 s of the accept. The Go TLS
     server answers a ClientHello at once and has no handshake deadline.
  2. **Resource bound of 1024 connections** (two 8 KiB buffers and two
     descriptors each). At the bound, the gate closes each new connection at
     once and records at most one `gate_full` event each minute.
  3. **TCP keep-alive on each accepted socket**: idle 15 s, interval 15 s,
     9 probes, as Go sets for `fleet-telemetry`. **Dead-peer bound**: about
     150 s for an idle gone peer; about 925 to 1,045 s for a gone peer with
     unacknowledged data (retransmission timeout, `tcp_retries2` 15) [L].
     Both are below the vehicle buffer. A rejected option leaves the
     connection open with `socket_options` `failed`. After rule 1, the gate
     closes no live connection by itself.

Start order: the entrypoint writes the TLS files, raises the soft open-file
limit, and starts `ingest run`, which starts the child, connects SUB, runs
the start-up check, and opens the gate. The image uses `CMD`, so the
pre-deploy command replaces it. A child exit closes the gate, records a gap
event, and restarts the child after 1 s, doubling to 30 s. When `ingest`
fails, the container ends, and Railway restarts it.

**Watchdog.** No later healthcheck would see a hung `ingest` [R3]. A
watchdog thread outside the Tokio runtime checks heartbeats. The reader, the
writer, and the gate acceptor beat at least each second. Database deadlines: connect 15 s,
statement 30 s, transaction attempt 90 s, close 5 s. At a heartbeat older
than 30 s (reader, gate acceptor) or 120 s (writer), the watchdog logs the
task and ends the process.

**ZMQ shortfall check.** PUB drops at its high water mark without notice
(fact 3). Every 15 s, `ingest` reads `dispatch_total` of `V`, `alerts`, and
`errors` on `127.0.0.1:9090/metrics` and compares its growth with its own
count of received messages. A gap in two reads in a row is a shortfall, and
`ingest` records each new part as a `zmq_shortfall` event (ADR 0002). The
shutdown makes one final read after its drain. `connectivity` records are
outside the check.

Railway settings: `STACK.md` §15 is the one documented place (lead decision
7). `/health` returns 200 only when the gate is open and the start-up check
passed, so a deploy during an outage fails its healthcheck and the old
deployment keeps running.

### `fleet-telemetry` configuration

`ingest/server_config.json`: port 4444 on `127.0.0.1`; `V`, `alerts`,
`errors`, and `connectivity` each to `["zmq"]`; `zmq.addr`
`tcp://127.0.0.1:5284`; `transmit_decoded_records` `false`; `log_level`
`info` (`debug` logs payloads); `rate_limit.message_limit` 10000 in 30 s;
no `reliable_ack_sources`; metrics port 9090 on `127.0.0.1`. The vehicle
config does not set `delivery_policy` (lead decision 4).

### Wire format: protobuf

`ingest` decodes the protobuf payload with code generated from the upstream
`.proto` files, vendored in `crates/ingest/proto/` with the image tag.
Protobuf keeps integer and float types and unknown enum numbers apart.
Upstream changes some values before dispatch: it parses location strings,
rewrites scientific notation to five decimals, and shifts the AEB enum for
client 1.3.0. The stored value is the value after these changes.

### Crates

`zeromq` 0.7 (pure Rust, no `unsafe` in its `src/`; the reader does not use
its reconnect, "SUB socket"), `socket2` 0.6 with feature `all` for gate rule
3, `prost` 0.14 with `prost-build` and `protox` (no `protoc`), `sqlx` 0.9
with the one rustls provider of `STACK.md` §5, and `jiff` 1. `STACK.md` §6
pins them. Rejected: `zmq` 0.10 (links the C++ libzmq) and `protobuf` 4.x
(builds C code). `prost` drops unknown fields, so the decode detects them
(ADR 0002, "Unknown data").

### Delivery path and bounded buffering

1. A reader task reads the SUB socket without pause, decodes each message,
   and queues one item (topic, payload bytes, receive time, decoded rows)
   until its rows commit.
2. The queue holds at most 64 MiB. At 75 % it closes the gate. Below 25 %
   after a successful write, or when empty, it opens the gate ("No
   permanent close").
3. A full queue drops the newest message and counts it; the writer stores a
   gap event (ADR 0002).
4. The writer flushes when the oldest record is 15 minutes old, at 5000
   signal rows, at the high mark, or at shutdown
   (`INGEST_FLUSH_INTERVAL_SECONDS`, 1 to 900, default 900; lead decision
   1).
5. A failed write rolls back; the writer keeps the batch and retries without
   end. Every error except SQLSTATE classes 22 and 23 is retried. For those,
   the writer splits the batch until it finds the bad record, which goes to
   the unprocessed table (ADR 0002). Retry cadence of the writer and the
   start-up check: no answer retries after 1 s, doubling to 5 minutes, with
   jitter. After three equal answered failures without progress, it is one
   attempt each 60 minutes, because each answered attempt keeps the compute
   awake. After the manual fix, the lead restarts the service.
6. No connection pool. Each database call opens its own connection (connect
   timeout 15 s) and closes it before it returns, so no connection stays
   open between flushes (fact 9). A `SQLx` 0.9.1 pool checks a used
   connection without a time limit.

**Why 15 minutes.** At 0.25 CU, 100 CU-hours allow 400 active hours in a
month. An interval under 5 minutes keeps the compute active as long as the
car streams. With the hours of `docs/telemetry-usage-estimate.md`
(provisional), a 10-s interval uses about 40 CU-hours (planning case) and
180 (car awake all month); 15 minutes uses about 18 and 60. The `test`
branch adds at most 5, so the total is about 23 and 65. Only 15 minutes
keeps the worst case inside the plan. The arithmetic needs a fixed 0.25 CU
compute (lead decision 3). The cost is the accepted loss budget: a crash
loses up to one flush interval. A restart during a long outage can lose
more (the whole queue, "Delivery guarantee"), and the gap stays visible.
Two cases leave no stored event of the loss. A crash in degraded mode: the
`started` and `degraded_mode` events wait in the memory queue. A database
that refuses every record with class `Data` (known limit N6): the gate
opens and closes in cycles, and no record is stored.

### Shutdown on SIGTERM or SIGINT

1. Close the gate and every forwarded connection (bound 1 s).
2. Read the SUB socket until no message arrives for 1 s, at most 3 s.
3. Make the final shortfall read (bound 1 s).
4. Stop the child with SIGKILL (bound 1 s).
5. Flush the queue, with a deadline of 20 s.
6. If the flush leaves data unstored, log `flush_incomplete` with the
   counts and exit with code 1.

The budget is 26 s, under `drainingSeconds` 30.

### Delivery guarantee

`fleet-telemetry` to `ingest` is at most once (ZMQ has no ack), and the gate
keeps the consumer present. `ingest` to PostgreSQL is at least once while
the process lives; every insert is idempotent (ADR 0002). End to end is at
least once, also over deploys and outages within the buffers, except:

| Case | Bound | Detection |
| ---- | ----- | --------- |
| `ingest` crashes | The queue: normally one flush interval; during an outage up to 64 MiB. | `started` without an earlier `stopped`. |
| `ingest` hangs | 30 s or 120 s, then as a crash. | The watchdog log line. |
| PUB drops at the high water mark | A hung reader: 30 s. A slow reader: no bound. | `zmq_shortfall` events. |
| Outage, process up | Nothing until the queue is full; then the vehicle buffer. | `records_dropped` events. |
| Restart during an outage | The queue before the restart. | As a crash and an outage. |
| Final flush fails | The queue at shutdown. | `flush_incomplete`. |
| The child dies | Milliseconds of its send queue. | `child_exited`. |
| SUB socket stalls with the gate open | Less than `4P + 2D + G` = 63.5 s ("Detection bound"). | `zmq_shortfall`, `gate_closed` with `subscriber_stalled`. |
| A gate slot outlives shutdown step 1 | Records forwarded after step 2. | `gate_connections_open`. |
| Sent messages without an ack at a close | Unless the vehicle resends them. | Stored `is_resend` values. |
| Handshake stall or flood at the gate (lead decision 6) | The bound is full, plus the dead-peer bound. | `gate_full`, `connectivity_event` gaps. |
| Downtime beyond the vehicle buffer | No bound. | `connectivity_event` gaps, missing `started`. |

### Logging

`ingest` logs JSON with an allowlist of fields. It never logs a VIN, a
location, a signal value, a payload, a database URL, or the text of a
database error. The child's output goes through `ingest`, which replaces
each VIN-shaped token (17 characters from `[A-HJ-NPR-Z0-9]`) with `[vin]`
and truncates lines at 8 KiB.

### Decisions of the pipeline change (issue #13)

`crates/ingest/src/pipeline/` and its tests state the details.

- **Test overrides.** Only a debug build reads `INGEST_TEST_*`, and each can
  only lower its limit. The image runs a release build; `image-check` proves
  that it ignores `INGEST_TEST_QUEUE_BYTES=1` and
  `INGEST_TEST_DEGRADED_AFTER_MS=1000`.
- **SUB socket.** The reconnect of `zeromq` 0.7.0 can stop for good after a
  failed subscription send to a new peer. The reader owns its socket.
  - **Defence 1.** One socket for each child, replaced at a child stop, a
    `Disconnected` event, or a failed subscription. The socket subscribes
    after the connect.
  - **Defence 2: the stall rule.** For two successful metrics reads `m` and
    `n` of one socket: when the child dispatched between their responses
    and the reader received nothing from before the request of `m` until 2
    s (the grace) after the response of `n`, the socket is stalled. The
    reader clears gate condition 2 with `subscriber_stalled` and replaces
    the socket. After repeated stalls without a received record, it waits
    30 s, doubling to 15 minutes, with the condition false.
  - **Unreadable metrics.** After two failed reads in a row (deadline
    0.75 s), the shortfall task clears gate condition 2 with
    `metrics_unreadable` and reads each 1 s until a read succeeds.
  - **Detection bound.** `P` is the read period (15 s), `D` the read
    deadline (0.75 s), and `G` the grace (2 s). The decision comes one grace
    after the second successful read after the stall starts. With the gate
    open, at most four failed reads and three openings fall into one stall.
    The time with the gate open is less than `4P + 2D + G` (63.5 s); with
    the gate open the whole time and `f` failed reads, less than
    `(2 + f) × P + D + G` (32.75 s, 47.75 s, 62.75 s). With pauses in the
    dispatch, at most two read intervals plus `D` and `G` lose records. The
    records are acked, so the vehicle does not resend them. The test
    `the_worst_case_timeline_meets_the_bound_of_adr_0001` pins the bound.
  - **Known limit (note N11).** The reader can count a record before the
    counter grows, so the rule can replace a healthy socket; the gate closes
    for about 0.5 s.
  - **Keep `zeromq` 0.7.0.** Owner decision of 27 November 2026: the stall
    detection stays. Reassess after the one-week follow-up or at a second
    defect of this class. No report of the reconnect defect goes upstream
    without the owner.
- **Reader and event bounds** (`Limits` in `pipeline.rs`). A connect
  attempt of the reader has 5 s. A socket rebuild has `rebuild_drain` 1 s,
  `rebuild_quiet` 200 ms, and a backoff of 100 ms to 5 s. The process keeps
  the events in memory until the writer stores them, at most 10,000. A
  metrics response has at most 1 MiB.
- **Writer rules.** SQLSTATE classes `08` and `57P` count as no answer. A
  write that stored records resets the cadence. After a statement timeout
  the next batch is half the size. The writer also flushes when the queue
  closes the gate. A batch is one transaction of at most 5,000 signal rows
  and 8 MiB. A `records_dropped` event has the reason `queue_full` or, when
  the database refuses even the unprocessed row, `database_refused`.
- **No permanent close.** Invariant: a gate condition is false only while a
  future event of the process can make it true. The queue opens the gate
  when it becomes empty, also after a failed write; the check always has a
  next time on the cadence; the reader is always in a bounded step; the
  shortfall task reads each 1 s while metrics fail; the supervisor restarts
  the child without end. Only the shutdown closes the gate for good.
- **Shutdown.** Each step aborts its work at its bound. Step 1 waits until
  every gate slot is free, at most 1 s, and logs a remaining slot as
  `gate_connections_open`. The `stopped` event is recorded before step 5.

### Decisions of the deployment change (issue #14)

`crates/ingest/tests/deployment.rs` checks that the files agree;
`image-check` checks the running container (`STACK.md` §3).

- The entrypoint is a short shell script, not Rust code:
  `std::env::remove_var` is `unsafe` in edition 2024. `ingest` starts the
  child with an empty environment.
- The `ENV` lines set the ports and paths. The service variables hold only
  `PORT` (8080) and the secrets.
- The builder image is `rust:<toolchain>-slim-trixie`, the Debian release of
  the final stage; it is not scanned (`STACK.md` §13).
- `.dockerignore` and the `COPY` lines name the same inputs; the watch paths
  add `ingest/**`, `rust-toolchain.toml`, and `.dockerignore`.
- Pre-deploy command `/usr/local/bin/ingest migrate`, timeout 1200 s.
- A rollback restores the image and the variables. A rollback to the image
  before this decision needs the pre-deploy command and the healthcheck
  path cleared first (`docs/ingest-release.md`).
- An absent `dispatch_total` series counts as 0, so the release reads the
  metrics of the real binary once.

### Process tree on Railway (observed at the release, 27 November 2026)

PID 1 is `/run/podman-init -- /entrypoint.sh`, an init that Railway adds;
`ingest run` is PID 2, and `fleet-telemetry` is its child. Assumption: the
init is catatonit [P1][P2]. It forwards signals, except `SIGCHLD`, the
terminal stop signals, and its own fault signals, to `ingest`, reaps orphans, and exits with `ingest`. So SIGTERM reaches
`ingest` (observed at the release of PR #30), and a forced end of `ingest`
ends the container. `ingest` is not PID 1, so a forwarded signal without a
handler ends it.

The init holds every service variable in `/proc/1/environ`. While the image
ran as user 10001, every process of that user could read it. Options for
the owner:

1. **Start as root, drop in the entrypoint.** Chosen on 27 November 2026,
   implemented by issue #29.
2. **A separate UID for the child**, with `CAP_SETUID` in `ingest`.
   Declined on 27 November 2026: the owner accepts that the child can read
   `DATABASE_URL` of `ingest` (note N1 below).
3. **Keep the accepted risk.** Not chosen.

**Decision of issue #29 (option 1).** The image has no `USER` line. The
entrypoint runs as root, removes the TLS and migrator variables, writes the
TLS files (mode 0600, owner 10001), and starts `ingest run` through
`setpriv` as UID and GID 10001 with no groups, no capabilities, and
`no_new_privs`. Observed at the release of PR #30: PID 1 has UID 0, and UID
10001 cannot read `/proc/1/environ`. A process of UID 10001 reads
`/proc/self/environ` but not `/proc/1/environ`.

**Exposure of an init that runs as UID 10001.** It applies again after a
rollback to an image that starts as user 10001 (`docs/ingest-release.md`,
"Recovery"). Precondition: code execution in `fleet-telemetry` (for example
after the mTLS handshake, `docs/security/image-advisories.md`, fact 2) or
in `ingest`. No path from outside the container reads the file.

- `MIGRATION_DATABASE_URL`: the role `telemetry_migrator` owns schema
  `raw`. A holder can read, change, and drop each table of `raw`, with the
  stored locations. Neon keeps 6 hours of restore history.
- `DATABASE_URL`: `SELECT` and `INSERT` on `raw` (role
  `telemetry_ingest`).
- `TLS_KEY`: a holder can impersonate the server to the vehicle, if the
  holder can also take the DNS name or the network path. `fleet-telemetry`
  reads the key file anyway; the init adds a second copy, not a new
  reader.

**Residual risks of option 1.** The owner accepted each on 27 November
2026.

- **Note N1: the child can read the environment of `ingest`.**
  `fleet-telemetry` runs as user 10001, as `ingest` does, so it can read
  `/proc/<pid of ingest>/environ` (PID 2 on Railway, PID 1 under Docker)
  with `DATABASE_URL` (`SELECT` and `INSERT` on `raw`). Only a
  code-execution fault in `fleet-telemetry` after the mTLS handshake can
  use it. Such a fault already reaches the records in flight; with the URL
  it can also read the stored raw history and insert rows. A file for the
  URL (mode 0600, read and deleted by `ingest` at start) does not help on
  Railway: `ptrace_scope` is 0 there, so the child can also read the
  memory of `ingest`. Only a separate user for the child (option 2) closes
  the exposure on every kernel. Decision: no separate user and no
  `PR_SET_DUMPABLE` change. Reassess with a security finding in the
  handshake path of `fleet-telemetry`, a change of the user model of the
  image, or more privileges for the role `telemetry_ingest`.
- **The entrypoint runs as root before the drop.** A fault in the
  entrypoint runs as root. The script is short, takes no input except the
  service variables, and writes only the two TLS files. `vuln` pins its
  git blob ID. It removes old TLS files with `rm -f` and then writes with
  noclobber (`set -C`). The noclobber of dash is not an `O_EXCL` open: for
  an existing target that is not a regular file, the write can succeed
  (for example through a symbolic link to a FIFO). So the defence against
  a link that a process of UID 10001 left in a reused container
  filesystem rests on the `rm -f`, and on the premise that no process of
  UID 10001 runs during the root part of the entrypoint.
- **The shell variables `cert` and `key`.** The entrypoint copies the TLS
  variables to them. If the container environment already holds a
  variable with one of these names, the shell keeps it exported, and the
  key reaches the environment of `ingest`. Precondition: a Railway service
  variable named `cert` or `key`. Issue #31 tracks the fix. It has no
  schedule, because the fix changes a watch path and so releases the
  service again.
- **The pre-deploy command runs as root.** It stays
  `/usr/local/bin/ingest migrate`. A wrapper that drops to UID 10001 would
  change a Railway setting, and a rollback to an earlier image without the
  wrapper would then fail its pre-deploy step. Bounds: the pre-deploy
  container is short-lived and separate from the service; it holds
  `MIGRATION_DATABASE_URL` anyway; `ingest migrate` opens no listener,
  takes no input except the answers of the database, and ends at the
  latest at the pre-deploy timeout. Root there adds no network exposure.
  Reassess when Railway can set the user of the pre-deploy command, or
  when `ingest migrate` gets a listener or another input.

Reassess option 1 when a release check shows a `Uid` other than 0 for
PID 1, or with a change of the user model of the image.

## Key alternatives

- **MQTT, Redis pub/sub.** Need a broker or a service. MQTT loses the
  vehicle time; Redis keeps nothing for an absent subscriber and logs the
  VIN. Rejected.
- **Logger output through a pipe.** No ack source and no stable format.
- **Kafka, Kinesis, Google Pub/Sub.** New paid providers.
- **A Rust implementation of the vehicle protocol.** The only true
  end-to-end ack, but first-party code on the mTLS endpoint and an
  undocumented envelope format. Rejected for now.
- **Two Railway services.** No gate across services; each deploy loses
  acked records.
- **Stop the child instead of a gate.** Cannot close the start race of fact
  6.
- **Flush every few seconds.** About 180 CU-hours in the worst case.
- **A durable spool on a Railway volume.** A crash would lose almost
  nothing. Rejected for now: with a volume each redeploy has a downtime,
  and a broken release stops ingest; the pre-deploy command has no volume
  [R8]. The spool is the known measure against a loss above the 15-minute
  budget at a restart during a long outage. Owner decision of 27 November
  2026: the owner accepts that a restart during a database outage longer
  than 15 minutes can lose the whole in-memory queue (at most 64 MiB), and
  there is no durable spool for now. Reassess when a crash or an
  outage restart shows a real gap, or when Railway keeps the old deployment
  when a new deployment with a volume fails. The earlier condition, a
  container build check before merge, is met (CI job `image-check`).

## Consequences

- No record reaches the Railway log.
- Each merge that touches the ingest paths redeploys the service; the
  vehicle disconnects for some seconds and buffers.
- Data reaches PostgreSQL up to 15 minutes late. Owner decision of 27
  November 2026: views up to 15 minutes behind are acceptable, and the live
  dashboard is optional. A live view would need another path, for example a
  second ZMQ subscriber.
- First-party code sits on the public port, but it only copies bytes.
- On Railway the init does the PID 1 duties; `ingest` still handles signals
  and reaps its own child.

## Cost of reversal

Medium. The transport is a few lines of config and one reader task. A Rust
protocol server would replace the gate and the child and keep the writer.

## Reassessment

- Gap events or crash restarts show a loss above the accepted budget.
- `zmq_shortfall` events appear, or the watchdog ends the process.
- The Neon CU-hours pass 65 in a month, or the owner asks for the optional
  live dashboard.
- The first `gate_full` event, or peers that stall inside the handshake.
- Upstream changes the ZMQ dispatcher or adds a consumer ack.
- `zeromq` stops releases, or the vendored `.proto` files fall two upstream
  releases behind.

## Lead decisions

1. **Flush interval: default 900 s.** A used-up CU-hour quota suspends Neon
   until the next month. An interval of 300 s or less keeps the Neon
   compute active for the whole awake time, because the compute suspends
   only after 5 minutes without activity. Owner decision of 27 November
   2026: it stays 900 s.
   Reassess after the one-week follow-up with the measured awake hours.
   Lower the interval only if the measured awake hours keep a month below
   60 CU-hours at the always-on rate.
2. **Second database role `telemetry_migrator`** (ADR 0002), so
   `neondb_owner` stays out of Railway.
3. **Fixed compute of 0.25 CU** for production (endpoint
   `ep-young-dawn-bgyaj83l`) and `test`, confirmed by the owner on 27
   November 2026, together with a storage without a connection pool.
   Verify the effective size and the consumption after the first week of
   ingest.
4. **`delivery_policy` stays unset.** Tesla documents neither the default
   nor the billing effect. Confirmed by the owner on 27 November 2026.
   Reassess with the `is_resend` data of the first week, or with Tesla
   documentation.
5. **`jiff` and `jiff-sqlx` line 1.**
6. **Accepted gate risk.** A live peer that stalls inside the TLS handshake
   holds a connection, and a flood that fills the 1024 bound blocks the
   vehicle while it lasts plus the dead-peer bound (gate rule 3), as with
   upstream. No `TCP_USER_TIMEOUT` and no handshake deadline; the owner
   accepted this risk on 27 November 2026. Reassess at the first
   `gate_full` event or on evidence of stalling peers.
7. **Railway settings by hand; Infrastructure as Code waits.** Railway
   deprecated config as code [R9]. The owner decided on 27 November 2026: no
   `railway.json` and no file in its place; Infrastructure as Code
   (`.railway/railway.ts`) when it is possible and safe. The blocking facts for Infrastructure as Code
   (`.railway/railway.ts`) [R11][R12]: the reference does not document the
   Dockerfile path, watch paths, pre-deploy timeout, draining and overlap
   time, restart policy, or TCP proxy, nor whether an apply keeps port
   57438 or starts a deployment. The December 2026 reassessment is a
   separate task with a `railway config plan` dry run (owner decision).
   Adopt Infrastructure as Code only if the plan does not touch the TCP
   proxy port and starts no deployment. A plan cannot show whether an
   apply starts a deployment.

## To verify at first deployment

`docs/ingest-release.md` gives the command and the expected result of each
point. `image-check` covers the container part before the merge. Status
after the releases of PR #23 and PR #30 (27 November 2026):

| Point | Status |
| ----- | ------ |
| Railway runs `CMD` through the entrypoint | Closed: `ingest` is PID 2. |
| Healthcheck on `PORT`; pre-deploy `ingest migrate` | Passed; a stop on failure is not observed. |
| Settle time of 250 ms | Pending: no `zmq_shortfall` after the first vehicle connection of a start. |
| SUB socket defences: `V`, `alerts`, `errors` go to `zmq` | Closed. |
| SUB socket defences: child exits, stall rule, high water mark | Pending: each child exit ends the socket; no false `subscriber_stalled` in the first week. |
| Client address through the TCP proxy | Closed: shared range 100.64.0.0/10; no per-source limit. |
| Gate rule 1 | Pending: first-week maximum of `first_byte_ms`. |
| Quiet connection of 30 minutes | Pending. |
| Dead-peer bound | Pending: about 150 s idle, about 1,045 s with unacknowledged data. |
| `dispatch_total` on the real binary | Pending: one read after the first records. |
| No `test_overrides` line | Closed. |
| `unknown_fields` false positives, Neon compute size and use | Pending (one week). |

Linux checks (`STACK.md` §13):

1. `cargo nextest run --workspace --locked` on linux/amd64. Closed by CI.
2. On a redeploy, the old deployment logs `shutdown_started` (`SIGTERM`),
   `gate_stopped`, `child_killed` (`exit_signal` 9), the `stopped` event,
   and `shutdown_complete` within `drainingSeconds`, without
   `task_stop_timeout` or `gate_connections_open`. Closed (PR #30).
3. No zombie after three child exits. Partly closed.
4. A forced end of `ingest` ends the container, Railway restarts it, and a
   new `started` event follows. Pending.
5. Each `gate_connection_closed` of a normal connection has `socket_options`
   `set`, `keepalive` `15/15/9`, and `nodelay` `true`. Partly closed.
6. `fleet-telemetry` starts with an empty environment and no listener of
   `ingest`. Closed by `image-check` only.

## Not verified

Whether the vehicle resends a sent message without an ack, or a
rate-limited message.

## Sources

- Upstream: <https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c> (tag `v0.10.1`).
- [T] Tesla Fleet Telemetry: <https://developer.tesla.com/docs/fleet-api/fleet-telemetry>
- [Z1] libzmq `zmq_socket`: <https://libzmq.readthedocs.io/en/latest/zmq_socket.html>
- [Z2] libzmq `zmq_setsockopt`: <https://libzmq.readthedocs.io/en/latest/zmq_setsockopt.html>
- [Z3] ZeroMQ Guide, chapter 1: <https://zguide.zeromq.org/docs/chapter1/>
- [R1] Railway, Deployment teardown: <https://docs.railway.com/deployments/deployment-teardown>
- [R3] Railway, Healthchecks: <https://docs.railway.com/deployments/healthchecks>
- [R4] Railway, Pre-deploy command: <https://docs.railway.com/deployments/pre-deploy-command>
- [R8] Railway, Volumes: <https://docs.railway.com/volumes>
- [R9] Railway, Config as code: <https://docs.railway.com/guides/config-as-code>
- [R11] Railway, Infrastructure as Code: <https://docs.railway.com/infrastructure-as-code>
- [R12] Railway, Infrastructure as Code reference: <https://docs.railway.com/infrastructure-as-code/reference>
- [L] Linux `v6.12`, `tcp_timer.c` and `ip-sysctl.rst`: <https://github.com/torvalds/linux/tree/v6.12>
- [K] Linux `pid_namespaces(7)`: <https://man7.org/linux/man-pages/man7/pid_namespaces.7.html>
- [P1] Podman `--init`: <https://github.com/containers/podman/blob/main/docs/source/markdown/options/init.md>
- [P2] catatonit: <https://github.com/openSUSE/catatonit/blob/main/catatonit.c>
- [N1] Neon plans: <https://neon.com/docs/introduction/plans>
