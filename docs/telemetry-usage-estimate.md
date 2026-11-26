# Telemetry usage estimate

This document estimates the Tesla usage of `config/telemetry_config.json`.
It also explains the usage that the earlier test config showed, and it
tells how to measure the usage again after the new config is on the vehicle.

Sources were read on 26 November 2026.

## Result

| Case                                                        | Signals each month | Cost each month |
| ----------------------------------------------------------- | -----------------: | --------------: |
| Planning estimate (assumptions below, every field at its maximum rate) |            134,886 |        0.90 USD |
| Worst case: the car is awake for all other hours of the month |            304,529 |        2.03 USD |
| Owner limit for this product                                 |            450,000 |        3.00 USD |

Both cases stay below the limit of 3 USD. The worst case leaves at least
7.97 USD of the 10 USD monthly credit for Home Assistant.

## Billing rules

- Tesla bills streaming signals at 150,000 signals for 1 USD. The Fleet
  Telemetry documentation gives the same rate: 15 signals each minute cost
  0.0001 USD.
- One signal is one field value in one vehicle message. The upstream server
  counts signals the same way: `Record.SignalsCount()` returns the number of
  data items in a `V` message and the number of alerts in an `alerts`
  message.
- The vehicle sends a field only when the value changed and
  `interval_seconds` elapsed since the last send of that field. A value that
  does not change costs nothing.
- `minimum_delta` sets how much a value must change before the vehicle sends
  it. For `Location`, the unit is meters. Tesla documents `minimum_delta`
  for `Location`, `Odometer` (default 0.1 mile), `InsideTemp`,
  `ChargerVoltage`, and `SelfDrivingMilesSinceReset`. The config uses it only
  for `Location` and `InsideTemp`.
- The vehicle sends all configured fields shortly after start-up.
- Tesla documents no `resend_interval_seconds` option on the Fleet Telemetry
  pages. The config does not use it.
- The config has no `alert_types`. Alerts count as signals in the upstream
  usage metrics, and the product does not need them.

## Field set

Each field name is in Tesla's `protos/vehicle_data.proto` (the `main` branch
and tag `v0.10.1` are identical) and in the Fleet Telemetry available-data
page. No field is marked as Semi-only or Cybertruck-only.
`NominalFullPackEnergyKwh` needs vehicle client 1.3.0 (firmware 2026.32).
The vehicle has client 1.3.0 and firmware 2026.39.100.

| Field                      | Interval (s) | Minimum delta | Unit (from Tesla) | View                                   |
| -------------------------- | -----------: | ------------- | ----------------- | -------------------------------------- |
| `Soc`                      |           60 | default       | % usable          | Battery, charging, parked loss         |
| `BatteryLevel`             |           60 | default       | % total           | Charge level (`battery_level`)         |
| `EnergyRemaining`          |           30 | default       | kWh               | Consumption, parked loss               |
| `NominalFullPackEnergyKwh` |         3600 | default       | kWh               | Capacity trend                         |
| `Gear`                     |            1 | not numeric   | enum              | Drive start and end                    |
| `VehicleSpeed`             |            2 | default       | mph               | Live dashboard, consumption by speed   |
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
| `TpmsPressureFl`           |          600 | default       | bar               | Tyre pressures                         |
| `TpmsPressureFr`           |          600 | default       | bar               | Tyre pressures                         |
| `TpmsPressureRl`           |          600 | default       | bar               | Tyre pressures                         |
| `TpmsPressureRr`           |          600 | default       | bar               | Tyre pressures                         |

Reasons for some choices:

- `VehicleSpeed` uses 2 seconds. `VISION.md` asks for new values within a
  few seconds on the live dashboard. Speed is the largest cost line.
- `Location` uses 5 seconds and 20 m. At 100 km/h, the route has a point
  every 139 m. The 20 m delta stops GPS noise from sending points while the
  car is parked.
- `Soc` is the usable state of charge (Tesla equivalent
  `charge_state.usable_battery_level`). `BatteryLevel` is the state of
  charge of the total capacity (equivalent `charge_state.battery_level`).
  Both cost little and keep the source meaning.
