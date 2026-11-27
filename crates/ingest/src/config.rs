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
//! | `INGEST_ZMQ_PORT`       | Loopback port of the ZMQ PUB socket of `fleet-telemetry` (`zmq.addr`), 5284 in production. |
//! | `INGEST_METRICS_PORT`   | Loopback port of the Prometheus metrics of `fleet-telemetry` (`monitoring.prometheus_metrics_port`), 9090 in production. |
//! | `INGEST_CHILD_PROGRAM`  | Absolute path of the `fleet-telemetry` program.                 |
//! | `INGEST_CHILD_CONFIG`   | Absolute path of its configuration file (`-config`).            |
//! | `DATABASE_URL`          | URL of the role `telemetry_ingest` (ADR 0002). A secret.        |
//! | `INGEST_FLUSH_INTERVAL_SECONDS` | Optional. The flush interval of ADR 0001, from 1 to 900. Default 900. |
//!
//! The ports must differ, except that `PORT` and `INGEST_GATE_PORT` can
//! both be 0. A gate port equal to the upstream port would make the gate
//! forward each connection to itself. The ZMQ and the metrics ports are on
//! the loopback interface; the process never connects to another host for
//! them.
//!
//! The flush interval has an upper bound of 900 s: a longer interval would
//! make the crash loss larger than the bound of ADR 0001 ("Delivery
//! guarantee"). A shorter interval costs Neon compute time (ADR 0001, "Why
//! 15 minutes").
//!
//! # Test overrides
//!
//! The end-to-end tests run the real binary with shorter limits and a
//! schema of their own ([`TestOverrides`]). Only a debug build reads these
//! variables (`src/main.rs`). A release build, which is the build of the
//! image, never reads them, so no variable can weaken a limit in
//! production. Each override can only make its limit smaller.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::child::ChildCommand;
use crate::lifecycle::{BUDGET, ShutdownBudget};
use crate::pipeline::Limits;
use crate::storage::Schema;

/// The default flush interval (ADR 0001, lead decision 1). It is also the
/// largest allowed value.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_mins(15);

/// Configuration of the `run` command.
#[derive(Debug)]
pub struct RunConfig {
    /// TCP port of the health endpoint, from `PORT`. Railway sets `PORT`.
    /// Port 0 asks the operating system for a free port. Tests use it.
    pub port: u16,
    /// Public TCP port of the gate, from `INGEST_GATE_PORT`. Port 0 asks
    /// the operating system for a free port.
    pub gate_port: u16,
    /// Loopback TCP port of `fleet-telemetry`, from `INGEST_UPSTREAM_PORT`.
    pub upstream_port: u16,
    /// Loopback TCP port of the ZMQ PUB socket, from `INGEST_ZMQ_PORT`.
    pub zmq_port: u16,
    /// Loopback TCP port of the metrics of `fleet-telemetry`, from
    /// `INGEST_METRICS_PORT`.
    pub metrics_port: u16,
    /// The child program and its configuration file, from
    /// `INGEST_CHILD_PROGRAM` and `INGEST_CHILD_CONFIG`.
    pub child: ChildCommand,
    /// The URL of the role `telemetry_ingest`, from `DATABASE_URL`. The
    /// storage always uses `sslmode=verify-full`, whatever the URL says.
    pub database_url: SecretString,
    /// The flush interval, from `INGEST_FLUSH_INTERVAL_SECONDS`.
    pub flush_interval: Duration,
    /// The schema of the raw tables: `raw` (ADR 0002), or the schema of a
    /// test ([`TestOverrides`]).
    pub schema: Schema,
    /// The bounds of the pipeline: [`Limits::PRODUCTION`], or shorter
    /// limits of a test.
    pub limits: Limits,
    /// The bounds of the shutdown steps: [`BUDGET`], or a shorter flush
    /// step of a test.
    pub budget: ShutdownBudget,
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
        let zmq_port = loopback_port(&var, "INGEST_ZMQ_PORT", &[port, gate_port, upstream_port])?;
        let metrics_port = loopback_port(
            &var,
            "INGEST_METRICS_PORT",
            &[port, gate_port, upstream_port, zmq_port],
        )?;
        let child = ChildCommand {
            program: absolute_path(&var, "INGEST_CHILD_PROGRAM")?,
            config: absolute_path(&var, "INGEST_CHILD_CONFIG")?,
        };
        let database_url = secret_url(&var, "DATABASE_URL")?;
        let flush_interval = match var("INGEST_FLUSH_INTERVAL_SECONDS") {
            None => DEFAULT_FLUSH_INTERVAL,
            Some(value) => Duration::from_secs(parse_number(
                &value,
                "INGEST_FLUSH_INTERVAL_SECONDS",
                1..=DEFAULT_FLUSH_INTERVAL.as_secs(),
                "expected only digits, a value from 1 to 900",
            )?),
        };
        Ok(Self {
            port,
            gate_port,
            upstream_port,
            zmq_port,
            metrics_port,
            child,
            database_url,
            flush_interval,
            schema: Schema::raw(),
            limits: Limits::PRODUCTION,
            budget: BUDGET,
        })
    }

    /// Applies the overrides of a test (module documentation).
    #[must_use]
    pub fn with_overrides(mut self, overrides: TestOverrides) -> Self {
        if let Some(schema) = overrides.schema {
            self.schema = schema;
        }
        if let Some(bytes) = overrides.queue_bytes {
            self.limits.queue_bytes = bytes;
        }
        if let Some(after) = overrides.degraded_after {
            self.limits.degraded_after = after;
        }
        if let Some(max) = overrides.retry_max {
            self.limits.retry_max = max;
            self.limits.retry_first = self.limits.retry_first.min(max);
        }
        if let Some(period) = overrides.shortfall_period {
            self.limits.shortfall_period = period;
        }
        if let Some(flush) = overrides.flush_deadline {
            self.budget.flush = flush;
        }
        self
    }
}

