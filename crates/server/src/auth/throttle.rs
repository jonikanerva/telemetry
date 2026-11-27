//! The global login throttle (ADR 0003, ADR 0012).
//!
//! Two limits apply to every login attempt of every client together. The
//! limit is not for each IP address: behind the Railway proxy an address
//! is not a reliable identity.
//!
//! 1. One verification slot: at most one password verification runs at a
//!    time. The slot is a `Semaphore(1)`. The handler takes it with
//!    `try_acquire`, so a second attempt waits for nothing and gets 429.
//!    The verification task owns the permit until Argon2 ends, also when
//!    the client leaves, so the memory bound holds for work that nobody
//!    waits for.
//! 2. A token bucket: [`ThrottleLimits::capacity`] attempts, and one more
//!    after each [`ThrottleLimits::refill`]. An empty bucket gives 429
//!    with the time to the next token.
//!
//! An attempt with a malformed request costs no token: the handler checks
//! the request before it asks the throttle.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::problem::RetryAfter;

/// The limits of the throttle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleLimits {
    /// The largest number of tokens. At least one.
    pub capacity: u32,
    /// The time for one new token. At least one second.
    pub refill: Duration,
}

impl ThrottleLimits {
    /// 5 attempts at once, then one attempt each 12 seconds: at most 300
    /// attempts an hour.
    pub const DEFAULT: Self = Self {
        capacity: 5,
        refill: Duration::from_secs(12),
    };

    /// The `Retry-After` in seconds of a busy verification slot. A
    /// verification takes well below one second at the accepted
    /// parameters.
    pub const BUSY_RETRY_SECONDS: u32 = 1;
}

/// The bucket arithmetic, apart from the clock and the lock, so the tests
/// can check it with any time sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    limits: ThrottleLimits,
    tokens: u32,
    /// The time from which the next token grows.
    since: Instant,
}

impl Bucket {
    /// A full bucket at `now`.
    #[must_use]
    pub fn new(limits: ThrottleLimits, now: Instant) -> Self {
        let limits = ThrottleLimits {
            capacity: limits.capacity.max(1),
            refill: limits.refill.max(Duration::from_secs(1)),
        };
        Self {
            limits,
            tokens: limits.capacity,
            since: now,
        }
    }

    /// Takes one token at `now`, or returns the time to wait for the next
    /// token. A time before the last call counts as the last call.
    ///
    /// # Errors
    ///
    /// Returns the wait in whole seconds, rounded up, from 1 to
    /// [`RetryAfter::MAX_SECONDS`], when the bucket is empty.
    pub fn take(&mut self, now: Instant) -> Result<(), u32> {
        self.refill(now);
        if self.tokens > 0 {
            self.tokens -= 1;
            if self.tokens + 1 == self.limits.capacity {
                // The bucket was full, so no time has grown a token yet.
                self.since = now.max(self.since);
            }
            return Ok(());
        }
        let grown = now.saturating_duration_since(self.since);
        let wait = self.limits.refill.saturating_sub(grown);
        Err(retry_after(wait))
    }

    fn refill(&mut self, now: Instant) {
        if self.tokens >= self.limits.capacity {
            return;
        }
        let elapsed = now.saturating_duration_since(self.since);
        let refill_nanos = self.limits.refill.as_nanos().max(1);
        let grown = elapsed.as_nanos() / refill_nanos;
        let room = self.limits.capacity - self.tokens;
        let added = u32::try_from(grown).unwrap_or(u32::MAX).min(room);
        self.tokens += added;
        if self.tokens >= self.limits.capacity {
            self.since = now;
        } else {
            // `added` is below the capacity, so the product fits.
            self.since += self.limits.refill * added;
        }
    }
}

/// Whole seconds, rounded up, from 1 to the largest `Retry-After`.
fn retry_after(wait: Duration) -> u32 {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    u32::try_from(seconds)
        .unwrap_or(RetryAfter::MAX_SECONDS)
        .clamp(1, RetryAfter::MAX_SECONDS)
}

/// Why the throttle refused an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Another verification runs now.
    Busy,
    /// The bucket is empty. The value is the wait in seconds.
    Rate(u32),
}

impl Refusal {
    /// The `Retry-After` of the refusal.
    #[must_use]
    pub const fn retry_after(self) -> Option<RetryAfter> {
        match self {
            Self::Busy => RetryAfter::from_seconds(ThrottleLimits::BUSY_RETRY_SECONDS),
            Self::Rate(seconds) => RetryAfter::from_seconds(seconds),
        }
    }

    /// A static word for the log.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Rate(_) => "rate",
        }
    }
}

/// The permit of the one verification slot. Move it into the blocking
/// task of the verification and drop it when the verification ends.
#[derive(Debug)]
pub struct VerificationPermit(
    #[expect(dead_code, reason = "the permit is held for its drop")] OwnedSemaphorePermit,
);

/// The login throttle of the process.
#[derive(Debug)]
pub struct LoginThrottle {
    limits: ThrottleLimits,
    bucket: Mutex<Bucket>,
    slot: Arc<Semaphore>,
}

impl LoginThrottle {
    /// A throttle with a full bucket and a free slot.
    #[must_use]
    pub fn new(limits: ThrottleLimits, now: Instant) -> Self {
        Self {
            limits,
            bucket: Mutex::new(Bucket::new(limits, now)),
            slot: Arc::new(Semaphore::new(1)),
        }
    }

