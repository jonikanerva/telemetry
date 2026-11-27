//! The CSRF rule: a state-changing request must come from the public
//! origin of the application.
//!
//! Every API request with a method other than `GET` and `HEAD` passes the
//! rule before its handler:
//!
//! 1. One `Origin` header that equals `WEB_PUBLIC_ORIGIN` byte for byte
//!    passes. Browsers send `Origin` on every cross-origin request and on
//!    every same-origin request with these methods.
//! 2. Without `Origin`, the header `Sec-Fetch-Site: same-origin` passes. A
//!    browser sets this header itself; a page cannot set it.
//! 3. Every other request gets 403 `forbidden_origin`. This includes
//!    `Origin: null`, two `Origin` headers, and a request with neither
//!    header.
//!
//! `SameSite=Strict` on the session cookie is the second layer: a
//! cross-site request carries no session. `GET` handlers change nothing
//! except the idle time of the session.

use axum::http::{HeaderMap, Method};

use crate::config::ConfigError;

/// The name of the variable that holds the public origin.
pub const ORIGIN_VARIABLE: &str = "WEB_PUBLIC_ORIGIN";

/// The serialised origin of the application, for example
/// `https://tesla.calmdonut.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin(String);

impl PublicOrigin {
    /// Parses an origin in the form that browsers send: `https://`, a
    /// lowercase host name, an optional port, and nothing after it.
    ///
    /// # Errors
    ///
    /// Returns a static reason for every other text.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        const REASON: &str =
            "expected https:// and a lowercase host name with an optional port, without a path";
        let rest = text.strip_prefix("https://").ok_or(REASON)?;
        let (host, port) = match rest.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (rest, None),
        };
        let host_ok = !host.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            });
        let port_ok = port.is_none_or(|port| {
            !port.is_empty()
                && !port.starts_with('0')
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port != 443)
        });
        if host_ok && port_ok {
            Ok(Self(text.to_owned()))
        } else {
            Err(REASON)
        }
    }

    /// Reads and checks `WEB_PUBLIC_ORIGIN`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] with the variable name and a static reason.
    pub fn from_variable(value: Option<std::ffi::OsString>) -> Result<Self, ConfigError> {
        let value = value.ok_or(ConfigError::Missing {
            name: ORIGIN_VARIABLE,
        })?;
        let invalid = |reason| ConfigError::Invalid {
            name: ORIGIN_VARIABLE,
            reason,
        };
        let text = value.to_str().ok_or_else(|| {
            invalid(
                "expected https:// and a lowercase host name with an optional port, without a path",
            )
        })?;
        Self::parse(text).map_err(invalid)
    }

    /// The origin text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// True when the method can change state: every method except `GET` and
/// `HEAD`.
#[must_use]
pub fn changes_state(method: &Method) -> bool {
    method != Method::GET && method != Method::HEAD
}

