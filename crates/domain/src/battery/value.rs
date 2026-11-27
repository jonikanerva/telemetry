//! The battery fields, their units, and the extraction of a number from a
//! stored value.

/// A battery field of the vehicle (`Field` enum of `vehicle_data.proto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// `Soc`, number 8: usable state of charge.
    Soc,
    /// `EnergyRemaining`, number 158.
    EnergyRemaining,
    /// `NominalFullPackEnergyKwh`, number 263.
    NominalFullPackEnergy,
    /// `OutsideTemp`, number 86.
    OutsideTemp,
}

impl Field {
    /// Every battery field.
    pub const ALL: [Self; 4] = [
        Self::Soc,
        Self::EnergyRemaining,
        Self::NominalFullPackEnergy,
        Self::OutsideTemp,
    ];

    /// The fields that have a series in the view.
    pub const SERIES: [Self; 2] = [Self::Soc, Self::NominalFullPackEnergy];

    /// The number of the field in `vehicle_data.proto`.
    #[must_use]
    pub const fn number(self) -> i32 {
        match self {
            Self::Soc => 8,
            Self::EnergyRemaining => 158,
            Self::NominalFullPackEnergy => 263,
            Self::OutsideTemp => 86,
        }
    }

    /// The field of a number, or `None` for another field.
    #[must_use]
    pub fn from_number(number: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.number() == number)
    }

    /// The unit of the field. `docs/telemetry-usage-estimate.md` lists the
    /// units that Tesla documents; Tesla states no unit for `OutsideTemp`.
    #[must_use]
    pub const fn unit(self) -> Unit {
        match self {
            Self::Soc => Unit::documented(UnitSymbol::Percent),
            Self::EnergyRemaining | Self::NominalFullPackEnergy => {
                Unit::documented(UnitSymbol::KilowattHour)
            }
            Self::OutsideTemp => Unit {
                symbol: UnitSymbol::DegreeCelsius,
                source: UnitSource::Assumed,
            },
        }
    }
}

/// The unit of a field and its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    /// The unit.
    pub symbol: UnitSymbol,
    /// Where the unit comes from.
    pub source: UnitSource,
}

impl Unit {
    const fn documented(symbol: UnitSymbol) -> Self {
        Self {
            symbol,
            source: UnitSource::Documented,
        }
    }
}

/// A unit symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitSymbol {
    /// Percent.
    Percent,
    /// Kilowatt-hour.
    KilowattHour,
    /// Degree Celsius.
    DegreeCelsius,
}

/// Where a unit comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitSource {
    /// Tesla documents the unit.
    Documented,
    /// Tesla states no unit; the unit is an assumption.
    Assumed,
}

/// The `value_type` of the `invalid` marker (ADR 0002).
pub const VALUE_TYPE_INVALID: i16 = 10;

/// The largest integer magnitude that a 64-bit float holds exactly: 2^53.
pub const MAX_EXACT_INT: i64 = 1 << 53;

/// The longest text that can be a number.
pub const MAX_NUMBER_TEXT: usize = 32;

/// The value columns of one `raw.vehicle_signal` row (ADR 0002, "Value
/// representation").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawValue<'a> {
    /// The protobuf field number of the `Value` variant.
    pub value_type: i16,
    /// Column `int_value`.
    pub int_value: Option<i64>,
    /// Column `float_value`.
    pub float_value: Option<f64>,
    /// Column `text_value`.
    pub text_value: Option<&'a str>,
    /// Column `bool_value`.
    pub bool_value: Option<bool>,
}

/// Why a row holds no usable number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// The `invalid` marker of the vehicle: no valid value.
    InvalidMarker,
    /// The row holds no finite number in a form that this rule accepts.
    NotNumeric,
}

/// The result of [`extract`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Extracted {
    /// A finite number: a float or an integer as stored, a text as the
    /// nearest 64-bit float.
    Usable(f64),
    /// No usable number.
    Excluded(Exclusion),
}

