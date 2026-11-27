//! The password hash: its validation at startup, the verification of a
//! login, and the command `hash-password`.
//!
//! The server holds only an Argon2id hash in the PHC string form, from the
//! variable `WEB_PASSWORD_HASH` (ADR 0003). The owner makes the hash on
//! the owner's computer with `telemetry-web-server hash-password`.
//!
//! Argon2 verifies with the algorithm and the parameters in the hash
//! itself. So the startup check is the only bound of the work and the
//! memory of one verification. It accepts only:
//!
//! | Part | Accepted |
//! | ---- | -------- |
//! | Algorithm | `argon2id` |
//! | Version | 19 (`v=19`) |
//! | Memory `m` | 19456 to 131072 KiB |
//! | Passes `t` | 2 to 10 |
//! | Lanes `p` | 1 to 4 |
//! | Salt | 16 to 64 bytes |
//! | Output | 32 bytes |
//! | Key id, associated data | none |
//!
//! `hash-password` uses `m=65536`, `t=3`, `p=1`, a 16-byte salt, and a
//! 32-byte output. A verification at these parameters uses 64 MiB, and the
//! throttle lets one run at a time.
//!
//! Neither the hash nor a password reaches a log, an error, or a `Debug`
//! output.

use std::fmt;
use std::io::{Read, Write};

use argon2::password_hash::phc::PasswordHash;
use argon2::{Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version};

use crate::config::ConfigError;

/// The name of the variable that holds the hash.
pub const HASH_VARIABLE: &str = "WEB_PASSWORD_HASH";

/// The accepted memory cost in KiB.
pub const M_COST: std::ops::RangeInclusive<u32> = 19_456..=131_072;
/// The accepted number of passes.
pub const T_COST: std::ops::RangeInclusive<u32> = 2..=10;
/// The accepted number of lanes.
pub const P_COST: std::ops::RangeInclusive<u32> = 1..=4;
/// The accepted salt length in bytes.
pub const SALT_BYTES: std::ops::RangeInclusive<usize> = 16..=64;
/// The output length in bytes.
pub const OUTPUT_BYTES: usize = 32;
/// The Argon2 version 1.3.
pub const VERSION: u32 = 0x13;

/// The parameters of `hash-password`.
pub const OWNER_M_COST: u32 = 65_536;
/// The passes of `hash-password`.
pub const OWNER_T_COST: u32 = 3;
/// The lanes of `hash-password`.
pub const OWNER_P_COST: u32 = 1;
/// The salt length of `hash-password`.
pub const OWNER_SALT_BYTES: usize = 16;

/// The accepted length of a password in bytes, at login and in
/// `hash-password`.
pub const PASSWORD_BYTES: std::ops::RangeInclusive<usize> = 1..=1024;

/// The shortest password that `hash-password` accepts, in bytes. The
/// owner uses a long random password (ADR 0003); the bound stops a short
/// password by mistake.
pub const MIN_NEW_PASSWORD_BYTES: usize = 16;

/// A validated Argon2id hash in the PHC form.
#[derive(Clone)]
pub struct StoredHash(PasswordHash);

impl fmt::Debug for StoredHash {
    // The hash lets a holder guess passwords offline. Print nothing of it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredHash([redacted])")
    }
}

