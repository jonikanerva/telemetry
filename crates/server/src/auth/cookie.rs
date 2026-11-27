//! The session cookie: its name, its attributes, and the parser of the
//! `Cookie` request header.
//!
//! The cookie is `__Host-telemetry_session=<64 hex digits>; Path=/;
//! Max-Age=<seconds>; Secure; HttpOnly; SameSite=Strict`:
//!
//! - `__Host-` makes the browser accept the cookie only with `Secure`,
//!   `Path=/`, and no `Domain`, so a sibling host cannot set or replace it.
//! - `HttpOnly` keeps it away from page scripts.
//! - `SameSite=Strict` keeps it off every cross-site request, the second
//!   layer of the CSRF rule (`crate::auth::origin`).
//! - `Max-Age` is the remaining absolute lifetime of the session. The
//!   server enforces the lifetimes itself; the attribute only lets the
//!   browser drop a dead cookie.
//!
//! The parser reads this one name by hand, with the strict token form of
//! [`SessionToken::from_hex`]; the `cookie` crate would add the `time`
//! crate as a second time library.

use axum::http::header::COOKIE;
use axum::http::{HeaderMap, HeaderValue};

use super::token::SessionToken;

/// The name of the session cookie.
pub const COOKIE_NAME: &str = "__Host-telemetry_session";

/// The attributes after the value. `Max-Age` comes before them.
const ATTRIBUTES: &str = "Path=/; Secure; HttpOnly; SameSite=Strict";

/// The largest number of session cookies that a request can carry and
/// that the server reads. A browser sends one; more come only from a
/// client that is not a browser.
pub const MAX_PRESENTED: usize = 4;

/// The `Set-Cookie` value of a new session.
#[must_use]
pub fn set_cookie(token: &SessionToken, max_age_seconds: u64) -> HeaderValue {
    let text = format!(
        "{COOKIE_NAME}={}; Max-Age={max_age_seconds}; {ATTRIBUTES}",
        token.to_hex()
    );
    // The text holds only ASCII letters, digits, and punctuation.
    HeaderValue::from_str(&text).unwrap_or_else(|_| clear_cookie())
}

/// The `Set-Cookie` value that removes the session cookie.
#[must_use]
pub fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "__Host-telemetry_session=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict",
    )
}

/// What the request carries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Presented {
    /// The session tokens in the valid form, at most [`MAX_PRESENTED`].
    pub tokens: Vec<SessionToken>,
    /// The request carries a session cookie, valid in form or not.
    pub any: bool,
}

/// Reads the session cookies of all `Cookie` headers.
///
/// A pair is `name=value` between `;` separators, with optional spaces
/// around it (RFC 6265, section 5.4). Only the exact name
/// [`COOKIE_NAME`] counts. A value that is not 64 lowercase hex digits
/// marks the cookie as present but gives no token. A header that is not
/// visible ASCII is ignored.
#[must_use]
pub fn presented(headers: &HeaderMap) -> Presented {
    let mut result = Presented::default();
    for header in headers.get_all(COOKIE) {
        let Ok(text) = header.to_str() else {
            continue;
        };
        for pair in text.split(';') {
            let Some((name, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if name != COOKIE_NAME {
                continue;
            }
            result.any = true;
            if let Some(token) = SessionToken::from_hex(value)
                && result.tokens.len() < MAX_PRESENTED
            {
                result.tokens.push(token);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use proptest::prelude::*;

    use super::*;
    use crate::auth::token::TOKEN_BYTES;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(COOKIE, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn token(byte: u8) -> SessionToken {
        SessionToken::from_bytes([byte; TOKEN_BYTES])
    }

    #[test]
    fn set_cookie_has_every_required_attribute() {
        let value = set_cookie(&token(0xab), 2_592_000);
        let text = value.to_str().unwrap();
        assert_eq!(
            text,
            format!(
                "__Host-telemetry_session={}; Max-Age=2592000; Path=/; Secure; HttpOnly; SameSite=Strict",
                "ab".repeat(32)
            )
        );
        assert!(!text.to_ascii_lowercase().contains("domain"));
    }

    #[test]
    fn clear_cookie_has_the_same_name_and_attributes_and_no_value() {
        let text = clear_cookie();
        assert_eq!(
            text,
            "__Host-telemetry_session=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict"
        );
    }

    #[test]
    fn reads_the_session_cookie_among_others() {
        let hex = "ab".repeat(32);
        let found = presented(&headers(&[&format!("a=1; {COOKIE_NAME}={hex}; b=2")]));
        assert_eq!(found.tokens, vec![token(0xab)]);
        assert!(found.any);
    }

    #[test]
    fn reads_cookies_of_several_headers() {
        let found = presented(&headers(&[
            &format!("{COOKIE_NAME}={}", "01".repeat(32)),
            &format!("{COOKIE_NAME}={}", "02".repeat(32)),
        ]));
        assert_eq!(found.tokens, vec![token(1), token(2)]);
    }

    #[test]
    fn a_malformed_value_is_present_but_gives_no_token() {
        for value in [
            String::new(),
            "AB".repeat(32),
            "ab".repeat(31),
            format!("\"{}\"", "ab".repeat(32)),
            format!("{}=", "ab".repeat(32)),
        ] {
            let found = presented(&headers(&[&format!("{COOKIE_NAME}={value}")]));
            assert!(found.tokens.is_empty(), "{value:?}");
            assert!(found.any, "{value:?}");
        }
    }

    #[test]
    fn other_names_do_not_count() {
        let hex = "ab".repeat(32);
        for name in [
            "telemetry_session",
            "__host-telemetry_session",
            "__Host-telemetry_session2",
            "x__Host-telemetry_session",
        ] {
            let found = presented(&headers(&[&format!("{name}={hex}")]));
            assert_eq!(found, Presented::default(), "{name}");
        }
    }

    #[test]
    fn keeps_at_most_four_tokens() {
        let pairs: Vec<String> = (1..=6_u8)
            .map(|byte| format!("{COOKIE_NAME}={}", format!("{byte:02x}").repeat(32)))
            .collect();
        let found = presented(&headers(&[&pairs.join("; ")]));
        assert_eq!(found.tokens.len(), MAX_PRESENTED);
    }

    #[test]
    fn a_request_without_cookies_presents_nothing() {
        assert_eq!(presented(&HeaderMap::new()), Presented::default());
    }

    proptest! {
        #[test]
        fn parsing_never_panics_and_every_token_is_canonical(text in "[ -~]{0,300}") {
            if let Ok(value) = HeaderValue::from_str(&text) {
                let mut map = HeaderMap::new();
                map.insert(COOKIE, value);
                let found = presented(&map);
                prop_assert!(found.tokens.len() <= MAX_PRESENTED);
                for token in &found.tokens {
                    let pair = format!("{COOKIE_NAME}={}", token.to_hex());
                    prop_assert!(text.contains(&pair));
                }
                if !found.tokens.is_empty() {
                    prop_assert!(found.any);
                }
            }
        }

        #[test]
        fn a_valid_cookie_is_found_between_any_other_pairs(
            before in "([a-z]{1,8}=[a-z0-9]{0,8}; ){0,4}",
            after in "(; [a-z]{1,8}=[a-z0-9]{0,8}){0,4}",
            bytes in proptest::array::uniform32(any::<u8>()),
        ) {
            let token = SessionToken::from_bytes(bytes);
            let text = format!("{before}{COOKIE_NAME}={}{after}", token.to_hex());
            let found = presented(&headers(&[&text]));
            prop_assert_eq!(found.tokens, vec![token]);
        }
    }
}