/// True when the request may change state under the rule above.
#[must_use]
pub fn is_allowed(headers: &HeaderMap, origin: &PublicOrigin) -> bool {
    let mut origins = headers.get_all("origin").iter();
    match (origins.next(), origins.next()) {
        (Some(value), None) => value.as_bytes() == origin.as_str().as_bytes(),
        (Some(_), Some(_)) => false,
        (None, _) => {
            let mut sites = headers.get_all("sec-fetch-site").iter();
            matches!(
                (sites.next(), sites.next()),
                (Some(site), None) if site.as_bytes() == b"same-origin"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use proptest::prelude::*;

    use super::*;

    const PUBLIC: &str = "https://tesla.calmdonut.com";

    fn origin() -> PublicOrigin {
        PublicOrigin::parse(PUBLIC).unwrap()
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn parses_the_accepted_origin_forms() {
        for text in [
            PUBLIC,
            "https://localhost:4173",
            "https://a.b-c.example",
            "https://x1.example:65535",
        ] {
            assert_eq!(PublicOrigin::parse(text).unwrap().as_str(), text);
        }
    }

    #[test]
    fn rejects_other_origin_forms() {
        for text in [
            "",
            "http://tesla.calmdonut.com",
            "https://",
            "https://Tesla.calmdonut.com",
            "https://tesla.calmdonut.com/",
            "https://tesla.calmdonut.com/path",
            "https://tesla.calmdonut.com?x",
            "https://user@tesla.calmdonut.com",
            "https://tesla.calmdonut.com:443",
            "https://tesla.calmdonut.com:0",
            "https://tesla.calmdonut.com:08080",
            "https://tesla.calmdonut.com:65536",
            "https://tesla.calmdonut.com:",
            "https://-a.example",
            "https://a..example",
            " https://tesla.calmdonut.com",
            "https://[::1]",
        ] {
            assert!(PublicOrigin::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn the_configuration_error_names_only_the_variable() {
        let err =
            PublicOrigin::from_variable(Some("http://secret-host.example".into())).unwrap_err();
        assert_eq!(err.name(), ORIGIN_VARIABLE);
        assert!(!format!("{err} {err:?}").contains("secret-host"));
        assert_eq!(
            PublicOrigin::from_variable(None).unwrap_err(),
            ConfigError::Missing {
                name: ORIGIN_VARIABLE
            }
        );
    }

    #[test]
    fn only_get_and_head_change_nothing() {
        assert!(!changes_state(&Method::GET));
        assert!(!changes_state(&Method::HEAD));
        for method in [
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::OPTIONS,
            Method::from_bytes(b"PURGE").unwrap(),
        ] {
            assert!(changes_state(&method), "{method}");
        }
    }

    #[test]
    fn the_public_origin_passes() {
        assert!(is_allowed(&headers(&[("origin", PUBLIC)]), &origin()));
        // A correct Origin needs no Sec-Fetch-Site.
        assert!(is_allowed(
            &headers(&[("origin", PUBLIC), ("sec-fetch-site", "cross-site")]),
            &origin()
        ));
    }

    #[test]
    fn same_origin_fetch_metadata_passes_without_origin() {
        assert!(is_allowed(
            &headers(&[("sec-fetch-site", "same-origin")]),
            &origin()
        ));
    }

    #[test]
    fn every_other_request_is_refused() {
        let cases: [&[(&'static str, &str)]; 11] = [
            &[],
            &[("origin", "https://evil.example")],
            &[("origin", "null")],
            &[("origin", "http://tesla.calmdonut.com")],
            &[("origin", "https://tesla.calmdonut.com:443")],
            &[("origin", "https://tesla.calmdonut.com/")],
            &[("origin", "https://TESLA.calmdonut.com")],
            &[("origin", "https://tesla.calmdonut.com.evil.example")],
            &[("origin", PUBLIC), ("origin", PUBLIC)],
            &[("sec-fetch-site", "same-site")],
            &[
                ("sec-fetch-site", "same-origin"),
                ("sec-fetch-site", "same-origin"),
            ],
        ];
        for case in cases {
            assert!(!is_allowed(&headers(case), &origin()), "{case:?}");
        }
        // A wrong Origin fails also with same-origin fetch metadata.
        assert!(!is_allowed(
            &headers(&[
                ("origin", "https://evil.example"),
                ("sec-fetch-site", "same-origin")
            ]),
            &origin()
        ));
    }

    proptest! {
        #[test]
        fn only_the_exact_origin_passes(text in "[ -~]{0,60}") {
            if let Ok(value) = HeaderValue::from_str(&text) {
                let mut map = HeaderMap::new();
                map.insert("origin", value);
                prop_assert_eq!(is_allowed(&map, &origin()), text == PUBLIC);
            }
        }

        #[test]
        fn a_parsed_origin_is_its_own_text(text in "https://[a-z0-9.:-]{1,30}") {
            if let Ok(parsed) = PublicOrigin::parse(&text) {
                prop_assert_eq!(parsed.as_str(), text.as_str());
                prop_assert!(!text.ends_with('/'));
            }
        }
    }
}
