//! Start-up configuration.
//!
//! The process reads and validates its configuration here, once, before it
//! starts any work. No other module reads the environment. The command
//! `export-openapi` reads no configuration.
//!
//! | Variable                 | Content |
//! | ------------------------ | ------- |
//! | `PORT`                   | Required. TCP port of the server, from 1 to 65535. Railway sets it. |
//! | `WEB_STATIC_DIR`         | Required. Directory of the built SPA: `index.html` and `assets/`. |
//! | `RAILWAY_GIT_COMMIT_SHA` | Optional. The commit of the deployment, 7 to 40 lowercase hex digits. Without it, the version is `unknown`. |
//!
//! `index.html` must hold [`VERSION_PLACEHOLDER`] exactly once. The server
//! replaces it with the version at startup, so the SPA can compare its own
//! version with the version of `/health`.
//!
//! A [`ConfigError`] names the variable and a static reason. It never
//! holds the value, so a log line of the error cannot leak a value that
//! was set by mistake.

use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The text in `index.html` that the server replaces with the version.
pub const VERSION_PLACEHOLDER: &str = "__TELEMETRY_VERSION__";

/// The largest `index.html` that the server reads. The real file is a few
/// kilobytes; the bound stops a wrong path from filling the memory.
pub const MAX_INDEX_BYTES: u64 = 1024 * 1024;

/// Configuration of the commands `serve` and `serve-test`.
#[derive(Debug, Clone)]
pub struct Config {
    /// TCP port, from `PORT`.
    pub port: u16,
    /// The built SPA, from `WEB_STATIC_DIR`.
    pub static_files: StaticFiles,
    /// The version of the deployment, from `RAILWAY_GIT_COMMIT_SHA`.
    pub version: Version,
}

impl Config {
    /// Reads and validates the configuration from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] as [`Config::from_vars`] does.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(|name| std::env::var_os(name))
    }

    /// Reads and validates the configuration.
    ///
    /// `var` returns the value of one variable. The binary passes
    /// `std::env::var_os`. Tests pass their own values.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a variable is missing or not valid, or
    /// when `index.html` cannot be read or has no single placeholder.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, ConfigError> {
        let port = parse_port(&var)?;
        let version = parse_version(&var)?;
        let dir = var("WEB_STATIC_DIR").ok_or(ConfigError::Missing {
            name: "WEB_STATIC_DIR",
        })?;
        if dir.is_empty() {
            return Err(invalid("WEB_STATIC_DIR", "expected a directory path"));
        }
        let static_files = StaticFiles::load(&PathBuf::from(dir))?;
        Ok(Self {
            port,
            static_files,
            version,
        })
    }
}

/// The version of the running server: the commit SHA of the deployment,
/// or unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version(Option<Arc<str>>);

impl Version {
    /// The version text of an unknown version.
    pub const UNKNOWN: &'static str = "unknown";

    /// An unknown version: the deployment did not give a commit SHA.
    #[must_use]
    pub const fn unknown() -> Self {
        Self(None)
    }

    /// Validates a commit SHA: 7 to 40 lowercase hex digits.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for any other text.
    pub fn from_commit_sha(sha: &str) -> Result<Self, ConfigError> {
        let valid = (7..=40).contains(&sha.len())
            && sha
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if valid {
            Ok(Self(Some(Arc::from(sha))))
        } else {
            Err(invalid(
                "RAILWAY_GIT_COMMIT_SHA",
                "expected 7 to 40 lowercase hex digits",
            ))
        }
    }

    /// The version text: the commit SHA or `unknown`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_deref().unwrap_or(Self::UNKNOWN)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The built SPA: the asset directory and the text of `index.html`.
#[derive(Debug, Clone)]
pub struct StaticFiles {
    assets_dir: PathBuf,
    index_template: Arc<str>,
}

impl StaticFiles {
    /// Reads `index.html` of `dir` and checks the placeholder.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when `index.html` cannot be read, is larger
    /// than [`MAX_INDEX_BYTES`], is not UTF-8, or does not hold
    /// [`VERSION_PLACEHOLDER`] exactly once.
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let index_template = read_index(&dir.join("index.html"))?;
        if index_template.matches(VERSION_PLACEHOLDER).count() != 1 {
            return Err(invalid(
                "WEB_STATIC_DIR",
                "index.html must hold the version placeholder exactly once",
            ));
        }
        Ok(Self {
            assets_dir: dir.join("assets"),
            index_template: Arc::from(index_template),
        })
    }