/// Shorter limits and the schema of an end-to-end test (module
/// documentation). Each value can only be smaller than the production
/// limit.
///
/// | Variable | Limit | Range |
/// | -------- | ----- | ----- |
/// | `INGEST_TEST_SCHEMA` | schema of the raw tables (`raw`) | a plain identifier ([`Schema::new`]) |
/// | `INGEST_TEST_QUEUE_BYTES` | queue bound (64 MiB) | 1,024 to 67,108,864 |
/// | `INGEST_TEST_DEGRADED_AFTER_MS` | no-answer time before degraded mode (120 s) | 1 to 120,000 |
/// | `INGEST_TEST_RETRY_MAX_MS` | longest retry delay (5 minutes) | 1 to 300,000 |
/// | `INGEST_TEST_SHORTFALL_PERIOD_MS` | period of the shortfall check (15 s) | 1 to 15,000 |
/// | `INGEST_TEST_FLUSH_DEADLINE_MS` | bound of the shutdown flush (20 s) | 1 to 20,000 |
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestOverrides {
    /// `INGEST_TEST_SCHEMA`.
    pub schema: Option<Schema>,
    /// `INGEST_TEST_QUEUE_BYTES`.
    pub queue_bytes: Option<usize>,
    /// `INGEST_TEST_DEGRADED_AFTER_MS`.
    pub degraded_after: Option<Duration>,
    /// `INGEST_TEST_RETRY_MAX_MS`.
    pub retry_max: Option<Duration>,
    /// `INGEST_TEST_SHORTFALL_PERIOD_MS`.
    pub shortfall_period: Option<Duration>,
    /// `INGEST_TEST_FLUSH_DEADLINE_MS`.
    pub flush_deadline: Option<Duration>,
}

