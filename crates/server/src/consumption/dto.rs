//! The wire types of `GET /api/v1/consumption` and their mapping from the
//! domain view (`telemetry_web_domain::consumption`).
//!
//! The schema rules of `crate::wire` apply. Every time is on the vehicle
//! clock (`created_at`). Within `/api/v1` a change only adds object keys.

use serde::{Deserialize, Serialize};
use telemetry_web_domain::consumption as domain;
use utoipa::{IntoParams, ToSchema};

use crate::battery::dto::{BatteryStatus, Gaps, RangeKey};
use crate::battery::map::{self, range_key};
use crate::wire::Instant;

/// The query of `GET /api/v1/consumption`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ConsumptionQuery {
    /// The time range that ends now. Required.
    pub range: RangeKey,
}

/// The body of a successful `GET /api/v1/consumption`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ConsumptionView {
    /// The time of the response (server clock). Equal to `range.to`.
    pub generated_at: Instant,
    /// The requested range.
    pub range: ConsumptionRange,
    /// Connection, newest data, and freshness, as in the battery view;
    /// `null` when no row of a battery field exists yet.
    #[schema(required = true)]
    pub status: Option<BatteryStatus>,
    /// The losses that the ingest process recorded in the range, as in the
    /// battery view.
    pub gaps: Gaps,
    /// The unit of each quantity.
    pub units: ConsumptionUnits,
    /// The drives that start in the range, newest first. Empty when no
    /// drive exists.
    pub drives: Vec<Drive>,
    /// The totals of the ended drives.
    pub totals: ConsumptionTotals,
    /// True when a row limit left out the older drives of the range.
    pub truncated: bool,
}

/// The range of a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConsumptionRange {
    /// The requested range.
    pub key: RangeKey,
    /// The start, inclusive (server clock).
    pub from: Instant,
    /// The end, exclusive (server clock). Equal to `generated_at`.
    pub to: Instant,
}

/// The units of the quantities. Each has one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConsumptionUnits {
    /// The distance: miles, as the vehicle sends the Odometer.
    pub distance: DistanceUnit,
    /// The energy: kWh, as the vehicle sends `EnergyRemaining`.
    pub energy: EnergyUnit,
    /// The consumption.
    pub consumption: ConsumptionUnit,
    /// The temperature: °C, an assumption (Tesla states no unit).
    pub temperature: TemperatureUnit,
    /// The mean speed.
    pub speed: SpeedUnit,
}

/// Miles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub enum DistanceUnit {
    /// Miles.
    #[serde(rename = "mi")]
    Mile,
}

/// Kilowatt-hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub enum EnergyUnit {
    /// Kilowatt-hours.
    #[serde(rename = "kWh")]
    KilowattHour,
}

/// Watt-hours for each kilometre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub enum ConsumptionUnit {
    /// Watt-hours for each kilometre.
    #[serde(rename = "Wh/km")]
    WattHourPerKilometre,
}

/// Degrees Celsius.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub enum TemperatureUnit {
    /// Degrees Celsius.
    #[serde(rename = "°C")]
    DegreeCelsius,
}

/// Kilometres for each hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub enum SpeedUnit {
    /// Kilometres for each hour.
    #[serde(rename = "km/h")]
    KilometrePerHour,
}

/// One drive: from the first R, N, or D after a P to the next P. A `null`
/// value is unknown, never 0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
pub struct Drive {
    /// The start.
    pub start: Instant,
    /// The end, or `null` while the drive is ongoing. An ongoing drive has
    /// only `start`.
    #[schema(required = true)]
    pub end: Option<Instant>,
    /// The duration in seconds.
    #[schema(required = true, minimum = 0, maximum = 4_294_967_295_u64)]
    pub duration_seconds: Option<u32>,
    /// The distance (the last minus the first Odometer in the drive).
    #[schema(required = true)]
    pub distance: Option<f64>,
    /// The energy used (the first minus the last `EnergyRemaining` in the
    /// drive). Negative when the battery gained energy.
    #[schema(required = true)]
    pub energy_used: Option<f64>,
    /// The consumption; only for a distance of at least 1 km.
    #[schema(required = true)]
    pub consumption: Option<f64>,
    /// The mean of the `OutsideTemp` samples in the drive.
    #[schema(required = true)]
    pub mean_outside_temp: Option<f64>,
    /// The distance divided by the duration.
    #[schema(required = true)]
    pub mean_speed: Option<f64>,
    /// True when a recorded loss meets the time span of the drive, ends
    /// included: the values can be wrong, for example two drives merged by
    /// a lost P. The ingest clock of the losses and the vehicle clock of
    /// the drive differ, so the mark is approximate.
    pub data_loss: bool,
}