    /// Admits one verification at `now`: takes the slot, then a token. A
    /// busy slot costs no token.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] when the slot is busy or the bucket is empty.
    pub fn admit(&self, now: Instant) -> Result<VerificationPermit, Refusal> {
        let permit = Arc::clone(&self.slot)
            .try_acquire_owned()
            .map_err(|_| Refusal::Busy)?;
        self.bucket
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(now)
            .map_err(Refusal::Rate)?;
        Ok(VerificationPermit(permit))
    }

    /// Fills the bucket. Only the test server uses it, between two browser
    /// tests; a running verification keeps its slot.
    pub fn reset(&self, now: Instant) {
        *self.bucket.lock().unwrap_or_else(PoisonError::into_inner) = Bucket::new(self.limits, now);
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const REFILL: Duration = Duration::from_secs(12);

    #[tokio::test(start_paused = true)]
    async fn five_attempts_pass_then_one_each_twelve_seconds() {
        let start = Instant::now();
        let mut bucket = Bucket::new(ThrottleLimits::DEFAULT, start);
        for _ in 0..5 {
            assert_eq!(bucket.take(start), Ok(()));
        }
        assert_eq!(bucket.take(start), Err(12));
        let later = start + Duration::from_millis(4_500);
        // 7.5 s are left; the wait rounds up.
        assert_eq!(bucket.take(later), Err(8));
        assert_eq!(bucket.take(start + REFILL), Ok(()));
        assert_eq!(bucket.take(start + REFILL), Err(12));
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_pause_fills_the_bucket_only_to_its_capacity() {
        let start = Instant::now();
        let mut bucket = Bucket::new(ThrottleLimits::DEFAULT, start);
        for _ in 0..5 {
            bucket.take(start).unwrap();
        }
        let much_later = start + Duration::from_hours(24);
        for _ in 0..5 {
            assert_eq!(bucket.take(much_later), Ok(()));
        }
        assert!(bucket.take(much_later).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_busy_slot_refuses_without_a_token() {
        let throttle = LoginThrottle::new(
            ThrottleLimits {
                capacity: 1,
                refill: REFILL,
            },
            Instant::now(),
        );
        let permit = throttle.admit(Instant::now()).unwrap();
        assert_eq!(throttle.admit(Instant::now()).unwrap_err(), Refusal::Busy);
        drop(permit);
        // The busy refusal took no token, so after one refill period the
        // next attempt passes.
        let after = Instant::now() + REFILL;
        assert!(throttle.admit(after).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_bucket_refuses_with_the_rate_reason() {
        let throttle = LoginThrottle::new(ThrottleLimits::DEFAULT, Instant::now());
        for _ in 0..5 {
            drop(throttle.admit(Instant::now()).unwrap());
        }
        let refusal = throttle.admit(Instant::now()).unwrap_err();
        assert_eq!(refusal.reason(), "rate");
        assert_eq!(refusal.retry_after().map(RetryAfter::seconds), Some(12));
        throttle.reset(Instant::now());
        assert!(throttle.admit(Instant::now()).is_ok());
    }

    #[test]
    fn retry_after_rounds_up_and_stays_in_bounds() {
        assert_eq!(retry_after(Duration::ZERO), 1);
        assert_eq!(retry_after(Duration::from_millis(1)), 1);
        assert_eq!(retry_after(Duration::from_millis(1_001)), 2);
        assert_eq!(retry_after(Duration::from_secs(12)), 12);
        assert_eq!(
            retry_after(Duration::from_hours(240)),
            RetryAfter::MAX_SECONDS
        );
        assert!(RetryAfter::from_seconds(retry_after(Duration::ZERO)).is_some());
    }

    proptest! {
        /// For any attempt times: the granted attempts up to a time `t`
        /// never exceed the capacity plus one per refill period, and each
        /// refusal names a wait after which an attempt passes.
        #[test]
        fn the_bucket_bounds_attempts_and_its_wait_is_exact(
            gaps in proptest::collection::vec(0_u64..30_000, 1..200),
            capacity in 1_u32..8,
            refill_secs in 1_u64..30,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .start_paused(true)
                .build()
                .unwrap();
            runtime.block_on(async move {
                let limits = ThrottleLimits { capacity, refill: Duration::from_secs(refill_secs) };
                let start = Instant::now();
                let mut bucket = Bucket::new(limits, start);
                let mut now = start;
                let mut granted = 0_u64;
                for gap in gaps {
                    now += Duration::from_millis(gap);
                    match bucket.take(now) {
                        Ok(()) => granted += 1,
                        Err(wait) => {
                            let mut probe = bucket;
                            assert!(RetryAfter::from_seconds(wait).is_some());
                            let at = now + Duration::from_secs(u64::from(wait));
                            assert!(probe.take(at).is_ok(), "the named wait must be enough");
                            if wait > 1 {
                                let mut early = bucket;
                                let before = at - Duration::from_secs(1);
                                assert!(early.take(before).is_err(), "the wait must not be one second too long");
                            }
                        }
                    }
                    let elapsed = (now - start).as_secs();
                    let bound = u64::from(capacity) + elapsed / refill_secs;
                    assert!(granted <= bound, "{granted} > {bound}");
                }
            });
        }
    }
}
