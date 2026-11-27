# Product Vision

> Draft for owner review. The owner approves this file with the adoption PR.
> This file owns product direction, not technology.

## Vision

The owner understands how their Tesla uses energy over time. The car's own
telemetry becomes a private history that the owner can explore.

## Goal

Collect the telemetry of one vehicle continuously. Turn it into a history of
drives, charging sessions, and parked periods. Show that history as charts.
Show a live dashboard while the car is awake.

## Audience and environment

- One user: the owner. One vehicle: a 2025 Tesla Model Y Long Range AWD.
- The owner uses a browser on a desktop computer and on a phone.
- The product runs on the owner's existing infrastructure: Railway (Hobby
  plan), Neon (Free plan), and Cloudflare (Free plan). The domain is
  `calmdonut.com`.
- The owner's Home Assistant uses the same Tesla developer application. Both
  share one Tesla usage credit of 10 USD per month and one billing limit.
- Tesla removes the telemetry configuration from the vehicle when usage
  exceeds the billing limit. Tesla does not restore the configuration.
- The vehicle sends data only when it is awake and online.

## Core Principles

- **History first.** A lost hour of data cannot be collected again. Data
  collection must continue when the web application fails or deploys.
- **Keep the source meaning.** No data is not zero. A gap stays a gap. Store
  the value and unit that the vehicle sent. Convert units for display.
- **Private by default.** The data shows where the owner is and when the
  owner is away. Nothing is public.
- **Stay in the free tiers.** The product adds no paid service. Field
  selection and write patterns respect the Tesla, Neon, and Railway limits.
- **Low operations.** The owner does not maintain the product weekly.
  Certificates, configuration, and retention work without routine manual steps.

## Product Shape

The vehicle streams signals to the ingest service. The application stores the
signals and derives three kinds of period:

| Period | Starts and ends                          | What the owner sees                                             |
| ------ | ---------------------------------------- | --------------------------------------------------------------- |
| Drive  | Gear leaves P until gear returns to P    | Route, distance, duration, energy used, Wh/km, temperatures     |
| Charge | Charging starts until charging ends      | Place, energy added, power, start and end charge level, cost    |
| Parked | End of a drive or charge until the next  | Charge level loss, duration, outside temperature                |

Views, in priority order:

1. Battery: charge level over time, capacity trend, and loss while parked.
2. Consumption: Wh/km against temperature and speed.
3. Charging sessions, with cost where a price source is available.
4. Drives, with the route on a map.
5. Live dashboard: the current values while the car is awake.

States that every view handles: loading, data, no data yet, a gap in the
data, the car is asleep, the data is stale, and the service is unavailable.
A failed request must not look like an empty result.

## Non-Goals & Drift Guardrails

- No vehicle commands. The product reads data only.
- No Home Assistant integration. Home Assistant keeps its own polling.
- No accounts, no sharing, and no second vehicle.
- No public service and no general product for other Tesla owners.
- No new paid infrastructure and no new external provider without owner
  approval.

Signs of drift: a feature that needs the `vehicle_cmds` scope, a second user,
a public URL without access control, or a monthly cost.

## Decision Filter

- Does the change protect or improve the completeness of the history?
- Does the change keep location data private?
- Does the change stay inside the free tiers and the shared Tesla credit?
- Does the change answer a question about battery, consumption, charging, or
  driving that the owner wants to follow over time?
- Does the change add routine manual work for the owner?

## Success Definition

- The history of drives, charges, and parked periods has no gaps except when
  the car was asleep or offline. Each gap is visible as a gap.
- The owner can see battery, consumption, and charging trends over months.
- The live dashboard shows new values within a few seconds while driving.
- The monthly Tesla, Neon, and Railway usage stays inside the free allowances.
- The car sleeps as it did before telemetry was enabled.

## Persistence and Privacy Posture

| Data or capability               | Purpose                               | Storage or recipient                  | Retention or expiry                           |
| -------------------------------- | ------------------------------------- | ------------------------------------- | --------------------------------------------- |
| Raw signals, including location  | Source for periods and charts         | Neon Postgres                         | Open question; assumption: 90 days            |
| Drives, charges, parked periods  | Long-term history                     | Neon Postgres                         | No expiry                                     |
| Vehicle identification number    | Identifies the data source            | Neon Postgres                         | No expiry                                     |
| Tesla application private key    | Signs the vehicle telemetry config    | Owner's computer only                 | Kept while the product is in use              |
| Ingest TLS certificate and key   | Server identity for the vehicle       | Railway service variables             | Certificate expires in 2041; CA root in 2029  |
| Tesla OAuth access token         | Sends the telemetry config to Tesla   | Owner's computer only, `.env`         | About 8 hours                                 |
| Database credentials of `telemetry_ingest` and `telemetry_migrator` | Let the ingest service write records and apply migrations | Railway service variables | Kept while the product is in use; rotate after a suspected leak |
| Operational records (`raw.ingest_event`): starts, stops, gate changes, child exits, drops, shortfalls | Make gaps and restarts visible | Neon Postgres | Open question; proposal: no expiry |

- Forbidden data flows: location, VIN, or signal data to any party other
  than Neon and Railway without owner approval. No secret in the repository,
  in an image, or in the browser bundle.
- Telemetry and diagnostics: none to external services. Application logs
  must not contain the VIN, location, or signal values.
- The ingest service no longer writes records to the Railway log.
  `fleet-telemetry` still writes a VIN on some error paths; `ingest`
  replaces each VIN-shaped token with `[vin]` before the log line. The
  Railway log of earlier deployments still holds records with the VIN and
  the location until Railway removes them.
- Approved deletion policies: none yet. The raw signal retention rule
  becomes one when the owner approves it.

## Open Questions

Owner decisions:

- Access control for the web application. Proposal: Cloudflare Access.
- Map provider for the drive view. A tile provider sees the viewed locations.
- Retention period for raw signals.
- Price source for charging cost. Candidate: the owner's `spot-price` service.
- Whether Codex also works in this repository (`AGENTS.md`).

Delegated technical choices, recorded as ADRs when significant:

- How signals move from the ingest service to the application without loss
  during an application deployment.