/// The totals of the ended drives of the range. `distance`,
/// `energy_used`, and `consumption` cover the same drives: those with a
/// consumption value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
pub struct ConsumptionTotals {
    /// The ended drives.
    #[schema(minimum = 0, maximum = 4_294_967_295_u64)]
    pub drives: u32,
    /// The distance of the drives with a consumption, or `null` without one.
    #[schema(required = true)]
    pub distance: Option<f64>,
    /// The energy of the same drives.
    #[schema(required = true)]
    pub energy_used: Option<f64>,
    /// The energy over the distance of the same drives.
    #[schema(required = true)]
    pub consumption: Option<f64>,
    /// The ended drives without a consumption value.
    #[schema(minimum = 0, maximum = 4_294_967_295_u64)]
    pub drives_without_consumption: u32,
    /// The drives with `data_loss`. They are in the sums.
    #[schema(minimum = 0, maximum = 4_294_967_295_u64)]
    pub drives_with_data_loss: u32,
}

/// A time below one microsecond, which the wire cannot hold. Stored times
/// have microsecond precision, so this does not occur in practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a value does not fit the wire contract")]
pub struct MapError;

fn instant(at: jiff::Timestamp) -> Result<Instant, MapError> {
    Instant::from_timestamp(at).map_err(|_| MapError)
}

/// Maps a domain view.
///
/// # Errors
///
/// Returns [`MapError`] for a time below one microsecond.
pub fn view(view: &domain::View) -> Result<ConsumptionView, MapError> {
    let window = &view.window;
    Ok(ConsumptionView {
        generated_at: instant(window.to())?,
        range: ConsumptionRange {
            key: range_key(window.key()),
            from: instant(window.from())?,
            to: instant(window.to())?,
        },
        status: view
            .status
            .as_ref()
            .map(map::status)
            .transpose()
            .map_err(|_| MapError)?,
        gaps: map::gaps(&view.gaps).map_err(|_| MapError)?,
        units: ConsumptionUnits {
            distance: DistanceUnit::Mile,
            energy: EnergyUnit::KilowattHour,
            consumption: ConsumptionUnit::WattHourPerKilometre,
            temperature: TemperatureUnit::DegreeCelsius,
            speed: SpeedUnit::KilometrePerHour,
        },
        drives: view.drives.iter().map(drive).collect::<Result<_, _>>()?,
        totals: ConsumptionTotals {
            drives: view.totals.drives,
            distance: view.totals.distance_mi,
            energy_used: view.totals.energy_kwh,
            consumption: view.totals.wh_per_km,
            drives_without_consumption: view.totals.without_consumption,
            drives_with_data_loss: view.totals.with_data_loss,
        },
        truncated: view.truncated,
    })
}

fn drive(drive: &domain::Drive) -> Result<Drive, MapError> {
    Ok(Drive {
        start: instant(drive.start)?,
        end: drive.end.map(instant).transpose()?,
        duration_seconds: drive
            .duration
            .map(|d| u32::try_from(d.as_secs()).map_err(|_| MapError))
            .transpose()?,
        distance: drive.distance_mi,
        energy_used: drive.energy_kwh,
        consumption: drive.wh_per_km,
        mean_outside_temp: drive.mean_outside_temp_c,
        mean_speed: drive.mean_speed_kmh,
        data_loss: drive.data_loss,
    })
}
