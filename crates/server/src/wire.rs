//! Wire types that several API modules share.
//!
//! [`Instant`] is the one wire form of an instant (`STACK.md` §10): RFC
//! 3339 in UTC with `Z`, and 0 to 6 fractional digits of a second, for
//! example `2026-11-27T08:15:30.123456Z`. PostgreSQL keeps microseconds,
//! so 6 digits keep every stored value. The browser keeps milliseconds
//! and uses the value for display only.
//!
//! The range is the range of `jiff::Timestamp` within the 4-digit year
//! form: from `0000-01-01T00:00:00Z` to `9999-12-30T22:00:00.999999Z`. A
//! later text has the right form but is a [`InstantError::Range`] error.
//!
//! Every value of [`Instant`] has microsecond precision by construction:
//! [`Instant::from_timestamp`] rejects a finer value, and
//! [`Instant::truncated`] drops the part below one microsecond. So the
//! serialised text never has more than 6 fractional digits, although the
//! `Display` of `jiff::Timestamp` writes up to 9.
//!
//! Schema rules of the S1 contract experiment (E1) for the DTOs of later
//! slices:
//!
//! - An instant field uses [`Instant`]. Its schema has the pattern
//!   `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,6})?Z$`, which the client
//!   generator turns into a Zod check.
//! - A count (`u32`) needs `#[schema(minimum = 0, maximum = 4294967295_u64)]`;
//!   without it utoipa writes `int32`, and the client caps it at 2^31 - 1.
//! - A field that is always present but can be null (`Option<T>`) needs
//!   `#[schema(required = true)]`, so the schema says nullable, not
//!   optional.
//! - A tagged enum becomes a plain `oneOf`, which the client validates as a
//!   union and narrows correctly. Do not use `#[schema(discriminator)]`.
//!
//! The utoipa feature `jiff_0_2` is not enabled: it supports jiff 0.2, and
//! this workspace uses jiff 1. The schema of [`Instant`] is set by hand
//! (`string`, format `date-time`), and the tests below check the round
//! trip of the serde form.

use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::ToSchema;

/// An instant on the wire: RFC 3339 UTC with `Z`, microsecond precision.
///
/// The value holds no sub-microsecond part, so the text form always has
/// 0 to 6 fractional digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ToSchema)]
#[schema(
    value_type = String,
    format = DateTime,
    pattern = r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,6})?Z$",
    example = "2026-11-27T08:15:30.123456Z"
)]
pub struct Instant(Timestamp);

impl Instant {
    /// Wraps a timestamp that has no sub-microsecond part.
    ///
    /// # Errors
    ///
    /// Returns [`InstantError::Precision`] when the timestamp has a
    /// nanosecond part below one microsecond. The conversion does not
    /// round, because a rounded value would no longer equal the source.
    pub fn from_timestamp(timestamp: Timestamp) -> Result<Self, InstantError> {
        if timestamp.subsec_nanosecond() % 1_000 == 0 {
            Ok(Self(timestamp))
        } else {
            Err(InstantError::Precision)
        }
    }

    /// Wraps a timestamp and drops its part below one microsecond, the
    /// precision of PostgreSQL. Use it only where the source can hold a
    /// finer value than the wire contract, for example the ingest clock of
    /// a test; a value read from PostgreSQL needs no truncation.
    #[must_use]
    pub fn truncated(timestamp: Timestamp) -> Self {
        // jiff gives a negative sub-second part before 1970. `rem_euclid`
        // gives 0 to 999 ns in both cases, so the value always moves to the
        // earlier microsecond (a floor, not a move toward 1970).
        let below = timestamp.subsec_nanosecond().rem_euclid(1_000);
        match timestamp.checked_sub(jiff::SignedDuration::from_nanos(i64::from(below))) {
            Ok(truncated) => Self(truncated),
            Err(_) => Self(timestamp),
        }
    }

