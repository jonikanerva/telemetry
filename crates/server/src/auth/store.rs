//! The session store: at most [`SessionLimits::max_sessions`] sessions in
//! the memory of the process (ADR 0012).
//!
//! Rules:
//!
//! - A session is valid while `now < created + absolute` and
//!   `now < last_seen + idle`. At the deadline itself the session has
//!   ended.
//! - Each valid use moves `last_seen` to now (the idle time slides). The
//!   absolute deadline never moves.
//! - A new session gets a new random token. The store never takes a token
//!   from a request.
//! - When the store is full, a new session replaces the session with the
//!   oldest `created`. Ended sessions go first, at every access.
//! - A lookup compares the token with every stored token in constant time
//!   and does not stop at a match.
//! - A restart of the process ends every session, because the store lives
//!   in memory only.
//!
//! The store holds no lock across an await: the caller (`crate::auth`)
//! takes a `std::sync::Mutex` for one synchronous call.

use std::time::Duration;

use subtle::{Choice, ConditionallySelectable};
use tokio::time::Instant;

use super::clock::Now;
use super::token::{RandomError, SessionToken};

/// The limits of the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    /// The largest number of sessions at the same time.
    pub max_sessions: usize,
    /// The longest life of a session from its login.
    pub absolute: Duration,
    /// The longest time between two uses of a session.
    pub idle: Duration,
}

impl SessionLimits {
    /// The limits of the owner decision: 8 sessions, 30 days absolute, and
    /// 7 days idle.
    pub const OWNER: Self = Self {
        max_sessions: 8,
        absolute: Duration::from_hours(720),
        idle: Duration::from_hours(168),
    };
}

/// The deadlines of a valid session, on the monotonic clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// The absolute end of the session.
    pub absolute: Instant,
    /// The end of the session without another use. Never later than
    /// [`Deadlines::absolute`].
    pub idle: Instant,
}

impl Deadlines {
    /// The whole seconds from `now` to the absolute end: the `Max-Age` of
    /// the cookie.
    #[must_use]
    pub fn max_age_seconds(&self, now: Instant) -> u64 {
        self.absolute.saturating_duration_since(now).as_secs()
    }
}

#[derive(Debug)]
struct Entry {
    token: SessionToken,
    created: Instant,
    last_seen: Instant,
}

impl Entry {
    fn deadlines(&self, limits: &SessionLimits) -> Deadlines {
        let absolute = self.created + limits.absolute;
        let idle = (self.last_seen + limits.idle).min(absolute);
        Deadlines { absolute, idle }
    }

    fn is_valid(&self, limits: &SessionLimits, now: Instant) -> bool {
        now < self.deadlines(limits).idle
    }
}

/// Why a session left the store, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// An ended session was dropped.
    Expired,
    /// The store was full; the oldest session made room.
    Evicted,
}

/// A new session.
#[derive(Debug)]
pub struct Created {
    /// The token of the cookie.
    pub token: SessionToken,
    /// The deadlines of the session.
    pub deadlines: Deadlines,
    /// The number of sessions that the store dropped for the new one, with
    /// the reason.
    pub removed: Vec<Removal>,
}

/// The sessions of the process.
#[derive(Debug)]
pub struct SessionStore {
    limits: SessionLimits,
    entries: Vec<Entry>,
}

impl SessionStore {
    /// An empty store. A limit of zero sessions is raised to one, so a
    /// login can always create a session.
    #[must_use]
    pub fn new(limits: SessionLimits) -> Self {
        let limits = SessionLimits {
            max_sessions: limits.max_sessions.max(1),
            ..limits
        };
        Self {
            limits,
            entries: Vec::with_capacity(limits.max_sessions),
        }
    }

    /// The limits of the store.
    #[must_use]
    pub const fn limits(&self) -> SessionLimits {
        self.limits
    }

