//! Failure classification of the database calls (ADR 0001, gate condition
//! 3 and "Delivery path and bounded buffering", step 5).
//!
//! Every failure becomes a [`StorageError`]: a [`FailureClass`] and an
//! [`ErrorCode`]. Both hold static text or a SQLSTATE only. The text of a
//! database error or of a library error is never kept, because it can
//! contain row values, a URL, or a credential (ADR 0001, "Logging").
//!
//! | Phase | Failure | Class |
//! | ----- | ------- | ----- |
//! | Connect (the pool opens a session) | SQLSTATE class 28 (authentication) | [`FailureClass::Answered`] |
//! | Connect | Every other failure: network, TLS, timeout, a server error before the session exists | [`FailureClass::NoAnswer`] |
//! | Statement (a session exists) | SQLSTATE class 22 or 23 | [`FailureClass::Data`] |
//! | Statement | Every other SQLSTATE, a row that does not decode, a missing or changed migration | [`FailureClass::Answered`] |
//! | Statement | The connection breaks (I/O, TLS, protocol), the pool closes | [`FailureClass::NoAnswer`] |
//!
//! [`StorageError::is_retryable`] is true for every class except
//! [`FailureClass::Data`]: ADR 0001 retries every error except the data
//! errors. The writer handles a data error itself: it splits the batch
//! ([`crate::storage::Writer`]).

use std::fmt;

/// The class of a database failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// No session answered: a network or TLS error, a timeout, a lost
    /// connection, or a server error before the session exists that is
    /// not an authentication error. ADR 0001 calls this "no answer". An
    /// attempt of this class adds no compute time, so the caller retries
    /// it on the fast cadence.
    NoAnswer,
    /// The database answered with a failure that is not a data error: an
    /// authentication error, a missing or changed migration, a missing
    /// schema, a full storage, a timeout of a statement, or any other
    /// SQLSTATE outside classes 22 and 23. ADR 0001 calls this an
    /// "answered failure". The caller retries it, and slows down after
    /// three equal codes in a row.
    Answered,
    /// SQLSTATE class 22 (data exception) or 23 (integrity constraint
    /// violation). Retrying the same rows gives the same error. The writer
    /// splits the batch to isolate the record.
    Data,
}

impl FailureClass {
    /// Static text for a log event.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoAnswer => "no_answer",
            Self::Answered => "answered",
            Self::Data => "data",
        }
    }
}

/// A SQLSTATE: five characters from `0`-`9` and `A`-`Z`.
///
/// PostgreSQL defines these codes. They hold no row values, so a log event
/// may contain them (ADR 0001, "Logging").
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SqlState([u8; 5]);

impl SqlState {
    /// Parses a SQLSTATE. Returns `None` when the text is not five
    /// characters from `0`-`9` and `A`-`Z`.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        let bytes: [u8; 5] = code.as_bytes().try_into().ok()?;
        bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
            .then_some(Self(bytes))
    }

    /// The code as text, for example `23505`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // The bytes are ASCII digits and capital letters (see `parse`).
        std::str::from_utf8(&self.0).unwrap_or("?????")
    }

    /// The class: the first two characters, for example `23`.
    #[must_use]
    pub fn class(&self) -> &str {
        self.as_str().get(..2).unwrap_or("")
    }
}

impl fmt::Debug for SqlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SqlState({})", self.as_str())
    }
}

impl fmt::Display for SqlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What failed. The text of each code is static or a SQLSTATE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The database answered with this SQLSTATE.
    Sqlstate(SqlState),
    /// An I/O error on the connection.
    Io,
    /// A TLS error, for example a certificate that does not verify.
    Tls,
    /// The pool found no free connection, or could not open one, before
    /// its acquire timeout.
    Timeout,
    /// The pool is closed.
    PoolClosed,
    /// The server sent a message that the client did not expect.
    Protocol,
    /// The connection settings are not valid.
    Configuration,
    /// A value could not be encoded, or a row could not be decoded.
    Decode,
    /// A migration that the binary embeds is not applied (ADR 0002,
    /// "Migrations").
    MigrationMissing,
    /// An applied migration has another checksum than the migration that
    /// the binary embeds.
    MigrationChanged,
    /// A migration failed earlier and left the history marked as dirty.
    MigrationDirty,
    /// The schema of the connection does not exist, or the `search_path`
    /// does not name it first.
    SchemaMissing,
    /// Any other failure of the client library.
    Other,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Sqlstate(state) => return state.fmt(f),
            Self::Io => "io",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::PoolClosed => "pool_closed",
            Self::Protocol => "protocol",
            Self::Configuration => "configuration",
            Self::Decode => "decode",
            Self::MigrationMissing => "migration_missing",
            Self::MigrationChanged => "migration_changed",
            Self::MigrationDirty => "migration_dirty",
            Self::SchemaMissing => "schema_missing",
            Self::Other => "other",
        };
        f.write_str(text)
    }
}