impl StoredHash {
    /// Parses and checks a PHC string.
    ///
    /// # Errors
    ///
    /// Returns a static reason; never the text or a part of it.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        const NOT_PHC: &str = "expected an Argon2id hash in the PHC string form";
        let hash = PasswordHash::new(text).map_err(|_| NOT_PHC)?;
        if hash.algorithm.as_str() != argon2::ARGON2ID_IDENT.as_str() {
            return Err("expected the algorithm argon2id");
        }
        if hash.version != Some(VERSION) {
            return Err("expected the Argon2 version 19");
        }
        let params = Params::try_from(&hash).map_err(|_| NOT_PHC)?;
        if !M_COST.contains(&params.m_cost()) {
            return Err("expected the memory cost m from 19456 to 131072");
        }
        if !T_COST.contains(&params.t_cost()) {
            return Err("expected the passes t from 2 to 10");
        }
        if !P_COST.contains(&params.p_cost()) {
            return Err("expected the lanes p from 1 to 4");
        }
        if !params.keyid().is_empty() || !params.data().is_empty() {
            return Err("expected no key id and no associated data");
        }
        let salt_len = hash.salt.as_ref().map_or(0, |salt| salt.as_ref().len());
        if !SALT_BYTES.contains(&salt_len) {
            return Err("expected a salt of 16 to 64 bytes");
        }
        if hash
            .hash
            .as_ref()
            .map(argon2::password_hash::phc::Output::len)
            != Some(OUTPUT_BYTES)
        {
            return Err("expected a hash output of 32 bytes");
        }
        Ok(Self(hash))
    }

    /// Reads and checks the hash of `WEB_PASSWORD_HASH`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] with the variable name and a static reason.
    pub fn from_variable(value: Option<std::ffi::OsString>) -> Result<Self, ConfigError> {
        let value = value.ok_or(ConfigError::Missing {
            name: HASH_VARIABLE,
        })?;
        let text = value.to_str().ok_or(ConfigError::Invalid {
            name: HASH_VARIABLE,
            reason: "expected an Argon2id hash in the PHC string form",
        })?;
        Self::parse(text).map_err(|reason| ConfigError::Invalid {
            name: HASH_VARIABLE,
            reason,
        })
    }

    /// The memory cost in KiB, for the measurements and the tests.
    #[must_use]
    pub fn m_cost(&self) -> u32 {
        Params::try_from(&self.0).map_or(0, |params| params.m_cost())
    }
}

/// The result of one verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The password matches the hash.
    Match,
    /// The password does not match.
    Mismatch,
}

/// A password check. It runs on a blocking thread
/// (`tokio::task::spawn_blocking`); it may take time and memory.
pub trait Verifier: Send + Sync + fmt::Debug {
    /// Checks `password` against the stored hash.
    fn verify(&self, password: &[u8]) -> Verification;
}

/// The Argon2id verifier of `serve`.
#[derive(Debug, Clone)]
pub struct Argon2Verifier {
    hash: StoredHash,
}

impl Argon2Verifier {
    /// A verifier for the validated hash.
    #[must_use]
    pub const fn new(hash: StoredHash) -> Self {
        Self { hash }
    }
}

impl Verifier for Argon2Verifier {
    fn verify(&self, password: &[u8]) -> Verification {
        // The output compare of the library is constant-time. Any error,
        // also an internal one, is a mismatch: it never admits a login.
        match Argon2::default().verify_password(password, &self.hash.0) {
            Ok(()) => Verification::Match,
            Err(_) => Verification::Mismatch,
        }
    }
}

/// A failure of `hash-password`. The text holds no input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HashPasswordError {
    /// Standard input cannot be read.
    #[error("cannot read the password from standard input")]
    Read,
    /// The input is not UTF-8.
    #[error("the password is not UTF-8")]
    NotUtf8,
    /// The input is too short or too long.
    #[error("the password must have 16 to 1024 bytes")]
    Length,
    /// The system gave no random bytes for the salt.
    #[error("the system gave no random bytes")]
    Random,
    /// Argon2 failed.
    #[error("cannot compute the hash")]
    Hash,
    /// Standard output cannot be written.
    #[error("cannot write the hash to standard output")]
    Write,
}

