# ADR 0010: Web architecture

- Status: Accepted, 28 November 2026. Decided by: the lead, within the
  delegated technical authority of `DOCTRINE.md` and ADR 0004. The
  exceptions in decision 9 are approved by the independent reviewer
  (qa-enforcer) in PR #46: <https://github.com/jonikanerva/telemetry/pull/46#pullrequestreview-6092253490>.
  Decision 9, the JSON-only formatter as an exception: superseded in part
  by ADR 0016 (28 November 2026); decision 8 stays as a rule.
  Owner decision, 28 November 2026: the product supports Chrome on a
  desktop viewport only, and the Playwright journeys run in chromium only.
  This supersedes the three-engine statements of decision 5 and of "Result
  of experiment E4"; the test server keeps HTTPS, as production has.
- Scope: the layout, the boundaries, the HTTP contract, the serving, and
  the image of the web application. Not in scope: the login and the
  sessions (the session ADR of the login slice), the derivation of the
  battery view (the derivation ADR of the battery slice), and
  the chart library (the battery view records it).

## Context

- `VISION.md` asks for a private web view of the vehicle history, on the
  owner's Railway, Neon, and Cloudflare accounts, with no new paid service.
- ADR 0009 selects Rust for the application server and the web profile
  (`docs/stack-web-profile.md`) for the web parts. The profile names Axum,
  utoipa, Hey API with Zod, React, Vite, and TanStack Router and Query.
- The ingest service is live. A change that releases ingest needs the
  checks of `docs/ingest-release.md`. The ingest image reads only
  `crates/ingest` in full (STACK.md §15).
- utoipa 6.0.0 has only the feature `jiff_0_2`; the project uses jiff 1
  (`docs/stack-web-profile.md` §6, "Known conflict").
- Experiment E1 sent a serde tagged union and an instant type through
  utoipa 6.0.0, OpenAPI 3.1, Hey API 0.99.0, and Zod 4.6.7, with executed
  tests (result below).

## Decision

1. **Layout.** `crates/domain` (package `telemetry-web-domain`) holds pure
   rules and depends on no HTTP, runtime, or database crate.
   `crates/server` (package `telemetry-web-server`) holds the HTTP server,
   the configuration, the problem mapping, the static files, the logging,
   the shutdown, and the contract export. `packages/api-client` holds the
   generated client and a handwritten facade. `web/` holds the SPA. `e2e/`
   holds the browser journeys. `webapp/Dockerfile` builds the image.
2. **No shared code with ingest.** `crates/server` does not depend on
   `crates/ingest`, and no third crate holds shared code. A shared crate
   would put the ingest data path into each change of that crate, with an
   ingest release and an independent reviewer run each time. The web
   server has its own small allowlist JSON logger and, when it needs a
   database, its own connection options with the same tests.
3. **HTTP contract.** utoipa and utoipa-axum export OpenAPI 3.1 to
   `contract/openapi.json`. Hey API 0.99.0 generates the SDK, the types,
   and the Zod schemas into `packages/api-client/src/generated/`. Both are
   committed; `contract-check` compares a new export and generation in a
   scratch directory with the complete committed trees.
   - The SDK runs Zod as validator and as response transformer, so it
     validates each success body and returns the parsed value without
     unknown keys. The facade validates each problem body with the
     generated Problem schema and maps every failure to one `ApiError`:
     `problem`, `invalid_response`, `network`, or `timeout`. A 2xx body
     that fails the schema is `invalid_response`, never data.
   - Every failure of the server is a problem document
     (`application/problem+json`) with a code of the closed `ProblemCode`
     set. A new code is a contract change.
   - Instants cross the wire as RFC 3339 UTC with `Z` and 0 to 6
     fractional digits. `wire::Instant` wraps `jiff::Timestamp` with an
     explicit schema (`format: date-time` and a pattern), so utoipa needs
     no jiff feature.
   - Compatibility window: additive changes only within `/api/v1`. The
     server writes its version into `index.html` at start-up (a `<meta>`
     element, no inline script), and `/health` returns the version. The
     SPA shows a reload banner when the two differ.