impl TestOverrides {
    /// Reads the overrides. `var` works as in [`RunConfig::from_vars`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a variable is set and not valid.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, ConfigError> {
        let production = Limits::PRODUCTION;
        let millis = |name: &'static str, max: Duration| -> Result<Option<Duration>, ConfigError> {
            let max = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
            var(name)
                .map(|value| {
                    parse_number(
                        &value,
                        name,
                        1..=max,
                        "expected a number of milliseconds within the production limit",
                    )
                    .map(Duration::from_millis)
                })
                .transpose()
        };
        let schema = var("INGEST_TEST_SCHEMA")
            .map(|value| {
                value
                    .to_str()
                    .and_then(Schema::new)
                    .ok_or(ConfigError::Invalid {
                        name: "INGEST_TEST_SCHEMA",
                        reason: "expected a plain identifier",
                    })
            })
            .transpose()?;
        let max_bytes = u64::try_from(production.queue_bytes).unwrap_or(u64::MAX);
        let queue_bytes = var("INGEST_TEST_QUEUE_BYTES")
            .map(|value| {
                parse_number(
                    &value,
                    "INGEST_TEST_QUEUE_BYTES",
                    1024..=max_bytes,
                    "expected a number of bytes from 1024 to the production limit",
                )
                .and_then(|bytes| {
                    usize::try_from(bytes).map_err(|_| ConfigError::Invalid {
                        name: "INGEST_TEST_QUEUE_BYTES",
                        reason: "too large for this platform",
                    })
                })
            })
            .transpose()?;
        Ok(Self {
            schema,
            queue_bytes,
            degraded_after: millis("INGEST_TEST_DEGRADED_AFTER_MS", production.degraded_after)?,
            retry_max: millis("INGEST_TEST_RETRY_MAX_MS", production.retry_max)?,
            shortfall_period: millis(
                "INGEST_TEST_SHORTFALL_PERIOD_MS",
                production.shortfall_period,
            )?,
            flush_deadline: millis("INGEST_TEST_FLUSH_DEADLINE_MS", BUDGET.flush)?,
        })
    }

    /// True when no override is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
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

/// A port from 1 to 65535 that differs from each port of `others`.
fn loopback_port(
    var: &impl Fn(&str) -> Option<OsString>,
    name: &'static str,
    others: &[u16],
) -> Result<u16, ConfigError> {
    let port = parse_port(var, name)?;
    if port == 0 {
        return Err(ConfigError::Invalid {
            name,
            reason: "expected a port from 1 to 65535",
        });
    }
    if others.contains(&port) {
        return Err(ConfigError::Invalid {
            name,
            reason: "must differ from the other ports",
        });
    }
    Ok(port)
}

/// A number of ASCII digits only, inside `range`.
fn parse_number(
    value: &OsString,
    name: &'static str,
    range: std::ops::RangeInclusive<u64>,
    reason: &'static str,
) -> Result<u64, ConfigError> {
    value
        .to_str()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|number| range.contains(number))
        .ok_or(ConfigError::Invalid { name, reason })
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

/// A non-empty UTF-8 URL that holds a secret. The error does not hold the
/// value.
fn secret_url(
    var: &impl Fn(&str) -> Option<OsString>,
    name: &'static str,
) -> Result<SecretString, ConfigError> {
    let value = var(name).ok_or(ConfigError::Missing { name })?;
    value
        .into_string()
        .ok()
        .filter(|url| !url.is_empty())
        .map(SecretString::new)
        .ok_or(ConfigError::Invalid {
            name,
            reason: "expected a non-empty UTF-8 URL",
        })
}

/// Configuration of the `migrate` command.
#[derive(Debug)]
pub struct MigrateConfig {
    /// The URL of the role `telemetry_migrator`, from
    /// `MIGRATION_DATABASE_URL` (ADR 0002, "Roles and privileges"). The
    /// storage always uses `sslmode=verify-full`, whatever the URL says.
    pub database_url: SecretString,
}

impl MigrateConfig {
    /// Reads the configuration of the `migrate` command. `var` works as in
    /// [`RunConfig::from_vars`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the variable is missing, empty, or not
    /// UTF-8.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, ConfigError> {
        Ok(Self {
            database_url: secret_url(&var, "MIGRATION_DATABASE_URL")?,
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
        ("INGEST_ZMQ_PORT", "5284"),
        ("INGEST_METRICS_PORT", "9090"),
        ("INGEST_CHILD_PROGRAM", "/fleet-telemetry"),
        ("INGEST_CHILD_CONFIG", "/etc/fleet-telemetry/config.json"),
        (
            "DATABASE_URL",
            "postgresql://telemetry_ingest:hunter2@db.invalid/neondb",
        ),
    ];

    /// A change that removes the variable.
    const UNSET: &str = "<unset>";

    fn vars(changes: &[(&str, &str)]) -> HashMap<String, OsString> {
        let mut vars: HashMap<String, OsString> = VALID
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect();
        for (name, value) in changes {
            if *value == UNSET {
                vars.remove(*name);
            } else {
                vars.insert((*name).to_owned(), OsString::from(value));
            }
        }
        vars
    }

    /// The valid variables with the given changes.
    fn run_config(changes: &[(&str, &str)]) -> Result<RunConfig, ConfigError> {
        let vars = vars(changes);
        RunConfig::from_vars(|name| vars.get(name).cloned())
    }

    fn overrides(values: &[(&str, &str)]) -> Result<TestOverrides, ConfigError> {
        let vars: HashMap<&str, OsString> = values
            .iter()
            .map(|(name, value)| (*name, OsString::from(value)))
            .collect();
        TestOverrides::from_vars(|name| vars.get(name).cloned())
    }

    fn invalid(error: ConfigError) -> &'static str {
        match error {
            ConfigError::Invalid { name, .. } => name,
            ConfigError::Missing { name } => panic!("{name} is missing, expected invalid"),
        }
    }