- `DCChargingEnergyIn` is measured at the battery for AC and DC charging.
  `ACChargingEnergyIn` is measured at the charger and is the energy that the
  owner pays for at home.
- `Gear`, `DetailedChargeState`, and `FastChargerPresent` change only at
  events. A short interval gives exact period boundaries at almost no cost.

## Assumptions

These hours are assumptions, not measurements. Measure them again after
deployment (see the last section).

| State                       | Hours each month | Basis of the assumption                          |
| --------------------------- | ---------------: | ------------------------------------------------ |
| Driving                     |               30 | About 1 hour each day                            |
| AC charging                 |               40 | About 12 home charges of 3 to 4 hours            |
| DC charging                 |                3 | About 6 fast charges of 30 minutes               |
| Other awake time, parked    |               60 | Climate, app use, wake-ups, time before sleep    |
| Connections (start-up sends)|   300 each month | About 10 wake-ups each day, 20 fields each       |

## Signals for each field in each state

The numbers are the maximum signals in one hour: 3600 divided by the
interval, when the value changes at every interval. A field that cannot
change in a state has 0. `Gear`, `DetailedChargeState`, and
`FastChargerPresent` have an assumed number of events. The tyre pressures
count at the maximum rate in every state, also when the car is parked.

| Field                      | Driving | AC charging | DC charging | Awake, parked |
| -------------------------- | ------: | ----------: | ----------: | ------------: |
| `Soc`                      |      60 |          60 |          60 |            60 |
| `BatteryLevel`             |      60 |          60 |          60 |            60 |
| `EnergyRemaining`          |     120 |         120 |         120 |           120 |
| `NominalFullPackEnergyKwh` |       1 |           1 |           1 |             1 |
| `Gear` (assumed events)    |      10 |           0 |           0 |             0 |
| `VehicleSpeed`             |    1800 |           0 |           0 |             0 |
| `Location`                 |     720 |           0 |           0 |             0 |
| `Odometer`                 |     120 |           0 |           0 |             0 |
| `DetailedChargeState` (assumed events) | 0 |       2 |           2 |             0 |
| `FastChargerPresent` (assumed events)  | 0 |       0 |           1 |             0 |
| `ACChargingPower`          |       0 |         120 |           0 |             0 |
| `ACChargingEnergyIn`       |       0 |          60 |           0 |             0 |
| `DCChargingPower`          |       0 |           0 |         360 |             0 |
| `DCChargingEnergyIn`       |       0 |          60 |          60 |             0 |
| `OutsideTemp`              |      12 |          12 |          12 |            12 |
| `InsideTemp`               |      12 |          12 |          12 |            12 |
| 4 × `TpmsPressure*`        |      24 |          24 |          24 |            24 |
| **Total each hour**        | **2939** |    **531** |     **712** |       **289** |

## Monthly total

| Line                    | Calculation    | Signals |
| ----------------------- | -------------- | ------: |
| Driving                 | 2939 × 30 h    |  88,170 |
| AC charging             | 531 × 40 h     |  21,240 |
| DC charging             | 712 × 3 h      |   2,136 |
| Other awake time        | 289 × 60 h     |  17,340 |
| Start-up sends          | 20 × 300       |   6,000 |
| **Planning estimate**   |                | **134,886** |
| **Cost**                | 134,886 / 150,000 | **0.90 USD** |

Worst case: the car is awake and parked for all other hours of a 30-day
month, for example with Sentry Mode on. That is 720 − 30 − 40 − 3 = 647
hours at 289 signals: 186,983 signals. The total is then 304,529 signals,
or 2.03 USD.

Driving is the largest line. With the other lines at the planning values,
the total reaches 3 USD at about 137 hours of driving each month.

The real usage is expected to be lower. Speed and location do not change
while the car stands still in traffic. `Soc` and `EnergyRemaining` change
slowly while the car is parked.

## Observed usage of the test config

The test config had 8 fields and `alert_types: ["service"]`. The server log
showed one connection of 1323 seconds while the car was parked and charging.
Its `socket_disconnected` line had `V: 2789`, `alerts: 1094`, and
`errors: 86`.