4. **Serving.** One image holds the server binary and the built SPA. The
   server serves `/assets/*` with precompressed gzip and an immutable
   cache header; a missing asset is a 404 problem, never `index.html`.
   An unknown `/api/*` path is a 404 problem. Any other GET returns
   `index.html` with `no-store`. Every response has a strict CSP without
   `'unsafe-inline'`, HSTS, `X-Content-Type-Options: nosniff`,
   `Referrer-Policy: no-referrer`, a `Permissions-Policy` that turns off
   geolocation, camera, and microphone, `Cross-Origin-Opener-Policy` and
   `Cross-Origin-Resource-Policy` `same-origin`, and
   `X-Robots-Tag: noindex`. API responses and `/health` have
   `Cache-Control: no-store`. Zod runs with `jitless: true`, because
   its `new Function` probe is a CSP violation.
5. **Test server.** The Cargo feature `test-server` adds the command
   `serve-test` with test-only routes on the loopback interface. The
   Playwright journeys run against it with the real router, middleware,
   and built SPA. The release image does not build the feature.
   `serve-test` serves HTTPS (rustls with the aws-lc-rs provider, TLS 1.2
   and 1.3, a 10 s handshake deadline) in the accept loop of `serve`, with
   a self-signed certificate for `localhost` that `e2e/tls-cert.sh` makes
   for each run. Reason: experiment E4 showed that WebKit drops a
   `__Host-` `Secure` cookie over `http://localhost`, and the session
   cookie of the login needs that prefix in all three engines (superseded:
   chromium only, see Status). rustls and
   tokio-rustls are optional dependencies of the feature, so the release
   binary has no TLS code; `serve` stays plain HTTP behind the Railway
   proxy. Alternatives: a session cookie without the `__Host-` prefix or
   without `Secure` in the tests only (the tests would not check the
   production cookie); a TLS proxy in front of the test server (one more
   tool to pin and run).
6. **Image and service.** `webapp/Dockerfile` has a Node builder, a Rust
   builder, and a `debian:trixie-slim` final stage with `apt-get upgrade`
   and `USER 10001:10001`. A `USER` line is safe here, unlike the ingest
   image: the server process holds all its own service variables, so
   `/proc/1/environ` exposes nothing more. The Railway service
   `telemetry-web` (ADR 0004, grant 2) has no TCP proxy and no domain
   until the login exists. Its healthcheck `/health` uses no database.
7. **One build context.** Both images use the root `.dockerignore`, so
   the web sources are in the build context of both images. The ingest
   image is kept apart from the web sources by its watch paths and by the
   `COPY` lines of `ingest/Dockerfile`; the web image is kept apart from
   the ingest sources in the same way. The deployment test of each image
   checks its Dockerfile with a text check:
   - It accepts a small set of instruction forms, the forms that the two
     Dockerfiles use, and it refuses everything else with the line number.
     It lists the sources of each `COPY` that reads the build context, and
     the test compares that list with the expected inputs.
   - For the accepted forms, it joins the lines as the `BuildKit` parser
     does (`moby/buildkit`, `frontend/dockerfile/parser/parser.go`, commit
     `ad25d4b2af8e4e3230ec3178ea0d121129e8650d`). Only a `RUN` instruction
     can continue on the next line. A parser directive, a non-ASCII
     character, a control character (except tabs in the leading
     indentation and the CR of a CRLF line end), a backslash in an
     instruction line that is not its last character, a double backslash,
     and an empty or comment line inside a continuation are refused.
   - On disk, no listed `COPY` source, no parent directory of it, and no
     entry below a listed directory that the `.dockerignore` rules send
     into the build context is a symbolic link, and the Git index holds no
     symbolic link and no submodule. A `COPY` follows a link, and the
     build context transfer includes its target. This rule reads the
     checkout; it does not see a link that a build tool makes during the
     build.
   - Limits: the check reads the text of the Dockerfile; it does not run
     Docker. It does not see an `ONBUILD` trigger of a base image or a
     builder setting such as `--build-context` or `BUILDKIT_SYNTAX`. A base
     image or a `RUN` command can read what an accepted `COPY` put into the
     image. `COPY --from=<name>` does not have to name an earlier stage of
     the same file; another name is an external image, not a read of the
     build context. The review of each Dockerfile change also covers the
     Dockerfiles. The CI job `image-check` proves that each image builds
     and runs; it builds with the same build context, so it does not detect
     a read of a source of the other image.
   - A per-Dockerfile ignore file is not used, because Railway does not
     document it.
