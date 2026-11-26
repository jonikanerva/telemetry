//! Start-up configuration.
//!
//! The process reads and validates its configuration here, once, before it
//! starts any work. Each command reads only the variables that it uses.

use std::ffi::OsString;
use std::fmt;

/// Configuration of the `run` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunConfig {
    /// TCP port of the health endpoint, from `PORT`. Railway sets `PORT`.
    /// Port 0 asks the operating system for a free port. Tests use it.
    pub port: u16,
}

impl RunConfig {
    /// Reads and validates the configuration of the `run` command.
    ///
    /// `var` returns the value of one variable. The binary passes
    /// `std::env::var_os`. Tests pass their own values.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a variable is missing or not valid.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, ConfigError> {
        let port = var("PORT").ok_or(ConfigError::Missing { name: "PORT" })?;
        // `u16::from_str` also accepts a leading `+`. Accept only digits.
        let port = port
            .to_str()
            .filter(|text| text.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|text| text.parse::<u16>().ok())
            .ok_or(ConfigError::Invalid {
                name: "PORT",
                reason: "expected only digits, a value from 0 to 65535",
            })?;
        Ok(Self { port })
    }
}

/// A configuration variable is missing or not valid.
///
/// The fields hold only static text. The type cannot hold the value of a
/// variable, so the error text cannot show a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The variable is not set.
    #[error("{name} is not set")]
    Missing {
        /// Name of the variable.
        name: &'static str,
    },
    /// The variable is set, but its value is not valid.
    #[error("{name} is not valid: {reason}")]
    Invalid {
        /// Name of the variable.
        name: &'static str,
        /// The rule that the value breaks.
        reason: &'static str,
    },
}

/// A secret configuration value, for example a database URL with a password.
///
/// `Debug` writes `[redacted]`. The type does not implement `Display` or
/// `serde::Serialize`, so a log event or an error cannot show the value by
/// accident. Call [`SecretString::expose`] only where the value is used.
pub struct SecretString(String);

impl SecretString {
    /// Wraps a secret value.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Returns the secret value.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn run_config(vars: &[(&str, &str)]) -> Result<RunConfig, ConfigError> {
        let vars: HashMap<&str, OsString> = vars
            .iter()
            .map(|(name, value)| (*name, OsString::from(value)))
            .collect();
        RunConfig::from_vars(|name| vars.get(name).cloned())
    }

    #[test]
    fn reads_a_valid_port() {
        assert_eq!(
            run_config(&[("PORT", "8080")]),
            Ok(RunConfig { port: 8080 })
        );
        assert_eq!(
            run_config(&[("PORT", "65535")]),
            Ok(RunConfig { port: 65535 })
        );
    }

    #[test]
    fn accepts_port_zero() {
        assert_eq!(run_config(&[("PORT", "0")]), Ok(RunConfig { port: 0 }));
    }

    #[test]
    fn rejects_a_missing_port() {
        assert_eq!(run_config(&[]), Err(ConfigError::Missing { name: "PORT" }));
    }

    #[test]
    fn rejects_a_port_that_is_not_valid() {
        for value in [
            "", " 8080", "8080 ", "+80", "-1", "65536", "80.0", "0x50", "http",
        ] {
            let error = run_config(&[("PORT", value)]).unwrap_err();
            assert!(
                matches!(error, ConfigError::Invalid { name: "PORT", .. }),
                "value {value:?} gave {error:?}"
            );
        }
    }

    #[test]
    fn error_text_does_not_contain_the_value() {
        let value = "postgres://user:hunter2@example.invalid/db";
        let error = run_config(&[("PORT", value)]).unwrap_err();
        let text = format!("{error} {error:?}");
        assert!(text.contains("PORT"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("example.invalid"), "{text}");
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let secret = SecretString::new("hunter2".to_owned());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{:?}", Some(&secret)), "Some([redacted])");
        assert_eq!(secret.expose(), "hunter2");
    }
}
