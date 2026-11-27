# Image advisories: triage of the ingest image

Status: triage of 26 November 2026 for issue #15. The reviewed ignore files
`docs/security/osv-scanner-tesla-fleet-telemetry.toml` and
`docs/security/osv-scanner-debian.toml` record the decisions of this
document for `mise run vuln`.

## Owner summary

**What is exposed.** The service has one public port: container port 4443
behind the Railway TCP proxy. Other services in the Railway project can
also reach this port over the private network. `fleet-telemetry` asks for
a client certificate from Tesla's vehicle CA in the TLS handshake and
closes the connection before it reads one HTTP byte when the certificate
does not verify. The metrics port 9090 listens on `127.0.0.1` only, so it
is not reachable from outside the container. The profiler and the status
port are off.

**What is not exposed.** All 33 advisory groups of the Go modules in
`/fleet-telemetry` are "not affected" in this deployment. 32 of them do not
affect the deployed code paths, and one is a false positive:

| Class | Groups | Meaning |
| ----- | ------ | ------- |
| M | 16 | Module only: the binary does not import the vulnerable package. |
| P | 7 | The package is imported, but the vulnerable symbol is not called. |
| D | 8 | The symbol is called only through a dispatcher (`pubsub` or `mqtt`) that this deployment does not configure. |
| C | 1 | The symbol is called, but the vulnerable condition does not occur (client-only behaviour). |
| F | 1 | False positive (GO-2026-5553). |

The CRITICAL gRPC advisory CVE-2026-33186 is class P: the binary has no
gRPC server. The 61 Debian 12 advisories of the upstream image are not in
the production image, because the Dockerfile copies only the binary.

**What was fixed.** The Debian 13 base image `debian:trixie-slim` has 4
advisory groups with a Debian fix (libzstd DSA-6657-1, shadow DSA-6661-1).
Debian published the fixes after the newest base image. The Dockerfile now
runs `apt-get upgrade`, so the next Railway build installs them. The merge
of this change is that build. 65 Debian 13 groups have no Debian fix; they
are accepted as a group with a reassessment rule. 14 of them are in
libraries that the server process loads (glibc, libstdc++, libgcc).

**Known exploited.** No compared CVE is in the CISA Known Exploited
Vulnerabilities catalog (version 2026.11.25, 1805 entries, read twice on
26 November 2026; 0 matches). Compared: the 152 CVE IDs of the two image
scans, CVE-2025-69277 (libsodium, which the binary does not contain;
kept from an earlier version of this triage), and 119 CVE IDs that OSV
lists for the static C libraries (see "Static C libraries").

**Needs action.** None.

**Decisions for the owner.**

1. Post the drafted comment `docs/security/upstream-report-draft.md` to
   the existing upstream issue 578 of the `fleet-telemetry` repository
   (`https://github.com/teslamotors/fleet-telemetry/issues/578`),
   or decide not to post it. The draft contains no deployment detail. Do
   not paste this URL or an issue reference into a commit message, an
   issue, a pull request, or a comment of this repository: GitHub turns
   it into a visible event on the upstream issue.
2. Accept the residual risk: the internet-facing binary carries outdated
   Go modules and static C libraries. The protection depends on mTLS, on
   the dispatcher configuration, and on the loopback listeners. A holder
   of a valid Tesla vehicle certificate passes mTLS. `mise run vuln` now
   fails when the configuration breaks one of these premises.
3. Know the consequence of the merge: Railway rebuilds and redeploys the
   ingest service, and the vehicle reconnects after a short disconnect.

## Scope, sources, and method