    /// The directory of the files under `/assets/`.
    #[must_use]
    pub fn assets_dir(&self) -> &Path {
        &self.assets_dir
    }

    /// The text of `index.html` with the version in place of the
    /// placeholder.
    #[must_use]
    pub fn render_index(&self, version: &Version) -> String {
        self.index_template
            .replacen(VERSION_PLACEHOLDER, version.as_str(), 1)
    }
}

fn read_index(path: &Path) -> Result<String, ConfigError> {
    let unreadable = || invalid("WEB_STATIC_DIR", "cannot read index.html");
    let file = File::open(path).map_err(|_| unreadable())?;
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_INDEX_BYTES) {
        return Err(invalid("WEB_STATIC_DIR", "index.html is larger than 1 MiB"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("WEB_STATIC_DIR", "index.html is not UTF-8"))
}

fn parse_port(var: &impl Fn(&str) -> Option<OsString>) -> Result<u16, ConfigError> {
    let value = var("PORT").ok_or(ConfigError::Missing { name: "PORT" })?;
    let reason = "expected only digits, a port from 1 to 65535";
    let text = value.to_str().ok_or_else(|| invalid("PORT", reason))?;
    // `u16::from_str` accepts a leading `+`; the contract is digits only.
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("PORT", reason));
    }
    match text.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(invalid("PORT", reason)),
    }
}

fn parse_version(var: &impl Fn(&str) -> Option<OsString>) -> Result<Version, ConfigError> {
    match var("RAILWAY_GIT_COMMIT_SHA") {
        None => Ok(Version::unknown()),
        Some(value) => {
            let text = value.to_str().ok_or_else(|| {
                invalid(
                    "RAILWAY_GIT_COMMIT_SHA",
                    "expected 7 to 40 lowercase hex digits",
                )
            })?;
            Version::from_commit_sha(text)
        }
    }
}

const fn invalid(name: &'static str, reason: &'static str) -> ConfigError {
    ConfigError::Invalid { name, reason }
}

/// A configuration failure. It holds the variable name and a static reason,
/// never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A required variable is not set.
    #[error("{name} is missing")]
    Missing {
        /// The variable name.
        name: &'static str,
    },
    /// A variable has a value that is not valid.
    #[error("{name} is not valid: {reason}")]
    Invalid {
        /// The variable name.
        name: &'static str,
        /// The static reason.
        reason: &'static str,
    },
}

