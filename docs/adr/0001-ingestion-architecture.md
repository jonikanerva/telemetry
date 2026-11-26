# ADR 0001: Ingestion architecture

- Status: Accepted, 26 November 2026. Issue #4. The lead decided the open
  points within delegated authority on 26 November 2026 (section "Lead
  decisions").
- Scope: the path from the vehicle to PostgreSQL. ADR 0002 owns the tables.

## Context

The Railway service `telemetry` runs Tesla's `fleet-telemetry` v0.10.1. The
server terminates the vehicle mTLS WebSocket behind a Railway TCP proxy
(`telemetry.calmdonut.com:57438` to container port 4443). The server uses
only the `logger` dispatcher. Every record, with the VIN and the location,
goes to the Railway log. This must end.

The goal of this phase is a Rust ingest process that stores every record in
PostgreSQL 18 on Neon (Free plan). `VISION.md` puts "History first": a lost
hour of data cannot be collected again.

Facts that drive the decision. Upstream links point to tag `v0.10.1`, commit
`6689adb`.

1. Tesla documents [T]: on a loss of connectivity, and when the server
   disconnects, the vehicle buffers 5000 messages, "at least 2,500 seconds
   of data", and delivers them after the reconnect. It reconnects with
   exponential backoff, at most 30 s apart. When `delivery_policy` is
   `latest`, it resends data that the server did not ack. The vehicle
   config does not set `delivery_policy`, and Tesla documents no default.
   Not verified: what the vehicle does with a message that it sent but
   that the server did not ack, without `delivery_policy`. The owner saw
   records with `IsResend` under the current config, so some resend
   happens.
2. The server acks a record right after it hands the record to the
   dispatchers. `Dispatch` calls each producer in order, in the read loop of
   the socket ([`telemetry/serializer.go#L90`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/telemetry/serializer.go#L90),
   [`server/streaming/socket.go#L273-L280`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/socket.go#L273-L280)). Without reliable acks, the
   vehicle gets the ack before the consumer has the record.
3. With `reliable_ack_sources`, the ack waits only until the producer call
   returns. No dispatcher waits for a consumer:

   | Dispatcher  | When the reliable ack fires                                   | Consumer down or slow                                                 |
   | ----------- | ------------------------------------------------------------- | --------------------------------------------------------------------- |
   | `zmq`       | After `SendMessage` on a PUB socket returns ([`datastore/zmq/zmq.go#L73-L86`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/zmq/zmq.go#L73-L86)) | PUB send never blocks. PUB drops all messages when no subscriber is connected, and drops messages for a subscriber at the high water mark (default 1000) [Z1][Z2][Z3]. The record is acked and lost. |
   | `mqtt`      | After every per-field publish completes ([`datastore/mqtt/mqtt.go#L139-L188`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/mqtt/mqtt.go#L139-L188)). The broker acks only at QoS 1 or 2; the default is QoS 0 ([`mqtt.go#L65`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/mqtt/mqtt.go#L65)) | At QoS 1 or 2 the broker holds the record. But each field goes out as a bare value, without `created_at` or `is_resend` ([`datastore/mqtt/mqtt_payload.go#L14-L28`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/mqtt/mqtt_payload.go#L14-L28)). |
   | `redis`     | After `PUBLISH` returns without error ([`datastore/redis/redis.go#L97-L120`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/redis/redis.go#L97-L120)) | Pub/sub keeps nothing. A publish with no subscriber succeeds. The record is acked and lost. |
   | `logger`    | Never. The config rejects `logger` as an ack source ([`config/config.go#L492-L521`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L492-L521)) | Not applicable.                                                       |

   `connectivity` cannot have reliable acks ([`config/config.go#L495`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L495)). The
   server creates these events itself, with its own clock
   ([`server/streaming/server.go#L149-L160`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/server.go#L149-L160)).
4. The ZMQ message has two frames: the topic `<namespace>_<record type>`
   and the record payload. It does not carry the transaction ID or the
   receive time ([`datastore/zmq/zmq.go#L77`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/zmq/zmq.go#L77)).
5. With `rate_limit.enabled`, the server skips a message over the limit. It
   does not dispatch it and does not ack it
   ([`server/streaming/socket.go#L205-L218`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/socket.go#L205-L218)). The current limit is 1000
   messages in 30 s. A reconnecting vehicle can send 5000 messages.
6. The server binds the ZMQ socket and then listens on its TLS port at once
   ([`cmd/main.go#L66-L79`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/cmd/main.go#L66-L79)). It installs no signal handler. On SIGTERM, the
   Go runtime ends the process at once.
7. Railway sends SIGTERM to the old deployment when the new one is active.
   The time to SIGKILL (`RAILWAY_DEPLOYMENT_DRAINING_SECONDS`) defaults to
   0 [R1][R2]. A healthcheck runs only before a deployment becomes active
   [R3]. A pre-deploy command runs in a separate container with the service
   variables. A failed command stops the deployment [R4]. The default restart
   policy is `On Failure` with at most 10 restarts [R5].
8. When PID 1 of a PID namespace ends, the kernel sends SIGKILL to all
   processes in the namespace [K].
9. Neon: compute scales to zero after 5 minutes of inactivity and cannot
   stay on. The Free plan has 100 CU-hours each month for each project. Neon
   suspends the compute when the CU-hours are used, and rejects writes when
   the 1 GB storage is full [N1]. A suspended compute starts again "within a
   few hundred milliseconds" [N2]. The documentation does not say whether an
   idle open connection keeps the compute active [N3].

## Decision

### Transport: ZMQ on the loopback interface

`fleet-telemetry` publishes all four record types with the `zmq`
dispatcher to `tcp://127.0.0.1:5284`. The Rust process subscribes with a SUB
socket to the prefix `tesla_telemetry_`. The `logger` dispatcher is removed.
`reliable_ack_sources` stays unset, because a reliable ack on ZMQ gives no
extra guarantee (fact 3) and the ack channel is unbuffered
([`config/config_initializer.go#L67`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config_initializer.go#L67)).

ZMQ is the only candidate without a broker or a new provider that delivers
the complete record. The Rust process compensates for the missing ZMQ
guarantee with the layout below.

### Layout: one Railway service, one image, Rust is PID 1

The service `telemetry` runs one image with two processes.

- `ingest` (Rust) is PID 1. It starts `fleet-telemetry` as its child and
  supervises it. Because of fact 8, `fleet-telemetry` never runs without
  the consumer.
- `ingest` owns the public port. It listens on `0.0.0.0:4443` and forwards
  each accepted TCP connection byte for byte to `fleet-telemetry` on
  `127.0.0.1:4444`. It does not terminate TLS and does not read the bytes.
  This is the **gate**. `fleet-telemetry` still terminates mTLS.
- The gate is open only when all of these are true:
  1. the child runs;
  2. the SUB socket is connected and 250 ms have passed since its
     `Connected` event, so that the PUB side has applied the subscription;
  3. the start-up check passed, or the database has given **no answer**
     for 120 s. The check is one database round trip, and every migration
     that the binary embeds is applied (ADR 0002).
     - **No answer** means that no session starts: a network or TLS error,
       a connect timeout, or any other error before the session is
       authenticated, except an authentication error. Neon does not
       document the error of a compute suspended for the CU-hour quota, so
       that error also counts as no answer.
     - **An answered failure** is an authentication error (SQLSTATE class
       28) or a missing migration. The gate stays closed, and `/health`
       fails. This is a configuration fault, and the vehicle buffers.
     - After 120 s of no answer, the gate opens in **degraded mode**: the
       records go to the queue, and the writer runs the same check before
       its first write. If that check is an answered failure, the gate
       closes at once and the writer keeps the queue.
     - Reason: after a restart during a database outage, the 64 MiB queue
       then protects the data, not only the vehicle buffer of 5000
       messages. The pre-deploy command proves the schema just before a
       deploy, so degraded mode does not accept records for a schema that
       is too old.
     - The `started` and `degraded_mode` events wait in the memory queue,
       so a crash in degraded mode leaves no event of that run.
     - The check repeats on the cadence of the retry rule in "Delivery
       path";
  4. the queue did not close the gate. The queue closes the gate at the high
     mark. The gate opens again when the queue is below the low mark and the
     last write succeeded.
- When the gate closes, `ingest` stops forwarding and closes every
  forwarded connection. The vehicle then keeps its data in its own buffer
  (fact 1). This moves buffering to the vehicle whenever the consumer cannot
  take records. A closed gate accepts each new connection and closes it at
  once (PR #18): the listener stays bound, so a wrong port fails the start,
  and no client waits in the listen backlog. The Railway TCP proxy accepts
  the client connection first in both designs, so the vehicle sees the same
  result as with a closed listener.
- The gate closes the start race of fact 6. The vehicle cannot reach
  `fleet-telemetry` before the subscription exists.
- Connection rules. The gate cannot see inside TLS, so it does not guess
  which connection is the vehicle. It has three rules only:
  1. **First answer within 10 s.** The gate closes a connection when
     `fleet-telemetry` has sent no byte 10 s after the accept. Evidence:
     the Go TLS server reads the complete ClientHello before it writes
     anything
     ([`crypto/tls/handshake_server.go#L43-L64`](https://github.com/golang/go/blob/go1.27.4/src/crypto/tls/handshake_server.go#L43-L64)),
     and it answers a ClientHello at once, with its first flight or with an
     alert. Upstream sets no timeout on its `http.Server`
     ([`server/streaming/server.go#L84`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/server.go#L84)),
     so Go applies no handshake deadline
     ([`net/http/server.go#L965-L977`](https://github.com/golang/go/blob/go1.27.4/src/net/http/server.go#L965-L977)).
     A peer that never sends a ClientHello therefore never gets a byte and
     is closed. A vehicle sends its ClientHello as the first message after
     the TCP connect. The ClientHello is one small message, and 10 s allows
     several TCP retransmissions on a poor mobile link. A vehicle that
     misses the rule reconnects with its normal backoff and buffers
     meanwhile. Not verified on the vehicle: the deployment PR checks it.
  2. **Resource bound of 1024 connections.** The bound exists only to bound
     memory and file descriptors. Each forwarded connection uses two
     copy buffers of 8 KiB (one `BufReader` of 8 KiB with
     `tokio::io::copy_buf` for each direction, PR #18), so 1024 connections
     use 16 MiB of buffers, plus small task state. The 16 MiB excludes kernel socket
     buffers and the memory that `fleet-telemetry` uses for each
     connection; the deployment PR records the container memory at the
     bound. `ingest` holds two descriptors for each connection (2048), and
     `fleet-telemetry` one. At the bound, the gate accepts and closes each
     new connection at once, counts it, and records at most one `gate_full`
     event each minute with the count (ADR 0002).
  3. **TCP keep-alive on each accepted socket**, with the values that Go
     uses for `fleet-telemetry` today: idle 15 s, interval 15 s, 9 probes
     ([`net/dial.go#L18-L27`](https://github.com/golang/go/blob/go1.27.4/src/net/dial.go#L18-L27),
     [`net/tcpsock.go#L289-L307`](https://github.com/golang/go/blob/go1.27.4/src/net/tcpsock.go#L289-L307)).
     Behind the gate, these Go probes reach only `ingest` on the loopback
     interface, so the gate must set them on the public side. The kernel
     then closes a connection whose peer is gone, and the copy task ends
     with an error. The **dead-peer bound** has two cases:
     - An idle gone peer (no unacknowledged data on the socket): keep-alive
       closes it after about 15 s + 9 × 15 s = 150 s.
     - A gone peer with unacknowledged data (the gate has sent bytes that
       the peer never acked): Linux sends no keep-alive probe while data is
       unacknowledged
       ([`net/ipv4/tcp_timer.c#L794-L795`](https://github.com/torvalds/linux/blob/v6.12/net/ipv4/tcp_timer.c#L794-L795)),
       so the retransmission timeout closes it instead. With the default
       `tcp_retries2` of 15, that is about 925 to 1,045 s
       ([`ip-sysctl.rst`, `tcp_retries2`](https://github.com/torvalds/linux/blob/v6.12/Documentation/networking/ip-sysctl.rst#L697-L707)).

     Go sets no `TCP_USER_TIMEOUT` either, so both cases equal the present
     deployment. The larger bound is below the vehicle buffer of at least
     2,500 s. A live peer answers the probes in its TCP stack, also when it
     sends no data, so a quiet, authenticated vehicle is not affected. Set
     in Rust with a direct dependency on `socket2` 0.6 with the crate
     feature `all` (0.6.5, MIT OR Apache-2.0):
     `SockRef::from(&stream).set_tcp_keepalive(&TcpKeepalive::new()
     .with_time(15 s).with_interval(15 s).with_retries(9))`. `with_retries`
     exists only with feature `all`, and `zeromq` 0.7 pulls `socket2`
     without features, so `ingest` declares it directly. This is a safe
     API; first-party code needs no `unsafe`. The ingest PR tests that an
     accepted socket has these values. When the operating system rejects
     an option, the connection stays open (PR #18): the history comes
     first, and the dead-peer bound of that connection then depends on the
     operating system defaults. The `gate_connection_closed` event of that
     connection has `socket_options` `failed`; deployment check 5 below
     looks for it.

  After rule 1, the gate never closes or evicts a connection by itself.
  Only the peer, the dead-peer bound (about 150 s idle, about 925 to
  1,045 s with unacknowledged data), `fleet-telemetry`, a closed gate, or
  the shutdown close it. A quiet, authenticated vehicle therefore keeps
  its connection. When `fleet-telemetry` ends its side, the gate forwards
  the remaining bytes and the end of the stream to the client, waits at
  most 5 s for the client to end its side, and then closes the whole
  connection (PR #18). So a client cannot keep a slot after
  `fleet-telemetry` closes; the 5 s start after the last bytes are written
  to the client socket.

  Comparison with the present deployment: today `fleet-telemetry` accepts
  connections without a count limit and without a handshake timeout, and
  the kernel closes a dead peer of Go within the same dead-peer bound (Go
  sets keep-alive and no `TCP_USER_TIMEOUT`). The Go runtime
  raises its open-file soft limit to the hard limit [G], so today the
  bound is the hard limit of the container and its memory. Railway
  documents neither. The gate keeps the same dead-peer bound and the same
  exposure class (a live peer that stalls inside the handshake holds a
  connection), and removes one case (a silent peer). A flood of 1024
  connections is enough to fill the gate. A flood is also enough today, but
  at a limit that nobody has measured. Keep-alive works only on the TCP
  connection that the container sees. Whether the Railway TCP proxy passes
  the client's TCP connection through or terminates it is not documented;
  if it terminates it, the close of a dead client depends on the proxy,
  today and with the gate (a deployment check).
  The deployment PR records `ulimit -n` of the container. If the soft
  open-file limit is below 2048 plus a margin, `entrypoint.sh` raises it to
  the hard limit (`ulimit -n`) before the `exec`, as Go does for itself.

Start order: `entrypoint.sh` writes the TLS files and replaces itself with
`ingest run` (`exec`). `ingest` starts the child, connects SUB, runs the
start-up check, and opens the gate. The image uses `CMD`, not `ENTRYPOINT`,
so that the pre-deploy command replaces the start command. The deployment PR
moves `Dockerfile`, `entrypoint.sh`, and `config/server_config.json` to
`ingest/`.

When the child exits: close the gate, record a gap event (ADR 0002),
restart the child after 1 s, doubling to at most 30 s. The queue stays in
memory. When `ingest` itself fails, the container ends. Railway restarts it
with restart policy `ALWAYS`.

**Watchdog.** Railway runs the healthcheck only at the start of a
deployment [R3], so a hung `ingest` would stay up while the child keeps
acking. `ingest` therefore runs a watchdog on a separate operating-system
thread, outside the Tokio runtime:

- The reader, the writer, and the gate acceptor each update a heartbeat
  timestamp at least once a second, also when they have no work.
- The writer's heartbeat covers its waits: every database call has a
  deadline (connect 15 s, statement 30 s), and the writer also beats while
  it sleeps before a retry. So a live writer always beats within 120 s.
- If the heartbeat of the reader or of the gate acceptor is older than
  30 s, or the heartbeat of the writer is older than 120 s, the watchdog
  logs the task name and ends the process with `std::process::exit`.
- PID 1 then ends, the kernel kills the child (fact 8), and Railway
  restarts the container. The vehicle buffers meanwhile.

**ZMQ shortfall check.** PUB drops messages without notice when its send
queue for the subscriber is full (fact 3). Upstream sets no `SNDHWM`, so
the libzmq default of 1000 applies, and the real limit can be up to 90 %
lower [Z2]. The reader never waits for the writer or the database, so the
send queue fills only when the reader is starved or hung. `ingest` cannot
see these drops directly. It detects them:

- `fleet-telemetry` exports `dispatch_total` for each record type on
  `127.0.0.1:9090/metrics`
  ([`server/streaming/socket.go#L288-L291`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/socket.go#L288-L291),
  [`server/monitoring/metrics_server.go#L31-L44`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/monitoring/metrics_server.go#L31-L44)).
- Every 60 s, `ingest` reads the counter for `V`, `alerts`, and `errors`,
  and compares the growth since the child started with its own count of
  received messages for these topics. The gap of one read is
  `dispatched - received`.
- The counter grows when `Dispatch` returns, so one read can count messages
  that are still in flight. `ingest` therefore reports a shortfall only
  when two reads in a row both show a gap. The confirmed gap is the smaller
  of the two. The gap is cumulative since the child started, so `ingest`
  reads its own count after each HTTP response and reports only the part of
  the confirmed gap that it has not reported before, as an `ingest_event`
  of kind `zmq_shortfall` with the count and the time window (ADR 0002).
  The values of the lost records are gone, but the gap is visible.
- The shutdown sequence makes one final read after its drain step. No
  message is in flight then, so one read is enough.
- `connectivity` records are outside the check: the server dispatches them
  directly, and `dispatch_total` does not count them
  ([`server/streaming/server.go#L149-L191`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/server.go#L149-L191)).
- When the watchdog ends the process, no final read happens and no
  `zmq_shortfall` event exists for that run.
- The ingest PR confirms the metric name and labels against the real
  binary.

Railway configuration as code in `/ingest/railway.json`:

| Setting                      | Value                                   | Reason                                            |
| ---------------------------- | --------------------------------------- | ------------------------------------------------- |
| `build.dockerfilePath`       | `ingest/Dockerfile`                     | The build needs the repository root as context.   |
| `build.watchPatterns`        | `ingest/**`, `crates/ingest/**`, `migrations/**`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | A web merge does not redeploy ingest. |
| `deploy.preDeployCommand`    | `["/usr/local/bin/ingest", "migrate"]`  | Migrations are a separate release step.           |
| `deploy.healthcheckPath`     | `/health` on `PORT`                     | A broken release does not replace a working one.  |
| `deploy.drainingSeconds`     | `30`                                    | Time for the shutdown sequence below.             |
| `deploy.overlapSeconds`      | `0`                                     | The vehicle reconnects by itself.                 |
| `deploy.restartPolicyType`   | `ALWAYS`                                | No limit of 10 restarts.                          |

`/health` returns 200 only when the gate is open and the start-up check
passed. Degraded mode does not count, so a deploy during a database outage
fails its healthcheck and the old deployment keeps running.

### `fleet-telemetry` configuration

`host` `127.0.0.1`, `port` 4444, `records` `V`, `alerts`, `errors`,
`connectivity` to `["zmq"]`, `zmq.addr` `tcp://127.0.0.1:5284`,
`transmit_decoded_records` `false`, `log_level` `info` (`debug` logs raw
payloads, [`telemetry/record.go#L130`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/telemetry/record.go#L130)), `rate_limit.message_limit`
10000 in 30 s (above the vehicle buffer of 5000), no `reliable_ack`,
`monitoring.prometheus_metrics_port` 9090 on the default host `127.0.0.1`
(for the shortfall check).

The vehicle telemetry config stays as merged in PR #6. It does not set
`delivery_policy` (lead decision 4). Tesla documents neither the default
nor the billing effect, so the 3 USD rule of `STACK.md` §15 cannot be
applied to it.

### Wire format: protobuf

`transmit_decoded_records` is `false`. The payload is the protobuf message
of the record type. The Rust process decodes it with code generated from
the upstream `.proto` files of the deployed tag. The files are vendored in
`crates/ingest/proto/`, with the tag and commit in a comment. They change
only together with the `fleet-telemetry` image tag.

Reasons: protobuf keeps `int32`, `int64`, `float`, and `double` apart.
Enums stay numbers, also unknown ones. JSON (`protojson`) writes `int64` as
strings and enum names, and needs a hand-written mapping for about 55 value
variants.

Note: upstream changes some values before dispatch, in every format. It
parses location strings, rewrites strings in scientific notation to five
decimals, and shifts the AEB enum for client 1.3.0, which this vehicle runs
([`telemetry/record.go#L152-L278`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/telemetry/record.go#L152-L278)). The stored value is the value after
these changes.

### Crates

| Purpose            | Crate and line                        | Licence             | Maintenance (crates.io, 26 Nov 2026) | C code or first-party `unsafe` |
| ------------------ | ------------------------------------- | ------------------- | ------------------------------------ | ------------------------------ |
| ZMQ SUB            | `zeromq` 0.7, features `tokio-runtime`, `tcp-transport` | MIT  | 0.7.0, 27 Oct 2026             | Pure Rust. No `unsafe` in its `src/`. Reconnects by itself. |
| Gate keep-alive    | `socket2` 0.6, feature `all` (direct dependency) | MIT OR Apache-2.0 | 0.6.5, 24 Nov 2026     | Safe API for gate rule 3. `TcpKeepalive::with_retries` exists only with feature `all`; `zeromq` pulls `socket2` without features. The crate uses `unsafe` inside; first-party code needs none. |
| Protobuf runtime   | `prost` 0.14, `prost-types` 0.14      | Apache-2.0          | 0.14.6, 12 Nov 2026                  | None.                          |
| Protobuf codegen   | `prost-build` 0.14 with `protox` 0.10 (build only) | Apache-2.0; MIT OR Apache-2.0 | 0.14.6; 0.10.0, 7 Oct 2026 | Pure Rust. No `protoc` binary. |
| Database           | `sqlx` 0.9, `postgres`, `runtime-tokio`, `tls-rustls-aws-lc-rs`, `migrate`, `macros`, `uuid`, `json` | MIT OR Apache-2.0 | 0.9.1, 18 Nov 2026 | aws-lc-rs builds C. It is the one rustls provider of `STACK.md` §5. |
| Time               | `jiff` 1, `jiff-sqlx` 1               | Unlicense OR MIT    | 1.0.3; 1.0.0, 15 Nov 2026            | None. Line 1 is the latest stable release (lead decision 5). |
| Runtime and rest   | `tokio`, `tokio-util`, `axum` (health), `tracing`, `tracing-subscriber`, `thiserror`, `uuid`, `serde_json`, `regex` | MIT or MIT/Apache-2.0 | Current | None. |

Rejected crates: `zmq` 0.10 (last release 4 November 2022, links the C++
library libzmq). `protobuf` 4.x from Google builds C code through `cc`.
`protobuf` 3.7 had its last release on 10 March 2025.

`prost` drops unknown fields
([prost-derive 0.14.6 `src/lib.rs` line 191](https://docs.rs/crate/prost-derive/0.14.6/source/src/lib.rs)).
A new `Value` variant therefore decodes as `None`. ADR 0002 stores such a
record unchanged instead of a guessed value. The decode also finds every
other field that the vendored schema does not know and stores the record
with reason `unknown_fields` (ADR 0002, "Unknown data").

### Delivery path and bounded buffering

1. A reader task reads the SUB socket without pause. It decodes each
   message and puts one item into one in-memory queue. An item holds the
   topic, the original payload bytes, the receive time, and the decoded
   rows. It stays in the queue until its rows commit, because ADR 0002 can
   need the original bytes.
2. The queue is bounded by 64 MiB, counted as the payload bytes plus the
   decoded rows of all items. High mark: 75 % closes
   the gate. Low mark: 25 %, after a successful write, opens it again. The
   last 25 % takes the records that are in flight when the gate closes.
3. If the queue is full, the reader drops the newest message, counts it, and
   keeps reading. The writer stores the count and the time range as a gap
   event (ADR 0002).
4. A writer task flushes when the oldest queued record is 15 minutes old,
   when 5000 signal rows wait, or at shutdown. A flush is normally one
   transaction (ADR 0002). The interval is the variable
   `INGEST_FLUSH_INTERVAL_SECONDS`, default 900 (lead decision 1).
5. On a write failure, the transaction rolls back. The writer keeps the
   batch and retries after 1 s, doubling to at most 5 minutes, with jitter.
   It never gives up while the process runs. Every error except SQLSTATE
   classes 22 and 23 is retried: connection errors, timeouts, and all other
   classes. This includes a full Neon storage, whose SQLSTATE Neon does not
   document; the storage PR records it. Classes 22 and 23 are not retried
   as they are. The writer splits the batch into halves and writes each
   half in its own transaction, until it finds the bad record. That record
   goes to the unprocessed table (ADR 0002) in its own transaction. The
   other records commit.
   Retry cadence, for the writer and for the start-up check: a failure
   with no answer retries after 1 s, doubling to at most 5 minutes. An
   attempt with no answer starts no session, so it adds no compute time.
   When the database answers with the same failure three times in a row
   (for example an authentication error, a missing migration, or a full
   storage), the cadence drops to one attempt each 60 minutes until an
   attempt succeeds. Each answered attempt keeps the compute active for
   about 5 minutes, so a rejection that lasts a whole month costs at most
   about 61 hours, or 15 CU-hours.
   Recovery: an answered failure needs a manual fix (a credential, a
   migration, or free storage). After the fix, the lead restarts the
   service; the new process starts again at the 1-s cadence. A change of a
   Railway variable already redeploys the service. Reason: the fix is
   manual in any case, and a shorter cadence would keep the compute awake
   during a long rejection. Without the restart, the next attempt can come
   up to 60 minutes later, which is longer than the vehicle buffer.
6. The connection pool has at most 2 connections, none when idle, an idle
   timeout of 30 s, and a connect timeout of 15 s. No connection stays open
   between flushes (fact 9).

Why 15 minutes. At 0.25 CU, 100 CU-hours allow 400 active hours in a
730-hour month. A flush interval under 5 minutes keeps the compute active
for as long as the car streams, plus 5 minutes. The hours below come from
the usage estimate of the tuned vehicle config
([`docs/telemetry-usage-estimate.md`](../telemetry-usage-estimate.md)).
The estimate uses assumed hours, so these numbers are provisional.

| Flush interval | Planning case (133 awake hours, about 300 wake-ups) | Worst case (car awake all month) |
| -------------- | --------------------------------------------------- | -------------------------------- |
| 10 s           | about 160 h, 40 CU-hours                            | about 720 h, 180 CU-hours        |
| 15 minutes     | about 70 h, 18 CU-hours                             | about 240 h, 60 CU-hours         |

With 15 minutes, each flush keeps the compute up for about 5 minutes of
each 15. Only this interval keeps the worst case inside the plan and
leaves room for the web application. The cost: a crash of `ingest` can
lose up to 15 minutes of acked records. A deploy or a database outage does
not lose them (below). The arithmetic needs a compute of 0.25 CU, so the
production compute has a fixed size of 0.25 CU (lead decision 3, applied
on 26 November 2026).

The `test` branch (ADR 0002) draws from the same 100 CU-hours of the
project. Its compute also has a fixed size of 0.25 CU and scales to zero
after 5 minutes. Assumption: verification runs keep it active for at most
20 hours each month, including the 5-minute tails. That is at most 5
CU-hours. Rule: tests close their connections when they end, and no
process keeps a connection to the `test` branch open between runs.

| Budget line (CU-hours each month)  | Planning case | Worst case |
| ---------------------------------- | ------------: | ---------: |
| Production, 15-minute flush        | about 18      | about 60   |
| `test` branch, at most 20 h        | at most 5     | at most 5  |
| **Total**                          | **about 23**  | **about 65** |

The rest of the 100 CU-hours is the margin for the web application and for
error in the assumed hours.

### Shutdown on SIGTERM or SIGINT

1. Close the gate and close all forwarded connections. The vehicle
   reconnects to the new deployment.
2. Read the SUB socket until no message arrives for 1 s, at most 3 s.
3. Make the final shortfall read (see "ZMQ shortfall check").
4. Stop the child with SIGKILL. It has no vehicle connection at this point.
5. Flush the queue, with a deadline of 20 s.
6. If the flush fails, log the count of lost rows and exit with code 1.

The total stays under `drainingSeconds` 30.

### Delivery guarantee

| Hop                                   | Guarantee                                                                 |
| ------------------------------------- | ------------------------------------------------------------------------- |
| Vehicle to `fleet-telemetry`          | Messages sent while no connection exists are buffered (5000) and delivered after the reconnect [T]. A resend of a message that was sent but not acked is documented only for `delivery_policy: latest`, which the config does not set. The owner saw `IsResend` records, so some resends happen. Not verified. |
| `fleet-telemetry` to `ingest`         | At most once. ZMQ has no ack. The gate keeps the consumer present.       |
| `ingest` to PostgreSQL                | At least once while the process lives. Retries are safe, because every insert is idempotent (ADR 0002). |
| End to end                            | At least once, except in the bounded cases below. This includes deploys and database outages up to the buffer limits. The database removes duplicates. |

Remaining loss. Each line names the bound and how the loss becomes
visible:

| Case | Bound | Detection |
| ---- | ----- | --------- |
| `ingest` crashes (panic, out of memory, SIGKILL, host failure) | The whole queue: normally at most one flush interval (15 minutes); during a database outage everything that waits, up to 64 MiB. | A `started` event without an earlier `stopped` event. The count of lost rows is unknown. |
| `ingest` hangs without exiting while the child keeps acking | Until the watchdog ends the process: 30 s for the reader or the gate, 120 s for the writer. Then as in the crash line. | The watchdog log line, and a `started` event without a `stopped` event. A hang in the watchdog thread itself has no detection. |
| PUB drops at the libzmq send high water mark (reader starved or hung) | A hung reader: at most 30 s of messages, then the watchdog acts. A slow but live reader: no time bound, but the reader only decodes and queues, so it is far faster than one vehicle stream. | A slow reader: `zmq_shortfall` event after two reads with a gap. A hung reader: only the watchdog log line, because the watchdog exit makes no final read. Not when the child dies before the next read. |
| Database outage, process stays up | No loss until the queue is full (64 MiB, many days at the planned rate). Then the gate is closed, and the vehicle buffer (5000 messages, at least 2,500 s) is the last protection. After that the vehicle drops data. | `records_dropped` events for drops in `ingest`. Drops in the vehicle are not visible. |
| Restart during a database outage, including a Neon suspension after 100 CU-hours, which lasts until the next billing period [N1] | The queue that existed before the restart is lost (crash line). After the restart the gate stays closed for 120 s (vehicle buffer only), then opens in degraded mode. Then as in the outage line. | As in the crash and outage lines. A Neon suspension also shows as failed writes in the log. |
| Shutdown flush fails within 20 s, for example a deploy during a database outage | The queue at shutdown. | The log has the count. A `stopped` event cannot be written. |
| The child dies by itself | Messages in its libzmq send queue: milliseconds of data. | `child_exited` event. The count is unknown. |
| A connection closes with messages that were sent but not acked (gate closing, deploy, rate limit) | Those messages, if the vehicle does not resend them (fact 1, not verified). | None in `ingest`. Stored `is_resend` values show whether resends happen. |
| Accepted risk: a peer that stalls inside the TLS handshake | A live peer holds one connection until it closes it. A peer that goes away holds it until the dead-peer bound closes it (gate rule 3): about 150 s after its last answer when no data is unacknowledged (keep-alive), about 925 to 1,045 s when data is unacknowledged (retransmission timeout, `tcp_retries2` 15). Both are below the vehicle buffer of at least 2,500 s. This is the same class of exposure that upstream has today. If the Railway proxy terminates TCP, the close of a gone peer depends on the proxy (deployment check). | None for a single peer. |
| Accepted risk: a flood that fills the 1024-connection bound | No new connection while the flood holds the bound: while the flooding peers keep their connections alive, plus the dead-peer bound after they go away (about 150 s idle, about 925 to 1,045 s with unacknowledged data). The vehicle cannot connect and relies on its own buffer (at least 2,500 s). Then as in the next line. | `gate_full` events, and gaps between `connectivity_event` rows. |
| Any downtime longer than the vehicle buffer for a reason other than the database: a crash loop of `ingest` or of the child, a watchdog loop, a closed gate after an answered failure, a broken release, a Railway outage, or a flood | No bound after the vehicle buffer (at least 2,500 s). The vehicle drops data. | Gaps between `connectivity_event` rows, missing `started` events, and the Railway deployment status. The amount of lost vehicle data is not visible. |

### Logging

`ingest` logs JSON through `tracing` with an allowlist of fields: event
name, record type, counts, sizes, durations, queue fill, gate state,
SQLSTATE, and constraint name. It never logs a VIN, a location, a signal
value, a payload, a database URL, or the text of a database error (the text
can contain row values). The child's stdout and stderr go through `ingest`.
`ingest` replaces each VIN-shaped token (17 characters from
`[A-HJ-NPR-Z0-9]`) with `[vin]` and truncates lines at 8 KiB. Upstream logs
a VIN on some error paths, for example
[`server/streaming/server.go#L197`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/server.go#L197).

## Key alternatives

- **MQTT.** Needs a broker process or service. The per-field messages lose
  `created_at` and `is_resend`, so the vehicle time is lost. Rejected.
- **Redis pub/sub.** Needs a Redis service. Pub/sub keeps nothing for an
  absent subscriber and the ack fires anyway. The Redis error logs contain
  the VIN ([`datastore/redis/redis.go#L103`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/redis/redis.go#L103)). Rejected.
- **Logger output through a pipe.** No ack source. The log format is not a
  contract. Diagnostics and records share one stream. A blocked reader stops
  all sockets. Rejected.
- **Kafka, Kinesis, Google Pub/Sub.** New paid providers. Rejected by
  `VISION.md`.
- **A Rust implementation of the vehicle protocol.** It could ack after the
  commit and use the vehicle buffer as the only queue. This is the only true
  end-to-end guarantee. But it puts first-party code on the internet-facing
  mTLS endpoint and re-implements an undocumented envelope format
  (`messages/` in upstream) with no specification except the source.
  `STACK.md` §0 keeps the upstream server. Rejected for now; see the
  reassessment conditions.
- **Two Railway services over the private network.** Each deploy or crash of
  `ingest` loses records while `fleet-telemetry` keeps acking, and the gate
  is not possible across services. It also costs about 1 to 2 USD each
  month, and `zeromq` has no CURVE encryption (the private network uses
  WireGuard [R6]). Rejected.
- **Stop the child instead of a gate.** Simpler, but it cannot close the
  start race of fact 6, and each pause restarts the server. Rejected.
- **Flush every few seconds.** Lowest crash loss, but the worst case uses
  about 180 CU-hours. Rejected for now (lead decision 1).
- **A durable spool on a Railway volume.** `ingest` would append each
  received message to a file on a volume (length, checksum, payload) and
  sync it before it counts as queued. It would remove the record from the
  file after the commit, and replay the file after a restart. This attacks
  the largest remaining loss: a crash would lose almost nothing, and a
  restart during a database outage would keep the queue. The volume would
  cost about 0.15 USD for each GB each month [R7], a few cents here.
  Rejected for now, because:
  - Railway keeps no two deployments active with the same volume, so each
    redeploy has a downtime even with a healthcheck [R3]. The old
    deployment stops before the new one is proven. A broken release then
    stops ingest until the lead redeploys the previous version, and the
    vehicle buffer (at least 2,500 s) is the only protection. Without a
    volume, a failed healthcheck keeps the old deployment running.
  - The repository cannot build or test the image before a merge (`STACK.md`
    §13), so a broken release is more likely than a crash of `ingest`.
  - The spool adds code with its own failure modes: sync, corruption,
    replay order, and a full volume. The pre-deploy command has no volume
    [R8].

  The crash loss stays bounded to one flush interval and is visible as a
  gap. Reassessment: the spool becomes the better trade when one of these
  is true: the repository gets a container build check before merge; a
  crash or a restart during an outage shows a real gap in `ingest_event`;
  or Railway keeps the old deployment when a new deployment with a volume
  fails.

## Consequences

- No record reaches the Railway log. The VIN leak of the logger ends.
- Every merge that touches the ingest paths redeploys the service. The
  vehicle disconnects for some seconds and buffers. Acked records are not
  lost. Messages in flight without an ack depend on a vehicle resend (not
  verified).
- Data reaches PostgreSQL up to 15 minutes late. The live dashboard of a
  later phase needs another path, for example a second ZMQ subscriber.
- First-party code now sits on the public port, but it only copies bytes.
- Accepted risk at the gate (lead decision 6): a live peer that stalls
  inside the TLS handshake holds a connection until it closes it; a peer
  that goes away is closed within the dead-peer bound (about 150 s idle,
  about 925 to 1,045 s with unacknowledged data), as with upstream today.
  A flood that fills the 1024-connection bound blocks the vehicle while
  the flood holds the bound, plus the dead-peer bound. Both bounds are
  below the vehicle buffer of at least 2,500 s, and the vehicle relies on
  that buffer meanwhile. `gate_full` events and
  connectivity gaps show it. See "Reassessment".
- The vehicle config does not change in this phase.
- `ingest` must handle PID 1 duties: signal handlers and reaping its child.

## Cost of reversal

Medium. The transport is a few lines of server config and one reader task.
A change to a two-service layout or to MQTT changes the deployment and the
reader. A Rust protocol server replaces the gate and the child, and keeps
the writer and the storage.

## Reassessment

- Gap events or crash restarts show material loss.
- `zmq_shortfall` events appear, or the watchdog ends the process.
- The Neon CU-hours of the project (production and `test` together) pass
  65 in a month, or the live dashboard needs data within seconds.
- The first `gate_full` event, or evidence of peers that stall inside the
  TLS handshake. A candidate measure then is to detect an established
  session (for example the first `connectivity` event after an accept) and
  to treat connections without one differently.
- Upstream changes the ZMQ dispatcher, adds a consumer ack, or stops
  maintenance.
- `zeromq` stops releases or the vendored `.proto` files fall two upstream
  releases behind.

## Lead decisions

The lead decided these points on 26 November 2026, within delegated
authority.

1. **Flush interval: default 900 s** (`INGEST_FLUSH_INTERVAL_SECONDS`).
   Reason: the Neon limit of 100 CU-hours is a hard stop. When it is used
   up, Neon suspends the compute until the next month, and days of history
   are lost. A crash loses at most one flush interval, and an
   `ingest_event` gap makes the loss visible. Each interval below the
   5-minute scale-to-zero timeout keeps the compute awake whenever the car
   is awake. So the real choice is "always on while awake" or about 15
   minutes. Reassessment: after 30 days of measured awake hours, lower the
   interval (for example to 60 s) if the worst observed month stays below
   60 CU-hours at the always-on rate.
2. **Second database role `telemetry_migrator`** (ADR 0002). The owner
   authorised an application role with limited privileges. A separate
   least-privilege migration role serves the same intent and keeps
   `neondb_owner` out of Railway. The lead reports this decision to the
   owner.
3. **Fixed production compute of 0.25 CU** (autoscaling minimum and maximum
   0.25). Applied by the lead on 26 November 2026 through the Neon API on
   the production endpoint `ep-young-dawn-bgyaj83l`. The response showed
   minimum 0.25, maximum 0.25, and `suspend_timeout_seconds` 0 (the plan
   default of 5 minutes). The `test` branch compute also has a fixed 0.25
   CU.
4. **`delivery_policy` is not adopted now.** Tesla documents neither the
   default nor the billing effect, so the 3 USD rule cannot be estimated.
   The vehicle config stays as merged in PR #6. Reassessment: evidence from
   the stored `is_resend` data or from Tesla documentation.
5. **`jiff` and `jiff-sqlx` line 1.** No manifest exists yet, so no
   separate `chore/deps` PR is needed. `DOCTRINE.md` P7 requires the latest
   stable release. `STACK.md` §6 now names line 1. The first workspace PR
   pins the exact resolved versions.
6. **Accepted gate risk.** The gate does not read TLS, so it cannot tell
   the vehicle from another peer. A live peer that stalls inside the TLS
   handshake holds a connection until it closes it, and a flood that fills
   the 1024-connection bound blocks the vehicle while it lasts plus the
   dead-peer bound: about 150 s for an idle gone peer (keep-alive), about
   925 to 1,045 s for a gone peer with unacknowledged data (retransmission
   timeout, `tcp_retries2` 15). The vehicle then relies on its own buffer
   (at least 2,500 s). This is the same class of exposure, and the same
   dead-peer bound, as upstream today. The decision adds no cost and the
   lead can reverse it. The lead reports this risk to the owner.
   `TCP_USER_TIMEOUT` was considered and not adopted: it would shorten the
   second case, but it is Linux-only conditional code that cannot be
   tested on the development machine, and Go sets none either, so it goes
   beyond parity with upstream. Reassessment: the first `gate_full` event,
   or evidence of peers that stall inside the handshake.

## To verify at first deployment

The deployment PR must produce evidence for each point. If a point fails,
the PR changes the design and updates this ADR.

- Railway runs the image `CMD` (through `entrypoint.sh` and `exec`) so that
  `ingest` is PID 1 in the container.
- Railway runs the healthcheck on `PORT` for a service that has only a TCP
  proxy and no HTTP domain, and keeps the old deployment when the
  healthcheck fails.
- The pre-deploy command runs `ingest migrate` in place of the start
  command, with the service variables, and a failure stops the deployment.
- A settle time of 250 ms after the SUB connection is enough for the PUB
  side to apply the subscription. The ingest PR also tests this against the
  real `fleet-telemetry` binary.
- Whether the container sees the client address of a connection through
  the Railway TCP proxy. Record the answer. A per-source limit is possible
  only if it does; that is a later decision.
- Gate rule 1 against the real vehicle: the gate records the time from
  accept to the first server byte for each connection. For the connections
  that stay open longer than 60 s (the vehicle), the maximum over the first
  week is far below 10 s, for example below 2 s. Record the values.
- An awake, parked vehicle that sends little, for example during AC
  charging, keeps one connection open for at least 30 minutes, with no
  reconnect loop: `connectivity_event` shows one `CONNECTED` and no
  `DISCONNECTED` in that time.
- Dead-peer bound through the Railway TCP proxy. A test client connects
  through the proxy, sends a ClientHello, receives the server flight, and
  then disappears without FIN or RST (for example, its network is cut).
  - Idle case: the client has acked everything before it disappears. The
    gate must close the connection within about 150 s plus a margin.
  - Unacknowledged-data case: the server side sends data after the client
    has disappeared (for example, the client disappears in the middle of
    the server flight). The gate must close the connection within about
    1,045 s plus a margin.
  - Record the container's `net.ipv4.tcp_retries2` value and kernel
    version, and whether the proxy passes the client's TCP connection
    through or terminates it. If a case does not close within its bound,
    or the proxy terminates TCP and keeps the container side open, record
    the result and decide the measure in the deployment PR.
- The container's open-file limit (`ulimit -n`) allows the 1024-connection
  bound, and the container memory at the bound. The ingest PR tests that
  the gate refuses connection 1025 and records `gate_full`.
- The `dispatch_total` metric name and labels on the real binary, and that
  the shortfall check reports zero in normal use.
- Linux runtime of `ingest` (PR #18 tested on macOS only; `STACK.md` §13).
  Before the first deployment that starts `ingest`:
  1. `cargo nextest run --workspace --locked` passes one time on
     linux/amd64, in a build stage of the image or a Linux VM. If this is
     not possible, the PR records why, and checks 2 to 6 are the only
     evidence.
  2. `ingest` is PID 1. On a redeploy, the log of the old deployment shows
     `shutdown_started` with reason `SIGTERM`, `gate_stopped`,
     `child_killed` with `exit_signal` 9, the `stopped` event, and
     `shutdown_complete` within `drainingSeconds`, and no
     `task_stop_timeout`.
  3. After at least three child exits and restarts, the process table of
     the container has no zombie (`ps` state `Z`).
  4. After a forced end of PID 1 (a watchdog exit or SIGKILL of `ingest`),
     the container ends, Railway restarts it, a new `started` event
     follows, and no `fleet-telemetry` process survives without `ingest`.
  5. Every `gate_connection_closed` event of a normal connection has
     `socket_options` `set`, `keepalive` `15/15/9` (read back from the
     socket), and `nodelay` `true` (none has `failed`), and the idle
     dead-peer case above closes in about 150 s. If the Railway proxy
     terminates TCP, `ss -tio` in the container shows the keep-alive
     timers, or check 1 covers them.
  6. `fleet-telemetry` starts with an empty environment, and
     `/proc/<child pid>/fd` has no listener of `ingest`.
- The false-positive rate of `unknown_fields` on real vehicle payloads.
  For each `raw.unprocessed_record` row with reason `unknown_fields` in the
  first week, decode the stored payload and confirm that it holds a field
  that the vendored schema does not know. Record the count and the result.
  A row without such a field is a false positive and a defect of the
  decode.
- After the first week, the effective compute size in the Neon console or
  API is still 0.25 CU for production and `test`, and the consumption
  matches the budget of section "Why 15 minutes". If Neon changed the size,
  repeat the arithmetic before a longer flush or a bigger compute is
  accepted.

## Not verified

- Whether the vehicle resends a message that it sent but that was not
  acked, without `delivery_policy: latest`. Tesla documents no default
  value.
- Whether the vehicle resends a rate-limited message before it reconnects.
- Whether an idle connection keeps a Neon compute active. The pool closes
  idle connections, so the answer does not change the design.

## Sources

- Upstream source: <https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c> (tag `v0.10.1`), README section "Reliable Acks".
- [T] Tesla Fleet Telemetry, "System Behavior: Failure handling" and "Changelog 1.0.0": <https://developer.tesla.com/docs/fleet-api/fleet-telemetry>
- [Z1] libzmq `zmq_socket`, ZMQ_PUB: <https://libzmq.readthedocs.io/en/latest/zmq_socket.html>
- [Z2] libzmq `zmq_setsockopt`, ZMQ_SNDHWM: <https://libzmq.readthedocs.io/en/latest/zmq_setsockopt.html>
- [Z3] ZeroMQ Guide, chapter 1: "If a publisher has no connected subscribers, then it will simply drop all messages", and the "slow joiner" explanation: <https://zguide.zeromq.org/docs/chapter1/>
- [R1] Railway, Deployment teardown: <https://docs.railway.com/deployments/deployment-teardown>
- [R2] Railway, Variables reference: <https://docs.railway.com/variables/reference>
- [R3] Railway, Healthchecks: <https://docs.railway.com/deployments/healthchecks>
- [R4] Railway, Pre-deploy command: <https://docs.railway.com/deployments/pre-deploy-command>
- [R5] Railway, Restart policy: <https://docs.railway.com/deployments/restart-policy>
- [R6] Railway, Private networking: <https://docs.railway.com/networking/private-networking>
- [R7] Railway, Plans and pricing (volume storage 0.15 USD for each GB each month): <https://docs.railway.com/pricing/plans>
- [R8] Railway, Volumes (not mounted during pre-deploy): <https://docs.railway.com/volumes>
- [L] Linux kernel at tag `v6.12`: `net/ipv4/tcp_timer.c` (no keep-alive probe while data is unacknowledged) and `Documentation/networking/ip-sysctl.rst` (`tcp_retries2`, default 15, about 924.6 s as a lower bound): <https://github.com/torvalds/linux/tree/v6.12>. The container's kernel version is a deployment check.
- [G] Go 1.19 release notes: programs that import `os` raise the `RLIMIT_NOFILE` soft limit to the hard limit: <https://go.dev/doc/go1.19>. Go TLS, HTTP server, and TCP keep-alive source at tag `go1.27.4` (upstream builds with Go 1.27): <https://github.com/golang/go/tree/go1.27.4/src>
- [K] Linux `pid_namespaces(7)`: <https://man7.org/linux/man-pages/man7/pid_namespaces.7.html>
- [N1] Neon plans: <https://neon.com/docs/introduction/plans>
- [N2] Neon scale to zero: <https://neon.com/docs/introduction/scale-to-zero>
- [N3] Neon computes, "Compute is not suspending": <https://neon.com/docs/manage/computes>
- Crate data: <https://crates.io/api/v1/crates/> for each crate, read 26 November 2026. `zeromq` 0.7.0 source read from the published crate.
