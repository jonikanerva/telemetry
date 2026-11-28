//! The rules of the consumption view: drives and their Wh/km.
//!
//! The real drives cannot be checked yet (no drive is stored), so each
//! rule below is an assumption:
//!
//! - **Gear.** Only `value_type` 9 (`ShiftState`) with P (2), R (3), N (4),
//!   or D (5) is a reading. Unknown (0), Invalid (1), SNA (6), the invalid
//!   marker (`value_type` 10), and every other row are no reading: they
//!   neither start nor end a drive. The SQL of the server selects the
//!   same rows.
//! - **Drive.** A drive starts at the first R, N, or D after a P, or after
//!   no reading at all, and ends at the next P. R, N, and D inside a drive
//!   do not split it. A drive without a P before the range end is ongoing
//!   and has no totals.
//! - **Range start.** Only drives that start inside the range count. When
//!   the newest reading before the range is R, N, or D, the drive that
//!   continues into the range is left out: it belongs to the earlier
//!   range, and its values in this range would be too short.
//! - **Values.** Odometer (miles, Tesla documents the unit), and
//!   `EnergyRemaining` (kWh) need two samples inside the drive, start and
//!   end included; the value is the last minus the first. One sample or
//!   none is unknown, never 0. Negative energy stays negative.
//! - **Wh/km** exists when distance and energy exist and the distance is
//!   at least [`MIN_DISTANCE_KM`]. The Odometer step is 0.1 mile, so a
//!   shorter drive gives noise.
//! - **Outside temperature** is the mean of the `OutsideTemp` samples
//!   inside the drive (°C assumed, as in the battery view). The interval
//!   is 300 s, so a short drive often has none.
//! - **Mean speed** is the distance divided by the duration.
//! - **Limits.** At most [`MAX_GEAR_ROWS`] Gear rows and
//!   [`MAX_DRIVE_SAMPLES`] value rows, the newest first. A cut leaves out
//!   the older drives and sets `truncated`.

use jiff::{SignedDuration, Timestamp};

use crate::battery::value::{Extracted, RawValue, extract};
use crate::battery::{ContractViolation, VehicleCount, Window};

/// One mile in kilometres. Tesla sends the Odometer in miles.
pub const MILE_IN_KM: f64 = 1.609_344;

/// The shortest distance that gets a Wh/km value.
pub const MIN_DISTANCE_KM: f64 = 1.0;

/// The most Gear rows of one read: at 50 rows each day, about a year.
pub const MAX_GEAR_ROWS: usize = 20_000;

/// The most value rows of one read: a year of one hour of driving each
/// day at the configured intervals is about 92,000 rows.
pub const MAX_DRIVE_SAMPLES: usize = 200_000;

/// The `value_type` of `ShiftState` (`vehicle_data.proto`).
pub const VALUE_TYPE_SHIFT_STATE: i16 = 9;

/// The field number of `Gear`.
pub const GEAR_FIELD: i32 = 10;

/// A gear reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gear {
    /// P.
    Park,
    /// R, N, or D.
    Moving,
}

/// The gear of a stored Gear row, or `None` when the row is no reading.
#[must_use]
pub const fn gear(raw: &RawValue<'_>) -> Option<Gear> {
    if raw.value_type != VALUE_TYPE_SHIFT_STATE {
        return None;
    }
    match raw.int_value {
        Some(2) => Some(Gear::Park),
        Some(3..=5) => Some(Gear::Moving),
        _ => None,
    }
}

/// The value fields of a drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueField {
    /// `Odometer`, number 5, miles.
    Odometer,
    /// `EnergyRemaining`, number 158, kWh.
    EnergyRemaining,
    /// `OutsideTemp`, number 86, °C (assumed).
    OutsideTemp,
}

impl ValueField {
    /// Every value field.
    pub const ALL: [Self; 3] = [Self::Odometer, Self::EnergyRemaining, Self::OutsideTemp];

    /// The number of the field in `vehicle_data.proto`.
    #[must_use]
    pub const fn number(self) -> i32 {
        match self {
            Self::Odometer => 5,
            Self::EnergyRemaining => 158,
            Self::OutsideTemp => 86,
        }
    }

    /// The field of a number, or `None` for another field.
    #[must_use]
    pub fn from_number(number: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.number() == number)
    }
}

/// What lies before the oldest Gear reading of the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Before {
    /// No reading exists before.
    Nothing,
    /// The newest reading before the range.
    Reading(Gear),
    /// The row limit cut the read: the earlier readings are not known.
    Cut,
}

/// The Gear input of the drive rule after the row limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GearInput {
    /// What lies before `readings`.
    pub before: Before,
    /// The readings of the range, oldest first.
    pub readings: Vec<(Timestamp, Gear)>,
    /// True when the limit cut the read.
    pub truncated: bool,
}

