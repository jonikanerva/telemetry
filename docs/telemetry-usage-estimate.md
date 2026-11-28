# Telemetry usage estimate

This document estimates the Tesla usage and the database growth of
`config/telemetry_config.json` (201 fields). It states the billing rules,
the model, its assumptions, and what is not measured. ADR 0018 sets the
budget that the estimate is checked against.

## Result

| Month (profile below)                     | Signals | Usage of the config | Storage |
| ----------------------------------------- | ------: | ------------------: | ------: |
| Planning month                            | 499,765 |            3.33 USD |   56 MB |
| Road-trip month                           | 878,488 |            5.86 USD |   98 MB |
| Awake all month                           | 796,022 |            5.31 USD |   89 MB |
| Extreme: road trip, awake all other hours | 1,152,430 |          7.68 USD |  129 MB |
| Noise case of the extreme month (below)   | 1,408,990 |          9.39 USD |  157 MB |

These numbers are a bound under the assumptions of "Model", not a
maximum. The highest modelled month without the noise case is 7.68 USD.
The noise case of the extreme month is 9.39 USD. ADR 0018 counts the noise
case in the highest modelled month.

The owner's Home Assistant uses the same Tesla application. The owner read
its usage on the Tesla dashboard as about 1 to 2 EUR each month (owner
reading, 28 November 2026; not a measurement of this project). With at
most 2.5 USD for Home Assistant, the planning, road-trip, and awake months
stay inside the monthly discount of 10 USD. The extreme month is about
10.18 USD, and its noise case about 11.89 USD. Both are inside the agreed
paid overage (ADR 0018) and far below the billing limit of 20 EUR.

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
  so the config has no `prefer_typed`. The same changelog says that
  location fields support `minimum_delta` and that "Changes in distance
  are measured in meters".
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
| Driving dynamics: pedals, accelerations (Δ0.5 m/s²), torque (5 s), `GpsHeading` (10 s, Δ10°), motor current and axle speed (10 s), inverter battery voltage (30 s, Δ1 V), navigation estimates (60 s), `GpsAccuracyMeters` (300 s, Δ5 m), turn signals and high beams (1 s) | 25 | The largest cost line |

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

The model gives a bound under these assumptions. It is not a maximum:
a field can change in a state where the model counts 0 (see the noise
case).

- A continuously changing field sends 3600 / `interval_seconds` signals
  each hour in each state where the model expects it to change (table
  "State classes"). No saving from `minimum_delta` is counted.
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
| Driving dynamics (25)     |   8,192 |          60 |          60 |             0 |
| **Total (201)**           | **11,813** | **1,033** | **1,158** |       **513** |
| Noise case: added         |       0 |         612 |         612 |           372 |

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

Planning month: 11,813 × 30 + 1,033 × 52 + 1,158 × 0.5 + 513 × 60 +
201 × 300 = 354,390 + 53,716 + 579 + 30,780 + 60,300 = 499,765 signals,
or 3.33 USD.

Noise case: sensor noise changes `GpsHeading` and `GpsAccuracyMeters` at
each interval in every awake hour that is not driving (372 signals an
hour), and `DiVBatF` and `DiVBatR` at each interval while the car charges
(240 signals an hour). The case assumes that the `minimum_delta` of the
accelerations (0.5 m/s²) and of `DiVBat*` (1 V) is larger than the noise
of a car that stands still. GPS heading and accuracy can jump more than
their deltas when the car stands still, so the case counts them in full.
Extreme month: 1,152,430 + 612 × 46 + 372 × 614 = 1,408,990 signals, or
9.39 USD. Without the delta assumption (the accelerations and `DiVBat*`
also at each interval in every awake hour that is not driving), the
extreme month is 2,506,750 signals, or 16.71 USD.

Sensitivity:

- Connections: 150, 300, or 600 connections in the planning month give
  469,615, 499,765, or 560,065 signals.
- The driving-dynamics group is 69 % of the driving rate. At 10 s instead
  of 5 s for its 7 fields at 5 s, the planning month falls by 75,600
  signals (0.50 USD).
- Driving dominates: each extra hour of driving adds 11,813 signals
  (0.08 USD).

### State classes