/// Reads one password from `input` and writes its PHC string and a newline
/// to `output`.
///
/// The input is the password, with one optional line end (`\n` or
/// `\r\n`) that is not part of it; every other byte is part of the
/// password, also a space. The login (`POST /api/v1/session`) compares
/// the same bytes, so `printf '%s'` and `echo` give the same hash.
///
/// # Errors
///
/// Returns [`HashPasswordError`]; the error holds no input.
pub fn hash_password(
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), HashPasswordError> {
    let max = *PASSWORD_BYTES.end() + 2;
    let mut bytes = Vec::with_capacity(max + 1);
    input
        .take(u64::try_from(max + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|_| HashPasswordError::Read)?;
    let password = strip_line_end(&bytes);
    if password.len() < MIN_NEW_PASSWORD_BYTES || !PASSWORD_BYTES.contains(&password.len()) {
        return Err(HashPasswordError::Length);
    }
    std::str::from_utf8(password).map_err(|_| HashPasswordError::NotUtf8)?;
    let mut salt = [0_u8; OWNER_SALT_BYTES];
    getrandom::fill(&mut salt).map_err(|_| HashPasswordError::Random)?;
    let line = hash_with_salt(password, &salt)?;
    output
        .write_all(line.as_bytes())
        .and_then(|()| output.write_all(b"\n"))
        .and_then(|()| output.flush())
        .map_err(|_| HashPasswordError::Write)
}

/// The PHC string of `password` with the owner parameters and `salt`.
fn hash_with_salt(password: &[u8], salt: &[u8]) -> Result<String, HashPasswordError> {
    let params = Params::new(OWNER_M_COST, OWNER_T_COST, OWNER_P_COST, Some(OUTPUT_BYTES))
        .map_err(|_| HashPasswordError::Hash)?;
    let hash = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_with_salt(password, salt)
        .map_err(|_| HashPasswordError::Hash)?;
    Ok(hash.to_string())
}

fn strip_line_end(bytes: &[u8]) -> &[u8] {
    bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes)
}

