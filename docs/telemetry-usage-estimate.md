# Telemetry usage estimate

This document estimates the Tesla usage and the database growth of
`config/telemetry_config.json` (201 fields). It states the billing rules,
the model, its assumptions, and what is not measured. ADR 0018 sets the
budget that the estimate is checked against.

## Result

| Month (profile below)                     | Signals | Usage of the config | Storage |
| ----------------------------------------- | ------: | ------------------: | ------: |
| Planning month                            | 512,005 |            3.41 USD |   57 MB |
| Road-trip month                           | 902,968 |            6.02 USD |  101 MB |
| Awake all month                           | 808,262 |            5.39 USD |   90 MB |
| Extreme: road trip, awake all other hours | 1,176,910 |          7.85 USD |  132 MB |

The highest modelled month is 7.85 USD. That is below the bound of 8 USD
of ADR 0018.

The owner's Home Assistant uses the same Tesla application. The owner read
its usage on the Tesla dashboard as about 1 to 2 EUR each month (owner
reading, 28 November 2026; not a measurement of this project). With at
most 2.5 USD for Home Assistant, the planning, road-trip, and awake months
stay inside the monthly discount of 10 USD. The extreme month is about
10.35 USD, which is inside the agreed paid overage (ADR 0018).

## Billing rules

Quotes from the Tesla developer pages, read on 28 November 2026:

