//! Command-line parsing. The commands have no options, so the parser is a
//! small match and needs no library.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// A command of the `telemetry-web-server` binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Serve the API and the SPA on `0.0.0.0:$PORT` until a shutdown
    /// signal arrives.
    Serve,
    /// Write the `OpenAPI` document to the path. Reads no environment, no
    /// database, and no SPA file.
    ExportOpenapi(PathBuf),
    /// Serve the same router as [`Command::Serve`] plus the test-only
    /// routes on `127.0.0.1:$PORT`. Only a build with the feature
    /// `test-server` has this command.
    #[cfg(feature = "test-server")]
    ServeTest,
}

/// The arguments do not name one known command with its arguments.
///
/// The error text does not repeat the arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("expected one command: serve, export-openapi <path>, or serve-test (feature test-server)")]
pub struct UsageError;

/// Parses the arguments that follow the program name.
///
/// ```
/// use std::ffi::OsString;
/// use telemetry_web_server::cli::{Command, parse};
///
/// assert_eq!(parse([OsString::from("serve")]), Ok(Command::Serve));
/// assert!(parse([OsString::from("run")]).is_err());
/// ```
///
/// # Errors
///
/// Returns [`UsageError`] for an unknown command, a missing or extra
/// argument, or a command that is not UTF-8.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let command = match args.next().as_deref().and_then(OsStr::to_str) {
        Some("serve") => Command::Serve,
        #[cfg(feature = "test-server")]
        Some("serve-test") => Command::ServeTest,
        Some("export-openapi") => match args.next() {
            Some(path) if !path.is_empty() => Command::ExportOpenapi(PathBuf::from(path)),
            _ => return Err(UsageError),
        },
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
    fn parses_serve() {
        assert_eq!(parse(args(&["serve"])), Ok(Command::Serve));
    }

    #[test]
    fn parses_export_openapi_with_a_path() {
        assert_eq!(
            parse(args(&["export-openapi", "contract/openapi.json"])),
            Ok(Command::ExportOpenapi(PathBuf::from(
                "contract/openapi.json"
            )))
        );
    }

    #[test]
    fn rejects_export_openapi_without_a_path() {
        assert_eq!(parse(args(&["export-openapi"])), Err(UsageError));
        assert_eq!(parse(args(&["export-openapi", ""])), Err(UsageError));
    }

    #[test]
    fn rejects_a_missing_or_unknown_command() {
        assert_eq!(parse(args(&[])), Err(UsageError));
        assert_eq!(parse(args(&["run"])), Err(UsageError));
        assert_eq!(parse(args(&["SERVE"])), Err(UsageError));
    }

    #[test]
    fn rejects_an_extra_argument() {
        assert_eq!(parse(args(&["serve", "--verbose"])), Err(UsageError));
        assert_eq!(parse(args(&["export-openapi", "a", "b"])), Err(UsageError));
    }

    #[test]
    fn rejects_a_command_that_is_not_utf8() {
        assert_eq!(parse([OsString::from_vec(vec![0xff])]), Err(UsageError));
    }

    #[cfg(feature = "test-server")]
    #[test]
    fn parses_serve_test_with_the_feature() {
        assert_eq!(parse(args(&["serve-test"])), Ok(Command::ServeTest));
    }

    #[cfg(not(feature = "test-server"))]
    #[test]
    fn rejects_serve_test_without_the_feature() {
        assert_eq!(parse(args(&["serve-test"])), Err(UsageError));
    }
}