impl ConfigError {
    /// The name of the variable.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Missing { name } | Self::Invalid { name, .. } => name,
        }
    }

    /// The static reason.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "missing",
            Self::Invalid { reason, .. } => reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::test_support::TempDir;

    const INDEX: &str = "<meta name=\"telemetry-version\" content=\"__TELEMETRY_VERSION__\">";

    fn spa(index: &str) -> TempDir {
        let dir = TempDir::new();
        dir.write("index.html", index.as_bytes());
        dir.write("assets/app.js", b"x");
        dir
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn config(dir: &TempDir, extra: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let path = dir.path().to_str().unwrap().to_owned();
        let mut pairs = vec![("PORT", "8080"), ("WEB_STATIC_DIR", path.as_str())];
        pairs.extend_from_slice(extra);
        Config::from_vars(vars(&pairs))
    }

    #[test]
    fn reads_a_valid_configuration() {
        let dir = spa(INDEX);
        let config = config(&dir, &[("RAILWAY_GIT_COMMIT_SHA", "0123abc")]).unwrap();
        assert_eq!(config.port, 8080);
        assert_eq!(config.version.as_str(), "0123abc");
        assert_eq!(config.static_files.assets_dir(), dir.path().join("assets"));
        assert_eq!(
            config.static_files.render_index(&config.version),
            "<meta name=\"telemetry-version\" content=\"0123abc\">"
        );
    }

    #[test]
    fn a_missing_commit_sha_is_the_unknown_version() {
        let dir = spa(INDEX);
        let config = config(&dir, &[]).unwrap();
        assert_eq!(config.version, Version::unknown());
        assert_eq!(config.version.to_string(), "unknown");
        assert!(
            config
                .static_files
                .render_index(&config.version)
                .contains("content=\"unknown\"")
        );
    }

    #[test]
    fn accepts_the_bounds_of_the_commit_sha() {
        assert!(Version::from_commit_sha("abcdef0").is_ok());
        assert!(Version::from_commit_sha(&"a".repeat(40)).is_ok());
    }

    #[test]
    fn rejects_a_commit_sha_that_is_not_lowercase_hex() {
        for sha in [
            "",
            "abcdef",
            &"a".repeat(41),
            "ABCDEF0",
            "abcdefg",
            "abc def0",
        ] {
            assert_eq!(
                Version::from_commit_sha(sha),
                Err(ConfigError::Invalid {
                    name: "RAILWAY_GIT_COMMIT_SHA",
                    reason: "expected 7 to 40 lowercase hex digits",
                }),
                "{sha:?}"
            );
        }
        let dir = spa(INDEX);
        assert!(config(&dir, &[("RAILWAY_GIT_COMMIT_SHA", "")]).is_err());
    }

    #[test]
    fn requires_port_and_static_dir() {
        let dir = spa(INDEX);
        let path = dir.path().to_str().unwrap();
        assert_eq!(
            Config::from_vars(vars(&[("WEB_STATIC_DIR", path)])).unwrap_err(),
            ConfigError::Missing { name: "PORT" }
        );
        assert_eq!(
            Config::from_vars(vars(&[("PORT", "8080")])).unwrap_err(),
            ConfigError::Missing {
                name: "WEB_STATIC_DIR"
            }
        );
        assert_eq!(
            Config::from_vars(vars(&[("PORT", "8080"), ("WEB_STATIC_DIR", "")]))
                .unwrap_err()
                .name(),
            "WEB_STATIC_DIR"
        );
    }

    #[test]
    fn rejects_a_port_outside_1_to_65535() {
        let dir = spa(INDEX);
        let path = dir.path().to_str().unwrap();
        for port in [
            "0",
            "65536",
            "-1",
            "+80",
            " 80",
            "80 ",
            "",
            "8o",
            "99999999999",
        ] {
            let err =
                Config::from_vars(vars(&[("PORT", port), ("WEB_STATIC_DIR", path)])).unwrap_err();
            assert_eq!(err.name(), "PORT", "{port:?}");
        }
        for port in ["1", "65535", "0080"] {
            assert!(
                Config::from_vars(vars(&[("PORT", port), ("WEB_STATIC_DIR", path)])).is_ok(),
                "{port:?}"
            );
        }
    }

    #[test]
    fn rejects_an_index_without_a_single_placeholder() {
        for index in [
            "<html></html>",
            "__TELEMETRY_VERSION__ __TELEMETRY_VERSION__",
        ] {
            let dir = spa(index);
            assert_eq!(
                config(&dir, &[]).unwrap_err(),
                ConfigError::Invalid {
                    name: "WEB_STATIC_DIR",
                    reason: "index.html must hold the version placeholder exactly once",
                }
            );
        }
    }

    #[test]
    fn rejects_a_missing_index_or_directory() {
        let dir = TempDir::new();
        assert_eq!(
            config(&dir, &[]).unwrap_err().reason(),
            "cannot read index.html"
        );
        let missing = dir.path().join("missing");
        let err = Config::from_vars(vars(&[
            ("PORT", "8080"),
            ("WEB_STATIC_DIR", missing.to_str().unwrap()),
        ]))
        .unwrap_err();
        assert_eq!(err.reason(), "cannot read index.html");
    }

    #[test]
    fn rejects_an_index_that_is_too_large_or_not_utf8() {
        let dir = spa("");
        let mut large = INDEX.as_bytes().to_vec();
        large.resize(usize::try_from(MAX_INDEX_BYTES).unwrap() + 1, b' ');
        dir.write("index.html", &large);
        assert_eq!(
            config(&dir, &[]).unwrap_err().reason(),
            "index.html is larger than 1 MiB"
        );
        dir.write("index.html", b"\xff__TELEMETRY_VERSION__");
        assert_eq!(
            config(&dir, &[]).unwrap_err().reason(),
            "index.html is not UTF-8"
        );
    }

    #[test]
    fn the_error_text_never_holds_the_value() {
        let secret = "postgres://user:hunter2@db.invalid/x";
        let dir = spa(INDEX);
        let path = dir.path().to_str().unwrap();
        let errors = [
            Config::from_vars(vars(&[("PORT", secret), ("WEB_STATIC_DIR", path)])).unwrap_err(),
            Config::from_vars(vars(&[("PORT", "1"), ("WEB_STATIC_DIR", secret)])).unwrap_err(),
            config(&dir, &[("RAILWAY_GIT_COMMIT_SHA", secret)]).unwrap_err(),
        ];
        for err in errors {
            let text = format!("{err} {err:?}");
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains(path), "{text}");
        }
    }
}