The model uses these classes. States: D driving, A AC charging, C DC
charging, P awake and parked; "-" counts 0 in every state. An event field
counts 2 signals each hour in D, A, and C, and 0.5 in P, unless the row
states another rate. A static field counts 0. With these classes, the
formula of this section gives every number of this document.

| Kind | Interval (s) | Δ | States | Fields |
| ---- | -----------: | - | ------ | ------ |
| continuous | 60 | - | DACP | `Soc`, `BatteryLevel` |
| continuous | 30 | - | DACP | `EnergyRemaining` |
| continuous | 3600 | - | DACP | `NominalFullPackEnergyKwh`, `IsolationResistance`, `RollingResistanceCoefficient` |
| event, 10 an hour | 1 | - | D | `Gear` |
| continuous | 2 | - | D | `VehicleSpeed` |
| continuous | 5 | 20 | D | `Location` |
| continuous | 30 | - | D | `Odometer` |
| event, 2 an hour | 1 | - | AC | `DetailedChargeState` |
| event, 1 an hour | 1 | - | C | `FastChargerPresent` |
| continuous | 30 | - | A | `ACChargingPower` |
| continuous | 60 | - | A | `ACChargingEnergyIn` |
| continuous | 10 | - | C | `DCChargingPower` |
| continuous | 60 | - | AC | `DCChargingEnergyIn`, `ChargeRateMilePerHour` |
| continuous | 300 | - | DACP | `OutsideTemp`, `HvacFanSpeed`, `HvacFanStatus` |
| continuous | 300 | 0.5 | DACP | `InsideTemp`, `PackVoltage` |
| continuous | 600 | - | DACP | `TpmsPressureFl`, `TpmsPressureFr`, `TpmsPressureRl`, `TpmsPressureRr` |
| continuous | 120 | 0.5 | DACP | `ModuleTempMax`, `ModuleTempMin` |
| event | 1 | - | DACP | `BatteryHeaterOn`, `BMSState`, `ChargeLimitSoc`, `DriveRail`, `SentryMode`, `Version`, `SoftwareUpdateInProgress`, `LocatedAtHome`, `HvacPower`, `HvacACEnabled`, `HvacLeftTemperatureRequest`, `ClimateKeeperMode`, `CabinOverheatProtectionMode`, `DefrostForPreconditioning`, `PreconditioningEnabled`, `NotEnoughPowerToHeat`, `PowerTransferStatus`, `ChargeState`, `DCDCEnable`, `Hvil`, `SuperchargerSessionTripPlanner`, `DiStateF`, `DiStateR`, `CruiseSetSpeed`, `GpsState`, `AccessoryRail`, `HvacAutoMode`, `HvacRightTemperatureRequest`, `DefrostMode`, `CabinOverheatProtectionTemperatureLimit`, `RearDefrostEnabled`, `RearDisplayHvacEnabled`, `SeatHeaterLeft`, `SeatHeaterRight`, `SeatHeaterRearLeft`, `SeatHeaterRearRight`, `SeatHeaterRearCenter`, `AutoSeatClimateLeft`, `AutoSeatClimateRight`, `ClimateSeatCoolingFrontLeft`, `ClimateSeatCoolingFrontRight`, `SeatVentEnabled`, `HvacSteeringWheelHeatAuto`, `HvacSteeringWheelHeatLevel`, `WiperHeatEnabled`, `Locked`, `DoorState`, `FdWindow`, `FpWindow`, `RdWindow`, `RpWindow`, `CenterDisplay`, `DriverSeatOccupied`, `DriverSeatBelt`, `LightsHazardsActive`, `SoftwareUpdateAvailable`, `SoftwareUpdateExpectedDurationMinutes`, `SoftwareUpdateScheduledStartTime`, `SoftwareUpdateVersion`, `ServiceMode`, `GuestModeEnabled`, `GuestModeMobileAccessState`, `PinToDriveEnabled`, `ValetModeEnabled`, `SpeedLimitMode`, `CurrentLimitMph`, `RemoteStartEnabled`, `RemoteStartActive`, `PairedPhoneKeyAndKeyFobQty`, `VehicleName`, `LocatedAtWork`, `LocatedAtFavorite`, `HomelinkNearby`, `HomelinkDeviceCount`, `TpmsHardWarnings`, `TpmsSoftWarnings`, `AutomaticBlindSpotCamera`, `BlindSpotCollisionWarningChime`, `SpeedLimitWarning`, `ForwardCollisionWarning`, `LaneDepartureAvoidance`, `EmergencyLaneDepartureAvoidance`, `AutomaticEmergencyBrakingOff`, `AutomaticEmergencyBrakingState`, `CruiseFollowDistance`, `ChargePort` |
| event | 1 | - | ACP | `ChargerPhases`, `ChargeCurrentRequest`, `ChargeCurrentRequestMax`, `ChargeEnableRequest`, `ChargePortDoorOpen`, `ChargePortLatch`, `ChargingCableType`, `FastChargerType`, `BmsFullchargecomplete`, `ChargePortColdWeatherMode`, `ScheduledChargingMode`, `ScheduledChargingPending`, `ScheduledChargingStartTime`, `ScheduledDepartureTime` |
| continuous | 300 | 1 | DACP | `PackCurrent` |
| continuous | 300 | - | DP | `LifetimeEnergyUsed` |
| continuous | 300 | - | D | `LifetimeEnergyGainedRegen` |
| continuous | 900 | - | AC | `LifetimeEnergyChargedKwh`, `EstimatedHoursToChargeTermination` |
| continuous | 900 | - | C | `LifetimeDcChargeEnergyKwh` |
| continuous | 900 | - | DACP | `EstBatteryRange`, `RatedRange`, `BrickVoltageMax`, `BrickVoltageMin`, `IdealBatteryRange`, `BrickSocMinPercent`, `NumBrickVoltageMax`, `NumBrickVoltageMin`, `NumModuleTempMax`, `NumModuleTempMin` |
| continuous | 300 | 2 | ACP | `ChargerVoltage` |
| continuous | 60 | 0.5 | A | `ChargeAmps` |
| continuous | 300 | - | AC | `TimeToFullCharge` |
| continuous | 30 | 1 | D | `GradeEstimatePercent`, `DiVBatF`, `DiVBatR` |
| continuous | 300 | 1 | D | `DiStatorTempF`, `DiStatorTempR`, `DiInverterTF`, `DiInverterTR`, `DiHeatsinkTF`, `DiHeatsinkTR` |
| continuous | 600 | - | D | `TpmsLastSeenPressureTimeFl`, `TpmsLastSeenPressureTimeFr`, `TpmsLastSeenPressureTimeRl`, `TpmsLastSeenPressureTimeRr` |
| continuous | 60 | - | - | `SoftwareUpdateDownloadPercentComplete`, `SoftwareUpdateInstallationPercentComplete` |
| continuous | 60 | 1 | D | `MilesSinceReset`, `SelfDrivingMilesSinceReset` |
| continuous | 5 | - | D | `PedalPosition`, `BrakePedalPos`, `BrakePedal`, `DiTorqueActualF`, `DiTorqueActualR` |
| continuous | 5 | 0.5 | D | `LongitudinalAcceleration`, `LateralAcceleration` |
| continuous | 10 | 10 | D | `GpsHeading` |
| continuous | 10 | - | D | `DiMotorCurrentF`, `DiMotorCurrentR`, `DiAxleSpeedF`, `DiAxleSpeedR`, `DiTorquemotor`, `DiSlaveTorqueCmd` |
| continuous | 300 | 5 | D | `GpsAccuracyMeters` |
| continuous | 60 | - | D | `MilesToArrival`, `MinutesToArrival`, `ExpectedEnergyPercentAtTripArrival`, `RouteTrafficMinutesDelay`, `MaxSpeedToReachDestinationMph` |
| event, 60 an hour | 1 | - | D | `LightsTurnSignal` |
| event, 20 an hour | 1 | - | D | `LightsHighBeams` |
| static | 3600 | - | - | `CarType`, `Trim`, `ExteriorColor`, `RoofColor`, `WheelType`, `EfficiencyPackage`, `EuropeVehicle`, `RightHandDrive`, `RearSeatHeaters`, `SunroofInstalled`, `SettingDistanceUnit`, `SettingTemperatureUnit`, `Setting24HourTime`, `SettingTirePressureUnit`, `SettingChargeUnit` |

