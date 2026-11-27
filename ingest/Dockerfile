# The ingest image (ADR 0001, "Layout"). The container starts as root. The
# entrypoint writes the TLS files and then replaces itself with the Rust
# process ingest as user 10001 (setpriv). ingest takes the process of the
# entrypoint: PID 1 under Docker without an init, PID 2 under the init of
# Railway. ingest starts the upstream fleet-telemetry server as its child.
#
# The build context is the repository root:
#   docker build -f ingest/Dockerfile .
# Each input of a COPY line has a Railway watch path of the service
# telemetry (STACK.md section 15) and an entry in the root .dockerignore;
# crates/ingest/tests/deployment.rs checks this. The root .dockerignore also
# serves webapp/Dockerfile, so the build context holds the web files too,
# but no COPY line of this file reads them. The build reads crates/ingest
# in full, but only the Cargo.toml of each other workspace member, so a
# change of the web sources does not change this image.
#
# The vuln task of mise.toml compares the COPY --from lines with a strict
# allowlist and scans each runtime FROM image. Change the allowlist and
# docs/security/image-advisories.md in the same change as these lines.

# Stage 1: the release build of ingest. The tag is the Rust version of
# rust-toolchain.toml on the Debian release of the final stage, so that the
# binary links against the same glibc. The vuln task checks both parts.
# rust-toolchain.toml is not copied: it also names rustfmt and Clippy,
# which the build does not need and the slim image does not have.
FROM rust:1.100.0-slim-trixie AS build
WORKDIR /build
# The query macros read the committed metadata in crates/ingest/.sqlx/ and
# never connect to a database. .cargo/config.toml sets the same value.
ENV SQLX_OFFLINE=true
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates/ingest crates/ingest
# Cargo loads the whole workspace, also for -p ingest. Each other member
# gets its Cargo.toml and an empty src/lib.rs as a stub target. The build
# does not compile the stubs. The deployment test checks that this list
# equals the workspace members other than ingest.
COPY crates/domain/Cargo.toml crates/domain/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
RUN for member in crates/domain crates/server; do mkdir -p "$member/src" && : > "$member/src/lib.rs"; done
COPY db db
COPY migrations migrations
# A release build never reads the INGEST_TEST_* variables (ADR 0001).
RUN cargo build --release --locked -p ingest

# Stage 2: the upstream server. The Tesla image has no shell. The final
# stage copies only the binary.
FROM tesla/fleet-telemetry:v0.10.1 AS upstream

FROM debian:trixie-slim
# The upgrade installs the Debian security fixes that are newer than the base
# image (docs/security/image-advisories.md#debian-13-base). The result depends
# on the build date.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y \
    && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=upstream /fleet-telemetry /fleet-telemetry
COPY --from=build /build/target/release/ingest /usr/local/bin/ingest
COPY ingest/server_config.json /etc/fleet-telemetry/config.json
COPY ingest/entrypoint.sh /entrypoint.sh
# The entrypoint writes the TLS files into the directory at each start.
# Only the user 10001 (and root) can read the directory.
RUN chmod 0755 /entrypoint.sh \
    && install -d -m 0700 -o 10001 -g 10001 /etc/fleet-telemetry/tls

# The fixed settings of ingest run (crates/ingest/src/config.rs). The ports
# must equal the ports of ingest/server_config.json; the deployment test
# (crates/ingest/tests/deployment.rs) checks this. The Railway service
# variables hold PORT, DATABASE_URL, TLS_CERT, and TLS_KEY.
ENV INGEST_GATE_PORT=4443
ENV INGEST_UPSTREAM_PORT=4444
ENV INGEST_ZMQ_PORT=5284
ENV INGEST_METRICS_PORT=9090
ENV INGEST_CHILD_PROGRAM=/fleet-telemetry
ENV INGEST_CHILD_CONFIG=/etc/fleet-telemetry/config.json

# No USER line: the container starts as root, so that the init of Railway
# (PID 1) and its environment with every service variable belong to root
# and a process of user 10001 cannot read /proc/1/environ (ADR 0001,
# "Process tree on Railway"). Only the entrypoint runs as root. It drops to
# user 10001 with no capabilities before it starts ingest. ingest and
# fleet-telemetry do not need root: all ports are above 1023, and they write
# no file. The Railway pre-deploy command (ingest migrate) runs as root in
# its own short-lived container (STACK.md section 15).

# The Railway TCP proxy forwards the public port to this port.
EXPOSE 4443

# CMD, not ENTRYPOINT: the Railway pre-deploy command replaces the start
# command and runs ingest migrate directly (ADR 0001).
CMD ["/entrypoint.sh"]
