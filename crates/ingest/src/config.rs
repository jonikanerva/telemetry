//! Start-up configuration.
//!
//! The process reads and validates its configuration here, once, before it
//! starts any work. Each command reads only the variables that it uses.
//!
//! Variables of the `run` command:
//!
//! | Variable                | Content                                                         |
//! | ----------------------- | --------------------------------------------------------------- |
//! | `PORT`                  | Port of `/health`. Railway sets it. 0 asks for a free port.     |
//! | `INGEST_GATE_PORT`      | Public port of the gate, 4443 in production. 0 asks for a free port. |
//! | `INGEST_UPSTREAM_PORT`  | Loopback port of `fleet-telemetry`, 4444 in production.         |
//! | `INGEST_CHILD_PROGRAM`  | Absolute path of the `fleet-telemetry` program.                 |
//! | `INGEST_CHILD_CONFIG`   | Absolute path of its configuration file (`-config`).            |
//!
//! The three ports must differ. A gate port equal to the upstream port
//! would make the gate forward each connection to itself.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use crate::child::ChildCommand;

/// Configuration of the `run` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    /// TCP port of the health endpoint, from `PORT`. Railway sets `PORT`.
    /// Port 0 asks the operating system for a free port. Tests use it.
    pub port: u16,
    /// Public TCP port of the gate, from `INGEST_GATE_PORT`. Port 0 asks
    /// the operating system for a free port.
    pub gate_port: u16,
    /// Loopback TCP port of `fleet-telemetry`, from `INGEST_UPSTREAM_PORT`.
    pub upstream_port: u16,
    /// The child program and its configuration file, from
    /// `INGEST_CHILD_PROGRAM` and `INGEST_CHILD_CONFIG`.
    pub child: ChildCommand,
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
        let port = parse_port(&var, "PORT")?;
        let gate_port = parse_port(&var, "INGEST_GATE_PORT")?;
        let upstream_port = parse_port(&var, "INGEST_UPSTREAM_PORT")?;
        if upstream_port == 0 {
            return Err(ConfigError::Invalid {
                name: "INGEST_UPSTREAM_PORT",
                reason: "expected a port from 1 to 65535",
            });
        }
        if gate_port == upstream_port || port == upstream_port {
            return Err(ConfigError::Invalid {
                name: "INGEST_UPSTREAM_PORT",
                reason: "must differ from PORT and INGEST_GATE_PORT",
            });
        }
        if gate_port != 0 && gate_port == port {
            return Err(ConfigError::Invalid {
                name: "INGEST_GATE_PORT",
                reason: "must differ from PORT",
            });
        }
        let child = ChildCommand {
            program: absolute_path(&var, "INGEST_CHILD_PROGRAM")?,
            config: absolute_path(&var, "INGEST_CHILD_CONFIG")?,
        };
        Ok(Self {
            port,
            gate_port,
            upstream_port,
            child,
        })
    }
}

fn parse_port(
    var: &impl Fn(&str) -> Option<OsString>,
    name: &'static str,
) -> Result<u16, ConfigError> {
    let value = var(name).ok_or(ConfigError::Missing { name })?;
    // `u16::from_str` also accepts a leading `+`. Accept only digits.
    value
        .to_str()
        .filter(|text| text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u16>().ok())
        .ok_or(ConfigError::Invalid {
            name,
            reason: "expected only digits, a value from 0 to 65535",
        })
}