    #[test]
    fn reads_a_valid_configuration() {
        let config = run_config(&[]).unwrap();
        assert_eq!(
            (
                config.port,
                config.gate_port,
                config.upstream_port,
                config.zmq_port,
                config.metrics_port
            ),
            (8080, 4443, 4444, 5284, 9090)
        );
        assert_eq!(
            config.child,
            ChildCommand {
                program: PathBuf::from("/fleet-telemetry"),
                config: PathBuf::from("/etc/fleet-telemetry/config.json"),
            }
        );
        assert_eq!(
            config.database_url.expose(),
            "postgresql://telemetry_ingest:hunter2@db.invalid/neondb"
        );
        // The production values when no override applies.
        assert_eq!(config.flush_interval, Duration::from_mins(15));
        assert_eq!(config.schema, Schema::raw());
        assert_eq!(config.limits, Limits::PRODUCTION);
        assert_eq!(config.budget, BUDGET);
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
                run_config(&[(name, UNSET)]).unwrap_err(),
                ConfigError::Missing { name },
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_a_port_that_is_not_valid() {
        for name in [
            "PORT",
            "INGEST_GATE_PORT",
            "INGEST_UPSTREAM_PORT",
            "INGEST_ZMQ_PORT",
            "INGEST_METRICS_PORT",
        ] {
            for value in [
                "", " 8080", "8080 ", "+80", "-1", "65536", "80.0", "0x50", "http",
            ] {
                let error = run_config(&[(name, value)]).unwrap_err();
                assert_eq!(invalid(error), name, "value {value:?} gave {error:?}");
            }
        }
        for name in ["INGEST_ZMQ_PORT", "INGEST_METRICS_PORT"] {
            assert_eq!(invalid(run_config(&[(name, "0")]).unwrap_err()), name);
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
        for (name, value) in [
            ("INGEST_ZMQ_PORT", "8080"),
            ("INGEST_ZMQ_PORT", "4443"),
            ("INGEST_ZMQ_PORT", "4444"),
            ("INGEST_METRICS_PORT", "8080"),
            ("INGEST_METRICS_PORT", "4443"),
            ("INGEST_METRICS_PORT", "4444"),
            ("INGEST_METRICS_PORT", "5284"),
        ] {
            let error = run_config(&[(name, value)]).unwrap_err();
            assert_eq!(invalid(error), name, "{name}={value}");
        }
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
    fn rejects_an_empty_database_url() {
        let error = run_config(&[("DATABASE_URL", "")]).unwrap_err();
        assert_eq!(invalid(error), "DATABASE_URL");
    }

    #[test]
    fn reads_the_flush_interval_from_1_to_900_seconds() {
        let interval = |value| run_config(&[("INGEST_FLUSH_INTERVAL_SECONDS", value)]);
        assert_eq!(
            interval("1").unwrap().flush_interval,
            Duration::from_secs(1)
        );
        assert_eq!(
            interval("900").unwrap().flush_interval,
            Duration::from_mins(15)
        );
        for value in ["", "0", "901", "+5", " 5", "5s", "1e3"] {
            assert_eq!(
                invalid(interval(value).unwrap_err()),
                "INGEST_FLUSH_INTERVAL_SECONDS",
                "{value:?}"
            );
        }
    }

    #[test]
    fn error_text_does_not_contain_the_value() {
        let value = "postgres://user:hunter2@example.invalid/db";
        for name in [
            "PORT",
            "INGEST_CHILD_PROGRAM",
            "INGEST_FLUSH_INTERVAL_SECONDS",
            "INGEST_ZMQ_PORT",
        ] {
            let error = run_config(&[(name, value)]).unwrap_err();
            let text = format!("{error} {error:?}");
            assert!(text.contains(name), "{text}");
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains("example.invalid"), "{text}");
        }
        let config = run_config(&[]).unwrap();
        let text = format!("{config:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("db.invalid"), "{text}");
    }

    #[test]
    fn no_override_is_set_by_default() {
        assert!(overrides(&[]).unwrap().is_empty());
    }

    #[test]
    fn overrides_can_only_make_a_limit_smaller() {
        let set = overrides(&[
            ("INGEST_TEST_SCHEMA", "itest_1_ab"),
            ("INGEST_TEST_QUEUE_BYTES", "4096"),
            ("INGEST_TEST_DEGRADED_AFTER_MS", "1500"),
            ("INGEST_TEST_RETRY_MAX_MS", "200"),
            ("INGEST_TEST_SHORTFALL_PERIOD_MS", "300"),
            ("INGEST_TEST_FLUSH_DEADLINE_MS", "5000"),
        ])
        .unwrap();
        let config = run_config(&[]).unwrap().with_overrides(set);
        assert_eq!(config.schema.as_str(), "itest_1_ab");
        assert_eq!(config.limits.queue_bytes, 4096);
        assert_eq!(config.limits.degraded_after, Duration::from_millis(1500));
        assert_eq!(config.limits.retry_max, Duration::from_millis(200));
        assert_eq!(config.limits.retry_first, Duration::from_millis(200));
        assert_eq!(config.limits.shortfall_period, Duration::from_millis(300));
        assert_eq!(config.budget.flush, Duration::from_secs(5));
        // The other bounds keep their production values.
        assert_eq!(config.budget.zmq_drain, BUDGET.zmq_drain);
        assert_eq!(config.limits.settle, Limits::PRODUCTION.settle);

        for (name, value) in [
            ("INGEST_TEST_SCHEMA", "Raw"),
            ("INGEST_TEST_SCHEMA", "raw;drop"),
            ("INGEST_TEST_QUEUE_BYTES", "1023"),
            ("INGEST_TEST_QUEUE_BYTES", "67108865"),
            ("INGEST_TEST_DEGRADED_AFTER_MS", "0"),
            ("INGEST_TEST_DEGRADED_AFTER_MS", "120001"),
            ("INGEST_TEST_RETRY_MAX_MS", "300001"),
            ("INGEST_TEST_SHORTFALL_PERIOD_MS", "60001"),
            ("INGEST_TEST_FLUSH_DEADLINE_MS", "20001"),
            ("INGEST_TEST_FLUSH_DEADLINE_MS", "-5"),
        ] {
            assert_eq!(
                invalid(overrides(&[(name, value)]).unwrap_err()),
                name,
                "{name}={value}"
            );
        }
    }

    fn migrate_config(value: Option<&OsString>) -> Result<MigrateConfig, ConfigError> {
        MigrateConfig::from_vars(|name| {
            assert_eq!(name, "MIGRATION_DATABASE_URL");
            value.cloned()
        })
    }

    #[test]
    fn reads_the_migration_url() {
        let url = "postgresql://telemetry_migrator:hunter2@db.invalid/neondb";
        let config = migrate_config(Some(&OsString::from(url))).unwrap();
        assert_eq!(config.database_url.expose(), url);
        assert!(!format!("{config:?}").contains("hunter2"));
        assert!(!format!("{config:#?}").contains("hunter2"));
    }

    #[test]
    fn rejects_a_missing_or_empty_migration_url() {
        assert_eq!(
            migrate_config(None).unwrap_err(),
            ConfigError::Missing {
                name: "MIGRATION_DATABASE_URL"
            }
        );
        for value in [OsString::new(), {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0xff])
        }] {
            assert!(matches!(
                migrate_config(Some(&value)).unwrap_err(),
                ConfigError::Invalid {
                    name: "MIGRATION_DATABASE_URL",
                    ..
                }
            ));
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
