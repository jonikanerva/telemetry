# Draft: comment on upstream issue 578 of fleet-telemetry

> **Warning:** do not paste any text of this file into a commit message,
> an issue, a pull request, or a comment of this repository. This
> repository is public. GitHub turns an issue reference or an issue URL
> in such text into a visible event on the upstream issue. Use this text
> only in the upstream issue itself, after the owner decides to post it.

Status: draft for the owner. Not posted. Post it as a comment on the
upstream issue `https://github.com/teslamotors/fleet-telemetry/issues/578`,
or decide not to post it. The text contains no detail of this deployment.

Reason for the draft (issue 15 of this repository): high-severity
advisories of the bundled Go modules (gRPC CRITICAL and HIGH, logrus HIGH)
have no fix in a `fleet-telemetry` release. v0.10.1 is the newest release,
and `main` is at the same commit. Upstream issue 578 already reports the
v0.9.5 state, so a comment with the v0.10.1 state and call-graph data is
better than a new issue.

---

## Comment text

The state is the same in v0.10.1 (`tesla/fleet-telemetry:v0.10.1`,
linux/amd64 digest
`sha256:dd3bfb47b730ab5c4c8a4efc7c7c6669ce21d4bf7e56e2a3ab1d25f30ce7adf0`,
commit `6689adb`, also the head of `main` on 2026-11-26). Go is now
1.27.4, so the standard library findings of the issue are fixed. The
module versions did not change, and some new advisories apply.

**Advisories in v0.10.1** (osv-scanner 2.7.1, 2026-11-26): 33 alias groups
in 9 modules.

| Module | Version | Advisories | Fixed in |
| ------ | ------- | ---------- | -------- |
| google.golang.org/grpc | v1.56.3 | GO-2026-4762 (CVE-2026-33186, CRITICAL), GO-2026-6061, GO-2026-6348, GO-2026-6441, GO-2026-6443 | v1.83.2 for all |
| golang.org/x/net | v0.33.0 | 19 advisories, including GO-2026-6860 and GO-2026-6866 (new since the issue) | v0.62.0 for all |
| golang.org/x/text | v0.21.0 | GO-2026-5970, GO-2026-6629 | v0.41.0 |
| golang.org/x/sys | v0.30.0 | GO-2026-5024 (`x/sys/windows` only) | v0.44.0 |
| github.com/sirupsen/logrus | v1.9.0 | GO-2025-4188 (CVE-2025-65637, HIGH) | v1.9.3 |
| github.com/gorilla/websocket | v1.5.3 | GO-2026-6278 (client mask key) | commit d67f418 |
| github.com/eclipse/paho.mqtt.golang | v1.5.0 | GO-2025-4173 | v1.5.1 |
| github.com/aws/aws-sdk-go | v1.44.278 | GO-2022-0635, GO-2022-0646 (`s3crypto`, not imported) | none in SDK v1 |
| github.com/teslamotors/fleet-telemetry | `(devel)` | GO-2026-5553 | false positive, see below |

**Reachability (govulncheck v1.10.0, source mode on tag v0.10.1, from
`cmd/main.go`).** This can help to set the priority:

- No gRPC server exists, so GO-2026-4762 and GO-2026-6443 are not called.
  The gRPC client advisories (GO-2026-6061, GO-2026-6348) and the
  `x/net/http2` advisories are reachable only through the Pub/Sub
  dispatcher.
- `x/net/proxy` (GO-2025-3503), paho (GO-2025-4173), and the WebSocket
  client role are reachable only through the MQTT dispatcher.
- The logrus `Entry.Writer` functions (GO-2025-4188) are not called.
- The vehicle-facing server uses the HTTP/2 code of the standard library,
  which Go 1.27.4 fixes.

So deployments that use only `logger`, `zmq`, `kafka`, `kinesis`, or
`redis` do not reach these symbols. Deployments with `pubsub` or `mqtt`
do. All deployments still ship them, and scanners flag every image.

**Three more observations on the build:**

1. The binary is dynamically linked (ELF interpreter
   `/lib64/ld-linux-x86-64.so.2`). A probable cause: the Makefile passes
   `--ldflags 'extldflags="-static"'`
   ([Makefile#L10](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Makefile#L10)),
   without the leading dash of `-extldflags`. We did not test this
   cause. The binary needs `libc.so.6`, `libm.so.6`,
   `libstdc++.so.6`, and `libgcc_s.so.1` from the base image. If static
   linking is the intent, the flag is a bug. If dynamic linking is the
   intent, a comment would help users who copy the binary into another
   base image.
2. The Dockerfile builds libzmq 4.3.4
   ([Dockerfile#L15](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Dockerfile#L15)).
   The issue text mentions 4.3.5, the newest libzmq tag. The libsodium
   version is 1.0.19
   ([Dockerfile#L6](https://github.com/teslamotors/fleet-telemetry/blob/6689adb831691b1de211b4aa8220babf7309f31c/Dockerfile#L6));
   the newest tag is 1.0.22. Scanners cannot see the static libzmq,
   libsodium, and librdkafka 2.3.0 in the binary. The librdkafka bundle
   of `confluent-kafka-go/v2` v2.3.0 also links OpenSSL 3.0.11 and
   libcurl 7.86.0 statically; OSV lists 45 and 123 records for these
   release commits. They are reachable only through the `kafka`
   dispatcher.
3. GO-2026-5553 is a false positive for v0.10.1: the fix d5ca0da is in the
   tag. Scanners report it because `go build cmd/main.go` records the main
   module version as `(devel)`. Since Go 1.24, a build of the package
   path (for example `go build ./cmd`) can record the version from the Git
   tag, when the build context contains the Git metadata. Then the false
   positive goes away.

**Request:** a release with grpc ≥ v1.83.2, golang.org/x/net ≥ v0.62.0,
golang.org/x/text ≥ v0.41.0, logrus ≥ v1.9.3, and paho ≥ v1.5.1 (the
`go get` line in the issue, with x/net v0.62.0). The genproto note in the
issue still applies.

Thank you.
