//! The public error contract: RFC 9457 problem details with a closed set
//! of application codes.
//!
//! Every failure of the server is a [`Problem`] with the content type
//! `application/problem+json`:
//!
//! ```json
//! {"type":"about:blank","title":"Service Unavailable","status":503,
//!  "code":"timeout","detail":"The request took too long.","retry_after_seconds":5}
//! ```
//!
//! - `type` is always `about:blank`, so `title` is the reason phrase of the
//!   status (RFC 9457, section 4.2.1).
//! - `code` is a [`ProblemCode`]. The client handles each code, and an
//!   unknown code as a failure.
//! - `detail` is a static English text for each code. It never holds a
//!   request value, a database text, or personal data.
//! - `retry_after_seconds` is present only with the status 429 and 503.
//!   The `Retry-After` header holds the same value.
//!
//! [`ProblemCode::status`] is the one exhaustive mapping from a code to a
//! status. [`Problem::from`] maps the axum JSON and query rejections, and
//! [`code_for_status`] maps a library response without a problem body
//! (`crate::app` uses it as a safety net).

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// The media type of a problem body.
pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

/// The `type` of every problem.
pub const ABOUT_BLANK: &str = "about:blank";

/// The application code of a problem. The set is closed: a new code is a
/// contract change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemCode {
    /// 401: the request has no valid session.
    Unauthenticated,
    /// 401: the login password is wrong.
    InvalidCredentials,
    /// 429: too many login attempts; try again later.
    LoginThrottled,
    /// 403: the request did not come from the application origin.
    ForbiddenOrigin,
    /// 400: the request is not valid.
    InvalidRequest,
    /// 415: the request body has a wrong content type.
    UnsupportedMediaType,
    /// 413: the request body is larger than the limit.
    PayloadTooLarge,
    /// 404: no such resource.
    NotFound,
    /// 405: the resource does not support the method.
    MethodNotAllowed,
    /// 503: the database cannot be reached.
    DatabaseUnavailable,
    /// 503: the server has no free capacity.
    ServiceBusy,
    /// 503: the request took too long.
    Timeout,
    /// 500: stored data breaks an assumption of the server, for example
    /// more than one vehicle.
    DataContractViolation,
    /// 500: an unexpected failure of the server.
    InternalError,
}

impl ProblemCode {
    /// Every code, in the order of the declaration.
    pub const ALL: [Self; 14] = [
        Self::Unauthenticated,
        Self::InvalidCredentials,
        Self::LoginThrottled,
        Self::ForbiddenOrigin,
        Self::InvalidRequest,
        Self::UnsupportedMediaType,
        Self::PayloadTooLarge,
        Self::NotFound,
        Self::MethodNotAllowed,
        Self::DatabaseUnavailable,
        Self::ServiceBusy,
        Self::Timeout,
        Self::DataContractViolation,
        Self::InternalError,
    ];