    /// The timestamp.
    #[must_use]
    pub const fn timestamp(self) -> Timestamp {
        self.0
    }
}

impl fmt::Display for Instant {
    /// Writes RFC 3339 UTC with `Z`. jiff writes the fraction only when it
    /// is not zero, with the digits it needs (here at most 6).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A text that is not a valid wire instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InstantError {
    /// The text is not `YYYY-MM-DDTHH:MM:SS[.f]Z` with 1 to 6 digits of
    /// fraction.
    #[error("expected RFC 3339 UTC with Z and at most 6 fractional digits")]
    Format,
    /// The text has the right form but is not a real date or time.
    #[error("the date or time does not exist")]
    Range,
    /// The value has a part below one microsecond.
    #[error("the value has a part below one microsecond")]
    Precision,
}

impl FromStr for Instant {
    type Err = InstantError;

    /// Parses the strict wire form. jiff alone also accepts other offsets,
    /// a lowercase `t` or `z`, a space separator, 9 fractional digits, and
    /// the leap second `60` (as `59`); the form check rejects these first.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        check_form(text.as_bytes())?;
        let timestamp = Timestamp::from_str(text).map_err(|_| InstantError::Range)?;
        Self::from_timestamp(timestamp)
    }
}

/// Checks `YYYY-MM-DDTHH:MM:SS[.f{1,6}]Z` and the seconds `00` to `59`.
fn check_form(bytes: &[u8]) -> Result<(), InstantError> {
    const SHAPE: &[u8; 19] = b"dddd-dd-ddTdd:dd:dd";
    let (head, rest) = bytes
        .split_at_checked(SHAPE.len())
        .ok_or(InstantError::Format)?;
    let shape_matches = head
        .iter()
        .zip(SHAPE)
        .all(|(byte, expected)| match expected {
            b'd' => byte.is_ascii_digit(),
            other => byte == other,
        });
    if !shape_matches {
        return Err(InstantError::Format);
    }
    if head.get(17).is_some_and(|tens| *tens > b'5') {
        return Err(InstantError::Format);
    }
    let fraction = match rest {
        [b'Z'] => return Ok(()),
        [b'.', fraction @ .., b'Z'] => fraction,
        _ => return Err(InstantError::Format),
    };
    if (1..=6).contains(&fraction.len()) && fraction.iter().all(u8::is_ascii_digit) {
        Ok(())
    } else {
        Err(InstantError::Format)
    }
}

impl Serialize for Instant {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Instant {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InstantVisitor;

        impl Visitor<'_> for InstantVisitor {
            type Value = Instant;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an RFC 3339 UTC instant with Z")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Instant, E> {
                // The error names the rule, never the text.
                text.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(InstantVisitor)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn instant(text: &str) -> Instant {
        text.parse().unwrap()
    }

    #[test]
    fn round_trips_through_json() {
        for text in [
            "2026-11-27T08:15:30Z",
            "2026-11-27T08:15:30.1Z",
            "2026-11-27T08:15:30.12Z",
            "2026-11-27T08:15:30.123456Z",
            "1970-01-01T00:00:00Z",
            "0001-01-01T00:00:00Z",
            "9999-12-30T22:00:00.999999Z",
        ] {
            let value = instant(text);
            let json = serde_json::to_value(value).unwrap();
            assert_eq!(json, Value::from(text));
            let back: Instant = serde_json::from_value(json).unwrap();
            assert_eq!(back, value);
        }
    }

    #[test]
    fn writes_the_shortest_fraction() {
        assert_eq!(
            instant("2026-11-27T08:15:30.100000Z").to_string(),
            "2026-11-27T08:15:30.1Z"
        );
        assert_eq!(
            instant("2026-11-27T08:15:30.000000Z").to_string(),
            "2026-11-27T08:15:30Z"
        );
    }