## Storage

One signal is one row of `raw.vehicle_signal`, 111.7 B (measured, ADR
0002, "Expected size"). The planning month adds about 56 MB. The Neon Free
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

Tesla's developer dashboard (`/dashboard/usage`) shows the billed usage
of the whole application, Home Assistant included. Only the owner can
read it. The owner read the Home Assistant share as about 1 to 2 EUR each
month.

`scripts/usage-measurement.sql` measures the usage of this product from
the stored records. Its statements give:

1. Signals and their cost for each UTC day.
2. Signals for each day and field number, the largest fields first.
3. The signals of the month to date and a projection to the full month.
   The projection is empty until 3 days of the month have signals.
4. Connections for each day: the count, the open connections, and the
   median and shortest length. Many short connections show as a high
   count with a short median. A connection is a connect event and the
   next disconnect event of the same connection ID, so a reused ID does
   not join two connections.
5. Connected time for each day. Overlapping connections count once. An
   open connection does not count.
6. The share of signals that the car marked as a resend.
7. The records in `raw.unprocessed_record` by reason.
8. The signals with the invalid marker, by field. A field that sends
   only the marker is a candidate to drop from the config.
9. The messages that the ingest process lost before the database
   (`records_dropped` and `zmq_shortfall`, not `gate_full`).
