//! The clock of the session store and the login throttle.
//!
//! The rules measure elapsed time with the monotonic clock of Tokio
//! (`STACK.md` §10), so a change of the system time cannot extend or end
//! a session. The wall clock gives only the instants of the response
//! (`expires_at`, `idle_expires_at`): the instant of a deadline is the
//! wall time now plus the monotonic time to the deadline.
//!
//! Tests pause the Tokio clock and move it with `tokio::time::advance`.
//! The test server cannot pause its clock, so [`ShiftableClock`] adds an
//! offset that a test-only route moves forward.

use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tokio::time::Instant;

/// One reading of both clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    /// The monotonic time, for every rule.
    pub mono: Instant,
    /// The wall time, for the instants of a response only.
    pub wall: Timestamp,
}

impl Now {
    /// The wall instant of the monotonic `deadline`. A deadline in the
    /// past gives the current wall time. A deadline beyond the range of
    /// `Timestamp` gives the largest timestamp; the session lifetimes are
    /// days, so this does not occur.
    #[must_use]
    pub fn wall_at(&self, deadline: Instant) -> Timestamp {
        let ahead = deadline.saturating_duration_since(self.mono);
        SignedDuration::try_from(ahead)
            .ok()
            .and_then(|ahead| self.wall.checked_add(ahead).ok())
            .unwrap_or(Timestamp::MAX)
    }
}

/// A source of [`Now`].
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current time.
    fn now(&self) -> Now;
}

/// The clock of `serve`: Tokio's monotonic clock and the system time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Now {
        Now {
            mono: Instant::now(),
            wall: Timestamp::now(),
        }
    }
}

/// A clock with an offset that only moves forward. The wall time follows
/// the monotonic time from a fixed start, so both clocks move together.
#[derive(Debug)]
pub struct ShiftableClock {
    start_mono: Instant,
    start_wall: Timestamp,
    offset: Mutex<Duration>,
}

impl ShiftableClock {
    /// A clock that starts at `start_wall` now.
    #[must_use]
    pub fn new(start_wall: Timestamp) -> Self {
        Self {
            start_mono: Instant::now(),
            start_wall,
            offset: Mutex::new(Duration::ZERO),
        }
    }

    /// Moves the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        let mut offset = self
            .offset
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *offset = offset.saturating_add(by);
    }

    /// Sets the offset back to zero. Only the test server uses it, between
    /// two browser tests.
    pub fn reset(&self) {
        *self
            .offset
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Duration::ZERO;
    }

    fn offset(&self) -> Duration {
        *self
            .offset
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Clock for ShiftableClock {
    fn now(&self) -> Now {
        let real = Instant::now();
        let offset = self.offset();
        let mono = real.checked_add(offset).unwrap_or(real);
        let since_start = mono.saturating_duration_since(self.start_mono);
        let wall = SignedDuration::try_from(since_start)
            .ok()
            .and_then(|elapsed| self.start_wall.checked_add(elapsed).ok())
            .unwrap_or(Timestamp::MAX);
        Now { mono, wall }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn the_shiftable_clock_moves_with_tokio_and_with_the_offset() {
        let clock = ShiftableClock::new(wall("2026-11-27T10:00:00Z"));
        let first = clock.now();
        assert_eq!(first.wall, wall("2026-11-27T10:00:00Z"));
        tokio::time::advance(Duration::from_mins(1)).await;
        assert_eq!(clock.now().wall, wall("2026-11-27T10:01:00Z"));
        clock.advance(Duration::from_hours(1));
        let later = clock.now();
        assert_eq!(later.wall, wall("2026-11-27T11:01:00Z"));
        assert_eq!(later.mono - first.mono, Duration::from_mins(61));
        clock.reset();
        assert_eq!(clock.now().wall, wall("2026-11-27T10:01:00Z"));
    }

    #[tokio::test(start_paused = true)]
    async fn wall_at_adds_the_time_to_the_deadline() {
        let now = Now {
            mono: Instant::now(),
            wall: wall("2026-11-27T10:00:00Z"),
        };
        let deadline = now.mono + Duration::from_hours(168);
        assert_eq!(now.wall_at(deadline), wall("2026-12-04T10:00:00Z"));
        // A past deadline gives the current wall time.
        tokio::time::advance(Duration::from_secs(10)).await;
        let later = Now {
            mono: Instant::now(),
            wall: wall("2026-11-27T10:00:10Z"),
        };
        assert_eq!(later.wall_at(now.mono), later.wall);
    }
}