    #[test]
    fn keeps_microseconds_of_a_timestamp() {
        let timestamp = Timestamp::new(1_800_000_000, 123_456_000).unwrap();
        let value = Instant::from_timestamp(timestamp).unwrap();
        assert_eq!(value.timestamp(), timestamp);
        assert_eq!(value.to_string(), "2027-01-15T08:00:00.123456Z");
    }

    #[test]
    fn rejects_a_timestamp_below_one_microsecond() {
        let timestamp = Timestamp::new(1_800_000_000, 123_456_789).unwrap();
        assert_eq!(
            Instant::from_timestamp(timestamp),
            Err(InstantError::Precision)
        );
    }

    #[test]
    fn rejects_other_forms() {
        for text in [
            "",
            "2026-11-27",
            "2026-11-27T08:15:30",
            "2026-11-27T08:15:30+00:00",
            "2026-11-27T10:15:30+02:00",
            "2026-11-27T08:15:30z",
            "2026-11-27t08:15:30Z",
            "2026-11-27 08:15:30Z",
            "2026-11-27T08:15:30.Z",
            "2026-11-27T08:15:30.1234567Z",
            "2026-11-27T08:15:30.123456789Z",
            "2026-11-27T08:15:60Z",
            "2026-11-27T08:15Z",
            "+2026-11-27T08:15:30Z",
            "2026-11-27T08:15:30ZZ",
            " 2026-11-27T08:15:30Z",
            "2026-11-27T08:15:3aZ",
        ] {
            assert_eq!(
                text.parse::<Instant>(),
                Err(InstantError::Format),
                "{text:?}"
            );
        }
    }

    #[test]
    fn rejects_a_date_or_time_that_does_not_exist() {
        for text in [
            "2026-02-30T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-11-27T24:00:00Z",
            "2026-11-27T08:61:00Z",
            "9999-12-31T00:00:00Z",
        ] {
            assert_eq!(
                text.parse::<Instant>(),
                Err(InstantError::Range),
                "{text:?}"
            );
        }
    }

    #[test]
    fn rejects_a_json_value_that_is_not_a_string() {
        for value in [json!(1_800_000_000), json!(null), json!({"at": "x"})] {
            assert!(serde_json::from_value::<Instant>(value).is_err());
        }
        let err = serde_json::from_value::<Instant>(json!("2026-11-27T08:15:30+02:00"))
            .unwrap_err()
            .to_string();
        assert!(!err.contains("+02:00"), "{err}");
    }

    #[test]
    fn truncation_drops_the_part_below_one_microsecond() {
        let fine = Timestamp::new(1_800_000_000, 123_456_789).unwrap();
        let value = Instant::truncated(fine);
        assert_eq!(value.to_string(), "2027-01-15T08:00:00.123456Z");
        assert_eq!(value.timestamp().subsec_nanosecond(), 123_456_000);
        let before_epoch = Timestamp::new(-1, 999_999_999).unwrap();
        assert_eq!(
            Instant::truncated(before_epoch).to_string(),
            "1969-12-31T23:59:59.999999Z"
        );
        let exact = Timestamp::new(1_800_000_000, 5_000).unwrap();
        assert_eq!(Instant::truncated(exact).timestamp(), exact);
    }

    #[test]
    fn the_text_never_has_more_than_6_fractional_digits() {
        for nanos in [1, 999, 1_000, 999_999_999, 100_000_001] {
            let text =
                Instant::truncated(Timestamp::new(1_800_000_000, nanos).unwrap()).to_string();
            let digits = text
                .split_once('.')
                .map_or(0, |(_, rest)| rest.trim_end_matches('Z').len());
            assert!(digits <= 6, "{text}");
            assert_eq!(text.parse::<Instant>().unwrap().to_string(), text);
        }
    }

    #[test]
    fn the_schema_is_a_date_time_string() {
        let schema = serde_json::to_value(<Instant as utoipa::PartialSchema>::schema()).unwrap();
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["format"], "date-time");
        assert_eq!(
            schema["pattern"],
            r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,6})?Z$"
        );
    }
}
