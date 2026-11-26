# Telemetry

A private service that collects the telemetry of one Tesla vehicle and turns
it into a history of drives, charging sessions, and parked periods.

The vehicle streams its data with Tesla
[Fleet Telemetry](https://github.com/teslamotors/fleet-telemetry). This
repository contains the ingest service that receives the stream. The
application that stores and shows the data is planned.

## Status

| Part                | State                                                             |
| ------------------- | ----------------------------------------------------------------- |
| Ingest service      | Deployed. The vehicle connects. The config selects 20 fields ([usage estimate](docs/telemetry-usage-estimate.md)). |
| Storage             | Planned. The ingest service writes the records to its log only.   |
| Application and web | Planned. See [VISION.md](VISION.md) for the product direction.    |

## How it works

```text
Tesla vehicle
   │  mTLS WebSocket, port 57438
   ▼
telemetry.calmdonut.com  (DNS only, CNAME to the Railway TCP proxy)
   │  raw TCP; the proxy does not terminate TLS
   ▼
fleet-telemetry          (Tesla's upstream server in a small wrapper image)
   │
   ▼
log                      (planned: Rust application → PostgreSQL → web)
```

- The vehicle authenticates with a client certificate from Tesla. The ingest
  service terminates mTLS itself.
- The vehicle trusts the server certificate through the CA in its telemetry
  config. The service uses a Cloudflare Origin CA certificate.
- A telemetry config tells the vehicle where to connect and which signals to
  send. The config is signed with the Tesla application private key.

## Repository layout

| Path                           | Content                                                        |
| ------------------------------ | -------------------------------------------------------------- |
| `Dockerfile`, `entrypoint.sh`  | Wrapper image for the upstream `fleet-telemetry` binary        |
| `config/server_config.json`    | Ingest service configuration                                   |
| `config/telemetry_config.json` | Telemetry config for the vehicle: host, port, CA, and signals  |
| `crates/ingest/`               | Rust ingest process. Not deployed yet.                         |
| `Cargo.toml`, `Cargo.lock`     | Rust workspace, lints, and locked dependencies                 |
| `rust-toolchain.toml`          | Rust toolchain version                                         |
| `mise.toml`                    | Tool pins and the project commands                             |
| `clippy.toml`, `deny.toml`     | Clippy and cargo-deny settings                                 |
| `VISION.md`                    | Product intent, non-goals, and privacy posture                 |
| `STACK.md`                     | Technology, commands, infrastructure, and release rules        |
| `DOCTRINE.md`                  | Quality principles P1–P9                                       |
| `CLAUDE.md`                    | Operating contract for Claude Code                             |

## Requirements

- A Tesla developer application with a public key hosted on its domain, and
  the matching virtual key paired with the vehicle.
- A host that forwards raw TCP to the container. An HTTP proxy that
  terminates TLS does not work.
- A DNS name for the ingest service and a TLS certificate for that name.

## Configuration

The ingest service reads two variables at start-up. The entrypoint writes
them to files for the server.

| Variable   | Content                               |
| ---------- | ------------------------------------- |
| `TLS_CERT` | Server certificate chain, PEM format  |
| `TLS_KEY`  | Server private key, PEM format        |

The server listens on port 4443 in the container.

Local files that must never be committed. `.gitignore` excludes them.

| Path                     | Content                                               |
| ------------------------ | ----------------------------------------------------- |
| `.env`                   | Short-lived Tesla OAuth access token                  |
| `secrets/tesla_fleet.key`| Tesla application private key                         |
| `secrets/tls.*`          | Server certificate and key                            |
| `.bin/`                  | Locally built Tesla tools                             |

## Deployment

Railway builds the image from `main` and deploys it. A merge to `main` is a
release. A redeploy disconnects the vehicle for a short time. The vehicle
reconnects by itself.

[STACK.md](STACK.md) section 15 lists the infrastructure, the release rules,
the post-release check, and the recovery path.

## Change the vehicle telemetry config

The vehicle accepts a config only when the Tesla application private key
signs it. Tesla's
[`tesla-http-proxy`](https://github.com/teslamotors/vehicle-command) signs
the request.

1. Edit `config/telemetry_config.json`.
2. Start `tesla-http-proxy` locally with the application private key.
3. Send `POST /api/1/vehicles/fleet_telemetry_config` through the proxy with
   the VIN and the config.
4. Read `GET /api/1/vehicles/{vin}/fleet_telemetry_config` until `synced` is
   `true`. The vehicle syncs when it is next awake.
5. Read `GET /api/1/vehicles/{vin}/fleet_telemetry_errors` if no data
   arrives.

Each signal that the vehicle sends counts against the Tesla usage credit.
Tesla removes the config when usage exceeds the billing limit.

## Verification

The project commands are mise tasks. Install
[mise](https://mise.jdx.dev/) and [rustup](https://rustup.rs/), then run:

```sh
mise trust
mise run setup
mise run verify
```

`mise run verify` is the single local gate. [STACK.md](STACK.md) section 3
lists the tasks and what each one checks.

## Contributing

This is a personal project. Work follows the contracts in this repository:

- Each change starts from a GitHub issue or a direct task.
- Each change uses a `feat|fix|chore|docs/<topic>` branch and a pull
  request. Nothing is pushed directly to `main`.
- Commits use [Conventional Commits](https://www.conventionalcommits.org/).
- Material changes get an independent review before merge.
- Pull requests merge with a merge commit.

[CLAUDE.md](CLAUDE.md) and [DOCTRINE.md](DOCTRINE.md) contain the full rules.

## License

No license is granted. All rights reserved.