8. **Logging.** Decision 3 of ADR 0009 (only the JSON formatter) also
   applies to the web server. Its allowlist logs the method, the route
   template (never the raw path or query), the status, and the latency.
9. **Exceptions (STACK.md §13):**
   - TypeScript 6.0.3 as a development dependency of
     `packages/api-client`, only for the generator, which needs the
     compiler API that TypeScript 7.0 does not have.
   - `exactOptionalPropertyTypes` off only for the generated directory,
     because the client runtime that Hey API copies does not pass it.
   - No scan of the web builder images.
   - No build of the web image by Railway before the merge (as ADR 0008
     for the ingest image).
   - The JSON-only formatter of ADR 0009, decision 3, for the web server
     too (decision 8).
   - The ignore file `docs/security/osv-scanner-web-debian.toml` for the
     web runtime image: its glibc and gcc-14 entries hold only while the
     web service has no public network input (`docs/security/image-advisories.md`,
     "Web image"; STACK.md §13, row of ADR 0005).

## Result of experiment E1

- The chain works with three rules for the battery contract:
  - A count needs `#[schema(minimum = 0, maximum = 4294967295_u64)]`;
    without it, utoipa writes `int32` and Zod caps the value at 2^31-1.
  - An `Option` that is always present needs `#[schema(required = true)]`;
    without it, Zod accepts an absent key (`nullish`).
  - A serde tagged enum gives a plain `oneOf`, and Hey API gives a
    `z.union` of objects with a literal tag. It validates correctly, and
    TypeScript narrows on the tag. `#[schema(discriminator)]` does not
    apply to tagged enums.
- Zod `z.iso.datetime()` rejects offsets other than `Z`, but accepts 9
  fractional digits; the pattern of `wire::Instant` limits them to 6.
- The output does not depend on the output directory
  (`output.tsConfigPath: null`).

## Result of experiment E4

- Question: do the three Playwright engines keep a
  `__Host-e4=1; Path=/; Secure; HttpOnly; SameSite=Strict` cookie over
  `http://localhost`?
- Playwright 1.65.1, on the CI runner `ubuntu-26.04` (linux/amd64) and on
  macOS arm64: chromium and firefox keep the cookie; webkit drops it.
- Consequence: the test server serves HTTPS (decision 5). Over
  `https://localhost` with the certificate of `e2e/tls-cert.sh`, all three
  engines keep the cookie; the E4 test asserts this in each engine.
- All three engines run on the CI runner with the system packages of
  `playwright install --with-deps`.

## Key alternatives

- **A shared crate for the database connection and the logger.** Rejected
  for decision 2.
- **Embedded SPA files in the binary.** Not needed: one image holds both,
  and `ServeDir` is the profile default.
- **A build argument for the version.** Rejected: the image has no `ARG`
  line, and the Railway build-argument behaviour is not verified. The
  runtime variable `RAILWAY_GIT_COMMIT_SHA` gives the version.
- **A per-Dockerfile ignore file.** Rejected for decision 7.

## Consequences

- A change of `Cargo.lock`, a member `Cargo.toml`, `/.cargo/**`,
  `rust-toolchain.toml`, or `.dockerignore` releases both services.
- Each HTTP change runs `mise run contract` and commits two generated
  trees. Two PRs that change the contract conflict there; the second one
  runs the task again.
- `verify` needs the Playwright browsers and the network for `pnpm audit`.

## Cost of reversal

Medium. The boundaries are package boundaries; a change of the contract
tooling regenerates the client.

## Reassessment

- A Hey API release that supports TypeScript 7 or passes
  `exactOptionalPropertyTypes`: remove the exceptions.
- A utoipa release with `jiff` 1 support: reassess `wire::Instant`.
- A third consumer of the database code: reassess decision 2.
