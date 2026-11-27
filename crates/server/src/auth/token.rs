//! The session token: 32 random bytes, 64 lowercase hex digits in the
//! cookie.
//!
//! The token is the only secret of a session. It never reaches a log: the
//! `Debug` output is `SessionToken([redacted])`, and the type has no
//! `Display`. Two tokens compare in constant time ([`SessionToken::ct_eq`]),
//! so the time of a lookup does not tell how many leading bytes of a
//! guessed token are correct.

use std::fmt;

use subtle::{Choice, ConstantTimeEq};

/// The number of random bytes of a token.
pub const TOKEN_BYTES: usize = 32;

/// The number of hex digits of a token in the cookie.
pub const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// A session token.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken([u8; TOKEN_BYTES]);

/// The operating system gave no random bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the system gave no random bytes")]
pub struct RandomError;

impl SessionToken {
    /// A new token from the random source of the operating system.
    ///
    /// # Errors
    ///
    /// Returns [`RandomError`] when the system gives no random bytes.
    pub fn generate() -> Result<Self, RandomError> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| RandomError)?;
        Ok(Self(bytes))
    }

    /// Parses exactly 64 lowercase hex digits. Every other text, also
    /// uppercase hex, gives `None`, so one token has one cookie form.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != TOKEN_HEX_LEN {
            return None;
        }
        let mut token = [0_u8; TOKEN_BYTES];
        for (byte, pair) in token.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *byte = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
        }
        Some(Self(token))
    }

    /// The 64 lowercase hex digits of the cookie value.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(TOKEN_HEX_LEN);
        for byte in self.0 {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        text
    }

    /// Compares two tokens in constant time.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> Choice {
        self.0.ct_eq(&other.0)
    }

    /// A token from fixed bytes, for tests.
    #[cfg(test)]
    pub(crate) const fn from_bytes(bytes: [u8; TOKEN_BYTES]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for SessionToken {
    // The token is a secret. Print no byte of it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken([redacted])")
    }
}

const fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn generated_tokens_differ_and_round_trip() {
        let first = SessionToken::generate().unwrap();
        let second = SessionToken::generate().unwrap();
        assert_ne!(first, second);
        let hex = first.to_hex();
        assert_eq!(hex.len(), TOKEN_HEX_LEN);
        assert_eq!(SessionToken::from_hex(&hex), Some(first));
    }

    #[test]
    fn rejects_a_text_that_is_not_64_lowercase_hex_digits() {
        let valid = "ab".repeat(32);
        assert!(SessionToken::from_hex(&valid).is_some());
        for text in [
            String::new(),
            "ab".repeat(31),
            "ab".repeat(33),
            format!("{}a", "ab".repeat(31)),
            "AB".repeat(32),
            format!("{}g0", "ab".repeat(31)),
            format!("{} 0", "ab".repeat(31)),
            format!("{}é", "ab".repeat(31)),
        ] {
            assert_eq!(SessionToken::from_hex(&text), None, "{text:?}");
        }
    }

    #[test]
    fn debug_shows_no_byte_of_the_token() {
        let token = SessionToken::from_bytes([0xab; TOKEN_BYTES]);
        let debug = format!("{token:?}");
        assert_eq!(debug, "SessionToken([redacted])");
        assert!(!debug.contains("ab"));
    }

    #[test]
    fn constant_time_compare_matches_equality() {
        let a = SessionToken::from_bytes([1; TOKEN_BYTES]);
        let mut other = [1; TOKEN_BYTES];
        other[TOKEN_BYTES - 1] = 2;
        let b = SessionToken::from_bytes(other);
        assert!(bool::from(a.ct_eq(&a.clone())));
        assert!(!bool::from(a.ct_eq(&b)));
    }

    proptest! {
        #[test]
        fn hex_round_trips(bytes in proptest::array::uniform32(any::<u8>())) {
            let token = SessionToken::from_bytes(bytes);
            prop_assert_eq!(SessionToken::from_hex(&token.to_hex()), Some(token));
        }

        #[test]
        fn any_text_parses_to_a_token_only_in_the_canonical_form(text in ".{0,80}") {
            if let Some(token) = SessionToken::from_hex(&text) {
                prop_assert_eq!(token.to_hex(), text);
            }
        }
    }
}
