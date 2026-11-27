//! Time ranges and buckets.

use jiff::{SignedDuration, Timestamp};

/// A range that ends at the time of the request. The length is a fixed
/// number of hours, not calendar days, so a change of daylight saving
/// time does not change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeKey {
    /// 24 hours.
    Day,
    /// 168 hours.
    Week,
    /// 720 hours.
    Month,
    /// 2160 hours.
    Quarter,
    /// 8760 hours.
    Year,
}

impl RangeKey {
    /// Every range.
    pub const ALL: [Self; 5] = [
        Self::Day,
        Self::Week,
        Self::Month,
        Self::Quarter,
        Self::Year,
    ];

    /// The length of the range in hours.
    #[must_use]
    pub const fn hours(self) -> i64 {
        match self {
            Self::Day => 24,
            Self::Week => 168,
            Self::Month => 720,
            Self::Quarter => 2_160,
            Self::Year => 8_760,
        }
    }

    /// The width of one bucket in seconds. Each range has at most about
    /// 1500 buckets, so a series has a bounded number of points.
    #[must_use]
    pub const fn bucket_seconds(self) -> u32 {
        match self {
            Self::Day => 60,
            Self::Week => 600,
            Self::Month => 3_600,
            Self::Quarter => 10_800,
            Self::Year => 43_200,
        }
    }
}

/// The origin of the buckets: 2000-01-01T00:00:00Z. It is the origin of
/// the PostgreSQL `date_bin` calls of the server.
pub const BUCKET_ORIGIN_SECONDS: i64 = 946_684_800;

/// The range of one request: `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    key: RangeKey,
    from: Timestamp,
    to: Timestamp,
}

/// A range end so early that the range start is not a valid time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the range start is out of the time range")]
pub struct WindowError;

impl Window {
    /// The range `key` that ends at `to`.
    ///
    /// # Errors
    ///
    /// Returns [`WindowError`] when the start is before the earliest
    /// `Timestamp`.
    pub fn ending_at(key: RangeKey, to: Timestamp) -> Result<Self, WindowError> {
        let from = to
            .checked_sub(SignedDuration::from_hours(key.hours()))
            .map_err(|_| WindowError)?;
        Ok(Self { key, from, to })
    }

    /// The range key.
    #[must_use]
    pub const fn key(&self) -> RangeKey {
        self.key
    }

    /// The start, inclusive.
    #[must_use]
    pub const fn from(&self) -> Timestamp {
        self.from
    }

    /// The end, exclusive.
    #[must_use]
    pub const fn to(&self) -> Timestamp {
        self.to
    }

    /// True when `at` is in `[from, to)`.
    #[must_use]
    pub fn contains(&self, at: Timestamp) -> bool {
        self.from <= at && at < self.to
    }

    /// The width of one bucket.
    #[must_use]
    pub const fn bucket(&self) -> SignedDuration {
        SignedDuration::from_secs(self.key.bucket_seconds() as i64)
    }

    /// The start of the bucket that holds `at`: the latest whole multiple
    /// of the bucket width since [`BUCKET_ORIGIN_SECONDS`] at or before
    /// `at`. It equals PostgreSQL `date_bin(width, at, origin)`.
    #[must_use]
    pub fn bucket_start(&self, at: Timestamp) -> Timestamp {
        let width = i128::from(self.key.bucket_seconds()) * 1_000_000_000;
        let origin = i128::from(BUCKET_ORIGIN_SECONDS) * 1_000_000_000;
        let offset = at.as_nanosecond() - origin;
        let start = origin + offset.div_euclid(width) * width;
        // The start is not later than `at`, and not earlier than one bucket
        // before it, so it is a valid timestamp for every `at` that is at
        // least one bucket after the earliest timestamp.
        Timestamp::from_nanosecond(start).unwrap_or(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn each_range_has_a_fixed_length() {
        // Across the change to summer time in Finland: still 24 hours. The
        // other lengths are written out in the next test.
        let to = at("2026-03-29T12:00:00Z");
        let window = Window::ending_at(RangeKey::Day, to).unwrap();
        assert_eq!(window.from(), at("2026-03-28T12:00:00Z"));
    }

    /// The values of the contract (`dto::RangeKey`) and of ADR 0013,
    /// written out, so a change of one value fails here.
    #[test]
    fn the_ranges_have_the_documented_lengths_and_buckets() {
        let to = at("2026-11-27T12:00:00Z");
        for (key, from, bucket) in [
            (RangeKey::Day, "2026-11-26T12:00:00Z", 60),
            (RangeKey::Week, "2026-11-20T12:00:00Z", 600),
            (RangeKey::Month, "2026-10-28T12:00:00Z", 3_600),
            (RangeKey::Quarter, "2026-08-29T12:00:00Z", 10_800),
            (RangeKey::Year, "2025-11-27T12:00:00Z", 43_200),
        ] {
            let window = Window::ending_at(key, to).unwrap();
            assert_eq!(window.from(), at(from), "{key:?}");
            assert_eq!(window.to(), to);
            assert_eq!(key.bucket_seconds(), bucket, "{key:?}");
            assert_eq!(
                window.bucket(),
                SignedDuration::from_secs(i64::from(bucket))
            );
        }
    }

    #[test]
    fn each_range_has_at_most_1500_buckets() {
        // A window that does not start on a bucket edge touches one more
        // bucket: 1,441, 1,009, 721, 721, and 731.
        let most: Vec<i64> = RangeKey::ALL
            .into_iter()
            .map(|key| key.hours() * 3_600 / i64::from(key.bucket_seconds()) + 1)
            .collect();
        assert_eq!(most, [1_441, 1_009, 721, 721, 731]);
        assert!(most.iter().all(|buckets| *buckets <= 1_500));
    }

    #[test]
    fn the_window_is_half_open() {
        let window = Window::ending_at(RangeKey::Day, at("2026-11-27T12:00:00Z")).unwrap();
        assert!(window.contains(window.from()));
        assert!(!window.contains(window.to()));
    }

    #[test]
    fn buckets_align_to_the_origin() {
        let window = Window::ending_at(RangeKey::Quarter, at("2026-11-27T12:00:00Z")).unwrap();
        assert_eq!(
            window.bucket_start(at("2026-11-27T11:59:59.999999Z")),
            at("2026-11-27T09:00:00Z")
        );
        assert_eq!(
            window.bucket_start(at("2026-11-27T09:00:00Z")),
            at("2026-11-27T09:00:00Z")
        );
        // Before the origin the bucket still starts at or before the time.
        assert_eq!(
            window.bucket_start(at("1999-12-31T23:59:59Z")),
            at("1999-12-31T21:00:00Z")
        );
    }

    #[test]
    fn a_too_early_end_is_an_error() {
        assert_eq!(
            Window::ending_at(RangeKey::Year, Timestamp::MIN),
            Err(WindowError)
        );
    }
}