/// Applies the row limit `max` to the readings of the range.
///
/// `newest_first` holds at most `max + 1` readings of the range, newest
/// first; `before` is the newest reading before the range. When
/// `newest_first` has more than `max` readings, the oldest are left out
/// and the start is [`Before::Cut`].
#[must_use]
pub fn gear_input(
    before: Option<Gear>,
    mut newest_first: Vec<(Timestamp, Gear)>,
    max: usize,
) -> GearInput {
    let truncated = newest_first.len() > max;
    newest_first.truncate(max);
    newest_first.reverse();
    GearInput {
        before: match (truncated, before) {
            (true, _) => Before::Cut,
            (false, Some(gear)) => Before::Reading(gear),
            (false, None) => Before::Nothing,
        },
        readings: newest_first,
        truncated,
    }
}

/// The time of one drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    /// The first R, N, or D.
    pub start: Timestamp,
    /// The P that ends the drive, or `None` while the drive is ongoing.
    pub end: Option<Timestamp>,
}

/// The drives of the Gear readings, oldest first (the rule of the module).
#[must_use]
pub fn periods(input: &GearInput) -> Vec<Period> {
    // `may_start`: the previous reading is P, or no reading exists.
    let mut may_start = matches!(input.before, Before::Nothing | Before::Reading(Gear::Park));
    let mut open: Option<Timestamp> = None;
    let mut periods = Vec::new();
    for &(at, gear) in &input.readings {
        match gear {
            Gear::Moving => {
                if open.is_none() && may_start {
                    open = Some(at);
                }
                may_start = false;
            }
            Gear::Park => {
                if let Some(start) = open.take() {
                    periods.push(Period {
                        start,
                        end: Some(at),
                    });
                }
                may_start = true;
            }
        }
    }
    if let Some(start) = open {
        periods.push(Period { start, end: None });
    }
    periods
}

/// One usable value inside a drive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriveSample {
    /// The field.
    pub field: ValueField,
    /// Vehicle clock.
    pub at: Timestamp,
    /// A finite value.
    pub value: f64,
}

/// The usable value of a stored row of a value field, with the rule of the
/// battery view ([`extract`]); `None` for a row that is not usable.
#[must_use]
pub fn sample(field: ValueField, at: Timestamp, raw: &RawValue<'_>) -> Option<DriveSample> {
    match extract(raw) {
        Extracted::Usable(value) => Some(DriveSample { field, at, value }),
        Extracted::Excluded(_) => None,
    }
}

/// The value rows after the row limit.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SampleInput {
    /// The usable samples, in any order.
    pub samples: Vec<DriveSample>,
    /// When the limit cut the read: the time of the oldest row that was
    /// read. A drive that starts at or before it can miss rows.
    pub cut_at: Option<Timestamp>,
}

/// Applies the row limit `max`. `newest_first` holds at most `max + 1`
/// rows, newest first, with `None` for a row that is not usable.
#[must_use]
pub fn sample_input(
    mut newest_first: Vec<(Timestamp, Option<DriveSample>)>,
    max: usize,
) -> SampleInput {
    let cut = newest_first.len() > max;
    newest_first.truncate(max);
    let cut_at = if cut {
        newest_first.last().map(|(at, _)| *at)
    } else {
        None
    };
    SampleInput {
        samples: newest_first.into_iter().filter_map(|(_, s)| s).collect(),
        cut_at,
    }
}

/// Everything a request reads, in one database snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The window of the request.
    pub window: Window,
    /// The vehicles.
    pub vehicles: VehicleCount,
    /// The Gear input.
    pub gears: GearInput,
    /// The values inside the ended drives of [`periods`].
    pub samples: SampleInput,
}

/// One drive of the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drive {
    /// The start (vehicle clock).
    pub start: Timestamp,
    /// The end, or `None` while the drive is ongoing. An ongoing drive has
    /// no other value.
    pub end: Option<Timestamp>,
    /// The duration of an ended drive.
    pub duration: Option<SignedDuration>,
    /// The distance in miles, as the vehicle sends it.
    pub distance_mi: Option<f64>,
    /// The energy used in kWh: the first minus the last `EnergyRemaining`.
    pub energy_kwh: Option<f64>,
    /// The consumption in Wh/km.
    pub wh_per_km: Option<f64>,
    /// The mean outside temperature in °C.
    pub mean_outside_temp_c: Option<f64>,
    /// The mean speed in km/h.
    pub mean_speed_kmh: Option<f64>,
}

