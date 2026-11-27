# The ingest image (ADR 0001, "Layout"). The Rust process ingest is PID 1.
# It starts the upstream fleet-telemetry server as its child.
#
# The build context is the repository root:
#   docker build -f ingest/Dockerfile .
# Each COPY line names one input of the build. The Railway watch paths and
# .dockerignore list the same paths (STACK.md section 15).
#
# The vuln task of mise.toml compares each instruction line with a strict
# allowlist. Change the allowlist and docs/security/image-advisories.md in
# the same change as this file.

# Stage 1: the release build of ingest. The tag is the Rust version of
# rust-toolchain.toml on the Debian release of the final stage, so that the
# binary links against the same glibc. The vuln task checks both parts.
# rust-toolchain.toml is not copied: it also names rustfmt and Clippy,
# which the build does not need and the slim image does not have.
FROM rust:1.100.0-slim-trixie AS build
WORKDIR /build
# The query macros read the committed metadata in .sqlx/ and never connect
# to a database. .cargo/config.toml sets the same value.
ENV SQLX_OFFLINE=true
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
COPY db db
COPY migrations migrations
COPY .sqlx .sqlx
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
# Only the user 10001 can read the directory.
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

# No part of the service needs root: all ports are above 1023, and no
# process writes outside the TLS directory.
USER 10001:10001

# The Railway TCP proxy forwards the public port to this port.
EXPOSE 4443

# CMD, not ENTRYPOINT: the Railway pre-deploy command replaces the start
# command and runs ingest migrate directly (ADR 0001).
CMD ["/entrypoint.sh"]