/// Extracts the number of a stored value.
///
/// The rule (the SQL of the server applies the same rule):
///
/// - `value_type` 4 or 5 (`float`, `double`): `float_value` when it is
///   finite.
/// - `value_type` 2 or 3 (`int32`, `int64`): `int_value` when its magnitude
///   is at most 2^53, so the conversion to a float is exact.
/// - `value_type` 1 (`string`): `text_value` when it has at most 32
///   characters of the form `-?[0-9]+(.[0-9]+)?`. The parse rounds to the
///   nearest float, as PostgreSQL `float8` does.
/// - `value_type` 10: the invalid marker.
/// - Every other variant (boolean, location, enums, objects) and a row
///   without the column of its variant: not numeric.
#[must_use]
pub fn extract(raw: &RawValue<'_>) -> Extracted {
    let usable = match raw.value_type {
        VALUE_TYPE_INVALID => return Extracted::Excluded(Exclusion::InvalidMarker),
        4 | 5 => raw.float_value.filter(|value| value.is_finite()),
        2 | 3 => raw
            .int_value
            .filter(|value| value.unsigned_abs() <= MAX_EXACT_INT.unsigned_abs())
            .map(int_to_float),
        1 => raw.text_value.and_then(parse_number_text),
        _ => None,
    };
    usable.map_or(
        Extracted::Excluded(Exclusion::NotNumeric),
        Extracted::Usable,
    )
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the caller passes only magnitudes up to 2^53, which a float holds exactly"
)]
const fn int_to_float(value: i64) -> f64 {
    value as f64
}