/// A failed database call: the class and the code. The text holds static
/// words and a SQLSTATE only, so a log event may show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[error("database {}: {code}", class.as_str())]
pub struct StorageError {
    /// How the caller treats the failure.
    pub class: FailureClass,
    /// What failed.
    pub code: ErrorCode,
}

impl StorageError {
    pub(crate) const fn new(class: FailureClass, code: ErrorCode) -> Self {
        Self { class, code }
    }

    /// True for every class except [`FailureClass::Data`] (ADR 0001,
    /// "Delivery path", step 5).
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        !matches!(self.class, FailureClass::Data)
    }

    /// The SQLSTATE, when the database answered with one.
    #[must_use]
    pub const fn sqlstate(&self) -> Option<SqlState> {
        match self.code {
            ErrorCode::Sqlstate(state) => Some(state),
            _ => None,
        }
    }

    /// True when the server answered: every class except
    /// [`FailureClass::NoAnswer`]. Only then may the connection of the call
    /// go back to the pool (`CallConnection`).
    pub(crate) const fn answered(self) -> bool {
        !matches!(self.class, FailureClass::NoAnswer)
    }
}

/// When a failure occurred. The class of a failure depends on it (module
/// documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The pool opens or hands out a connection.
    Connect,
    /// A statement runs on a session that exists.
    Statement,
}

/// Classifies an error of the client library. The error text is dropped.
pub(crate) fn classify(phase: Phase, error: &sqlx::Error) -> StorageError {
    use FailureClass::{Answered, Data, NoAnswer};

    let (class, code) = match error {
        sqlx::Error::Database(database) => {
            let state = database.code().as_deref().and_then(SqlState::parse);
            let code = state.map_or(ErrorCode::Other, ErrorCode::Sqlstate);
            let class = match (phase, state.as_ref().map(SqlState::class)) {
                (_, Some("28")) => Answered,
                (Phase::Connect, _) => NoAnswer,
                (Phase::Statement, Some("22" | "23")) => Data,
                (Phase::Statement, _) => Answered,
            };
            (class, code)
        }
        sqlx::Error::Io(_) => (NoAnswer, ErrorCode::Io),
        sqlx::Error::Tls(_) => (NoAnswer, ErrorCode::Tls),
        sqlx::Error::Protocol(_) => (NoAnswer, ErrorCode::Protocol),
        sqlx::Error::PoolTimedOut => (NoAnswer, ErrorCode::Timeout),
        sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed => (NoAnswer, ErrorCode::PoolClosed),
        sqlx::Error::Configuration(_) => (phase_class(phase), ErrorCode::Configuration),
        sqlx::Error::RowNotFound
        | sqlx::Error::TypeNotFound { .. }
        | sqlx::Error::ColumnIndexOutOfBounds { .. }
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Encode(_)
        | sqlx::Error::Decode(_) => (phase_class(phase), ErrorCode::Decode),
        _ => (phase_class(phase), ErrorCode::Other),
    };
    StorageError::new(class, code)
}

/// The class of a client-side failure: before the session exists it is "no
/// answer" (ADR 0001); after that, the server has answered.
const fn phase_class(phase: Phase) -> FailureClass {
    match phase {
        Phase::Connect => FailureClass::NoAnswer,
        Phase::Statement => FailureClass::Answered,
    }
}

