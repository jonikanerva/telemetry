//! Command-line parsing. The binary has two commands and no options.

use std::ffi::{OsStr, OsString};

/// A command of the `ingest` binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Run the gate, the child `fleet-telemetry`, and the health endpoint
    /// until a shutdown signal arrives.
    Run,
    /// Apply the database migrations. The repository has no migrations yet.
    Migrate,
}

/// The arguments do not name exactly one known command.
///
/// The error text does not repeat the arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("expected exactly one command: run or migrate")]
pub struct UsageError;

/// Parses the arguments that follow the program name.
///
/// ```
/// use std::ffi::OsString;
/// use ingest::cli::{Command, parse};
///
/// assert_eq!(parse([OsString::from("run")]), Ok(Command::Run));
/// assert!(parse([OsString::from("serve")]).is_err());
/// ```
///
/// # Errors
///
/// Returns [`UsageError`] when the arguments are not exactly `run` or
/// `migrate`.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let command = match args.next().as_deref().and_then(OsStr::to_str) {
        Some("run") => Command::Run,
        Some("migrate") => Command::Migrate,
        _ => return Err(UsageError),
    };
    if args.next().is_some() {
        return Err(UsageError);
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_run() {
        assert_eq!(parse(args(&["run"])), Ok(Command::Run));
    }

    #[test]
    fn parses_migrate() {
        assert_eq!(parse(args(&["migrate"])), Ok(Command::Migrate));
    }

    #[test]
    fn rejects_a_missing_command() {
        assert_eq!(parse(args(&[])), Err(UsageError));
    }

    #[test]
    fn rejects_an_unknown_command() {
        assert_eq!(parse(args(&["serve"])), Err(UsageError));
        assert_eq!(parse(args(&["RUN"])), Err(UsageError));
    }

    #[test]
    fn rejects_an_extra_argument() {
        assert_eq!(parse(args(&["run", "--verbose"])), Err(UsageError));
    }

    #[test]
    fn rejects_an_argument_that_is_not_utf8() {
        assert_eq!(parse([OsString::from_vec(vec![0xff])]), Err(UsageError));
    }
}
