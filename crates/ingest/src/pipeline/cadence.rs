//! The retry cadence of the writer and of the start-up check (ADR 0001,
//! "Delivery path", step 5).
//!
//! | Failure | Next attempt |
//! | ------- | ------------ |
//! | No answer ([`is_no_answer`]) | after 1 s, doubling to at most 5 minutes, with jitter |
//! | Answered, other than the failure before | as for no answer |
//! | Answered with the same code for the third time in a row | after 60 minutes, until an attempt succeeds |
//!
//! Decisions that ADR 0001 leaves open (ADR 0001 records them):
//!
//! - **What counts as no answer for the cadence.** The storage classes
//!   ([`FailureClass`]) follow the session: SQLSTATE class `08`
//!   (connection exception) and `57P` (operator intervention, for example
//!   `57P01` when Neon restarts the compute) come from a session, so they
//!   are answered. They are transient and need no manual fix, so the
//!   cadence and gate condition 3 treat them as no answer. Without this
//!   rule, three compute restarts in a row would park the writer at the
//!   60-minute cadence, which is longer than the vehicle buffer.
//! - **Progress resets the row.** A write that stored records before it
//!   failed ([`crate::storage::WriteError::committed`] above 0) made
//!   progress. It starts the cadence again from the first delay, so only
//!   attempts without progress count toward the three equal failures.
//! - **Jitter.** Each delay of the doubling cadence gets 0 % to 10 % more,
//!   from the random source of the operating system. Without random bytes,
//!   the delay has no jitter. The 60-minute delay has no jitter.

use std::time::Duration;

use super::Limits;
use crate::storage::{ErrorCode, FailureClass, StorageError};

/// True when the cadence and gate condition 3 treat the failure as no
/// answer (module documentation).
#[must_use]
pub fn is_no_answer(error: &StorageError) -> bool {
    error.class == FailureClass::NoAnswer
        || error
            .sqlstate()
            .is_some_and(|state| state.class() == "08" || state.as_str().starts_with("57P"))
}

/// The state of the cadence.
#[derive(Debug, Clone)]
pub struct Cadence {
    first: Duration,
    max: Duration,
    answered_repeats: u32,
    answered_delay: Duration,
    next: Duration,
    answered: Option<(ErrorCode, u32)>,
}

impl Cadence {
    /// A cadence with the delays of `limits`, at its first delay.
    #[must_use]
    pub const fn new(limits: &Limits) -> Self {
        Self {
            first: limits.retry_first,
            max: limits.retry_max,
            answered_repeats: limits.answered_repeats,
            answered_delay: limits.answered_delay,
            next: limits.retry_first,
            answered: None,
        }
    }

    /// An attempt succeeded, or made progress: the next failure waits the
    /// first delay again.
    pub const fn reset(&mut self) {
        self.next = self.first;
        self.answered = None;
    }

    /// An attempt failed with `error`. Returns the delay before the next
    /// attempt. `jitter` is a fraction from 0 to 1 of the 10 % jitter.
    pub fn failed(&mut self, error: &StorageError, jitter: f64) -> Duration {
        if is_no_answer(error) {
            self.answered = None;
        } else {
            let repeats = match self.answered {
                Some((code, repeats)) if code == error.code => repeats.saturating_add(1),
                _ => 1,
            };
            self.answered = Some((error.code, repeats));
            if repeats >= self.answered_repeats {
                return self.answered_delay;
            }
        }
        let delay = self.next;
        self.next = delay.saturating_mul(2).min(self.max);
        delay + delay.mul_f64(jitter.clamp(0.0, 1.0) / 10.0)
    }

    /// True when the cadence waits the long delay of equal answered
    /// failures.
    #[must_use]
    pub fn is_slow(&self) -> bool {
        self.answered
            .is_some_and(|(_, repeats)| repeats >= self.answered_repeats)
    }
}

/// A jitter fraction from 0 to 1 from the random source of the operating
/// system, or 0 when it gives no bytes.
#[must_use]
pub fn random_jitter() -> f64 {
    let mut bytes = [0_u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.0;
    }
    f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqlState;

    fn no_answer() -> StorageError {
        StorageError {
            class: FailureClass::NoAnswer,
            code: ErrorCode::Io,
        }
    }

    fn answered(code: &str) -> StorageError {
        StorageError {
            class: FailureClass::Answered,
            code: ErrorCode::Sqlstate(SqlState::parse(code).unwrap()),
        }
    }

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn no_answer_doubles_from_1_s_to_5_minutes() {
        let mut cadence = Cadence::new(&Limits::PRODUCTION);
        let delays: Vec<Duration> = (0..12).map(|_| cadence.failed(&no_answer(), 0.0)).collect();
        assert_eq!(
            delays,
            [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300, 300].map(secs)
        );
        assert!(!cadence.is_slow());
        cadence.reset();
        assert_eq!(cadence.failed(&no_answer(), 0.0), secs(1));
    }

    #[test]
    fn the_jitter_adds_at_most_10_percent() {
        let mut cadence = Cadence::new(&Limits::PRODUCTION);
        assert_eq!(
            cadence.failed(&no_answer(), 1.0),
            Duration::from_millis(1100)
        );
        assert_eq!(
            cadence.failed(&no_answer(), 0.5),
            Duration::from_millis(2100)
        );
        for _ in 0..100 {
            let jitter = random_jitter();
            assert!((0.0..=1.0).contains(&jitter), "{jitter}");
        }
    }

    #[test]
    fn three_equal_answered_failures_slow_the_cadence_to_60_minutes() {
        let mut cadence = Cadence::new(&Limits::PRODUCTION);
        let auth = answered("28P01");
        assert_eq!(cadence.failed(&auth, 0.0), secs(1));
        assert_eq!(cadence.failed(&auth, 0.0), secs(2));
        assert_eq!(cadence.failed(&auth, 0.0), secs(3600));
        assert!(cadence.is_slow());
        assert_eq!(cadence.failed(&auth, 0.0), secs(3600));
        // Only a success ends the slow cadence.
        cadence.reset();
        assert!(!cadence.is_slow());
        assert_eq!(cadence.failed(&auth, 0.0), secs(1));
    }

    #[test]
    fn a_different_failure_breaks_the_row() {
        let mut cadence = Cadence::new(&Limits::PRODUCTION);
        assert_eq!(cadence.failed(&answered("28P01"), 0.0), secs(1));
        assert_eq!(cadence.failed(&answered("28P01"), 0.0), secs(2));
        assert_eq!(cadence.failed(&answered("53100"), 0.0), secs(4));
        assert_eq!(cadence.failed(&no_answer(), 0.0), secs(8));
        assert_eq!(cadence.failed(&answered("53100"), 0.0), secs(16));
        assert_eq!(cadence.failed(&answered("53100"), 0.0), secs(32));
        assert_eq!(cadence.failed(&answered("53100"), 0.0), secs(3600));
    }

    #[test]
    fn connection_and_operator_failures_count_as_no_answer() {
        for code in ["57P01", "57P02", "57P03", "08006", "08003"] {
            assert!(is_no_answer(&answered(code)), "{code}");
        }
        for code in ["28P01", "57014", "53100", "40P01", "42501", "57000"] {
            assert!(!is_no_answer(&answered(code)), "{code}");
        }
        // Three compute restarts in a row keep the fast cadence.
        let mut cadence = Cadence::new(&Limits::PRODUCTION);
        for expected in [1, 2, 4, 8] {
            assert_eq!(cadence.failed(&answered("57P01"), 0.0), secs(expected));
        }
    }
}
