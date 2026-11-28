//! The rules of the battery view (ADR 0013).
//!
//! A data source reads one [`Snapshot`] of the raw tables for a
//! [`Window`]. [`derive_view`] turns it into a [`View`]. The PostgreSQL
//! adapter of the server and the fake source of the test server give the
//! same snapshot for the same rows: the fake uses the reference rules of
//! this module ([`tile_snapshot`], [`series_snapshot`], [`select_events`],
//! [`newest_connectivity`]), and a database test compares the SQL with
//! them.
//!
//! Source meaning: a value that is not usable is never zero; a missing
//! value stays missing; the unit is the unit that the vehicle used.

pub mod connection;
pub mod gaps;
pub mod range;
pub mod series;
pub mod value;

pub use connection::{
    Connection, ConnectivityRow, Freshness, connection, freshness, newest_connectivity,
};
pub use gaps::{EventKind, EventRow, EventSnapshot, Gap, Limited, derive_gaps, select_events};
pub use range::{RangeKey, Window, WindowError};
pub use series::{
    ExcludedCounts, Point, RowTimes, Sample, SeriesSnapshot, SignalRow, TileSnapshot, segments,
    series_snapshot, tile_snapshot,
};
pub use value::{Field, RawValue, Unit};

use jiff::Timestamp;

/// Stored data that breaks a contract of the raw tables or of this view.
/// The whole request fails; no partial view is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContractViolation {
    /// More than one vehicle exists.
    #[error("more than one vehicle exists")]
    SecondVehicle,
    /// An ingest event has an unknown `kind`.
    #[error("an ingest event has an unknown kind")]
    UnknownEventKind,
    /// An ingest event breaks the rules of its kind.
    #[error("an ingest event breaks the rules of its kind")]
    BadEventRow,
    /// A count does not fit in 32 bits.
    #[error("a count does not fit in 32 bits")]
    CountTooLarge,
}

/// How many vehicles exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleCount {
    /// None.
    None,
    /// One, with its id.
    One(i16),
    /// Two or more: a contract violation (ADR 0002 has one vehicle).
    Many,
}

/// A value for each battery field.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PerField<T> {
    /// `Soc`.
    pub soc: T,
    /// `EnergyRemaining`.
    pub energy_remaining: T,
    /// `NominalFullPackEnergyKwh`.
    pub nominal_full_pack_energy: T,
    /// `OutsideTemp`.
    pub outside_temp: T,
}

impl<T> PerField<T> {
    /// The value of `field`.
    pub const fn get(&self, field: Field) -> &T {
        match field {
            Field::Soc => &self.soc,
            Field::EnergyRemaining => &self.energy_remaining,
            Field::NominalFullPackEnergy => &self.nominal_full_pack_energy,
            Field::OutsideTemp => &self.outside_temp,
        }
    }

    /// The value of `field`, to change it.
    pub const fn get_mut(&mut self, field: Field) -> &mut T {
        match field {
            Field::Soc => &mut self.soc,
            Field::EnergyRemaining => &mut self.energy_remaining,
            Field::NominalFullPackEnergy => &mut self.nominal_full_pack_energy,
            Field::OutsideTemp => &mut self.outside_temp,
        }
    }
}

/// The series of the view.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesPair<T> {
    /// `Soc`.
    pub soc: T,
    /// `NominalFullPackEnergyKwh`.
    pub nominal_full_pack_energy: T,
}

/// Everything a request reads, in one database snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The window of the request; `window.to()` is the time of the
    /// request.
    pub window: Window,
    /// The vehicles.
    pub vehicles: VehicleCount,
    /// The tile data of each field. Empty when no vehicle exists.
    pub tiles: PerField<TileSnapshot>,
    /// The series data.
    pub series: SeriesPair<SeriesSnapshot>,
    /// The loss rows and the restart markers of the window
    /// ([`select_events`]).
    pub events: EventSnapshot,
    /// The newest connectivity row ([`newest_connectivity`]).
    pub connectivity: Option<ConnectivityRow>,
}

/// The view of one request.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// No row of a battery field exists before the window end.
    NoDataYet {
        /// The window.
        window: Window,
    },
    /// At least one row exists.
    Available(Box<Available>),
}

/// The content of an available view.
#[derive(Debug, Clone, PartialEq)]
pub struct Available {
    /// The window.
    pub window: Window,
    /// The connection state.
    pub connection: Connection,
    /// The newest `created_at` of a battery row.
    pub latest_data_at: Timestamp,
    /// The newest `received_at` of the newest row of each battery field.
    pub latest_received_at: Timestamp,
    /// The freshness.
    pub freshness: Freshness,
    /// The tiles.
    pub tiles: PerField<Reading>,
    /// The change of the capacity.
    pub capacity_change: CapacityChange,
    /// The series.
    pub series: SeriesPair<SeriesView>,
    /// The recorded losses of the window.
    pub gaps: Limited<Gap>,
    /// The ingest starts of the window.
    pub restarts: Limited<Timestamp>,
}

/// The newest usable value of a field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reading {
    /// A usable value.
    Value {
        /// The sample.
        sample: Sample,
        /// The unit.
        unit: Unit,
        /// The newest unusable row after the sample.
        newer_unusable_at: Option<Timestamp>,
    },
    /// No usable value.
    Missing {
        /// The unit.
        unit: Unit,
        /// The newest unusable row, if any.
        latest_unusable_at: Option<Timestamp>,
    },
}