fn absolute_path(
    var: &impl Fn(&str) -> Option<OsString>,
    name: &'static str,
) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(var(name).ok_or(ConfigError::Missing { name })?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(ConfigError::Invalid {
            name,
            reason: "expected an absolute path",
        })
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

    const VALID: &[(&str, &str)] = &[
        ("PORT", "8080"),
        ("INGEST_GATE_PORT", "4443"),
        ("INGEST_UPSTREAM_PORT", "4444"),
        ("INGEST_CHILD_PROGRAM", "/fleet-telemetry"),
        ("INGEST_CHILD_CONFIG", "/etc/fleet-telemetry/config.json"),
    ];

    /// A change that removes the variable.
    const UNSET: &str = "<unset>";

    /// The valid variables with the given changes.
    fn run_config(changes: &[(&str, &str)]) -> Result<RunConfig, ConfigError> {
        let mut vars: HashMap<&str, OsString> = VALID
            .iter()
            .map(|(name, value)| (*name, OsString::from(value)))
            .collect();
        for (name, value) in changes {
            if *value == UNSET {
                vars.remove(name);
            } else {
                vars.insert(name, OsString::from(value));
            }
        }
        RunConfig::from_vars(|name| vars.get(name).cloned())
    }

    fn invalid(error: ConfigError) -> &'static str {
        match error {
            ConfigError::Invalid { name, .. } => name,
            ConfigError::Missing { name } => panic!("{name} is missing, expected invalid"),
        }
    }

    #[test]
    fn reads_a_valid_configuration() {
        assert_eq!(
            run_config(&[]),
            Ok(RunConfig {
                port: 8080,
                gate_port: 4443,
                upstream_port: 4444,
                child: ChildCommand {
                    program: PathBuf::from("/fleet-telemetry"),
                    config: PathBuf::from("/etc/fleet-telemetry/config.json"),
                },
            })
        );
        assert_eq!(run_config(&[("PORT", "65535")]).unwrap().port, 65535);
    }

    #[test]
    fn accepts_port_zero_for_the_health_and_the_gate_port() {
        let config = run_config(&[("PORT", "0"), ("INGEST_GATE_PORT", "0")]).unwrap();
        assert_eq!((config.port, config.gate_port), (0, 0));
    }

    #[test]
    fn rejects_a_missing_variable() {
        for (name, _) in VALID {
            assert_eq!(
                run_config(&[(name, UNSET)]),
                Err(ConfigError::Missing { name }),
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_a_port_that_is_not_valid() {
        for name in ["PORT", "INGEST_GATE_PORT", "INGEST_UPSTREAM_PORT"] {
            for value in [
                "", " 8080", "8080 ", "+80", "-1", "65536", "80.0", "0x50", "http",
            ] {
                let error = run_config(&[(name, value)]).unwrap_err();
                assert_eq!(invalid(error), name, "value {value:?} gave {error:?}");
            }
        }
    }

    #[test]
    fn rejects_ports_that_are_equal() {
        let error = run_config(&[("INGEST_GATE_PORT", "4444")]).unwrap_err();
        assert_eq!(invalid(error), "INGEST_UPSTREAM_PORT");
        let error = run_config(&[("PORT", "4444")]).unwrap_err();
        assert_eq!(invalid(error), "INGEST_UPSTREAM_PORT");
        let error = run_config(&[("PORT", "4443")]).unwrap_err();
        assert_eq!(invalid(error), "INGEST_GATE_PORT");
        let error = run_config(&[("INGEST_UPSTREAM_PORT", "0")]).unwrap_err();
        assert_eq!(invalid(error), "INGEST_UPSTREAM_PORT");
    }

    #[test]
    fn rejects_a_relative_path() {
        for name in ["INGEST_CHILD_PROGRAM", "INGEST_CHILD_CONFIG"] {
            for value in ["", "fleet-telemetry", "./config.json", "../x"] {
                let error = run_config(&[(name, value)]).unwrap_err();
                assert_eq!(invalid(error), name, "value {value:?}");
            }
        }
    }

    #[test]
    fn error_text_does_not_contain_the_value() {
        let value = "postgres://user:hunter2@example.invalid/db";
        for name in ["PORT", "INGEST_CHILD_PROGRAM"] {
            let error = run_config(&[(name, value)]).unwrap_err();
            let text = format!("{error} {error:?}");
            assert!(text.contains(name), "{text}");
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains("example.invalid"), "{text}");
        }
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let secret = SecretString::new("hunter2".to_owned());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{:?}", Some(&secret)), "Some([redacted])");
        assert_eq!(secret.expose(), "hunter2");
    }
}