/// The totals of the ended drives of the range. The sums cover the
/// drives with a Wh/km value only, so the three numbers belong together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Totals {
    /// The ended drives.
    pub drives: u32,
    /// The distance in miles of the drives with a Wh/km value, or `None`
    /// without one.
    pub distance_mi: Option<f64>,
    /// The energy in kWh of the same drives.
    pub energy_kwh: Option<f64>,
    /// The energy over the distance of the same drives.
    pub wh_per_km: Option<f64>,
    /// The ended drives without a Wh/km value.
    pub without_consumption: u32,
}

/// The view of one request.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// The window.
    pub window: Window,
    /// The drives, newest first.
    pub drives: Vec<Drive>,
    /// The totals.
    pub totals: Totals,
    /// True when a row limit left out older drives.
    pub truncated: bool,
}

/// Derives the view of a snapshot.
///
/// # Errors
///
/// Returns [`ContractViolation::SecondVehicle`] for two vehicles and
/// [`ContractViolation::CountTooLarge`] for a count above `u32`.
pub fn derive_view(snapshot: &Snapshot) -> Result<View, ContractViolation> {
    if snapshot.vehicles == VehicleCount::Many {
        return Err(ContractViolation::SecondVehicle);
    }
    let cut_at = snapshot.samples.cut_at;
    let mut drives: Vec<Drive> = periods(&snapshot.gears)
        .into_iter()
        .filter(|period| cut_at.is_none_or(|cut| period.start > cut))
        .map(|period| drive(period, &snapshot.samples.samples))
        .collect();
    drives.reverse();
    let totals = totals(&drives)?;
    Ok(View {
        window: snapshot.window,
        drives,
        totals,
        truncated: snapshot.gears.truncated || cut_at.is_some(),
    })
}

/// The values of one drive from the samples inside it.
#[must_use]
pub fn drive(period: Period, samples: &[DriveSample]) -> Drive {
    let Some(end) = period.end else {
        return Drive {
            start: period.start,
            end: None,
            duration: None,
            distance_mi: None,
            energy_kwh: None,
            wh_per_km: None,
            mean_outside_temp_c: None,
            mean_speed_kmh: None,
        };
    };
    let inside = |field: ValueField| {
        let mut values: Vec<&DriveSample> = samples
            .iter()
            .filter(|s| s.field == field && period.start <= s.at && s.at <= end)
            .collect();
        values.sort_by_key(|s| s.at);
        values
    };
    let change = |field: ValueField| match inside(field).as_slice() {
        [first, .., last] => Some((first.value, last.value)),
        _ => None,
    };
    let distance_mi = change(ValueField::Odometer).map(|(first, last)| last - first);
    let energy_kwh = change(ValueField::EnergyRemaining).map(|(first, last)| first - last);
    let distance_km = distance_mi.map(|mi| mi * MILE_IN_KM);
    let wh_per_km = match (energy_kwh, distance_km) {
        (Some(kwh), Some(km)) if km >= MIN_DISTANCE_KM => Some(kwh * 1000.0 / km),
        _ => None,
    };
    let temps = inside(ValueField::OutsideTemp);
    let mean_outside_temp_c = mean(temps.iter().map(|s| s.value));
    let duration = end.duration_since(period.start);
    let hours = duration.as_secs_f64() / 3600.0;
    let mean_speed_kmh = distance_km.filter(|_| hours > 0.0).map(|km| km / hours);
    Drive {
        start: period.start,
        end: Some(end),
        duration: Some(duration),
        distance_mi,
        energy_kwh,
        wh_per_km,
        mean_outside_temp_c,
        mean_speed_kmh,
    }
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0_u32), |(sum, count), v| (sum + v, count + 1));
    (count > 0).then(|| sum / f64::from(count))
}

fn count(value: usize) -> Result<u32, ContractViolation> {
    u32::try_from(value).map_err(|_| ContractViolation::CountTooLarge)
}

/// The totals of the ended drives.
///
/// # Errors
///
/// Returns [`ContractViolation::CountTooLarge`] for a count above `u32`.
pub fn totals(drives: &[Drive]) -> Result<Totals, ContractViolation> {
    let ended = drives.iter().filter(|d| d.end.is_some()).count();
    let mut counted = 0_usize;
    let (mut miles, mut kwh) = (0.0, 0.0);
    for drive in drives {
        if let (Some(_), Some(distance), Some(energy)) =
            (drive.wh_per_km, drive.distance_mi, drive.energy_kwh)
        {
            counted += 1;
            miles += distance;
            kwh += energy;
        }
    }
    let any = counted > 0;
    Ok(Totals {
        drives: count(ended)?,
        distance_mi: any.then_some(miles),
        energy_kwh: any.then_some(kwh),
        wh_per_km: any.then(|| kwh * 1000.0 / (miles * MILE_IN_KM)),
        without_consumption: count(ended - counted)?,
    })
}

#[cfg(test)]
mod tests;