    /// The HTTP status of the code.
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated | Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
            Self::LoginThrottled => StatusCode::TOO_MANY_REQUESTS,
            Self::ForbiddenOrigin => StatusCode::FORBIDDEN,
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::DatabaseUnavailable | Self::ServiceBusy | Self::Timeout => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::DataContractViolation | Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The wire text of the code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::InvalidCredentials => "invalid_credentials",
            Self::LoginThrottled => "login_throttled",
            Self::ForbiddenOrigin => "forbidden_origin",
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::PayloadTooLarge => "payload_too_large",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::DatabaseUnavailable => "database_unavailable",
            Self::ServiceBusy => "service_busy",
            Self::Timeout => "timeout",
            Self::DataContractViolation => "data_contract_violation",
            Self::InternalError => "internal_error",
        }
    }

    /// The static English `detail` of the code.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Unauthenticated => "The request has no valid session.",
            Self::InvalidCredentials => "The password is not correct.",
            Self::LoginThrottled => "Too many login attempts. Try again later.",
            Self::ForbiddenOrigin => "The request did not come from the application origin.",
            Self::InvalidRequest => "The request is not valid.",
            Self::UnsupportedMediaType => "The request body has an unsupported content type.",
            Self::PayloadTooLarge => "The request body is larger than the limit.",
            Self::NotFound => "The resource does not exist.",
            Self::MethodNotAllowed => "The resource does not support this method.",
            Self::DatabaseUnavailable => "The database is not available. Try again later.",
            Self::ServiceBusy => "The server is busy. Try again later.",
            Self::Timeout => "The request took too long. Try again later.",
            Self::DataContractViolation => "The stored data is not in the expected form.",
            Self::InternalError => "An unexpected error occurred on the server.",
        }
    }

    /// The default `Retry-After` of a code with the status 429 or 503, and
    /// `None` for every other code.
    ///
    /// The values are provisional choices of the scaffold; no requirement
    /// sets them. The login throttle (`login_throttled`) gives its real
    /// wait with [`Problem::with_retry_after`].
    #[must_use]
    pub const fn default_retry_after(self) -> Option<RetryAfter> {
        match self {
            Self::LoginThrottled => Some(RetryAfter(60)),
            Self::DatabaseUnavailable => Some(RetryAfter(30)),
            Self::ServiceBusy | Self::Timeout => Some(RetryAfter(5)),
            Self::Unauthenticated
            | Self::InvalidCredentials
            | Self::ForbiddenOrigin
            | Self::InvalidRequest
            | Self::UnsupportedMediaType
            | Self::PayloadTooLarge
            | Self::NotFound
            | Self::MethodNotAllowed
            | Self::DataContractViolation
            | Self::InternalError => None,
        }
    }
}

/// A `Retry-After` value in whole seconds, from 1 to 86400.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RetryAfter(u32);

impl RetryAfter {
    /// The largest value: one day.
    pub const MAX_SECONDS: u32 = 86_400;

    /// Makes a value from whole seconds. Returns `None` outside 1 to 86400.
    #[must_use]
    pub const fn from_seconds(seconds: u32) -> Option<Self> {
        if seconds >= 1 && seconds <= Self::MAX_SECONDS {
            Some(Self(seconds))
        } else {
            None
        }
    }

    /// The value in seconds.
    #[must_use]
    pub const fn seconds(self) -> u32 {
        self.0
    }
}

/// A problem details body (RFC 9457) and its status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Problem {
    /// Always `about:blank`.
    #[serde(rename = "type")]
    #[schema(example = "about:blank")]
    type_: &'static str,
    /// The reason phrase of the status.
    #[schema(example = "Service Unavailable")]
    title: &'static str,
    /// The HTTP status of the response.
    #[schema(minimum = 100, maximum = 599, example = 503)]
    status: u16,
    /// The application code.
    code: ProblemCode,
    /// A static English text for the code.
    #[schema(example = "The request took too long. Try again later.")]
    detail: &'static str,
    /// Seconds to wait before a new attempt. Present only with the status
    /// 429 and 503; the `Retry-After` header holds the same value.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(
        required = false,
        nullable = false,
        minimum = 1,
        maximum = 86400,
        example = 5
    )]
    retry_after_seconds: Option<u32>,
}

impl Problem {
    /// The problem of `code` with its default `Retry-After`.
    #[must_use]
    pub fn new(code: ProblemCode) -> Self {
        let status = code.status();
        // Every status of `ProblemCode::status` has a reason phrase.
        let title = status.canonical_reason().unwrap_or("Error");
        let retry_after_seconds = code.default_retry_after().map(RetryAfter::seconds);
        Self {
            type_: ABOUT_BLANK,
            title,
            status: status.as_u16(),
            code,
            detail: code.detail(),
            retry_after_seconds,
        }
    }

    /// Replaces the `Retry-After` of a code with the status 429 or 503.
    /// For every other code the problem stays without `Retry-After`, as
    /// the contract requires.
    #[must_use]
    pub const fn with_retry_after(mut self, retry: RetryAfter) -> Self {
        if self.retry_after_seconds.is_some() {
            self.retry_after_seconds = Some(retry.seconds());
        }
        self
    }

    /// The application code.
    #[must_use]
    pub const fn code(&self) -> ProblemCode {
        self.code
    }

    /// The `Retry-After` in seconds, if the code has one.
    #[must_use]
    pub const fn retry_after_seconds(&self) -> Option<u32> {
        self.retry_after_seconds
    }
}

impl From<ProblemCode> for Problem {
    fn from(code: ProblemCode) -> Self {
        Self::new(code)
    }
}