/// The change of the capacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CapacityChange {
    /// Both values exist and are different samples.
    Value {
        /// The baseline.
        from: Sample,
        /// The newest value.
        to: Sample,
    },
    /// No usable value exists.
    NoLatest,
    /// The newest value is the only one.
    NoBaseline,
}

/// One series of the view.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesView {
    /// The unit.
    pub unit: Unit,
    /// The newest usable sample before the window.
    pub carry_in: Option<Sample>,
    /// The points, split at gaps.
    pub segments: Vec<Vec<Point>>,
    /// The excluded rows of the window.
    pub excluded: ExcludedCounts,
}

/// Connection, newest data, and freshness (ADR 0013, decisions 2 and 3).
/// The consumption view uses the same status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// The connection state.
    pub connection: Connection,
    /// The newest `created_at` of a battery row.
    pub latest_data_at: Timestamp,
    /// The newest `received_at` of the newest row of each battery field.
    pub latest_received_at: Timestamp,
    /// The freshness.
    pub freshness: Freshness,
}

/// The status of the tiles of a window, or `None` when no row of a
/// battery field exists before the window end.
#[must_use]
pub fn status(
    window: &Window,
    tiles: &PerField<TileSnapshot>,
    connectivity: Option<ConnectivityRow>,
) -> Option<Status> {
    let latest_rows = || {
        Field::ALL
            .into_iter()
            .filter_map(|field| tiles.get(field).latest_row)
    };
    let latest_row = latest_rows().max_by_key(|row| row.created_at)?;
    let latest_received_at = latest_rows()
        .map(|row| row.received_at)
        .max()
        .unwrap_or(latest_row.received_at);
    let connection = connection(connectivity);
    Some(Status {
        connection,
        latest_data_at: latest_row.created_at,
        latest_received_at,
        freshness: freshness(window.to(), latest_received_at, connection),
    })
}

/// Derives the view of a snapshot.
///
/// # Errors
///
/// Returns a [`ContractViolation`] when the stored data breaks a
/// contract; no partial view is published.
pub fn derive_view(snapshot: &Snapshot) -> Result<View, ContractViolation> {
    let window = snapshot.window;
    match snapshot.vehicles {
        VehicleCount::None => return Ok(View::NoDataYet { window }),
        VehicleCount::Many => return Err(ContractViolation::SecondVehicle),
        VehicleCount::One(_) => {}
    }
    let gaps = derive_gaps(&snapshot.events.losses)?;
    let Some(status) = status(&window, &snapshot.tiles, snapshot.connectivity) else {
        return Ok(View::NoDataYet { window });
    };

    let mut tiles = PerField::<Reading>::default_for_units();
    for field in Field::ALL {
        *tiles.get_mut(field) = reading(field, snapshot.tiles.get(field));
    }
    let capacity = &snapshot.series.nominal_full_pack_energy;
    let capacity_change = match snapshot.tiles.nominal_full_pack_energy.latest_usable {
        None => CapacityChange::NoLatest,
        Some(to) => match capacity.carry_in.or(capacity.first_in_range) {
            Some(from) if from.at != to.at => CapacityChange::Value { from, to },
            _ => CapacityChange::NoBaseline,
        },
    };
    let series = SeriesPair {
        soc: series_view(Field::Soc, &snapshot.series.soc, &gaps.items),
        nominal_full_pack_energy: series_view(Field::NominalFullPackEnergy, capacity, &gaps.items),
    };
    Ok(View::Available(Box::new(Available {
        window,
        connection: status.connection,
        latest_data_at: status.latest_data_at,
        latest_received_at: status.latest_received_at,
        freshness: status.freshness,
        tiles,
        capacity_change,
        series,
        gaps,
        restarts: snapshot.events.restarts.clone(),
    })))
}

impl PerField<Reading> {
    fn default_for_units() -> Self {
        let missing = |field: Field| Reading::Missing {
            unit: field.unit(),
            latest_unusable_at: None,
        };
        Self {
            soc: missing(Field::Soc),
            energy_remaining: missing(Field::EnergyRemaining),
            nominal_full_pack_energy: missing(Field::NominalFullPackEnergy),
            outside_temp: missing(Field::OutsideTemp),
        }
    }
}

fn reading(field: Field, tile: &TileSnapshot) -> Reading {
    let unit = field.unit();
    let latest_row_at = tile.latest_row.map(|row| row.created_at);
    match tile.latest_usable {
        Some(sample) => Reading::Value {
            sample,
            unit,
            newer_unusable_at: latest_row_at.filter(|at| *at > sample.at),
        },
        None => Reading::Missing {
            unit,
            latest_unusable_at: latest_row_at,
        },
    }
}

fn series_view(field: Field, snapshot: &SeriesSnapshot, gaps: &[Gap]) -> SeriesView {
    SeriesView {
        unit: field.unit(),
        carry_in: snapshot.carry_in,
        segments: segments(&snapshot.points, gaps),
        excluded: snapshot.excluded,
    }
}

#[cfg(test)]
mod tests;
