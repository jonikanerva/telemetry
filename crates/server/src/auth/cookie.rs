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

/// The session cookies that a request carries.
///
/// A browser sends at most one `__Host-telemetry_session` pair: the
/// `__Host-` prefix allows only one cookie of the name, with `Path=/` and
/// no `Domain`. So every endpoint refuses a request with more than one
/// pair (`crate::auth`, the guard, answers 400 `invalid_request`), and no
/// endpoint must choose between two tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presented {
    /// No session cookie.
    None,
    /// One session cookie. The token is `None` when the value is not 64
    /// lowercase hex digits; such a cookie is no session.
    One(Option<SessionToken>),
    /// More than one session cookie.
    Many,
}

impl Presented {
    /// The token of exactly one well-formed session cookie.
    #[must_use]
    pub const fn token(&self) -> Option<&SessionToken> {
        match self {
            Self::One(Some(token)) => Some(token),
            Self::None | Self::One(None) | Self::Many => None,
        }
    }
}

/// Reads the session cookies of all `Cookie` headers.
///
/// A pair is `name=value` between `;` separators, with optional spaces
/// around it (RFC 6265, section 5.4). Only the exact name [`COOKIE_NAME`]
/// counts. The parser works on bytes, so a header with other bytes than
/// visible ASCII cannot hide a pair.
#[must_use]
pub fn presented(headers: &HeaderMap) -> Presented {
    let mut found = Presented::None;
    for header in headers.get_all(COOKIE) {
        for pair in header.as_bytes().split(|byte| *byte == b';') {
            let pair = pair.trim_ascii();
            let Some(split) = pair.iter().position(|byte| *byte == b'=') else {
                continue;
            };
            let (name, value) = (&pair[..split], &pair[split + 1..]);
            if name != COOKIE_NAME.as_bytes() {
                continue;
            }
            if found != Presented::None {
                return Presented::Many;
            }
            let token = std::str::from_utf8(value)
                .ok()
                .and_then(SessionToken::from_hex);
            found = Presented::One(token);
        }
    }
    found
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
        assert_eq!(found, Presented::One(Some(token(0xab))));
        assert_eq!(found.token(), Some(&token(0xab)));
    }

    #[test]
    fn two_session_cookies_are_many_in_any_form_and_header() {
        let one = format!("{COOKIE_NAME}={}", "01".repeat(32));
        let two = format!("{COOKIE_NAME}={}", "02".repeat(32));
        let bad = format!("{COOKIE_NAME}=x");
        for case in [
            vec![format!("{one}; {two}")],
            vec![one.clone(), two.clone()],
            vec![format!("{one}; {bad}")],
            vec![bad.clone(), one.clone()],
            vec![format!("{one}; {one}")],
        ] {
            let refs: Vec<&str> = case.iter().map(String::as_str).collect();
            let found = presented(&headers(&refs));
            assert_eq!(found, Presented::Many, "{case:?}");
            assert_eq!(found.token(), None);
        }
    }

    #[test]
    fn a_malformed_value_is_one_cookie_without_a_token() {
        for value in [
            String::new(),
            "AB".repeat(32),
            "ab".repeat(31),
            format!("\"{}\"", "ab".repeat(32)),
            format!("{}=", "ab".repeat(32)),
        ] {
            let found = presented(&headers(&[&format!("{COOKIE_NAME}={value}")]));
            assert_eq!(found, Presented::One(None), "{value:?}");
        }
    }

    #[test]
    fn a_header_with_other_bytes_cannot_hide_a_second_pair() {
        let mut map = HeaderMap::new();
        map.append(
            COOKIE,
            HeaderValue::from_str(&format!("{COOKIE_NAME}={}", "01".repeat(32))).unwrap(),
        );
        let hidden = format!("x=\u{e9}; {COOKIE_NAME}={}", "02".repeat(32)).into_bytes();
        map.append(COOKIE, HeaderValue::from_bytes(&hidden).unwrap());
        assert_eq!(presented(&map), Presented::Many);
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
            assert_eq!(found, Presented::None, "{name}");
        }
    }

    #[test]
    fn a_request_without_cookies_presents_nothing() {
        assert_eq!(presented(&HeaderMap::new()), Presented::None);
    }

    /// Hand-written vectors with the expected result written down, not
    /// computed: the forms a browser or another client can send.
    #[test]
    fn hand_written_vectors() {
        let t = "0a".repeat(32);
        let none = Presented::None;
        let one_token = Presented::One(Some(SessionToken::from_bytes([0x0a; TOKEN_BYTES])));
        let one_bad = Presented::One(None);
        let cases: Vec<(String, Presented)> = vec![
            (String::new(), none.clone()),
            (";;;".to_owned(), none.clone()),
            (format!("{COOKIE_NAME}={t}"), one_token.clone()),
            (format!("   {COOKIE_NAME}={t}   "), one_token.clone()),
            (format!("a=1;{COOKIE_NAME}={t};b=2"), one_token.clone()),
            (format!("a=1; ; {COOKIE_NAME}={t}; "), one_token.clone()),
            (format!("{COOKIE_NAME} ={t}"), none.clone()),
            (format!("{COOKIE_NAME}= {t}"), one_bad.clone()),
            (format!("{COOKIE_NAME}=\"{t}\""), one_bad.clone()),
            (COOKIE_NAME.to_owned(), none.clone()),
            (format!("{COOKIE_NAME}="), one_bad.clone()),
            (format!("{COOKIE_NAME}={t}={t}"), one_bad.clone()),
            (format!("x={COOKIE_NAME}={t}"), none.clone()),
            (
                format!("{COOKIE_NAME}={t}; {COOKIE_NAME}={t}"),
                Presented::Many,
            ),
            (format!("{COOKIE_NAME}=; {COOKIE_NAME}="), Presented::Many),
            (
                format!("{COOKIE_NAME}=1;x=2;{COOKIE_NAME}=3"),
                Presented::Many,
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(presented(&headers(&[&text])), expected, "{text:?}");
        }
    }

    /// A name of a generated pair: the session name or another name.
    fn name() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(COOKIE_NAME.to_owned()),
            Just("__host-telemetry_session".to_owned()),
            Just(format!("{COOKIE_NAME}2")),
            "[a-z_]{1,10}",
        ]
    }

    proptest! {
        /// An independent oracle: the header is built from a list of
        /// pairs, and the expected result comes from that list, not from a
        /// parse of the text.
        #[test]
        fn the_result_follows_the_generated_pairs(
            pairs in proptest::collection::vec(
                (name(), prop_oneof![
                    proptest::array::uniform32(any::<u8>()).prop_map(|b| SessionToken::from_bytes(b).to_hex()),
                    "[a-zA-Z0-9]{0,12}",
                ]),
                0..6,
            ),
            separator in prop_oneof![Just(";"), Just("; "), Just(" ;  ")],
        ) {
            let text = pairs
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(separator);
            let session: Vec<&String> = pairs
                .iter()
                .filter(|(name, _)| name == COOKIE_NAME)
                .map(|(_, value)| value)
                .collect();
            let expected = match session.as_slice() {
                [] => Presented::None,
                [value] => Presented::One(SessionToken::from_hex(value)),
                _ => Presented::Many,
            };
            prop_assert_eq!(presented(&headers(&[&text])), expected);
        }
    }

    /// The number of pairs with the exact session name, counted in a
    /// second, independent way: the rule is "0 is none, 1 is one, more is
    /// many".
    fn count_pairs(text: &str) -> usize {
        text.split(';')
            .filter(|pair| {
                pair.trim()
                    .split_once('=')
                    .is_some_and(|(name, _)| name == COOKIE_NAME)
            })
            .count()
    }

    proptest! {
        #[test]
        fn the_result_follows_the_number_of_session_pairs(
            parts in proptest::collection::vec(
                prop_oneof![
                    "[a-z]{1,8}=[a-z0-9]{0,8}".prop_map(|pair| pair),
                    proptest::array::uniform32(any::<u8>())
                        .prop_map(|bytes| format!("{COOKIE_NAME}={}", SessionToken::from_bytes(bytes).to_hex())),
                    "[ -:<-~]{0,12}".prop_map(|value| format!("{COOKIE_NAME}={value}")),
                ],
                0..6,
            ),
        ) {
            let text = parts.join("; ");
            if let Ok(value) = HeaderValue::from_str(&text) {
                let mut map = HeaderMap::new();
                map.insert(COOKIE, value);
                let found = presented(&map);
                match count_pairs(&text) {
                    0 => prop_assert_eq!(found, Presented::None),
                    1 => prop_assert!(matches!(found, Presented::One(_))),
                    _ => prop_assert_eq!(found, Presented::Many),
                }
            }
        }

        #[test]
        fn parsing_never_panics_and_a_token_is_canonical(text in "[ -~]{0,300}") {
            if let Ok(value) = HeaderValue::from_str(&text) {
                let mut map = HeaderMap::new();
                map.insert(COOKIE, value);
                if let Some(token) = presented(&map).token() {
                    let pair = format!("{COOKIE_NAME}={}", token.to_hex());
                    prop_assert!(text.contains(&pair));
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
            prop_assert_eq!(found, Presented::One(Some(token)));
        }
    }
}