/// Parses a text of the form `-?[0-9]+(.[0-9]+)?` with at most
/// [`MAX_NUMBER_TEXT`] characters.
fn parse_number_text(text: &str) -> Option<f64> {
    if text.len() > MAX_NUMBER_TEXT {
        return None;
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = match digits.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (digits, None),
    };
    let all_digits =
        |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !all_digits(whole) || fraction.is_some_and(|part| !all_digits(part)) {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const NONE: RawValue<'static> = RawValue {
        value_type: 0,
        int_value: None,
        float_value: None,
        text_value: None,
        bool_value: None,
    };

    fn float(value_type: i16, value: f64) -> Extracted {
        extract(&RawValue {
            value_type,
            float_value: Some(value),
            ..NONE
        })
    }

    fn int(value_type: i16, value: i64) -> Extracted {
        extract(&RawValue {
            value_type,
            int_value: Some(value),
            ..NONE
        })
    }

    fn text(value: &str) -> Extracted {
        extract(&RawValue {
            value_type: 1,
            text_value: Some(value),
            ..NONE
        })
    }

    #[test]
    fn the_field_numbers_match_the_proto() {
        let proto = include_str!("../../../ingest/proto/vehicle_data.proto");
        for (name, field) in [
            ("Soc", Field::Soc),
            ("EnergyRemaining", Field::EnergyRemaining),
            ("NominalFullPackEnergyKwh", Field::NominalFullPackEnergy),
            ("OutsideTemp", Field::OutsideTemp),
        ] {
            let line = format!("  {name} = {};", field.number());
            assert!(proto.lines().any(|l| l == line), "{line}");
            assert_eq!(Field::from_number(field.number()), Some(field));
        }
        assert_eq!(Field::from_number(42), None);
    }

    #[test]
    fn only_outside_temp_has_an_assumed_unit() {
        for field in Field::ALL {
            let assumed = field.unit().source == UnitSource::Assumed;
            assert_eq!(assumed, field == Field::OutsideTemp, "{field:?}");
        }
    }

    #[test]
    fn floats_are_kept_as_sent() {
        assert_eq!(float(5, 80.25), Extracted::Usable(80.25));
        assert_eq!(float(4, -3.5), Extracted::Usable(-3.5));
        assert_eq!(float(5, 0.0), Extracted::Usable(0.0));
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(float(5, value), Extracted::Excluded(Exclusion::NotNumeric));
        }
    }

    #[test]
    fn integers_beyond_2_pow_53_are_not_numeric() {
        assert_eq!(int(2, 80), Extracted::Usable(80.0));
        assert_eq!(
            int(3, MAX_EXACT_INT),
            Extracted::Usable(9_007_199_254_740_992.0)
        );
        assert_eq!(
            int(3, -MAX_EXACT_INT),
            Extracted::Usable(-9_007_199_254_740_992.0)
        );
        assert_eq!(
            int(3, MAX_EXACT_INT + 1),
            Extracted::Excluded(Exclusion::NotNumeric)
        );
        assert_eq!(int(3, i64::MIN), Extracted::Excluded(Exclusion::NotNumeric));
    }

    #[test]
    fn text_numbers_need_the_plain_decimal_form() {
        assert_eq!(text("80"), Extracted::Usable(80.0));
        assert_eq!(text("-12.50"), Extracted::Usable(-12.5));
        assert_eq!(text("0.1"), Extracted::Usable(0.1));
        for bad in [
            "", "-", ".5", "5.", "1e5", "NaN", "Infinity", " 5", "5 ", "+5", "5,0", "0x10", "--1",
            "1.2.3", "١٢",
        ] {
            assert_eq!(
                text(bad),
                Extracted::Excluded(Exclusion::NotNumeric),
                "{bad:?}"
            );
        }
        let long = "1".repeat(MAX_NUMBER_TEXT + 1);
        assert_eq!(text(&long), Extracted::Excluded(Exclusion::NotNumeric));
        let longest = "9".repeat(MAX_NUMBER_TEXT);
        assert!(matches!(text(&longest), Extracted::Usable(_)));
    }

    #[test]
    fn the_invalid_marker_is_never_a_value() {
        let raw = RawValue {
            value_type: VALUE_TYPE_INVALID,
            bool_value: Some(true),
            ..NONE
        };
        assert_eq!(extract(&raw), Extracted::Excluded(Exclusion::InvalidMarker));
        let raw = RawValue {
            value_type: VALUE_TYPE_INVALID,
            bool_value: Some(false),
            ..NONE
        };
        assert_eq!(extract(&raw), Extracted::Excluded(Exclusion::InvalidMarker));
    }

    #[test]
    fn other_variants_and_missing_columns_are_not_numeric() {
        let boolean = RawValue {
            value_type: 6,
            bool_value: Some(true),
            ..NONE
        };
        assert_eq!(
            extract(&boolean),
            Extracted::Excluded(Exclusion::NotNumeric)
        );
        // An enum variant stores its number in `int_value`; it is no measure.
        assert_eq!(int(8, 3), Extracted::Excluded(Exclusion::NotNumeric));
        let missing = RawValue {
            value_type: 5,
            ..NONE
        };
        assert_eq!(
            extract(&missing),
            Extracted::Excluded(Exclusion::NotNumeric)
        );
    }

    proptest! {
        #[test]
        fn a_usable_value_is_always_finite(
            value_type in 0_i16..60,
            int_value in proptest::option::of(any::<i64>()),
            float_value in proptest::option::of(any::<f64>()),
            text_value in proptest::option::of("-?[0-9.]{0,40}"),
        ) {
            let raw = RawValue {
                value_type,
                int_value,
                float_value,
                text_value: text_value.as_deref(),
                bool_value: None,
            };
            if let Extracted::Usable(value) = extract(&raw) {
                prop_assert!(value.is_finite());
            }
        }

        #[test]
        fn a_decimal_text_round_trips(whole in 0_u32..1_000_000, fraction in 0_u32..1_000) {
            // The expected value comes from the parts, not from a parse:
            // the nearest float to whole + fraction / 1000 prints as the
            // same three decimals.
            let text_value = format!("{whole}.{fraction:03}");
            let Extracted::Usable(value) = text(&text_value) else {
                return Err(TestCaseError::fail("not usable"));
            };
            prop_assert_eq!(format!("{value:.3}"), text_value);
            let approx = f64::from(whole) + f64::from(fraction) / 1_000.0;
            prop_assert!((value - approx).abs() <= 1e-9);
        }
    }
}