| Item | Value |
| ---- | ----- |
| Upstream image | `tesla/fleet-telemetry:v0.10.1`, linux/amd64 digest `sha256:dd3bfb47b730ab5c4c8a4efc7c7c6669ce21d4bf7e56e2a3ab1d25f30ce7adf0`, created 2026-11-25 |
| Upstream source | tag `v0.10.1` = commit [`6689adb`](https://github.com/teslamotors/fleet-telemetry/tree/6689adb831691b1de211b4aa8220babf7309f31c). `main` was at the same commit on 26 November 2026. |
| Binary | `/fleet-telemetry`, SHA-256 `6c52c888d42471f309491c00470202a72f4123134bd2e828070a8255a26375cd`, Go 1.27.4 (the newest Go release on 26 November 2026), `CGO_ENABLED=1`, linux/amd64 |
| Base image | `debian:trixie-slim`, linux/amd64 digest `sha256:ab3b3c8c5159eebfca4376430132d53c4d425f869c765ecbe47177a030da575d`. This is the same image as `trixie-20261123-slim`, the newest dated tag. Debian 13.8. |
| Scanner | osv-scanner 2.7.1 (commit `02f96a4`), through `mise run vuln` |
| Call-graph check | govulncheck v1.10.0, vulnerability database of 2026-11-24 |
| Known exploited list | CISA KEV JSON feed, catalog version 2026.11.25 |

Counts of the scan without ignore files:

- `tesla/fleet-telemetry:v0.10.1`: the table shows 103 advisories. These
  are 33 Go alias groups (44 IDs) in `/fleet-telemetry`, plus 70 Debian 12
  group rows. The distroless image has one dpkg status file for each
  binary package, and the table counts a group once for each file: gcc-12
  appears four times (4 × 6). The Debian 12 rows contain 61 distinct IDs.
  12 more Debian 12 rows have the urgency "unimportant" and are hidden.
- `debian:trixie-slim`: the table shows 79 advisories. These are 69
  distinct groups; the table counts the 5 util-linux groups three times,
  once for each util-linux version string. 25 more groups have the Debian
  urgency "unimportant". osv-scanner hides them, and they do not fail the
  scan.

### Call-graph analysis (govulncheck)

govulncheck reports three levels: a called symbol, an imported package
without a called symbol, and a required module without an imported
package. The source mode builds a call graph from `main`. The binary mode
reads the symbols that the linker kept.

Commands, in an isolated scratch directory with a checkout of tag
`v0.10.1`:

```sh
go install golang.org/x/vuln/cmd/govulncheck@latest   # v1.10.0
# Source mode. pebbe/zmq4 needs zmq.h for type checking: a pkg-config
# shim gives the include path of zmq.h from libzmq tag v4.3.4.
CGO_ENABLED=1 PKG_CONFIG=<shim> govulncheck -C <checkout> -show verbose ./cmd/...
CGO_ENABLED=1 PKG_CONFIG=<shim> govulncheck -C <checkout> -format json ./cmd/...
# Binary mode on the binary from the image layer.
govulncheck -mode=binary -show verbose <extracted>/fleet-telemetry
```

Results:

| Mode | Called symbol | Imported package only | Required module only |
| ---- | ------------- | --------------------- | -------------------- |
| Source (`./cmd/...`) | 9 | 7 | 16 |
| Binary | 12 | 4 | 15 |

Limitations:

- The source mode ran for darwin/arm64, the host platform. A cross
  analysis for linux/amd64 needs a Linux C tool chain for cgo, and none is
  available. The binary mode on the real linux/amd64 binary is the cross
  check. GO-2026-5024 (x/sys/windows) does not appear in the binary mode.
- The binary mode shows 3 more "called" groups (GO-2026-4762,
  GO-2026-6443, GO-2025-4188). The binary mode reports symbols that the
  linker kept, for example methods reachable through an interface. It does
  not prove a call. The source mode shows these 3 groups as "package
  only".
- Some source traces go from `main` directly into a module through an
  interface call, for example `fmt.Fprintf` into `io.Writer`, or
  `err.Error()`. The call graph over-approximates interface calls: it
  includes every type that implements the interface. Such a trace needs a
  value of the vulnerable type. The decisions below name where these
  values are created.
- govulncheck does not see the version of the main module, because
  upstream builds with `go build cmd/main.go`. GO-2026-5553 therefore
  needs separate evidence (see below).

## Deployment facts

Each decision relies on these facts. `mise run vuln` checks the
configuration facts (marked "checked") before it scans, and fails when
one breaks.

1. **Image composition (checked).** The production Dockerfile
   (`ingest/Dockerfile` since issue #14) copies only one file from the
   upstream image:
   `COPY --from=upstream /fleet-telemetry /fleet-telemetry`. The check is
   a strict allowlist, not a Dockerfile parser: all bytes must be printable
   ASCII, tab, or newline; no parser directive (`# name=value`) is
   allowed; and after removal of empty lines and of lines that start with
   `#`, the lines must equal the expected lines of this project exactly.
   Only the three image tags may change (`[A-Za-z0-9._-]+`). The expected
   lines contain exactly three `FROM` lines (the Rust builder `AS build`,
   the upstream image `AS upstream`, then `debian`), one
   `COPY --from=upstream /fleet-telemetry /fleet-telemetry`, one
   `COPY --from=build` of the `ingest` binary only, the copy of
   `ingest/server_config.json` to `/etc/fleet-telemetry/config.json`, and
   `ENV INGEST_CHILD_CONFIG=/etc/fleet-telemetry/config.json`. The builder
   tag must be the channel of `rust-toolchain.toml` with `-slim-` and the
   Debian codename of the final stage. Any other form fails, also a
   continuation inside a token, a Unicode space, a quote or an escape
   character in a flag, or a trailing space after a continuation
   backslash; the message says that the allowlist must change together
   with the Dockerfile. The task scans the two runtime images (upstream
   and `debian`) and fails when it did not scan two images; the builder
   image is not scanned (`STACK.md` §13). The server reads the config
   that the Dockerfile copies: `ingest/entrypoint.sh` is checked against a
   strict allowlist like the Dockerfile (first line `#!/bin/sh`; after
   removal of empty lines and `#` lines, the lines must equal the expected
   lines; printable ASCII only), so it writes the TLS files, unsets the
   TLS variables and `MIGRATION_DATABASE_URL`, and starts
   `exec setpriv --reuid 10001 --regid 10001 --clear-groups
   --inh-caps=-all --bounding-set=-all --no-new-privs
   /usr/local/bin/ingest run` and nothing else (issue #29: the image has
   no `USER` line and starts as root; `setpriv` starts `ingest` as user
   10001 without capabilities); `ingest` starts the
   server with `-config` and `INGEST_CHILD_CONFIG`
   (`crates/ingest/src/child.rs`). Limit: a Railway service variable
   `INGEST_CHILD_CONFIG` would replace the image value; the release steps
   forbid any `INGEST_` service variable (`docs/ingest-release.md`).
   `scripts/vuln-premise-test.sh` proves with 55 changed inputs that the
   premise checks fail. The upstream
   layer of the binary contains only `fleet-telemetry`. The Debian 12
   files of the upstream image (`gcr.io/distroless/cc-debian12` base,
   upstream [`Dockerfile#L31-L33`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Dockerfile#L31-L33))
   are not in the production image.
2. **mTLS before HTTP.** `ExtractServiceTLSConfig` sets
   `ClientAuth: tls.RequireAndVerifyClientCert` with the embedded Tesla
   production CA
   ([`config/config.go#L308-L345`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L308-L345)).
   `main` puts this config on the server before `ListenAndServeTLS`
   ([`cmd/main.go#L75-L79`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/cmd/main.go#L75-L79)).
   Go 1.27.4 `net/http` completes the TLS handshake before it reads a
   request, and returns on a handshake error
   ([`net/http/server.go#L1986-L2010`](https://github.com/golang/go/blob/go1.27.4/src/net/http/server.go#L1986-L2010);
   `readRequest` is at line 2079). The handshake fails when the client
   sends no certificate or a certificate that does not verify against the
   CA pool
   ([`crypto/tls/handshake_server.go#L961-L998`](https://github.com/golang/go/blob/go1.27.4/src/crypto/tls/handshake_server.go#L961-L998)).
   In TLS 1.3, the server reads the client certificate inside the
   handshake
   ([`handshake_server_tls13.go#L95-L98`](https://github.com/golang/go/blob/go1.27.4/src/crypto/tls/handshake_server_tls13.go#L95-L98)).
   No handler, including `/status`, is reachable without a verified
   certificate. Before the handshake completes, only Go 1.27.4 `crypto/tls`
   and `crypto/x509` process peer bytes. govulncheck found no standard
   library advisory for Go 1.27.4.
   **Limit:** mTLS is a compensating control, not a reason for "not
   affected". Any holder of a valid certificate from Tesla's vehicle CA
   passes it, for example a vehicle with a compromised infotainment
   system (compare GO-2026-5553).
3. **Client CAs (checked).** `ingest/server_config.json` does not set
   `use_default_eng_ca` or `tls.ca_file`, so only the Tesla production CA
   is in the pool.
4. **HTTP/2 on the TLS port.** Since issue #14, the Rust gate of `ingest`
   owns port 4443 and copies the bytes of each connection without parsing
   them to `fleet-telemetry` on `127.0.0.1:4444` (ADR 0001, "Layout"), so
   the same peer bytes reach the same Go code. The server is the standard `net/http` server
   ([`server/streaming/server.go#L80-L84`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/server.go#L80-L84)).
   Its HTTP/2 code is the copy inside the standard library
   (`net/http/internal/http2`), which Go 1.27.4 contains with the fixes of
   Go 1.27.2. No upstream source file imports `golang.org/x/net`,
   `google.golang.org/grpc`, `golang.org/x/text`, or `golang.org/x/sys`
   directly (search of all non-test `.go` files at the tag).
5. **Dispatchers (checked).** `ingest/server_config.json` routes all
   record types to `zmq` (issue #14; before, `logger`). The `vuln` task
   allows only these two, and it requires `zmq.addr` of the form
   `tcp://127.0.0.1:<port>` with lowercase keys in the `zmq` object, so
   that libzmq listens on the loopback interface only. `ConfigureProducers` creates a producer
   only for a dispatcher that a record type names: Kafka
   [`#L386-L396`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L386-L396),
   Pub/Sub [`#L398-L407`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L398-L407),
   Kinesis [`#L409-L423`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L409-L423),
   ZMQ [`#L425-L434`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L425-L434),
   MQTT [`#L436-L445`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L436-L445),
   Redis [`#L447-L460`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L447-L460).
   The Pub/Sub topic setup and the MQTT connect run only when these
   producers exist
   ([`#L479-L487`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L479-L487)).
6. **Listeners (checked).** `monitoring.prometheus_metrics_port` is 9090
   and `prometheus_metrics_host` is not set, so the metrics server binds
   `127.0.0.1:9090`
   ([`server/monitoring/metrics_server.go#L35-L47`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/monitoring/metrics_server.go#L35-L47)).
   The loopback default exists since upstream commit
   [`d5ca0da`](https://github.com/teslamotors/fleet-telemetry/commit/d5ca0dab55812029fd38eb77f079f74ce4f47286)
   (v0.9.0). `profiler_port` is not set, so the profiler is off
   ([`#L49-L68`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/monitoring/metrics_server.go#L49-L68)).
   `status_port` is not set, so the status server is off
   ([`cmd/main.go#L59-L61`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/cmd/main.go#L59-L61)).
   If it were set, it would bind all interfaces without TLS
   ([`status_server.go#L28`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/monitoring/status_server.go#L28)).
   Railway exposes only the TCP proxy to port 4443. A loopback listener is
   not reachable over the Railway private network.
7. **Dynamic linking.** The binary is dynamically linked (ELF
   interpreter `/lib64/ld-linux-x86-64.so.2`). A probable cause: the
   upstream Makefile passes `--ldflags 'extldflags="-static"'`, without
   the `-` before `extldflags`
   ([`Makefile#L10`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Makefile#L10));
   this cause is not tested. Its ELF `NEEDED` entries are `libm.so.6`, `libstdc++.so.6`, `libgcc_s.so.1`, `libc.so.6`, and
   `ld-linux-x86-64.so.2`. At run time, the server loads these Debian 13
   libraries of the production image. libzmq 4.3.4 with its own copies of
   tweetnacl and SHA-1, and librdkafka 2.3.0 with its bundled libraries (OpenSSL 3.0.11, libcurl
   7.86.0, zlib 1.2.13, zstd 1.5.2, lz4 1.9.3) are linked statically (see
   "Static C libraries").
8. **Config key form (checked).** Upstream decodes the config with Go
   `encoding/json`
   ([`config/config_initializer.go#L52`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config_initializer.go#L52)).
   `encoding/json` matches keys to fields without letter case. A repeated
   key decodes again into the same field, so a repeated object merges
   into the first one. The decoder reads only the first JSON value of the
   file. jq is case sensitive, keeps only the last of two equal keys, and
   reads every value. The `vuln` task therefore requires:
   - lowercase keys (`^[a-z0-9_]+$`) in the root, `monitoring`, and `tls`
     objects (the record type names inside `records` are data and keep
     their case);
   - exactly one JSON value in the file (`jq -s 'length == 1'`);
   - the same sorted list of leaf paths in the token stream
     (`jq --stream`) and in the parsed value. A repeated key of any kind,
     also a repeated object, adds leaf paths to the stream that the parsed
     value does not have, so it breaks this rule.

## Decision classes

- **M (not affected):** module only. The binary does not import the
  package that contains the vulnerable code.
- **P (not affected):** the package is imported, but govulncheck finds no
  call of a vulnerable symbol.
- **D (not affected while the premise holds):** vulnerable symbols are
  reachable only through a dispatcher that this deployment does not
  configure (fact 5). The `vuln` task fails when the premise breaks.
- **C (not affected):** the code is called, but the vulnerable condition
  does not occur, with evidence from the source.
- **F (false positive):** the advisory does not apply to the deployed
  version.
- **Fixed in build:** the build installs the fixed version.
- **Accepted:** no fix exists. A compensating control and a reassessment
  rule apply.
- **Needs action:** a fix is due. No ignore entry is allowed.

The KEV column shows "no" when the catalog version 2026.11.25 does not
list the CVE. The catalog lists only CVE IDs, so an advisory without a CVE
ID cannot be in it. Severity is the GitHub advisory rating with the score
that osv-scanner shows. "not rated" means that OSV has no severity for the
group.

## Go modules in `/fleet-telemetry`

Fix versions are from the OSV records. No `fleet-telemetry` release
contains them; v0.10.1 is the newest release, and upstream `main` is at
the same commit. Upstream issue 578 asks for the update.

### github.com/aws/aws-sdk-go

v1.44.278. Imported only by `datastore/kinesis` (`aws`, `aws/session`,
`service/kinesis`).

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2022-0635 (CVE-2020-8912, GHSA-7f33-f4f5-xwgw) | not rated | no | `service/s3/s3crypto` is not imported. govulncheck: module only. No fix exists in SDK v1. | M |
| GO-2022-0646 (CVE-2020-8911, GHSA-f5pg-7wfw-84q9) | not rated | no | Same as above. | M |

### github.com/eclipse/paho.mqtt.golang

v1.5.0, fixed in 1.5.1. Imported only by `datastore/mqtt`.

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2025-4173 (CVE-2025-10543, GHSA-32fw-gq77-f2f2) | MODERATE 6.3 | no | The OSV record lists no symbols, so govulncheck counts every function of the module. The faulty code is `encodeString` and `encodeBytes` ([paho v1.5.0 `packets/packets.go#L308-L340`](https://github.com/eclipse/paho.mqtt.golang/blob/v1.5.0/packets/packets.go#L308-L340)), which the packet `Write` methods call. The traces reach the module through `mqtt.NewProducer`, `Producer.Connect`, and `Producer.Produce`; these need an MQTT producer (fact 5). Two traces are the compiler-made package initialisers (`init`), which run at start but only set package variables: the imported packages have no `func init()`. The `String` traces are `fmt` interface calls; they need packet values, which only the MQTT client creates. | D |

### github.com/gorilla/websocket

v1.5.3. The vehicle connection uses this module in the server role.

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2026-6278 (GHSA-w67g-5rqw-f597) | not rated | no CVE | The advisory is about the weak random source of the WebSocket mask key. `Upgrader.Upgrade` and `Conn.ReadMessage` are called on the vehicle path ([`server.go#L211`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/server.go#L211), [`socket.go#L200`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/server/streaming/socket.go#L200)). Only a client makes mask keys: `newMaskKey` is called only when `!c.isServer` ([websocket v1.5.3 `conn.go#L430-L436`, `#L603-L606`](https://github.com/gorilla/websocket/blob/v1.5.3/conn.go#L430-L436)), and `Upgrader` creates the connection with `isServer` true ([`server.go#L204`](https://github.com/gorilla/websocket/blob/v1.5.3/server.go#L204)). The client role (`Dialer.Dial`) is used only by the MQTT WebSocket transport (fact 5). The connection is also inside TLS. | C |

### github.com/sirupsen/logrus

v1.9.0, fixed in 1.9.1 and 1.9.3.

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2025-4188 (CVE-2025-65637, GHSA-4f99-4q7p-p3gh) | HIGH 8.7 | no | The vulnerable symbols are `Entry.Writer`, `Entry.WriterLevel`, `Logger.Writer`, and `Logger.WriterLevel`. govulncheck source mode: package only, no call. The binary mode lists the symbols as linked, not as called. | P |

### github.com/teslamotors/fleet-telemetry

The binary reports the main module as `(devel)`, because upstream builds
with `go build cmd/main.go`
([`Makefile#L27`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Makefile#L27)).
osv-scanner cannot compare `(devel)` with a version range and reports it.

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2026-5553 (GHSA-prxj-3gcv-cqrh) | MODERATE 5.9 | no CVE | Affected range: before 0.9.0. The fix is commit [`d5ca0da`](https://github.com/teslamotors/fleet-telemetry/commit/d5ca0dab55812029fd38eb77f079f74ce4f47286) (the advisory names it). `git merge-base --is-ancestor d5ca0da v0.10.1` succeeds. The binary contains the string `missing_verified_client_certificate`, which the fix adds, and not `missing_certificate_error`, which the fix removes. The image tag is `v0.10.1`. | F |

### golang.org/x/net

v0.33.0. The binary uses `x/net` only through other modules. Import graph
(`GOOS=linux GOARCH=amd64 go list -deps ./cmd` at the tag):

- `x/net/http2` is imported by `google.golang.org/grpc/internal/transport`
  and `google.golang.org/api/transport/http`. `x/net/http2`,
  `google.golang.org/grpc`, `x/net/idna`, and `x/text/unicode/norm` enter
  the build through `cloud.google.com/go/pubsub`. Two upstream packages
  import that module: `datastore/googlepubsub` (the Pub/Sub dispatcher)
  and `config`, which uses it only as a field type (`Publisher
  *pubsub.Client`,
  [`config/config.go#L148`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L148)).
  The only call that creates a Pub/Sub client is in
  `datastore/googlepubsub/publisher.go#L56`, which runs only for the
  Pub/Sub producer.
- `x/net/proxy` is imported only by paho MQTT. The only upstream package
  through which it enters is `datastore/mqtt`.

The vehicle port uses the HTTP/2 copy of the standard library (fact 4).
For the HTTP/2 advisories GO-2026-6603, -6610, -6611, -6612, and -6617,
the standard library part is fixed in Go 1.27.2, and the binary has Go
1.27.4. Every govulncheck trace that enters `x/net/http2` through an
upstream function passes `datastore/googlepubsub`. The other traces start
at `main` with an interface call (`err.Error()`, `String`, `io.Writer`).
They need values of `x/net/http2` types. Only code of the Google Cloud
client stack creates such values, and that stack runs only when the
Pub/Sub producer exists (fact 5).

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2025-3503 (CVE-2025-22870, GHSA-qxp5-gwg8-xv66) | MODERATE 4.4 | no | `proxy.FromEnvironment` and `proxy.PerHost.Dial` only through `mqtt.Producer.Connect`. | D |
| GO-2025-3595 (CVE-2025-22872, GHSA-vvgc-356p-c3xw) | MODERATE 5.3 | no | `x/net/html` is not imported. Module only. | M |
| GO-2026-4440 (CVE-2025-47911, GHSA-w4gw-w5jq-g9jh) | not rated | no | `x/net/html`. Module only. | M |
| GO-2026-4441 (CVE-2025-58190) | not rated | no | `x/net/html`. Module only. | M |
| GO-2026-4918 (CVE-2026-33814) | not rated | no | HTTP/2 client transport. Package only, no call. | P |
| GO-2026-5025 (CVE-2026-42506) | not rated | no | `x/net/html`. Module only. | M |
| GO-2026-5026 (CVE-2026-39821) | not rated | no | `x/net/idna`. Package only, no call. The standard library part is fixed in Go 1.27.0. | P |
| GO-2026-5027 (CVE-2026-42502) | not rated | no | `x/net/html`. Module only. | M |
| GO-2026-5028 (CVE-2026-25680, GHSA-5cv4-jp36-h3mw) | MODERATE 6.5 | no | `x/net/html`. Module only. | M |
| GO-2026-5029 (CVE-2026-25681) | not rated | no | `x/net/html`. Module only. | M |
| GO-2026-5030 (CVE-2026-27136) | not rated | no | `x/net/html`. Module only. | M |
| GO-2026-5942 (CVE-2026-46600) | not rated | no | `x/net/dns/dnsmessage` is not imported. Module only. The standard library part is fixed in Go 1.27.0. | M |
| GO-2026-6603 (CVE-2026-78659) | not rated | no | HTTP/2 server memory exhaustion. `x/net/http2` only through `googlepubsub.configurePubsub`, and through interface calls (see above). | D |
| GO-2026-6610 (CVE-2026-78660) | not rated | no | HTTP/2 transport. Package only, no call. | P |
| GO-2026-6611 (CVE-2026-78669) | not rated | no | Same traces as GO-2026-6603. | D |
| GO-2026-6612 (CVE-2026-78663) | not rated | no | Same traces as GO-2026-6603. | D |
| GO-2026-6617 (CVE-2026-97032) | not rated | no | Same traces as GO-2026-6603. | D |
| GO-2026-6860 (CVE-2026-78661) | not rated | no | `x/net/html`. Module only. | M |
| GO-2026-6866 (CVE-2026-97045) | not rated | no | `x/net/html`. Module only. | M |

### golang.org/x/sys

v0.30.0.

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2026-5024 (CVE-2026-39824) | not rated | no | `x/sys/windows`. Not compiled into the linux/amd64 binary; the binary mode does not report it. Module only. | M |

### golang.org/x/text

v0.21.0.

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2026-5970 (CVE-2026-56852) | not rated | no | `x/text/unicode/norm`. Package only, no call. | P |
| GO-2026-6629 (CVE-2026-56851) | not rated | no | `x/text/secure/precis` is not imported. Module only. | M |

### google.golang.org/grpc

v1.56.3. Imported only through `cloud.google.com/go/pubsub`. Only
`datastore/googlepubsub` creates a Pub/Sub client; `config` uses the
module only as a field type (see `golang.org/x/net`). The binary has no gRPC server: no upstream
file creates one, and govulncheck finds no call of `grpc.Server.Serve`
or `grpc.Server.ServeHTTP`.

| ID (aliases) | Severity | KEV | Reachability and evidence | Decision |
| ------------ | -------- | --- | ------------------------- | -------- |
| GO-2026-4762 (CVE-2026-33186, GHSA-p77j-4mvh-x3m3) | CRITICAL 9.1 | no | Authorization bypass in the gRPC server. Package only, no call. The binary mode lists `grpc.Server.Serve` as linked, not as called. | P |
| GO-2026-6061 (GHSA-hrxh-6v49-42gf) | HIGH 8.8 | no CVE | Client transport symbols only through `googlepubsub.NewProducer`, `ProvisionTopics`, and `Close`. The `transportReader.Read` trace is an `io.Reader` interface call; it needs a gRPC transport, which only the Pub/Sub client creates. | D |
| GO-2026-6348 (CVE-2026-84304, GHSA-vp52-pcj8-j9qc) | HIGH 8.7 | no | Same traces as GO-2026-6061. | D |
| GO-2026-6441 (CVE-2026-84303, GHSA-qc2q-p7wx-3px3) | MODERATE 6.3 | no | xDS RBAC HTTP filter is not imported. Module only. | M |
| GO-2026-6443 (CVE-2026-84445, GHSA-2v4p-qf9q-27wj) | HIGH 8.7 | no | Server transport (`http2Server.HandleStreams`). Package only, no call. | P |

## Static C libraries in `/fleet-telemetry`

osv-scanner and govulncheck do not see C libraries that are linked into
the binary. Two sources put static C code into `/fleet-telemetry`:

- The upstream Dockerfile builds libzmq 4.3.4 as a static library
  ([`Dockerfile#L14-L20`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Dockerfile#L14-L20)).
  It also builds libsodium 1.0.19
  ([`Dockerfile#L5-L11`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Dockerfile#L5-L11)),
  but the libzmq build does not use it: libzmq's `./configure` runs
  without `--with-libsodium`, and libzmq 4.3.4 then uses its own copy of
  tweetnacl ([`configure.ac#L536-L541`](https://github.com/zeromq/libzmq/blob/v4.3.4/configure.ac#L536-L541):
  "defaults to tweetnacl", `--with-libsodium` "default=no"). The binary
  confirms this: the symbol table has the file symbol `tweetnacl.c`;
  `sodium_init` is the 3-byte stub `xorl %eax, %eax; retq`; the only
  C symbols with `sodium`, `crypto_`, or `randombytes` in the name are
  the 14 tweetnacl names of libzmq (`crypto_box*`, `crypto_core_hsalsa20`,
  `crypto_core_salsa20`, `crypto_sign*`, `randombytes`,
  `randombytes_close`, `sodium_init`); no `sodium_version_string`,
  `crypto_core_ed25519_*`, or `crypto_generichash` symbol exists; and the
  binary contains no `libsodium` string. libsodium is not linked.
- `confluent-kafka-go/v2` v2.3.0 links its prebuilt bundle
  `kafka/librdkafka_vendor/librdkafka_glibc_linux_amd64.a` (SHA-256
  `7e6c80d323ad6a0dc9fbcb693cf607bc62997a4591e940e7f9d4e8ae56f0f198`;
  the module hash in the binary is `h1:icCHutJouWlQREayFwCc7lxDAhws08td+W3/gdqgZts=`).
  The bundle contains librdkafka and the libraries that librdkafka v2.3.0
  builds from source
  ([`mklove/modules`](https://github.com/confluentinc/librdkafka/tree/v2.3.0/mklove/modules)).

| Component | Version | Evidence of the version | Use in this deployment |
| --------- | ------- | ----------------------- | ---------------------- |
| libzmq | 4.3.4 | Upstream `Dockerfile#L15`; `zmq_version` in the binary returns 4, 3, 4. | At each start, the Go package `zmq4` creates a libzmq context, without a socket (see "Code that runs at each start"). Since issue #14, the `zmq` dispatcher binds a PUB socket on `tcp://127.0.0.1:5284` (see "Code that runs with the `zmq` dispatcher"). |
| tweetnacl (copy in libzmq 4.3.4, `src/tweetnacl.c`) | the libzmq 4.3.4 copy | Binary: file symbol `tweetnacl.c` and its local functions (`car25519`, `pack25519`, `scalarmult`, ...); `sodium_init` is the tweetnacl stub; `randombytes` calls only `getrandom` and `sleep`. | Used by libzmq only for CURVE. The configuration does not enable CURVE. |
| SHA-1 (copy in libzmq 4.3.4, `external/sha1`) | the libzmq 4.3.4 copy | Binary: file symbol `sha1`, next to the libzmq WebSocket files (`ws_engine`, `ws_listener`). | Used by the libzmq WebSocket transport only. Not configured. |
| libsodium | not linked | See above: built by the upstream Dockerfile, but not used by the libzmq build; no libsodium symbol or string in the binary. | Not applicable. |
| librdkafka | 2.3.0 | String `librdkafka v2.3.0` in the binary. | Only behind the `kafka` dispatcher. |
| OpenSSL (libssl, libcrypto) | 3.0.11 | String `OpenSSL 3.0.11 19 Sep 2023` in the binary; `configure.libssl` sets 3.0.11. | The CPU feature detection runs at each start (see "Code that runs at each start"). All other code only behind the `kafka` dispatcher. |
| libcurl | 7.86.0 | String `libcurl/7.86.0` in the bundle archive; `configure.libcurl` sets 7.86.0. The binary contains the names of 44 curl objects, but not `version.o`. | Used by librdkafka for the OAUTHBEARER OIDC token request. Only behind the `kafka` dispatcher. |
| zlib | 1.2.13 | Strings `deflate 1.2.13` and `inflate 1.2.13` in the binary. | Only behind the `kafka` dispatcher. |
| zstd | 1.5.2 | `ZSTD_versionNumber` returns 10502; `configure.libzstd` sets 1.5.2. | Only behind the `kafka` dispatcher. |
| lz4 | 1.9.3 (librdkafka copy) | `LZ4_versionNumber` returns 10903; `src/lz4.h` of librdkafka v2.3.0. | Only behind the `kafka` dispatcher. |
| Cyrus SASL | not linked | The bundle excludes GSSAPI, because Cyrus SASL needs dynamic linking ([`packaging/nuget/staticpackage.py#L17-L20`](https://github.com/confluentinc/librdkafka/blob/v2.3.0/packaging/nuget/staticpackage.py#L17-L20)). The binary has no Cyrus SASL symbol; `rd_kafka_sasl_client_new` is librdkafka code (PLAIN, SCRAM, OAUTHBEARER). | Not applicable. |

Evidence against the binary itself, for each row (round 4 re-check,
after the libsodium error of an earlier version, which took a component
name from a build file):

- **From the binary alone:** libzmq (`zmq_version`), tweetnacl (file
  symbol and functions), SHA-1 (file symbol), librdkafka (version
  string), OpenSSL (version string; 502 `libcrypto-lib-*` and 39
  `libssl-lib-*` object names), zlib (version strings; `adler32.o`,
  `deflate.o`, `inflate.o`), zstd (`ZSTD_versionNumber`; `zstd_*.o`
  object names), lz4 (`LZ4_versionNumber`), and the absence of libsodium
  and Cyrus SASL (no symbol, no string).
- **Partly from the bundle archive:** libcurl. The binary proves that
  libcurl is linked (44 `libcurl_la-*.o` object names, 84 `curl_*`
  symbols), but it has no version string, because `version.o` is not
  linked. The version 7.86.0 comes from the string `libcurl/7.86.0` in
  the bundle archive of the module that the binary records (same `h1:`
  hash) and from `configure.libcurl` of librdkafka v2.3.0.
- **Completeness:** the ELF symbol table has 973 file symbols. After
  removal of the files of libzmq 4.3.4 (`src/`, `external/sha1`,
  `tweetnacl`), librdkafka v2.3.0 (`src/`), OpenSSL, libcurl, zlib, zstd,
  the cgo files of the Go packages (also `dummy.c` of `zmq4`), the Go
  linker entry `go.go`, and the C run time (`crt1.o`,
  `crtstuff`, the Go `runtime/cgo` files), no file symbol is left. The
  list above is complete for the code that has file symbols.

**Code that runs at each start.** Some code of the static components runs
at each start, in every configuration. Method: the ELF dynamic section
(`objdump -p`: `INIT`, `INIT_ARRAY` of 2 entries, `FINI`, `FINI_ARRAY`,
no `PREINIT_ARRAY`), the disassembly of `_init` and of each
`.init_array` entry, and the direct calls of the 457 Go package `init`
functions (`go tool objdump`). The glibc start code calls `_init`
(`DT_INIT`) and then the `.init_array` entries before `main`.

| Start path | What it runs | Input | Evidence |
| ---------- | ------------ | ----- | -------- |
| ELF `_init` | `OPENSSL_cpuid_setup` of OpenSSL 3.0.11: CPU feature detection. It reads the environment variable `OPENSSL_ia32cap` (`getenv`), parses it with `ossl_strtouint64`, and runs `OPENSSL_ia32_cpuid` (the `CPUID` instruction). | The process environment and the CPU. No network input. The environment comes from the Railway service variables; no file of this repository sets `OPENSSL_ia32cap`. | `_init` at `0x408000` calls `OPENSSL_cpuid_setup`; its only calls are `getenv@plt`, `ossl_strtouint64`, and `OPENSSL_ia32_cpuid`. OpenSSL 3.0.11 [`crypto/cpuid.c#L95-L157`](https://github.com/openssl/openssl/blob/openssl-3.0.11/crypto/cpuid.c#L95-L157). None of the 45 OSV records for OpenSSL 3.0.11 contains `cpuid` or `ia32cap` in its summary or details. |
| ELF `.init_array` | `frame_dummy` (C run time) and the libzmq constructor of `ctx.cpp`, which only sets `zmq::ctx_t::max_socket_id` to 0. | None. | Disassembly of both entries. |
| Go `zmq4.init.0` | `zmq_version`, then `zmq4_ctx_new` (libzmq `zmq_ctx_new`): it allocates a `ctx_t`, reads `getpid`, and creates the local signalling descriptor of the terminate mailbox (`eventfd`; `make_fdpair` calls `eventfd@plt`). `zmq::random_open()` is a single `retq` in this build, so no random source is read. It opens no network socket and starts no I/O thread: libzmq starts the reaper and the I/O threads only with the first socket. | None (the process ID only). No network input. | `go tool objdump`: the only Go `init` with a direct call into C is `zmq4.init.0` (calls `Version` and `_C2func_zmq4_ctx_new`); [`zmq4.go#L129-L158`](https://github.com/pebbe/zmq4/blob/v1.2.10/zmq4.go#L129-L158). libzmq 4.3.4 [`zmq.cpp#L130-L148`](https://github.com/zeromq/libzmq/blob/v4.3.4/src/zmq.cpp#L130-L148), [`ctx.cpp#L76-L105`](https://github.com/zeromq/libzmq/blob/v4.3.4/src/ctx.cpp#L76-L105), [`ctx.cpp#L478-L491`](https://github.com/zeromq/libzmq/blob/v4.3.4/src/ctx.cpp#L478-L491) (`start` only in `create_socket`). |

**Code that runs with the `zmq` dispatcher (issue #14).** The deployed
configuration sends every record type to `zmq`. At start,
`zmq.NewProducer` creates a PUB socket (libzmq starts its reaper and one
I/O thread) and binds `tcp://127.0.0.1:5284`
([`datastore/zmq/zmq.go#L115-L187`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/datastore/zmq/zmq.go#L115-L187)).
Then libzmq runs its TCP listener, the ZMTP handshake with the NULL
mechanism, the subscription handling, and the message framing for each
peer that connects. Input: only peers on the loopback interface of the
container. Two processes run there: `ingest`, whose SUB socket (the
`zeromq` crate) is the only peer, and `fleet-telemetry` itself. The
`vuln` premise requires the literal address `127.0.0.1`, so no other host
can connect. CURVE is off (no `server_key_json_path`), so the tweetnacl
copy does not run. The transport is `tcp://`, not `ws://`, so the SHA-1
copy of the WebSocket transport does not run. The OSV query for the
release commit of libzmq 4.3.4 has no record (table below).

Which libzmq code takes input from a peer: the TCP listener and accept;
the greeting, which also selects the older protocols, because libzmq
4.3.4 accepts ZMTP 1.0 and 2.0 peers (`src/zmtp_engine.cpp`, the
`v1_decoder` and `v2_decoder` paths) besides ZMTP 3.x; the NULL mechanism
handshake (the peer sends its greeting, mechanism name, and READY command
with its socket type); the ZMTP 3.1 `PING` command, which libzmq answers
with `PONG`; the SUB/XSUB subscription messages (the topic prefixes that
PUB matches); and the decoders of the frame headers. The PUB socket sends
records, and it reads and discards every other message of a peer. The CI job `image-check` shows that the SUB
socket of `ingest` connects, subscribes, and settles on this socket of
the real binary, and that it reconnects after each child exit.

Who can be a peer: only a process inside the container, through the
loopback interface. Railway exposes only the TCP proxy to port 4443, and
the `vuln` premise requires `zmq.addr` on the literal address
`127.0.0.1`. The container runs two processes, `ingest` and
`fleet-telemetry`, both as user 10001 without capabilities, and no shell
or other program stays running. Since issue #29 the container starts as
root: on Railway the init of Railway (PID 1) runs as root (assumption
until the post-release check), and the entrypoint runs as root until it
replaces itself with `ingest` through `setpriv`. A process of that user that ran in the container (for
example after a code-execution fault in `fleet-telemetry`; a
`railway ssh` session has at least the same access) could connect to
port 5284 and subscribe: it
would get a copy of every record, with the VIN and the location, and it
could send malformed ZMTP to libzmq. Such a process could already read
the records and the TLS key directly (same user, same files and
memory), so the listener adds no access that the user does not have.

Decision: unchanged. The new code is reachable only from inside the
container, and only by the user that already holds the data.

The direct-call scan does not follow indirect calls from a Go `init`
function. The Go packages with C calls in the binary are `kafka` of
`confluent-kafka-go` (no `init` function), `zmq4`, and the standard
packages `net` and `os/user`, which call libc only for name and user
lookups.

**Bundled OpenSSL in the deployed configuration.** Apart from
`OPENSSL_cpuid_setup` at start, no OpenSSL, libcurl, or librdkafka code
runs without the `kafka` dispatcher:

- The vehicle TLS server and every Go TLS client use Go `crypto/tls`,
  which is pure Go. The binary does not call the Debian `libssl` either:
  `libssl.so` is not in its `NEEDED` list.
- OpenSSL and libcurl are in the bundle only for librdkafka. Go code
  reaches librdkafka only through the `confluent-kafka-go/v2/kafka`
  package, which has no `init` function. Upstream imports that package
  in `datastore/kafka` and in `config`. `config` uses only the Go type
  `ConfigMap` and calls `convertKafkaConfig` only for the Kafka producer
  ([`config/config.go#L386-L396`](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/config/config.go#L386-L396)).
- libzmq does not use OpenSSL: the binary has no GnuTLS symbol and no
  libzmq TLS transport; libzmq uses its tweetnacl copy for CURVE.
- The `vuln` task fails when a record type names a dispatcher other than
  `logger` or `zmq`, also through a repeated `records` object.

The start code takes no network input, so the decision for OpenSSL and
libcurl stays class D: the network-facing code of these components is
reachable only through the `kafka` dispatcher.

**Manual advisory comparison** (26 November 2026, OSV queries by release
commit, not a scanner result):

| Component | Release commit | OSV records | Decision |
| --------- | -------------- | ----------- | -------- |
| OpenSSL 3.0.11 | `6ba3884` (tag `openssl-3.0.11`) | 45 (45 CVE IDs) | Not affected while the `kafka` dispatcher is not configured (class D). The records were not compared one by one. |
| libcurl 7.86.0 | `cd95ee9` (tag `curl-7_86_0`) | 123 (68 CVE IDs) | Class D, as above. Not compared one by one. |
| zlib 1.2.13 | `04f42ce` (tag `v1.2.13`) | 5 | Class D. 3 of them are in `minizip`, `untgz`, or the Perl module, which are not part of the library. |
| zstd 1.5.2 | `e47e674` (tag `v1.5.2`) | 0 | No record. |
| lz4 1.9.3 | `d443718` (tag `v1.9.3`) | 1 (CVE-2021-3520) | Fixed in the librdkafka copy: librdkafka v2.1.0 updated `lz4.c` for CVE-2021-3520 ([`CHANGELOG.md#L237`](https://github.com/confluentinc/librdkafka/blob/v2.3.0/CHANGELOG.md#L237)). |
| librdkafka 2.3.0 | `95a542c` (tag `v2.3.0`) | 0 | No record. Class D. |
| libzmq 4.3.4 | `4097855` (tag `v4.3.4`) | 0 | No record. The known advisories CVE-2020-15166, CVE-2021-20235, and CVE-2021-20236 affect versions before 4.3.3. |
| tweetnacl (libzmq copy) | none of its own | not applicable | tweetnacl has no release commits and no OSV package. The copy is a file of the libzmq repository, so the libzmq 4.3.4 query above covers it (0 records). Reachable only through CURVE, which is not enabled. Not compared further. |
| SHA-1 (libzmq copy) | none of its own | not applicable | A file of the libzmq repository, covered by the libzmq 4.3.4 query. Reachable only through the WebSocket transport, which is not configured. |
| libsodium 1.0.19 | `fb4533b` (tag `1.0.19-RELEASE`) | 0 | Not linked (see above). CVE-2025-69277 does not apply: `crypto_core_ed25519_is_valid_point` is not in the binary. |

An empty OSV answer does not prove that no vulnerability exists. The 119
CVE IDs of these records are not in the CISA KEV catalog 2026.11.25. The
OpenSSL and curl records are not compared one by one, because the code is
not reachable in this deployment; the `kafka` premise of the `vuln` task
guards this decision. Compare them before any change enables the `kafka`
dispatcher.

STACK.md §13 records this surface as a gap without a scanner.

## Debian 12 packages of the upstream image

The upstream image is based on `gcr.io/distroless/cc-debian12`. The scan
finds 61 Debian 12 advisories in 5 source packages: `ca-certificates`
(1), `gcc-12` (7), `glibc` (30), `openssl` (20), and `tzdata` (3).

Decision: **not in the production image.** Evidence: the production
`Dockerfile` copies only `/fleet-telemetry` from the stage `upstream`
(fact 1). The production base is Debian 13, and its packages are triaged
below. One `PackageOverrides` entry with `ecosystem = "Debian:12"`
ignores these advisories in the upstream image scan only. The `vuln` task
fails when the Dockerfile differs from its allowlist, which allows only
the copy of `/fleet-telemetry` from the upstream image and a final stage
from `debian`.

## Debian 13 base

The production image is `debian:trixie-slim` with `ca-certificates` and
`libstdc++6`. `libstdc++6` is already in the base image.

### Fixed in the build

| Source package | Groups | Fixed version | Evidence |
| -------------- | ------ | ------------- | -------- |
| libzstd | DEBIAN-CVE-2026-124851, DSA-6657-1 (24 November 2026) | 1.5.7+dfsg-1+deb13u1 | `trixie-security` Packages index of 26 November 2026 lists `libzstd1` 1.5.7+dfsg-1+deb13u1. |
| shadow | DEBIAN-CVE-2026-135901, DSA-6661-1 (25 November 2026) | 1:4.17.4-2+deb13u1 | The same index lists `passwd` and `login.defs` 1:4.17.4-2+deb13u1. |

No newer base image contains these fixes: `trixie-slim` has the same
digest as `trixie-20261123-slim`, and the advisories are from 24 and
25 November. The base image lists `trixie-security` in
`/etc/apt/sources.list.d/debian.sources`. The Dockerfile therefore runs
`apt-get upgrade` after `apt-get update`.

Trade-off: the build installs the security fixes of the build date, so
two builds of the same commit can differ. The base tag stays
`debian:trixie-slim`; a digest pin or an apt snapshot gives a repeatable
build, but holds back every security fix until the next pin update. The
built image is not scanned (STACK.md §13). The post-release check reads
the build log (STACK.md §15).

The scan reads the `FROM` image, not the built image, so these 4 groups
stay in the scan result. Their ignore entries have the reason "fixed in
the build".

### No Debian fix: accepted as a group

65 groups have no fix in Debian 13 on 26 November 2026. Decision:
**accepted.** The only network input to the service is the mTLS port, and
the running service does not execute the affected package tools.

Loaded by the running server process and by `ingest` (14 groups). Since
issue #14, `ingest` (Rust; PID 1 under Docker, PID 2 under the init of Railway) runs in the same image. Premise
(checked): it links only glibc (`libc.so.6`, `libm.so.6`, the ELF
interpreter) and `libgcc_s.so.1` (gcc-14), the packages of this table;
the C code of `aws-lc-sys` is linked statically. This is the expected
result for the Rust target `x86_64-unknown-linux-gnu`. The CI job
`image-check` fails when `ldd` of `/usr/local/bin/ingest` in the built
image lists another library.

| Source package | Groups (max score) |
| -------------- | ------------------ |
| glibc (`libc6`) | DEBIAN-CVE-2026-84243 (7.0), -100290 (3.7), -115819 (4.8), -121757 (5.9), -121758 (5.9), -124024 (5.9), -126765 (3.7) |
| gcc-14 (`libstdc++6`, `libgcc-s1`) | DEBIAN-CVE-2026-95619 (7.7), -102010 (7.0), -114690 (6.2), -114691 (5.9), -115455 (6.5), -115456 (3.7), -125709 (4.2) |

Run by the entrypoint, not affected (5 groups, issue #29). Since issue
#29, `ingest/entrypoint.sh` runs as root at each start, before the drop
to user 10001. Besides the builtins of `/bin/sh` (dash), it runs three
programs once, with fixed arguments and no input from the network: `rm`
and `chown` (coreutils 9.7-3) and `setpriv` (util-linux
2.41.5-0+deb13u1). Checked in `debian:trixie-slim` for linux/amd64 on
27 November 2026 (`crane export` and `objdump -p`): `rm` and `chown` load
only `libc.so.6`; `setpriv` loads `libcap-ng.so.0` (libcap-ng
0.8.5-4+b1) and `libc.so.6`. The scan has no gating advisory for
coreutils, libcap-ng, or dash. The five util-linux groups are in other
programs of the package (OSV details, read on 27 November 2026):
DEBIAN-CVE-2026-3184 is in `login -h`; -76642, -78409, and -78410 are
in `mount` and its `X-mount.*` options (libmount); -78408 is in
`nsenter --join-cgroup`. `setpriv` does not load libmount and does not
run these programs. Decision: **accepted**, with the reason "not
affected". The entrypoint allowlist of `vuln` fixes the three programs:
a change of the entrypoint fails `vuln` and repeats this split.

| Source package | Groups (max score) |
| -------------- | ------------------ |
| util-linux | DEBIAN-CVE-2026-3184 (5.3), -76642 (8.5), -78408 (7.9), -78409 (7.0), -78410 (7.8) |

On disk only; the running service does not execute or load these
packages (46 groups). Before issue #29, the entrypoint (since issue #14)
ran shell builtins only. Since issue #29 it also runs `rm`, `chown`, and
`setpriv` (above); dash and these programs load only the libraries
named above. The `chmod` and `install` steps run at build time.

| Source package | Groups (max score) |
| -------------- | ------------------ |
| acl | DEBIAN-CVE-2026-54369 (8.4), -54370 (7.2) |
| attr | DEBIAN-CVE-2026-54371 (8.4) |
| bzip2 | DEBIAN-CVE-2026-42250 (4.8) |
| dpkg | DEBIAN-CVE-2026-91094 (5.5) |
| ncurses | DEBIAN-CVE-2025-6141 (4.8), DEBIAN-CVE-2025-69720 (7.8) |
| openssl | DEBIAN-CVE-2026-35190 (5.3), -84785 (not rated), -84786 (7.5), -88822 (6.5), -88823 (7.5), -95690 (5.3), -95691 (8.1), -95692 (7.5), -107493 (7.7), -107494 (7.5), -107496 (7.5), -121481 (5.9), -121482 (7.5), -124709 (7.5) |
| pam | DEBIAN-CVE-2025-8941 (7.8), DEBIAN-CVE-2026-54411 (6.9) |
| perl | DEBIAN-CVE-2026-9538 (7.5), -15534 (5.7), -40558 (not rated), -82560 (7.5) |
| shadow | DEBIAN-CVE-2024-56433 (3.6) |
| sqlite3 | DEBIAN-CVE-2026-50812 (5.5), -50813 (6.1), -119371 (6.8), -119372 (6.8), -119373 (8.5), -119374 (8.5), -119375 (6.8), -119376 (6.8), -119377 (3.3), -119378 (6.8), -119379 (8.5), -119586 (8.5), -119591 (6.8) |
| systemd | DEBIAN-CVE-2026-15059 (5.5), -16742 (6.7) |
| tar | DEBIAN-CVE-2026-5704 (5.5), -18477 (4.4), -18508 (4.4) |

The Go server does not use the Debian OpenSSL: it uses Go `crypto/tls`,
and `libssl` is not in its `NEEDED` list. The binary carries its own
static OpenSSL 3.0.11 for librdkafka. Of that code, only the CPU feature
detection `OPENSSL_cpuid_setup` runs in the deployed configuration, at
each start (see "Static C libraries").

Reassessment rule:

- The ignore entries expire on 2027-01-25. Then `vuln` fails. Repeat the
  scan without the Debian ignore file, and record the new state.
- When Debian publishes a fix for a group of glibc or gcc-14 (loaded by the
  server), start a rebuild of the ingest image. `apt-get upgrade` installs
  the fix. A build can reuse a cached layer of the `apt-get` step, so the
  build log must show that the step ran. The ignore entry does not show
  that a fix appeared; this gap lasts until the expiry date.
- When the Rust ingest process joins the image (ADR 0001), repeat the
  split above for the libraries that it loads.

## Ignore files and premises

- `docs/security/osv-scanner-tesla-fleet-telemetry.toml`: 33 entries
  (one for each Go group; osv-scanner also ignores the aliases) and the
  Debian 12 package override. Expiry 2027-02-24.
- `docs/security/osv-scanner-debian.toml`: 69 entries, one for each
  gating Debian 13 group. Expiry 2027-01-25.
- The `vuln` task selects the file from the image repository name:
  `osv-scanner-<repository, / replaced by ->.toml`. An image without a
  file fails the task.
- Before the scans, the `vuln` task checks these premises and fails when
  one breaks. `ingest/Dockerfile` (fact 1): a strict allowlist of the
  expected lines; only the three image tags may change; the builder tag
  follows `rust-toolchain.toml` and the Debian release of the final
  stage; printable ASCII only; no parser directive. After the scans, the
  task fails when it did not scan the two runtime images.
  `ingest/entrypoint.sh`: a strict allowlist of the expected lines (first
  line `#!/bin/sh`, printable ASCII only). `ingest/server_config.json`
  (fact 8): exactly one
  JSON value; the same leaf paths in the token stream and in the parsed
  value, so no repeated key of any kind; lowercase keys
  (`^[a-z0-9_]+$`) in the root, `monitoring`, `tls`, and `zmq` objects;
  `zmq.addr` of the form `tcp://127.0.0.1:<port>`; all
  dispatchers in `records` are `logger` or `zmq`; `use_default_eng_ca` is not true;
  `tls.ca_file` is empty; `status_port` and `monitoring.profiler_port`
  are not set; `monitoring.prometheus_metrics_host` is unset or
  `127.0.0.1`.
- `scripts/vuln-premise-test.sh` (in the `checks` task) runs the premise
  part of the task on 55 changed inputs and fails when a change passes.
  Examples: a `USER root` or `USER 10001:10001` line, an extra `COPY . .`, a builder tag of another
  Rust version or Debian release, a parser directive, `ENTRYPOINT` in
  place of `CMD`, an entrypoint that starts the server itself (also
  through `PATH`, a name built from parts, or a sourced file, the bypasses
  of the earlier denylist found in review round 1 of PR #23), an `exec`
  in a block that never runs, `set -eux`, a kept migrator URL, each
  changed or missing argument of the `setpriv` line, a missing or changed
  `chown` of the TLS files, an `unset` after the first program, a missing
  `rm -f` or noclobber, and `zmq.addr` values `tcp://0.0.0.0:5284`, `tcp://*:5284`,
  `tcp://localhost:5284`, `tcp://[::]:5284`, a key `Addr`, and a
  repeated `zmq` object.
- After an expiry date, an entry stops to apply, and `vuln` fails on the
  finding again. An unused entry gives a warning only. Remove unused
  entries in the next triage.

## `vuln` and `verify`

STACK.md §3 kept `vuln` out of `verify` until a reviewed ignore list
exists. With the ignore files, `vuln` passes (exit 0 on 26 November 2026,
54 s, about 67 MB of image downloads). Decision: `vuln` stays a separate
required check and does not join `verify`. Evidence:

- New gating advisories arrive often. The OSV publication date is a proxy
  for the day on which a finding first appears in the scan. Publication
  dates of the 69 gating Debian 13 groups: 27 in November 2026 (until the 26th), 16 in
  October. They arrived on 9 different days between 30 October and
  25 November, and 6 of them after the base image of 23 November. For the
  Go groups: 8 in October 2026.
- In `verify`, each new advisory would fail the check for every open
  change, also a change that does not touch the image. Each new ignore
  entry is a security decision with independent review. The result would
  be about two blocked merges each week, mostly for packages that the
  service does not load.
- `cargo deny --locked check` in `verify` has the same kind of network
  dependency, but the 56 locked crates get new advisories much less
  often.

`vuln` stays mandatory at the points of DOCTRINE.md P7: before each
delivery task and in each dependency PR. Reassess when a scheduled CI run
can execute `vuln` and report new findings outside the merge path, or when
the gating advisory rate falls below one each month over 60 days.

## Reassessment triggers

- An expiry date passes (`vuln` fails).
- A new `tesla/fleet-telemetry` release or another base image: repeat the
  Go triage in the dependency PR.
- A configuration premise breaks (`vuln` fails).
- An ignored CVE appears in the CISA KEV catalog: the fix becomes due
  (DOCTRINE.md P7). Compare the CVE IDs with the catalog at each
  vulnerability check; `vuln` does not read the catalog.
- A change of the builder image `rust:<version>-slim-<codename>`: the
  builder image is not scanned (`STACK.md` §13); compare its gcc-14,
  binutils, and glibc advisories with the CISA KEV catalog.

## Reproduction

1. `mise run vuln` (needs the network). Expected on 26 November 2026:
   exit 0; "Filtered 44 vulnerabilities" for the upstream image and
   "Filtered 159 vulnerabilities" for the Debian image.
2. Raw counts: run `osv-scanner scan image --archive <tar>` without
   `--config` on the archive that `crane pull --platform linux/amd64`
   writes. Expected: 103 and 79 in the summary lines.
3. Call graph: the govulncheck commands above, on a checkout of tag
   `v0.10.1` in a scratch directory.
4. KEV: download
   `https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json`,
   collect the CVE IDs and the `upstream` CVE IDs of the osv-scanner JSON
   output (152 IDs on 26 November 2026), add CVE-2025-69277 (libsodium,
   not linked) and the CVE IDs of the OSV commit queries for the static C libraries (119 IDs), and
   intersect with `.vulnerabilities[].cveID` of the catalog.
5. Static C libraries: `POST https://api.osv.dev/v1/query` with
   `{"commit": "<release commit>"}` for each release commit of the table
   in "Static C libraries". Versions in the binary: search the binary for
   the version strings, and read `ZSTD_versionNumber`,
   `LZ4_versionNumber`, and `zmq_version` with `go tool objdump`.
6. Config premises: `scripts/vuln-premise-test.sh` runs the premise part
   of the `vuln` task (the lines before `cargo deny`) in a scratch
   directory for each variant of `ingest/Dockerfile`,
   `ingest/entrypoint.sh`, `ingest/server_config.json`, and
   `rust-toolchain.toml`. Expected: "56 of 56 cases as expected".