/// Classifies an error of the `SQLx` migrator.
pub(crate) fn classify_migrate(error: &sqlx::migrate::MigrateError) -> StorageError {
    use sqlx::migrate::MigrateError;

    match error {
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => {
            classify(Phase::Statement, error)
        }
        MigrateError::VersionMissing(_) => {
            StorageError::new(FailureClass::Answered, ErrorCode::MigrationMissing)
        }
        MigrateError::VersionMismatch(_) => {
            StorageError::new(FailureClass::Answered, ErrorCode::MigrationChanged)
        }
        MigrateError::Dirty(_) => {
            StorageError::new(FailureClass::Answered, ErrorCode::MigrationDirty)
        }
        _ => StorageError::new(FailureClass::Answered, ErrorCode::Other),
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::error::Error as StdError;
    use std::fmt;

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    /// A database error with a SQLSTATE and a message that holds a value.
    #[derive(Debug)]
    struct FakeDatabaseError(&'static str);

    impl fmt::Display for FakeDatabaseError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("value TESTVIN0000000001 is not valid")
        }
    }

    impl StdError for FakeDatabaseError {}

    impl DatabaseError for FakeDatabaseError {
        fn message(&self) -> &'static str {
            "value TESTVIN0000000001 is not valid"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }

        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn database(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(FakeDatabaseError(code)))
    }

    fn class(phase: Phase, error: &sqlx::Error) -> FailureClass {
        classify(phase, error).class
    }

    #[test]
    fn authentication_errors_are_answered_in_both_phases() {
        for phase in [Phase::Connect, Phase::Statement] {
            for code in ["28P01", "28000"] {
                assert_eq!(class(phase, &database(code)), FailureClass::Answered);
            }
        }
    }

    #[test]
    fn other_server_errors_before_the_session_are_no_answer() {
        for code in ["53300", "57P03", "3D000", "08006", "XX000", "22021"] {
            assert_eq!(
                class(Phase::Connect, &database(code)),
                FailureClass::NoAnswer,
                "{code}"
            );
        }
    }

    #[test]
    fn data_errors_are_classes_22_and_23_in_a_session() {
        for code in ["22021", "22P05", "22003", "23505", "23514", "23503"] {
            let error = classify(Phase::Statement, &database(code));
            assert_eq!(error.class, FailureClass::Data, "{code}");
            assert!(!error.is_retryable());
            assert_eq!(
                error.sqlstate().map(|state| state.to_string()),
                Some(code.to_owned())
            );
        }
        for code in [
            "53100", "57014", "40001", "40P01", "42P01", "54000", "XX000",
        ] {
            let error = classify(Phase::Statement, &database(code));
            assert_eq!(error.class, FailureClass::Answered, "{code}");
            assert!(error.is_retryable());
        }
    }

    #[test]
    fn transport_errors_are_no_answer_in_both_phases() {
        for phase in [Phase::Connect, Phase::Statement] {
            for error in [
                sqlx::Error::Io(std::io::Error::other("x")),
                sqlx::Error::Tls("x".into()),
                sqlx::Error::Protocol("x".to_owned()),
                sqlx::Error::PoolTimedOut,
                sqlx::Error::PoolClosed,
            ] {
                assert_eq!(class(phase, &error), FailureClass::NoAnswer, "{error:?}");
            }
        }
    }

    #[test]
    fn client_errors_depend_on_the_phase() {
        let error = sqlx::Error::Decode("x".into());
        assert_eq!(class(Phase::Connect, &error), FailureClass::NoAnswer);
        assert_eq!(class(Phase::Statement, &error), FailureClass::Answered);
        assert_eq!(classify(Phase::Statement, &error).code, ErrorCode::Decode);
    }

    #[test]
    fn a_code_that_is_not_a_sqlstate_becomes_other() {
        for code in ["", "2350", "235051", "23a05", "23 05"] {
            assert_eq!(SqlState::parse(code), None, "{code:?}");
            assert_eq!(
                classify(Phase::Statement, &database_owned(code)).code,
                ErrorCode::Other
            );
        }
    }

    fn database_owned(code: &str) -> sqlx::Error {
        database(Box::leak(code.to_owned().into_boxed_str()))
    }

    #[test]
    fn the_error_text_holds_no_database_text() {
        let error = classify(Phase::Statement, &database("23514"));
        let text = format!("{error} {error:?}");
        assert_eq!(error.to_string(), "database data: 23514");
        assert!(!text.contains("TESTVIN"), "{text}");
        let error = classify(Phase::Connect, &sqlx::Error::Tls("secret host".into()));
        assert_eq!(error.to_string(), "database no_answer: tls");
    }

    #[test]
    fn migration_errors_are_answered() {
        use sqlx::migrate::MigrateError;
        assert_eq!(
            classify_migrate(&MigrateError::VersionMismatch(1)),
            StorageError::new(FailureClass::Answered, ErrorCode::MigrationChanged)
        );
        assert_eq!(
            classify_migrate(&MigrateError::Dirty(1)),
            StorageError::new(FailureClass::Answered, ErrorCode::MigrationDirty)
        );
        assert_eq!(
            classify_migrate(&MigrateError::Execute(database("42501"))).class,
            FailureClass::Answered
        );
    }
}