These numbers are bytes, not signals. In `server/streaming/socket.go` of
`fleet-telemetry` v0.10.1, `ReportMetricBytesPerRecords` adds
`record.Length()` to the counter of the record type. `record.Length()` is the
length of the record payload. With `transmit_decoded_records: true`, the
payload is the JSON form of the record.

The `record_payload` lines of the same connection give the signal count:

| Record type | Records | Signals | Bytes in the counter |
| ----------- | ------: | ------: | -------------------: |
| `V`         |      16 |      19 |                 2789 |
| `alerts`    |       2 |       6 |                 1094 |
| `errors`    |       1 |       0 |                   86 |

- The 16 `V` records hold 13 `Soc`, 3 `InsideTemp`, 2
  `DetailedChargeState`, and 1 `OutsideTemp` values. Each record also
  carries the VIN and `CreatedAt`. That is about 174 bytes for each record.
  The `data` objects of the same records in the log are 1781 bytes. The
  counter measures `record.toJSON()`, which is the protobuf JSON encoding
  (`jsonOptions.Marshal`) of the full message. The log line shows the data
  in a different form. This estimate does not analyse the exact byte
  difference.
- Each alert record holds 3 alerts and is 547 bytes in the counter. The
  first short connection had 1 alert record and `alerts: 547`.
  2 × 547 = 1094.
- The real rate was 25 signals in 22 minutes, about 68 signals each hour.
  The cost of that connection was about 0.0002 USD.
- `Soc` sent 13 values in 22 minutes. The maximum at 60 seconds is 22. The
  value did not change in every minute.

The test config was not expensive. The byte counters only looked like signal
counts.

## Measure the usage again after deployment

Tesla's developer dashboard shows the billed usage. Only the owner can read
it. The server log gives an independent count.

The log contains the VIN and the location. Keep the log file outside the
repository and delete it after the count. Do not copy values from it.

1. Find the deployment that ran during the period. Add
   `--project <project-id>` when the directory is not linked to the Railway
   project.

   ```sh
   railway deployment list --service telemetry --environment production
   ```

2. Save the log of one drive or one charge to a file outside the
   repository. Use `--since` and `--until` to select the period. One driving
   hour can give about 1,800 log lines, so keep the period short:

   ```sh
   railway logs <deployment-id> --service telemetry --environment production \
     --json --since <start> --until <end> --lines 20000 > "$TMPDIR/telemetry-log.jsonl"
   wc -l "$TMPDIR/telemetry-log.jsonl"
   ```

   The line count must be lower than the `--lines` value. If it is equal,
   the log is cut. Select a shorter period and fetch again.

3. Count records and signals for each record type:

   ```sh
   jq -s '[.[] | select(.message == "record_payload")]
     | group_by(.metadata.txtype)
     | map({type: .[0].metadata.txtype, records: length,
            signals: (map(if (.data | type) == "object"
                          then (.data | keys - ["Vin", "CreatedAt", "IsResend"] | length)
                          else (.data | length) end) | add)})' \
     "$TMPDIR/telemetry-log.jsonl"
   ```

4. Count signals for each field:

   ```sh
   jq -r 'select(.message == "record_payload" and .metadata.txtype == "V")
     | .data | keys[] | select(. != "Vin" and . != "CreatedAt" and . != "IsResend")' \
     "$TMPDIR/telemetry-log.jsonl" | sort | uniq -c | sort -rn
   ```

5. Sum the connected time from the `duration_sec` values of the
   `socket_disconnected` lines. Divide the signals by the connected hours, and
   compare the rate with the table above.

6. Delete the log file.

Do not use the `V`, `alerts`, or `total` values of `socket_disconnected` as
signal counts; they are bytes. Do not use the Prometheus signal counter of
the server either. In v0.10.1, `trackSignalUsage` runs only for messages
that exceed the server rate limit.

When the application replaces the log dispatcher, count the stored signals
in the database instead.

## Limits of this estimate

- No drive was measured. The driving line is a calculation from the
  intervals.
- The hours for each state are assumptions.
- Tesla does not document how many fields the vehicle sends again after a
  reconnect. The test log showed all 8 fields on the first connection and 2
  fields on a fast reconnect.
- The Railway log of an old deployment can expire. The numbers above come
  from deployment logs that were read on 26 November 2026.