10. The size of `raw.vehicle_signal` against the 1 GB of Neon.
11. The estimated storage growth for each day.

The statements read only. They return counts, days, field numbers,
kinds, and sizes, and no VIN, no location, and no signal value. A
database test runs every statement of the file
(`crates/ingest/tests/storage/usage.rs`).

Privacy rule: a row for one day shows when the car was awake or driven,
so it shows when the owner was away. A public record (an issue, a PR,
or a document of this repository) gets only totals of the whole
measured period: signals, cost, connected hours, the number of
connections, the totals for each field, and sizes. It never gets a
value of a single day or a time of day. The rows of each day go to the
owner in the private report of the lead.

How to run:

- The lead runs the statements on the branch `production` with the
  owner role through the Neon tools. Each run is one read-only
  transaction: `BEGIN TRANSACTION READ ONLY`, one statement, then
  `ROLLBACK`. A wrong paste then cannot write.
- The repository has no SQL client (`psql` is not a project tool). The
  Neon SQL Editor of the branch `production` also works, with the same
  read-only transaction.
- The role `telemetry_ingest` can also run every statement: it has
  SELECT on each table of `raw`, and the size function needs no other
  privilege (tested). The web role cannot read every column that the
  statements use.
- Days are UTC days of `received_at`. The file states the reason.

Compare with the dashboard:

- One row of `raw.vehicle_signal` is one datum of a `V` record. Tesla
  bills one signal for each datum (`Record.SignalsCount()`, "Billing
  rules"). Statement 1 has the cost before the monthly discount.
- The rows can be fewer than the billed signals. The storage keeps an
  exact copy of a stored signal once, also a resend (ADR 0002,
  "Idempotency"); Tesla does not document whether it bills the copy. A
  datum with an unknown value variant, a datum with a conflicting value,
  and each datum of a record that does not decode have no row; statement
  7 counts these records. Statement 9 counts the messages that the
  ingest process lost; each lost `V` message held one or more billed
  signals.
- Alerts are billed signals, one for each alert, but they are rows of
  `raw.vehicle_alert`. The config has no `alert_types`, so the vehicle
  sends none.
- Connectivity events and error records are not signals.
- The dashboard also counts Home Assistant. Subtract its share before
  the comparison.
- When the config changed during the month, the projection of statement
  3 mixes two rates. Use the days of statement 1 after the change.

Record the totals of each measurement in issue #41, under the privacy
rule above. Put the measured totals in this document with the date of
the measurement.

Do not use the Prometheus signal counter of the server: in v0.10.1,
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
- The model counts the driving-dynamics fields at 0 outside driving. The
  noise case above counts the GPS fields in every awake hour; without
  the `minimum_delta` assumption of that case, the extreme month is
  16.71 USD, or about 19.2 USD with Home Assistant. That is still below
  the billing limit of 20 EUR in both readings of the limit.
- The model counts each event field at 2 signals an hour. An event field
  that in fact changes at each interval of 1 s can send up to 3,600
  signals an hour. The README send procedure checks the dashboard usage
  two days after the send.
- No source states an effect of the field set on sleep. The README send
  procedure compares the awake hours before and after a config change.
