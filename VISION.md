# Product Vision

> A change of this file needs the owner's approval.
> This file owns product direction, not technology.

## Vision

The owner understands how their Tesla uses energy over time. The car's own
telemetry becomes a private history that the owner can explore.

## Goal

Collect the telemetry of one vehicle continuously. Turn it into a history of
drives, charging sessions, and parked periods. Show that history as charts.
A live dashboard while the car is awake is optional.

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
  Accepted loss budget: a crash, a
  deploy, or a Neon outage can lose up to 15 minutes of queued data. Each
  such loss stays visible as a gap.
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
4. Drives, with the route as a line.
5. Live dashboard: the current values while the car is awake. Optional, a
   later view.

Charging cost: the price source is the
owner's `spot-price` service, for charging at home. The cost of a session
is the energy of each interval times the price of that interval, plus the
fixed parts from configuration: transfer, taxes, and margin. At other
places the cost is unknown. The view shows an unknown cost as unknown,
never as zero. The product does not use the Tesla charging history,
because it spends the shared Tesla credit.

Drive route: at first the drive view
has no background map. The view draws the route as a line on an empty
background, with the start, the end, the distance, and the duration. No
map tile provider gets a request. The owner selects a map provider when
the drive view is built (Open Questions).

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
  the car was asleep or offline, or a loss within the accepted budget of 15
  minutes. Each gap is visible as a gap.
- The owner can see battery, consumption, and charging trends over months.
- The views can be up to 15 minutes behind the vehicle.
- The monthly Tesla, Neon, and Railway usage stays inside the free allowances.
- The car sleeps as it did before telemetry was enabled.

## Persistence and Privacy Posture

| Data or capability               | Purpose                               | Storage or recipient                  | Retention or expiry                           |
| -------------------------------- | ------------------------------------- | ------------------------------------- | --------------------------------------------- |
| Raw signals, including location  | Source for periods and charts         | Neon Postgres                         | No deletion until the derived periods exist; the owner sets the period in the web phase |
| Drives, charges, parked periods  | Long-term history                     | Neon Postgres                         | No expiry                                     |
| Vehicle identification number    | Identifies the data source            | Neon Postgres                         | No expiry                                     |
| Tesla application private key    | Signs the vehicle telemetry config    | Owner's computer only                 | Kept while the product is in use              |
| Ingest TLS certificate and key   | Server identity for the vehicle       | Railway service variables             | Certificate expires in 2041; CA root in 2029  |
| Tesla OAuth access token         | Sends the telemetry config to Tesla   | Owner's computer only, `.env`         | About 8 hours                                 |
| Database credentials of `telemetry_ingest` and `telemetry_migrator` | Let the ingest service write records and apply migrations | Railway service variables | Kept while the product is in use; rotate after a suspected leak |
| Operational records (`raw.ingest_event`): starts, stops, gate changes, child exits, drops, shortfalls | Make gaps and restarts visible | Neon Postgres | No expiry |
| Argon2id hash of the web password | Checks the owner's password at login | Railway service variables of the web service; the owner creates the hash on the owner's computer | Until the owner changes the password |
| Login sessions of the web application | Keep the owner signed in after a login | Session cookie in the owner's browser; where the server keeps session data is a technical choice of the first web PR | Session expiry or logout |
| Time range of a charge at home | Read the electricity price for the charging cost | The owner's `spot-price` service; the request holds no location and no VIN | Not checked yet (the owner's service) |

- Forbidden data flows: location, VIN, or signal data to any party other
  than Neon and Railway without owner approval. No secret in the repository,
  in an image, or in the browser bundle. Allowed flow: the request to the
  owner's `spot-price` service holds a time range only, with no location
  and no VIN.
- Access control (ADR 0003): one
  password protects the web application. The owner uses a long random
  password from a password manager. There is no second factor. The
  application stores no password, only a hash. A login gives a session
  with an expiry, and the owner can log out. The server slows down
  repeated login attempts. The login page is reachable from the internet.
  Cloudflare Access is not used, because it needs a payment method on
  the Cloudflare account, also for the free plan. It can go in front of
  the application later without an application change.
- Agent review: the repository has no
  `AGENTS.md`. Claude Code is the only agent with a contract in the
  repository. The lead may ask Codex for a read-only second review of a
  single security-critical PR. The task text of the lead states the rules of that
  review. The review sends the code of the PR to OpenAI. The repository is
  public. Codex gets no secret and no production data.
- Telemetry and diagnostics: none to external services. Application logs
  must not contain the VIN, location, or signal values.
- The ingest service writes no record to the Railway log.
  `fleet-telemetry` writes a VIN on some error paths; `ingest` replaces
  each VIN-shaped token with `[vin]` before the log line. The Railway log
  of the deployments before the image of ADR 0001 holds records with the
  VIN and the location until Railway removes them.
- Approved deletion policies: none. No raw signal is deleted until the
  derived periods exist. The raw signal retention rule becomes a deletion policy when the owner
  approves it in the web phase.

## Open Questions

Owner decisions:

- Map provider for the drive view. A tile provider sees the viewed locations.
  Current direction: no background map (Product Shape).
  The owner selects a provider when the drive view is built. Candidates: a
  self-hosted map file (Protomaps, PMTiles) or a hosted tile service based
  on OpenStreetMap.
- Retention period for raw signals. The owner decides it in the web
  phase, with the measured growth. If the first week of data shows that
  the 1 GB of Neon fills before the web phase, the lead reports it to the
  owner at once.

Delegated technical choices, recorded as ADRs when significant:

- How signals move from the ingest service to the application without loss
  during an application deployment.
- The interface of the `spot-price` service, and whether it keeps a price
  history. Not checked yet. The lead checks both when the charging view is
  built, and reports to the owner if the history is missing.
- Where the server keeps the session data of the web application
  (ADR 0003).