    /// The number of stored sessions, valid or not yet dropped.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the store holds no session.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Creates a session with a new token.
    ///
    /// # Errors
    ///
    /// Returns [`RandomError`] when the system gives no random bytes; the
    /// store does not change.
    pub fn create(&mut self, now: Now) -> Result<Created, RandomError> {
        let token = SessionToken::generate()?;
        let mut removed = self.drop_ended(now.mono);
        while self.entries.len() >= self.limits.max_sessions {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.created)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                self.entries.swap_remove(index);
                removed.push(Removal::Evicted);
            }
        }
        let entry = Entry {
            token: token.clone(),
            created: now.mono,
            last_seen: now.mono,
        };
        let deadlines = entry.deadlines(&self.limits);
        self.entries.push(entry);
        Ok(Created {
            token,
            deadlines,
            removed,
        })
    }

    /// Uses the session of `token`: when it is valid, moves its idle
    /// deadline and returns its deadlines. Returns `None` for an unknown
    /// or ended token.
    pub fn touch(&mut self, token: &SessionToken, now: Now) -> Option<Deadlines> {
        self.drop_ended(now.mono);
        let index = self.find(token)?;
        let limits = self.limits;
        let entry = self.entries.get_mut(index)?;
        entry.last_seen = now.mono;
        Some(entry.deadlines(&limits))
    }

    /// Removes the session of `token`. Returns true when it existed.
    pub fn remove(&mut self, token: &SessionToken) -> bool {
        match self.find(token) {
            Some(index) => {
                self.entries.swap_remove(index);
                true
            }
            None => false,
        }
    }

    /// Removes every session.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The index of `token`. Compares with every entry in constant time.
    fn find(&self, token: &SessionToken) -> Option<usize> {
        let mut found = Choice::from(0);
        let mut index = 0_u64;
        for (position, entry) in self.entries.iter().enumerate() {
            let matches = entry.token.ct_eq(token);
            let position = u64::try_from(position).unwrap_or(u64::MAX);
            index = u64::conditional_select(&index, &position, matches);
            found |= matches;
        }
        if bool::from(found) {
            usize::try_from(index).ok()
        } else {
            None
        }
    }

    fn drop_ended(&mut self, now: Instant) -> Vec<Removal> {
        let limits = self.limits;
        let before = self.entries.len();
        self.entries.retain(|entry| entry.is_valid(&limits, now));
        vec![Removal::Expired; before - self.entries.len()]
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;

    const DAY: Duration = Duration::from_hours(24);

    fn now() -> Now {
        Now {
            mono: Instant::now(),
            wall: Timestamp::UNIX_EPOCH,
        }
    }

    fn store() -> SessionStore {
        SessionStore::new(SessionLimits::OWNER)
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_session_is_valid_and_has_the_owner_deadlines() {
        let mut store = store();
        let start = now();
        let created = store.create(start).unwrap();
        assert_eq!(created.deadlines.absolute, start.mono + 30 * DAY);
        assert_eq!(created.deadlines.idle, start.mono + 7 * DAY);
        assert_eq!(created.deadlines.max_age_seconds(start.mono), 30 * 86_400);
        assert_eq!(store.touch(&created.token, now()), Some(created.deadlines));
    }

    #[tokio::test(start_paused = true)]
    async fn the_idle_deadline_ends_an_unused_session() {
        let mut store = store();
        let created = store.create(now()).unwrap();
        tokio::time::advance((7 * DAY).checked_sub(Duration::from_secs(1)).unwrap()).await;
        assert!(store.touch(&created.token, now()).is_some());
        // The use above moved the idle deadline by 7 days from now.
        tokio::time::advance((7 * DAY).checked_sub(Duration::from_secs(1)).unwrap()).await;
        assert!(store.touch(&created.token, now()).is_some());
        tokio::time::advance(7 * DAY).await;
        assert_eq!(store.touch(&created.token, now()), None);
        assert!(store.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_session_ends_at_the_idle_deadline_itself() {
        let mut store = store();
        let created = store.create(now()).unwrap();
        tokio::time::advance(7 * DAY).await;
        assert_eq!(store.touch(&created.token, now()), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_absolute_deadline_ends_a_session_in_daily_use() {
        let mut store = store();
        let start = now();
        let created = store.create(start).unwrap();
        for _ in 0..29 {
            tokio::time::advance(DAY).await;
            let deadlines = store.touch(&created.token, now()).unwrap();
            assert_eq!(deadlines.absolute, start.mono + 30 * DAY);
            assert!(deadlines.idle <= deadlines.absolute);
        }
        tokio::time::advance(DAY.checked_sub(Duration::from_secs(1)).unwrap()).await;
        let last = store.touch(&created.token, now()).unwrap();
        // The idle deadline never passes the absolute deadline.
        assert_eq!(last.idle, last.absolute);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(store.touch(&created.token, now()), None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_token_is_not_valid() {
        let mut store = store();
        store.create(now()).unwrap();
        let other = SessionToken::generate().unwrap();
        assert_eq!(store.touch(&other, now()), None);
        assert_eq!(store.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn remove_ends_only_that_session() {
        let mut store = store();
        let first = store.create(now()).unwrap();
        let second = store.create(now()).unwrap();
        assert!(store.remove(&first.token));
        assert!(!store.remove(&first.token));
        assert_eq!(store.touch(&first.token, now()), None);
        assert!(store.touch(&second.token, now()).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_store_replaces_the_oldest_session() {
        let mut store = store();
        let mut tokens = Vec::new();
        for _ in 0..8 {
            tokens.push(store.create(now()).unwrap().token);
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        // A use does not protect the oldest session: the rule is the age.
        assert!(store.touch(&tokens[0], now()).is_some());
        let ninth = store.create(now()).unwrap();
        assert_eq!(ninth.removed, vec![Removal::Evicted]);
        assert_eq!(store.len(), 8);
        assert_eq!(store.touch(&tokens[0], now()), None);
        for token in &tokens[1..] {
            assert!(store.touch(token, now()).is_some());
        }
        assert!(store.touch(&ninth.token, now()).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn ended_sessions_make_room_before_an_eviction() {
        let mut store = store();
        let old = store.create(now()).unwrap();
        tokio::time::advance(8 * DAY).await;
        let mut fresh = Vec::new();
        for _ in 0..7 {
            fresh.push(store.create(now()).unwrap());
        }
        let last = store.create(now()).unwrap();
        assert_eq!(last.removed, Vec::<Removal>::new());
        assert_eq!(store.touch(&old.token, now()), None);
        assert_eq!(store.len(), 8);
        for created in &fresh {
            assert!(store.touch(&created.token, now()).is_some());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn each_login_gets_a_different_token() {
        let mut store = store();
        let first = store.create(now()).unwrap();
        let second = store.create(now()).unwrap();
        assert_ne!(first.token, second.token);
    }

    #[tokio::test(start_paused = true)]
    async fn clear_ends_every_session() {
        let mut store = store();
        let created = store.create(now()).unwrap();
        store.clear();
        assert_eq!(store.touch(&created.token, now()), None);
    }

    #[test]
    fn a_zero_limit_still_allows_one_session() {
        let limits = SessionLimits {
            max_sessions: 0,
            ..SessionLimits::OWNER
        };
        assert_eq!(SessionStore::new(limits).limits().max_sessions, 1);
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        #[derive(Debug, Clone)]
        enum Step {
            Wait(u64),
            Create,
            Touch(usize),
            Remove(usize),
        }

        fn step() -> impl Strategy<Value = Step> {
            prop_oneof![
                (0_u64..(10 * 86_400)).prop_map(Step::Wait),
                Just(Step::Create),
                (0_usize..16).prop_map(Step::Touch),
                (0_usize..16).prop_map(Step::Remove),
            ]
        }

        /// A model of the rules, with plain integers of seconds.
        #[derive(Debug, Clone)]
        struct Model {
            created: u64,
            last_seen: u64,
            removed: bool,
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            #[test]
            fn the_store_follows_the_model(steps in proptest::collection::vec(step(), 1..60)) {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .start_paused(true)
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let limits = SessionLimits::OWNER;
                    let absolute = limits.absolute.as_secs();
                    let idle = limits.idle.as_secs();
                    let mut store = SessionStore::new(limits);
                    let mut issued: Vec<(SessionToken, Model)> = Vec::new();
                    let mut clock = 0_u64;
                    let valid = |model: &Model, at: u64| {
                        !model.removed && at < model.created + absolute && at < model.last_seen + idle
                    };
                    for step in steps {
                        match step {
                            Step::Wait(seconds) => {
                                tokio::time::advance(Duration::from_secs(seconds)).await;
                                clock += seconds;
                            }
                            Step::Create => {
                                // Distinct creation times keep the oldest
                                // session unique.
                                tokio::time::advance(Duration::from_secs(1)).await;
                                clock += 1;
                                let live = issued.iter().filter(|(_, model)| valid(model, clock)).count();
                                if live >= limits.max_sessions {
                                    // The oldest live session leaves.
                                    let oldest = issued
                                        .iter_mut()
                                        .filter(|(_, model)| valid(model, clock))
                                        .min_by_key(|(_, model)| model.created)
                                        .map(|(_, model)| model);
                                    if let Some(model) = oldest {
                                        model.removed = true;
                                    }
                                }
                                let created = store.create(now()).unwrap();
                                issued.push((created.token, Model { created: clock, last_seen: clock, removed: false }));
                            }
                            Step::Touch(pick) => {
                                if let Some((token, model)) = issued.get_mut(pick) {
                                    let expected = valid(model, clock);
                                    let actual = store.touch(token, now());
                                    assert_eq!(actual.is_some(), expected);
                                    if expected {
                                        model.last_seen = clock;
                                    } else {
                                        model.removed = true;
                                    }
                                }
                            }
                            Step::Remove(pick) => {
                                if let Some((token, model)) = issued.get_mut(pick) {
                                    let existed = store.remove(token);
                                    // An ended session can still be stored
                                    // until the next drop of ended sessions.
                                    if valid(model, clock) {
                                        assert!(existed);
                                    }
                                    model.removed = true;
                                }
                            }
                        }
                        assert!(store.len() <= limits.max_sessions);
                    }
                });
            }
        }
    }
}
