# The web image: the server telemetry-web-server and the built SPA files in
# one image (docs/stack-web-profile.md, section 15). The server serves the
# SPA from WEB_STATIC_DIR. The container runs as user 10001 from the start.
# A USER line is safe here: the server process holds every service variable
# of the web service in any case, so the environment of PID 1 exposes
# nothing more (compare ADR 0001, "Process tree on Railway", for ingest).
#
# The build context is the repository root:
#   docker build -f webapp/Dockerfile .
# The root .dockerignore serves this image and the ingest image. Each input
# of a COPY line has a Railway watch path of the service telemetry-web and
# an entry in .dockerignore; crates/server/tests/deployment.rs checks this.
# The build reads crates/server and crates/domain in full, but only the
# Cargo.toml of crates/ingest, so a change of the ingest sources does not
# change this image. The image has no ARG line: no build argument and no
# secret reaches a build step. The server reads its version from the
# runtime variable RAILWAY_GIT_COMMIT_SHA.
#
# The vuln task of mise.toml compares the COPY --from lines with a strict
# allowlist and scans the runtime FROM image (debian). It does not scan the
# builder stages web and build (STACK.md section 13). Change the allowlist
# and docs/security/image-advisories.md in the same change as these lines.

# Stage 1: the SPA. The tag is the Node version of mise.toml on the Debian
# release of the final stage. npm installs the exact pnpm version of
# mise.toml from the npm registry. crates/server/tests/deployment.rs checks
# that both pins equal mise.toml. The build uses the committed contract and the committed client
# (packages/api-client/src/generated); it generates no code. The filter
# installs only the SPA and the workspace packages that it depends on.
# Vite reads the tsconfig.json of web/ and of packages/api-client/, and
# both extend tsconfig.base.json.
FROM node:24.22.3-trixie-slim AS web
WORKDIR /build
RUN npm install -g pnpm@12.23.0
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml .npmrc tsconfig.base.json ./
COPY packages packages
COPY web web
RUN pnpm install --frozen-lockfile --filter "telemetry-web-spa..."
RUN pnpm run build:web

# Stage 2: the release build of the server. The tag is the Rust version of
# rust-toolchain.toml on the Debian release of the final stage, so that the
# binary links against the same glibc. The release build never has the
# feature test-server, so the test-only routes are not in the binary.
FROM rust:1.100.0-slim-trixie AS build
WORKDIR /build
# The same value as .cargo/config.toml: SQLx query macros read the
# committed metadata in crates/<crate>/.sqlx/ and never connect to a
# database.
ENV SQLX_OFFLINE=true
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates/server crates/server
COPY crates/domain crates/domain
# Cargo loads the whole workspace, also for -p. Each other member gets its
# Cargo.toml and an empty src/lib.rs as a stub target. The build does not
# compile the stubs. The deployment test checks that this list equals the
# workspace members other than server and domain.
COPY crates/ingest/Cargo.toml crates/ingest/Cargo.toml
RUN for member in crates/ingest; do mkdir -p "$member/src" && : > "$member/src/lib.rs"; done
RUN cargo build --release --locked -p telemetry-web-server

FROM debian:trixie-slim
# The upgrade installs the Debian security fixes that are newer than the
# base image (docs/security/image-advisories.md#debian-13-base). The result
# depends on the build date. The image installs no package: the server
# links only glibc and libgcc_s, and it has no TLS client, so it needs no
# ca-certificates.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /build/target/release/telemetry-web-server /usr/local/bin/telemetry-web-server
COPY --from=web /build/web/dist /usr/local/share/telemetry-web

# The fixed settings of the server. The Railway service variables hold PORT
# and the secrets.
ENV WEB_STATIC_DIR=/usr/local/share/telemetry-web

# A numeric user needs no entry in /etc/passwd. The SPA files belong to
# root, and user 10001 can only read them.
USER 10001:10001

# The port of the variable PORT on Railway.
EXPOSE 8080

CMD ["/usr/local/bin/telemetry-web-server", "serve"]