- Price (<https://developer.tesla.com/>): "Streaming Signals … 150,000
  signals/ $1". Commands cost "1,000 requests/ $1", data requests
  "500 requests/ $1", and wakes "50 requests/ $1".
- Discount (<https://developer.tesla.com/docs/fleet-api/billing-and-limits>):
  "A monthly discount of $10 is provided to support individual
  developers/small applications." "Billing cycles are monthly and begin on
  the first day of the month."
- Billing limit (same page): "By default, each account has a billing limit
  of 0. This limit can be increased once a payment method is added to the
  account." "An email notification will be sent when 80% of the billing
  limit has been reached." "Note that actual usage may be slightly higher
  than the billing limit, but pre-tax bills will be capped at the limit."
- At the limit (same page): "If the billing limit is exceeded, API usage
  will be suspended and any vehicles configured for Fleet Telemetry will
  have their streaming configurations removed. Access will be re-enabled
  once the billing limit is raised or a new billing cycle begins."
  "Fleet Telemetry configurations will not be restored." Home Assistant
  stops too, because it uses the same application.
- The page does not say whether the limit counts the usage before or after
  the discount. The dashboard shows the usage against the limit.
- Send rule (<https://developer.tesla.com/docs/fleet-api/fleet-telemetry>,
  "System Behavior"): "The event collector gathers data in 500 millisecond
  buckets." A field is sent when "interval_seconds has elapsed since the
  field was last emitted and the field's value has changed". A value that
  does not change costs nothing.
- `minimum_delta` (<https://developer.tesla.com/docs/fleet-api/announcements>,
  2025-01-09): "The amount a field's value must change before it is
  considered changed." "Only numeric fields are supported." The same
  announcement says that the option requires `prefer_typed`; the client
  changelog of the Fleet Telemetry page says that client 1.1.0 removed
  `prefer_typed` and returns all data typed. The vehicle has client 1.3.0,
  so the config has no `prefer_typed`. Location fields use meters.
- `resend_interval_seconds` (same announcement): "Resend a field's value
  every x seconds, even if its value has not changed." The config does not
  use it, because a resend of an unchanged value costs a signal.
- One signal is one field value in one vehicle message. The upstream server
  counts signals the same way: `Record.SignalsCount()` returns the number of
  data items in a `V` message and the number of alerts in an `alerts`
  message. The config has no `alert_types`.
- The location fields need the location scope. The Fleet Telemetry page
  names `Location`, `GpsState`, `GpsHeading`, and `GpsAccuracyMeters` of
  this config in that list. The application has the scope, because
  `Location` streams.

## Field set

Each field name is in the `Field` enum of the vendored
`crates/ingest/proto/vehicle_data.proto`. The groups below explain the
intervals. The config file is the complete list.

| Group | Fields | Intervals and `minimum_delta` |
| ----- | -----: | ----------------------------- |
| Charge level, energy, drive, charging power, temperatures, tyre pressures | 20 | Table "Base fields" below |
| Battery and charging detail: module temperatures (120 s, Δ0.5), pack voltage and current (300 s), brick voltages, ranges, lifetime energy counters (300 to 900 s), `ChargerVoltage` (300 s, Δ2), `ChargeAmps` (60 s), `GradeEstimatePercent` (30 s, Δ1), charging and climate events (1 s), `SentryMode`, `Version`, `LocatedAtHome` | 45 | Events at 1 s: they change only at an event |
| States and settings: seats, windows, locks, doors, display, safety settings, software update, guest and valet mode, drive-unit temperatures (300 s, Δ1), `LocatedAtWork`, `LocatedAtFavorite`, `Homelink*`, vehicle configuration (3600 s) | 111 | Events at 1 s; slow values at 300 to 3600 s |
| Driving dynamics: pedals, accelerations, `GpsHeading` (Δ5), torque (5 s), motor current and axle speed (10 s), inverter voltage (30 s), navigation estimates and `GpsAccuracyMeters` (60 s), turn signals and high beams (1 s) | 25 | The largest cost line |

### Base fields

The unit comments of `crates/domain` cite this table.

| Field                      | Interval (s) | Minimum delta | Unit (from Tesla) | Use                                    |
| -------------------------- | -----------: | ------------- | ----------------- | -------------------------------------- |
| `Soc`                      |           60 | default       | % usable          | Battery, charging, parked loss         |
| `BatteryLevel`             |           60 | default       | % total           | Charge level (`battery_level`)         |
| `EnergyRemaining`          |           30 | default       | kWh               | Consumption, parked loss               |
| `NominalFullPackEnergyKwh` |         3600 | default       | kWh               | Capacity trend                         |
| `Gear`                     |            1 | not numeric   | enum              | Drive start and end                    |
| `VehicleSpeed`             |            2 | default       | mph               | Consumption by speed                   |
| `Location`                 |            5 | 20 m          | latitude, longitude | Route, charge place                  |
| `Odometer`                 |           30 | default (0.1) | miles             | Distance, Wh/km                        |
| `DetailedChargeState`      |            1 | not numeric   | enum              | Charge start and end                   |
| `FastChargerPresent`       |            1 | not numeric   | boolean           | AC or DC                               |
| `ACChargingPower`          |           30 | default       | not stated        | Charging power                         |
| `ACChargingEnergyIn`       |           60 | default       | kWh, charger side | Energy from the grid (AC)              |
| `DCChargingPower`          |           10 | default       | kW                | DC power curve                         |
| `DCChargingEnergyIn`       |           60 | default       | kWh, battery side | Energy added (AC and DC)               |
| `OutsideTemp`              |          300 | default       | not stated        | Consumption by temperature, parked     |
| `InsideTemp`               |          300 | 0.5           | °C                | Live dashboard                         |
| `TpmsPressureFl`, `Fr`, `Rl`, `Rr` |  600 | default       | bar               | Tyre pressures                         |

### Fields that the config leaves out

| Fields | Reason |
| ------ | ------ |
| 18 Semi fields (`Semi*`, `Semitruck*`, `Cabin12vPortKeepOn`, `Cabin48vPortKeepOn`, `LifetimeEnergyUsedDrive`), 3 `Tonneau*` | Tesla marks them as Semi or Cybertruck fields |
| `OffroadLightbarPresent` | An accessory of another vehicle type (assumption, not a Tesla note) |
| 16 rear-left and rear-right drive-unit fields (`*REL`, `*RER`), 5 `Powershare*` | Not in a two-motor Model Y (assumption, not a Tesla note) |
| 15 `Experimental_*`, `Deprecated_1` to `Deprecated_3` | No documented meaning |
| `RouteLastUpdated` | Tesla: "This field is broken and does not return data." |
| `PassengerSeatBelt` | Tesla: "This field improperly reports if the 2nd row center seat belt is buckled." |
| `SelfDrivingSoftwareVersion` | Needs firmware 2026.44.3 (proto comment); the vehicle has 2026.39.100 |
| `DestinationLocation`, `DestinationName`, `OriginLocation`, `RouteLine` | Privacy: planned places and routes, beyond the visited places of `Location` |
| 11 `Media*` fields | Privacy: listening habits; no view uses them |

## Model

The model counts the maximum number of signals:

- A continuously changing field sends 3600 / `interval_seconds` signals
  each hour in each state where it can change. No saving from
  `minimum_delta` is counted.
- An event field sends 2 signals each hour while the car drives or
  charges, and 0.5 each hour while it is parked and awake (assumption).
  `Gear` counts 10 each hour of driving, the turn signals 60, and the high
  beams 20.
- A vehicle configuration field (3600 s) counts 0 each hour.
- Each connection sends every field once: 201 signals. Tesla documents
  that the vehicle sends fields "on vehicle startup"; a full send at each
  connection is an assumption.
- Monthly signals = Σ (signals each hour of the state × hours of the
  state) + 201 × connections. Usage = signals / 150,000 USD.

Signals each hour of a state:

| Group                     | Driving | AC charging | DC charging | Awake, parked |
| ------------------------- | ------: | ----------: | ----------: | ------------: |
| Base fields (20)          |   2,939 |         531 |         712 |           289 |
| Battery and charging (45) |     274 |         246 |         190 |         138.5 |
| States and settings (111) |     408 |         196 |         196 |          85.5 |
| Driving dynamics (25)     |   8,600 |          60 |          60 |             0 |
| **Total (201)**           | **12,221** | **1,033** | **1,158** |       **513** |

Profiles (hours each month; assumptions, not measurements):

| Profile                  | Driving | AC charging | DC charging | Awake, parked | Connections |
| ------------------------ | ------: | ----------: | ----------: | ------------: | ----------: |
| Planning month           |      30 |          52 |         0.5 |            60 |         300 |
| Road-trip month          |      60 |          40 |           6 |            80 |         400 |
| Awake all month          |      30 |          52 |         0.5 |         637.5 |         300 |
| Extreme                  |      60 |          40 |           6 |           614 |         400 |

The planning month assumes about 1 hour of driving each day, two home
charges of 6 hours each week, and one fast charge. "Awake all month" and
"Extreme" fill every other hour of a 30-day month with awake, parked time,
for example with Sentry Mode on.

Planning month: 12,221 × 30 + 1,033 × 52 + 1,158 × 0.5 + 513 × 60 +
201 × 300 = 366,630 + 53,716 + 579 + 30,780 + 60,300 = 512,005 signals,
or 3.41 USD.

Sensitivity:

- Connections: 150, 300, or 600 connections in the planning month give
  481,855, 512,005, or 572,305 signals.
- The driving-dynamics group is 70 % of the driving rate. At 10 s instead
  of 5 s for its 8 fields at 5 s, the planning month falls by 86,400
  signals (0.58 USD).
- Driving dominates: each extra hour of driving adds 12,221 signals
  (0.08 USD).

## Storage

One signal is one row of `raw.vehicle_signal`, 111.7 B (measured, ADR
0002, "Expected size"). The planning month adds about 57 MB. The Neon Free
plan has 1 GB, so the database fills in about 17 months at the planning
rate, less the data already stored, and in about 11 months when the car is
awake all the time. The raw-signal retention period (`VISION.md`, "Open
Questions") must be decided before then.

## Observed usage

A test config with 8 fields and `alert_types: ["service"]` had one
connection of 1323 seconds while the car was parked and charging. The
`record_payload` lines of the server log show 16 `V` records with 19
signals and 2 alert records with 6 alerts: 25 signals in 22 minutes, about
68 each hour. `Soc` sent 13 values in 22 minutes against a maximum of 22.
The `V: 2789` and `alerts: 1094` of the `socket_disconnected` line are
bytes, not signals: `ReportMetricBytesPerRecords` of `fleet-telemetry`
v0.10.1 adds `record.Length()`, the payload length. The real usage is
therefore expected to be below the model, but no discount is measured.

## Measure the usage

- Tesla's developer dashboard (`/dashboard/usage`) shows the billed usage
  of the whole application, Home Assistant included. Only the owner can
  read it.
- The stored rows in `raw.vehicle_signal` give the signal count of this
  product. A procedure that counts them does not exist yet (issue #41).
- Do not use the Prometheus signal counter of the server: in v0.10.1,
  `trackSignalUsage` runs only for messages that exceed the server rate
  limit.

## Limits of this estimate

- No drive was measured with this config. All hours are assumptions.
- Tesla does not document how many fields the vehicle sends again after a
  reconnect.
- The Home Assistant share is an owner reading of the dashboard.
- Five fields (`AccessoryRail`, `PowerTransferStatus`,
  `AutomaticEmergencyBrakingState`, `RollingResistanceCoefficient`,
  `LifetimeDcChargeEnergyKwh`) need firmware 2026.32.300 or 2026.38.6
  (proto comment). The vehicle has 2026.39.100; that these versions
  include the fields is an assumption.
- The Tesla pages do not say what the vehicle does with a field that it
  does not support.
- The model counts each event field at 2 signals an hour. An event field
  that in fact changes at each interval of 1 s can send up to 3,600
  signals an hour. The README send procedure checks the dashboard usage
  two days after the send.
- No source states an effect of the field set on sleep. The README send
  procedure compares the awake hours before and after a config change.