/// Test-only hashes: made in the test from a test-only password, never a
/// real password.
#[cfg(any(test, feature = "test-server"))]
pub mod testing {
    use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};

    use super::StoredHash;

    /// A hash of `password` with the given parameters and a random salt.
    ///
    /// # Panics
    ///
    /// Panics when the parameters are not valid or the system gives no
    /// random bytes; only tests call it.
    #[must_use]
    #[expect(clippy::expect_used, reason = "a test fixture fails loudly")]
    pub fn hash(password: &str, m_cost: u32, t_cost: u32, p_cost: u32) -> String {
        let mut salt = [0_u8; 16];
        getrandom::fill(&mut salt).expect("random salt");
        let params = Params::new(m_cost, t_cost, p_cost, Some(32)).expect("valid parameters");
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_with_salt(password.as_bytes(), &salt)
            .expect("hash")
            .to_string()
    }

    /// A validated hash at the lowest accepted cost.
    ///
    /// # Panics
    ///
    /// Panics when the hash is not valid; only tests call it.
    #[must_use]
    #[expect(clippy::expect_used, reason = "a test fixture fails loudly")]
    pub fn stored(password: &str) -> StoredHash {
        StoredHash::parse(&hash(password, 19_456, 2, 1)).expect("valid test hash")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "test-only password, never a real one";

    fn hash_text(m: u32, t: u32, p: u32) -> String {
        testing::hash(PASSWORD, m, t, p)
    }

    #[test]
    fn accepts_a_hash_inside_the_bounds() {
        for (m, t, p) in [(19_456, 2, 1), (65_536, 3, 1), (131_072, 10, 4)] {
            // The largest cost takes long; parse only, without verifying.
            let text = hash_text_cheap(m, t, p);
            assert!(StoredHash::parse(&text).is_ok(), "m={m} t={t} p={p}");
        }
    }

    /// A PHC string with these parameters and the salt and output of a
    /// cheap hash: the parser checks the form, not the output.
    fn hash_text_cheap(m: u32, t: u32, p: u32) -> String {
        let cheap = hash_text(19_456, 2, 1);
        cheap.replacen("m=19456,t=2,p=1", &format!("m={m},t={t},p={p}"), 1)
    }

    #[test]
    fn rejects_parameters_outside_the_bounds() {
        let cases = [
            (
                19_455,
                2,
                1,
                "expected the memory cost m from 19456 to 131072",
            ),
            (
                131_073,
                2,
                1,
                "expected the memory cost m from 19456 to 131072",
            ),
            (19_456, 1, 1, "expected the passes t from 2 to 10"),
            (19_456, 11, 1, "expected the passes t from 2 to 10"),
            (19_456, 2, 5, "expected the lanes p from 1 to 4"),
        ];
        for (m, t, p, reason) in cases {
            assert_eq!(
                StoredHash::parse(&hash_text_cheap(m, t, p)).unwrap_err(),
                reason,
                "m={m} t={t} p={p}"
            );
        }
    }

    #[test]
    fn rejects_other_algorithms_versions_and_forms() {
        let good = hash_text(19_456, 2, 1);
        let argon2i = good.replacen("$argon2id$", "$argon2i$", 1);
        assert_eq!(
            StoredHash::parse(&argon2i).unwrap_err(),
            "expected the algorithm argon2id"
        );
        let data_dependent = good.replacen("$argon2id$", "$argon2d$", 1);
        assert_eq!(
            StoredHash::parse(&data_dependent).unwrap_err(),
            "expected the algorithm argon2id"
        );
        let v16 = good.replacen("$v=19$", "$v=16$", 1);
        assert_eq!(
            StoredHash::parse(&v16).unwrap_err(),
            "expected the Argon2 version 19"
        );
        let no_version = good.replacen("$v=19$", "$", 1);
        assert_eq!(
            StoredHash::parse(&no_version).unwrap_err(),
            "expected the Argon2 version 19"
        );
        let without_output = good.rsplit_once('$').unwrap().0.to_owned();
        assert_eq!(
            StoredHash::parse(&without_output).unwrap_err(),
            "expected a hash output of 32 bytes"
        );
        for text in ["", "secret", "$2b$12$abcdefghijklmnopqrstuu", " $argon2id$"] {
            assert!(StoredHash::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn rejects_a_short_salt_and_a_short_output() {
        let good = hash_text(19_456, 2, 1);
        let parts: Vec<&str> = good.split('$').collect();
        // ["", "argon2id", "v=19", "m=..,t=..,p=..", salt, output]
        let short_salt = format!(
            "${}${}${}$AAAAAAAAAAAA${}",
            parts[1], parts[2], parts[3], parts[5]
        );
        assert_eq!(
            StoredHash::parse(&short_salt).unwrap_err(),
            "expected a salt of 16 to 64 bytes"
        );
        let short_output = format!(
            "${}${}${}${}$AAAAAAAAAAAAAAAAAAAAAA",
            parts[1], parts[2], parts[3], parts[4]
        );
        assert_eq!(
            StoredHash::parse(&short_output).unwrap_err(),
            "expected a hash output of 32 bytes"
        );
    }

    #[test]
    fn a_configuration_error_never_holds_the_value() {
        let value = "$argon2i$v=19$m=19456,t=2,p=1$c2VjcmV0c2FsdHNlY3JldA$c2VjcmV0b3V0cHV0";
        let err = StoredHash::from_variable(Some(value.into())).unwrap_err();
        let text = format!("{err} {err:?}");
        assert!(!text.contains("c2Vj"), "{text}");
        assert!(!text.contains("argon2i$"), "{text}");
        assert_eq!(err.name(), HASH_VARIABLE);
        assert_eq!(
            StoredHash::from_variable(None).unwrap_err(),
            ConfigError::Missing {
                name: HASH_VARIABLE
            }
        );
    }

    #[test]
    fn debug_shows_nothing_of_the_hash() {
        let text = hash_text(19_456, 2, 1);
        let stored = StoredHash::parse(&text).unwrap();
        let salt = text.split('$').nth(4).unwrap();
        for debug in [
            format!("{stored:?}"),
            format!("{:?}", Argon2Verifier::new(stored)),
        ] {
            assert!(!debug.contains(salt), "{debug}");
            assert!(!debug.contains("argon2id"), "{debug}");
            assert!(debug.contains("[redacted]"), "{debug}");
        }
    }

    #[test]
    fn the_verifier_accepts_only_the_password() {
        let verifier = Argon2Verifier::new(testing::stored(PASSWORD));
        assert_eq!(verifier.verify(PASSWORD.as_bytes()), Verification::Match);
        assert_eq!(verifier.verify(b"another password"), Verification::Mismatch);
        assert_eq!(verifier.verify(b""), Verification::Mismatch);
        let with_newline = format!("{PASSWORD}\n");
        assert_eq!(
            verifier.verify(with_newline.as_bytes()),
            Verification::Mismatch
        );
    }

    #[test]
    fn hash_password_output_verifies_through_the_login_path() {
        // The command and the login must agree byte for byte, or the owner
        // cannot log in. Each line end form gives the same password.
        for input in [
            format!("{PASSWORD}\n"),
            format!("{PASSWORD}\r\n"),
            PASSWORD.to_owned(),
        ] {
            let mut output = Vec::new();
            hash_password(&mut input.as_bytes(), &mut output).unwrap();
            let line = String::from_utf8(output).unwrap();
            let phc = line.strip_suffix('\n').unwrap();
            assert!(!phc.contains('\n'));
            assert!(phc.starts_with("$argon2id$v=19$m=65536,t=3,p=1$"), "{phc}");
            let stored = StoredHash::parse(phc).unwrap();
            assert_eq!(stored.m_cost(), OWNER_M_COST);
            let verifier = Argon2Verifier::new(stored);
            assert_eq!(verifier.verify(PASSWORD.as_bytes()), Verification::Match);
        }
    }

    #[test]
    fn hash_password_keeps_spaces_and_strips_only_one_line_end() {
        let password = format!(" {PASSWORD} ");
        let mut output = Vec::new();
        hash_password(&mut format!("{password}\n").as_bytes(), &mut output).unwrap();
        let stored = StoredHash::parse(std::str::from_utf8(&output).unwrap().trim_end()).unwrap();
        let verifier = Argon2Verifier::new(stored);
        assert_eq!(verifier.verify(password.as_bytes()), Verification::Match);
        assert_eq!(verifier.verify(PASSWORD.as_bytes()), Verification::Mismatch);
    }

    #[test]
    fn hash_password_gives_a_new_salt_each_time() {
        let mut first = Vec::new();
        let mut second = Vec::new();
        hash_password(&mut PASSWORD.as_bytes(), &mut first).unwrap();
        hash_password(&mut PASSWORD.as_bytes(), &mut second).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn hash_password_rejects_a_bad_input_without_output() {
        let long = "x".repeat(1025);
        let cases: [(&[u8], HashPasswordError); 5] = [
            (b"", HashPasswordError::Length),
            (b"\n", HashPasswordError::Length),
            (b"fifteen bytes!!\n", HashPasswordError::Length),
            (long.as_bytes(), HashPasswordError::Length),
            (
                b"\xff\xfe sixteen bytes or more",
                HashPasswordError::NotUtf8,
            ),
        ];
        for (input, expected) in cases {
            let mut output = Vec::new();
            assert_eq!(hash_password(&mut &input[..], &mut output), Err(expected));
            assert_eq!(output, Vec::<u8>::new());
        }
        // 1024 bytes and a line end are accepted.
        let longest = format!("{}\n", "y".repeat(1024));
        let mut output = Vec::new();
        assert!(hash_password(&mut longest.as_bytes(), &mut output).is_ok());
    }

    /// Measures one verification at the `hash-password` parameters. Run it
    /// on request with a release build:
    /// `cargo test --release -p telemetry-web-server --lib -- --ignored --nocapture measure_the_verification_time`.
    /// The login budget is p95 ≤ 1 s (design §9).
    #[test]
    #[ignore = "a measurement, not a check; run on request with --release"]
    fn measure_the_verification_time() {
        const RUNS: usize = 40;
        let mut output = Vec::new();
        hash_password(&mut PASSWORD.as_bytes(), &mut output).unwrap();
        let stored = StoredHash::parse(std::str::from_utf8(&output).unwrap().trim_end()).unwrap();
        let verifier = Argon2Verifier::new(stored);
        let mut times: Vec<std::time::Duration> = (0..RUNS)
            .map(|_| {
                let started = std::time::Instant::now();
                assert_eq!(verifier.verify(PASSWORD.as_bytes()), Verification::Match);
                started.elapsed()
            })
            .collect();
        times.sort_unstable();
        let p50 = times[RUNS / 2];
        let p95 = times[RUNS * 95 / 100 - 1];
        let max = times[RUNS - 1];
        println!("argon2id m=65536 t=3 p=1, {RUNS} runs: p50 {p50:?}, p95 {p95:?}, max {max:?}");
        assert!(p95 <= std::time::Duration::from_secs(1), "p95 {p95:?}");
    }

    #[test]
    fn errors_hold_no_input() {
        let secret = "zz-secret-zz";
        for err in [
            HashPasswordError::Read,
            HashPasswordError::NotUtf8,
            HashPasswordError::Length,
            HashPasswordError::Random,
            HashPasswordError::Hash,
            HashPasswordError::Write,
        ] {
            assert!(!format!("{err} {err:?}").contains(secret));
        }
    }
}