/// The body of an internal error, for the case that the serialisation of
/// a problem fails. The serialisation of these fixed fields cannot fail
/// in practice; the constant keeps the response a valid problem anyway.
const INTERNAL_ERROR_BODY: &str = r#"{"code":"internal_error","detail":"An unexpected error occurred on the server.","status":500,"title":"Internal Server Error","type":"about:blank"}"#;

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let (status, body) = match serde_json::to_vec(&self) {
            Ok(body) => (self.code.status(), body),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR_BODY.as_bytes().to_vec(),
            ),
        };
        let mut response = (status, body).into_response();
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(PROBLEM_CONTENT_TYPE));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(seconds) = self.retry_after_seconds
            && status != StatusCode::INTERNAL_SERVER_ERROR
        {
            headers.insert(RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

impl From<JsonRejection> for Problem {
    /// Maps a rejection of the axum `Json` extractor. The text of the
    /// rejection names the failing field or the parser position; the
    /// problem never holds it.
    fn from(rejection: JsonRejection) -> Self {
        let code = match rejection {
            JsonRejection::MissingJsonContentType(_) => ProblemCode::UnsupportedMediaType,
            JsonRejection::BytesRejection(_) => {
                code_for_status(rejection.status()).unwrap_or(ProblemCode::InvalidRequest)
            }
            // `JsonDataError` (422), `JsonSyntaxError` (400), and any later
            // variant: the body is not valid for the request.
            _ => ProblemCode::InvalidRequest,
        };
        Self::new(code)
    }
}

impl From<QueryRejection> for Problem {
    /// Maps a rejection of the axum `Query` extractor: a missing, unknown,
    /// or invalid query parameter. The text of the rejection names the
    /// parameter and the value; the problem never holds it.
    fn from(_rejection: QueryRejection) -> Self {
        Self::new(ProblemCode::InvalidRequest)
    }
}

/// The code for an error status that a library response has, or `None`
/// for a status without a code.
///
/// The mapping covers the statuses that axum and tower-http produce: 400,
/// 404, 405, 413, 415, 422 (as `invalid_request`, status 400), 500, and
/// 503 (as `service_busy`). A status without a code (for example 416 of a
/// range request) keeps its library response.
#[must_use]
pub const fn code_for_status(status: StatusCode) -> Option<ProblemCode> {
    match status.as_u16() {
        400 | 422 => Some(ProblemCode::InvalidRequest),
        404 => Some(ProblemCode::NotFound),
        405 => Some(ProblemCode::MethodNotAllowed),
        413 => Some(ProblemCode::PayloadTooLarge),
        415 => Some(ProblemCode::UnsupportedMediaType),
        500 => Some(ProblemCode::InternalError),
        503 => Some(ProblemCode::ServiceBusy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use axum::body::to_bytes;
    use serde_json::Value;

    use super::*;

    /// The position of each code in [`ProblemCode::ALL`]. The match is
    /// exhaustive, so a new variant fails to compile until it is added
    /// here and to `ALL`.
    const fn position(code: ProblemCode) -> usize {
        match code {
            ProblemCode::Unauthenticated => 0,
            ProblemCode::InvalidCredentials => 1,
            ProblemCode::LoginThrottled => 2,
            ProblemCode::ForbiddenOrigin => 3,
            ProblemCode::InvalidRequest => 4,
            ProblemCode::UnsupportedMediaType => 5,
            ProblemCode::PayloadTooLarge => 6,
            ProblemCode::NotFound => 7,
            ProblemCode::MethodNotAllowed => 8,
            ProblemCode::DatabaseUnavailable => 9,
            ProblemCode::ServiceBusy => 10,
            ProblemCode::Timeout => 11,
            ProblemCode::DataContractViolation => 12,
            ProblemCode::InternalError => 13,
        }
    }

    #[test]
    fn all_lists_every_code_once() {
        for (index, code) in ProblemCode::ALL.iter().enumerate() {
            assert_eq!(position(*code), index, "{code:?}");
        }
        let unique: HashSet<_> = ProblemCode::ALL.iter().collect();
        assert_eq!(unique.len(), ProblemCode::ALL.len());
    }

    /// The expected status of each code, from the contract (design §5).
    const EXPECTED: [(&str, u16); 14] = [
        ("unauthenticated", 401),
        ("invalid_credentials", 401),
        ("login_throttled", 429),
        ("forbidden_origin", 403),
        ("invalid_request", 400),
        ("unsupported_media_type", 415),
        ("payload_too_large", 413),
        ("not_found", 404),
        ("method_not_allowed", 405),
        ("database_unavailable", 503),
        ("service_busy", 503),
        ("timeout", 503),
        ("data_contract_violation", 500),
        ("internal_error", 500),
    ];

    #[test]
    fn each_code_has_the_status_of_the_contract() {
        for (code, (text, status)) in ProblemCode::ALL.iter().zip(EXPECTED) {
            assert_eq!(code.as_str(), text);
            assert_eq!(code.status().as_u16(), status, "{text}");
            assert_eq!(serde_json::to_value(code).unwrap(), Value::from(text));
        }
    }

    #[tokio::test]
    async fn each_code_gives_a_problem_response() {
        for code in ProblemCode::ALL {
            let response = Problem::new(code).into_response();
            let status = response.status();
            assert_eq!(status, code.status());
            let headers = response.headers().clone();
            assert_eq!(headers[CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
            assert_eq!(headers[CACHE_CONTROL], "no-store");
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            let object = body.as_object().unwrap();
            assert_eq!(body["type"], "about:blank");
            assert_eq!(body["title"], status.canonical_reason().unwrap());
            assert_eq!(body["status"], status.as_u16());
            assert_eq!(body["code"], code.as_str());
            assert_eq!(body["detail"], code.detail());
            let retryable = matches!(status.as_u16(), 429 | 503);
            if retryable {
                let seconds = body["retry_after_seconds"].as_u64().unwrap();
                assert!(seconds >= 1);
                assert_eq!(headers[RETRY_AFTER], seconds.to_string().as_str());
                assert_eq!(object.len(), 6);
            } else {
                assert!(!object.contains_key("retry_after_seconds"), "{code:?}");
                assert!(!headers.contains_key(RETRY_AFTER), "{code:?}");
                assert_eq!(object.len(), 5);
            }
        }
    }

    #[test]
    fn with_retry_after_changes_only_a_retryable_code() {
        let retry = RetryAfter::from_seconds(120).unwrap();
        let throttled = Problem::new(ProblemCode::LoginThrottled).with_retry_after(retry);
        assert_eq!(throttled.retry_after_seconds(), Some(120));
        let not_found = Problem::new(ProblemCode::NotFound).with_retry_after(retry);
        assert_eq!(not_found.retry_after_seconds(), None);
    }

    #[test]
    fn retry_after_has_bounds() {
        assert_eq!(RetryAfter::from_seconds(0), None);
        assert_eq!(RetryAfter::from_seconds(86_401), None);
        assert_eq!(
            RetryAfter::from_seconds(1).map(RetryAfter::seconds),
            Some(1)
        );
        assert_eq!(
            RetryAfter::from_seconds(86_400).map(RetryAfter::seconds),
            Some(86_400)
        );
    }

    #[test]
    fn maps_library_statuses() {
        let cases = [
            (400, Some(ProblemCode::InvalidRequest)),
            (422, Some(ProblemCode::InvalidRequest)),
            (404, Some(ProblemCode::NotFound)),
            (405, Some(ProblemCode::MethodNotAllowed)),
            (413, Some(ProblemCode::PayloadTooLarge)),
            (415, Some(ProblemCode::UnsupportedMediaType)),
            (500, Some(ProblemCode::InternalError)),
            (503, Some(ProblemCode::ServiceBusy)),
            (416, None),
            (304, None),
            (200, None),
        ];
        for (status, code) in cases {
            assert_eq!(
                code_for_status(StatusCode::from_u16(status).unwrap()),
                code,
                "{status}"
            );
        }
    }

    #[test]
    fn the_fallback_body_is_the_internal_error_problem() {
        let expected = serde_json::to_value(Problem::new(ProblemCode::InternalError)).unwrap();
        let fallback: Value = serde_json::from_str(INTERNAL_ERROR_BODY).unwrap();
        assert_eq!(fallback, expected);
    }
}
