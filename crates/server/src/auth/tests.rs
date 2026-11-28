//! HTTP tests of the session API and the guard: real requests through the
//! real router and middleware (`crate::app`), with `oneshot` and no
//! socket.
//!
//! Expected values come from ADR 0003, the owner decisions of the task
//! (8 sessions, 30 days absolute, 7 days idle, the cookie attributes, a
//! global throttle), design §3 and §5, and the committed
//! `contract/openapi.json`. The verifier is a fake, so the tests run no
//! Argon2 and can pause the clock; `password.rs` tests the real verifier.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, RETRY_AFTER, SET_COOKIE};
use axum::http::{HeaderMap, Request as HttpRequest, StatusCode};
use axum::routing::get;
use jiff::Timestamp;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

use super::*;
use crate::app::{AppState, build_with_api};
use crate::config::{StaticFiles, Version};
use crate::logging::capture;
use crate::problem::PROBLEM_CONTENT_TYPE;
use crate::test_support::TempDir;

/// A test-only password of the fake verifier.
pub(crate) const PASSWORD: &str = "test-only password 0123456789";
const ORIGIN: &str = "https://telemetry.test";
const START: &str = "2026-11-27T10:00:00Z";
const DAY: Duration = Duration::from_hours(24);

/// A verifier that accepts [`PASSWORD`], counts its calls, and can hold a
/// call until the test releases it.
#[derive(Debug, Default)]
pub(crate) struct FakeVerifier {
    calls: AtomicUsize,
    running: AtomicUsize,
    most_at_once: AtomicUsize,
    gate: Option<Gate>,
}

#[derive(Debug, Default)]
struct Gate {
    entered: Notify,
    open: Mutex<bool>,
    opened: Condvar,
}

impl FakeVerifier {
    fn gated() -> Self {
        Self {
            gate: Some(Gate::default()),
            ..Self::default()
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    async fn wait_until_entered(&self) {
        if let Some(gate) = &self.gate {
            gate.entered.notified().await;
        }
    }

    fn release(&self) {
        if let Some(gate) = &self.gate {
            *gate.open.lock().unwrap() = true;
            gate.opened.notify_all();
        }
    }
}

impl Verifier for FakeVerifier {
    fn verify(&self, password: &[u8]) -> Verification {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_at_once.fetch_max(running, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            let mut open = gate.open.lock().unwrap();
            while !*open {
                open = gate.opened.wait(open).unwrap();
            }
        } else {
            // A short real pause, so concurrent requests overlap.
            std::thread::sleep(Duration::from_millis(2));
        }
        self.running.fetch_sub(1, Ordering::SeqCst);
        if password == PASSWORD.as_bytes() {
            Verification::Match
        } else {
            Verification::Mismatch
        }
    }
}

fn start() -> Timestamp {
    START.parse().unwrap()
}

pub(crate) fn state_with(verifier: Arc<FakeVerifier>) -> State {
    state_with_limits(verifier, SessionLimits::OWNER, ThrottleLimits::DEFAULT)
}

fn state_with_limits(
    verifier: Arc<FakeVerifier>,
    sessions: SessionLimits,
    throttle: ThrottleLimits,
) -> State {
    State::from_parts(Parts {
        origin: PublicOrigin::parse(ORIGIN).unwrap(),
        clock: Arc::new(clock::ShiftableClock::new(start())),
        verifier,
        sessions,
        throttle,
    })
}

#[derive(Debug)]
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn problem(&self, code: ProblemCode) -> Value {
        assert_eq!(
            self.status,
            code.status(),
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        assert_eq!(self.headers[CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
        assert_eq!(self.headers[CACHE_CONTROL], "no-store");
        let body = self.json();
        assert_eq!(body["code"], code.as_str());
        body
    }

    fn set_cookie(&self) -> Option<&str> {
        self.headers
            .get(SET_COOKIE)
            .map(|value| value.to_str().unwrap())
    }

    /// The `name=value` pair of the new session cookie.
    fn session_pair(&self) -> String {
        let cookie = self.set_cookie().expect("a session cookie");
        cookie.split(';').next().unwrap().to_owned()
    }
}

struct App {
    _dir: TempDir,
    router: Router,
    verifier: Arc<FakeVerifier>,
    battery_calls: Arc<AtomicUsize>,
}

impl App {
    fn new() -> Self {
        Self::with(
            Arc::new(FakeVerifier::default()),
            SessionLimits::OWNER,
            ThrottleLimits::DEFAULT,
        )
    }

    fn with(
        verifier: Arc<FakeVerifier>,
        sessions: SessionLimits,
        throttle: ThrottleLimits,
    ) -> Self {
        let state = state_with_limits(Arc::clone(&verifier), sessions, throttle);
        Self::from_state(state, verifier)
    }

    fn from_state(state: State, verifier: Arc<FakeVerifier>) -> Self {
        let dir = TempDir::new();
        dir.write(
            "index.html",
            b"<meta name=\"telemetry-version\" content=\"__TELEMETRY_VERSION__\">",
        );
        dir.write("assets/app.js", b"x");
        let static_files = StaticFiles::load(dir.path()).unwrap();
        let (battery, consumption) = crate::battery::unit_test_state();
        let app_state = AppState::new(Version::unknown(), state, battery, consumption);
        // The production assembly (`app::build_with_api` with the routes of
        // the contract), with a counting route at the mount point of the
        // battery API, where slice S3 mounts `battery::router()`.
        let battery_calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&battery_calls);
        let slice = utoipa_axum::router::OpenApiRouter::new().route(
            "/battery",
            get(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { "data" }
            })
            .post(|| async { "changed" }),
        );
        // The consumption API is mounted as in production; its source is
        // never reached without a session.
        let api = crate::openapi::documented_router_with(slice.merge(crate::consumption::router()));
        let router = build_with_api(app_state, &static_files, Router::new(), api);
        Self {
            _dir: dir,
            router,
            verifier,
            battery_calls,
        }
    }

    async fn call(&self, request: HttpRequest<Body>) -> Reply {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn login_with(&self, password: &str, cookie: Option<&str>) -> Reply {
        let mut request = login_request(&json!({ "password": password }).to_string());
        if let Some(cookie) = cookie {
            request
                .headers_mut()
                .insert(COOKIE, cookie.parse().unwrap());
        }
        self.call(request).await
    }

    async fn login(&self) -> String {
        let reply = self.login_with(PASSWORD, None).await;
        assert_eq!(reply.status, StatusCode::OK);
        reply.session_pair()
    }

    async fn session(&self, cookie: Option<&str>) -> Reply {
        self.call(with_cookie(get_request("/api/v1/session"), cookie))
            .await
    }

    async fn logout(&self, cookie: Option<&str>) -> Reply {
        let mut request = HttpRequest::builder()
            .method("DELETE")
            .uri("/api/v1/session")
            .header("origin", ORIGIN)
            .body(Body::empty())
            .unwrap();
        if let Some(cookie) = cookie {
            request
                .headers_mut()
                .insert(COOKIE, cookie.parse().unwrap());
        }
        self.call(request).await
    }
}

fn get_request(uri: &str) -> HttpRequest<Body> {
    HttpRequest::builder().uri(uri).body(Body::empty()).unwrap()
}

fn with_cookie(mut request: HttpRequest<Body>, cookie: Option<&str>) -> HttpRequest<Body> {
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert(COOKIE, cookie.parse().unwrap());
    }
    request
}

fn login_request(body: &str) -> HttpRequest<Body> {
    HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/session")
        .header("origin", ORIGIN)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn instant(value: &Value) -> Timestamp {
    value.as_str().unwrap().parse().unwrap()
}

// ---- Login, session, and the cookie ----------------------------------

#[tokio::test(start_paused = true)]
async fn a_request_without_a_session_gets_401_unauthenticated() {
    let app = App::new();
    let reply = app.session(None).await;
    reply.problem(ProblemCode::Unauthenticated);
    // Without a presented cookie the response sets none.
    assert_eq!(reply.set_cookie(), None);
}

#[tokio::test(start_paused = true)]
async fn a_correct_login_sets_the_host_cookie_and_returns_the_deadlines() {
    let app = App::new();
    let reply = app.login_with(PASSWORD, None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.headers[CONTENT_TYPE], "application/json");
    assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
    let cookie = reply.set_cookie().unwrap();
    let (pair, attributes) = cookie.split_once("; ").unwrap();
    let (name, value) = pair.split_once('=').unwrap();
    assert_eq!(name, "__Host-telemetry_session");
    assert_eq!(value.len(), 64);
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    assert_eq!(
        attributes,
        "Max-Age=2592000; Path=/; Secure; HttpOnly; SameSite=Strict"
    );
    let body = reply.json();
    assert_eq!(
        instant(&body["expires_at"]),
        start().checked_add(30 * DAY).unwrap()
    );
    assert_eq!(
        instant(&body["idle_expires_at"]),
        start().checked_add(7 * DAY).unwrap()
    );
    assert_eq!(body.as_object().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn the_session_cookie_gives_the_session_and_slides_the_idle_time() {
    let app = App::new();
    let cookie = app.login().await;
    tokio::time::advance(DAY).await;
    let reply = app.session(Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    // The absolute end stays; the idle end moves with the use.
    assert_eq!(
        instant(&body["expires_at"]),
        start().checked_add(30 * DAY).unwrap()
    );
    assert_eq!(
        instant(&body["idle_expires_at"]),
        start().checked_add(8 * DAY).unwrap()
    );
    // Reading the session sets no new cookie.
    assert_eq!(reply.set_cookie(), None);
}

#[tokio::test(start_paused = true)]
async fn a_wrong_password_gets_401_invalid_credentials_without_a_cookie() {
    let app = App::new();
    let reply = app.login_with("wrong password", None).await;
    reply.problem(ProblemCode::InvalidCredentials);
    assert_eq!(reply.set_cookie(), None);
    assert_eq!(app.verifier.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_login_ends_the_presented_session_and_never_reuses_its_token() {
    let app = App::new();
    let first = app.login().await;
    let second_reply = app.login_with(PASSWORD, Some(&first)).await;
    let second = second_reply.session_pair();
    assert_ne!(first, second);
    app.session(Some(&first))
        .await
        .problem(ProblemCode::Unauthenticated);
    assert_eq!(app.session(Some(&second)).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_login_never_takes_a_token_that_the_client_chose() {
    let app = App::new();
    let chosen = format!("__Host-telemetry_session={}", "ab".repeat(32));
    let reply = app.login_with(PASSWORD, Some(&chosen)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_ne!(reply.session_pair(), chosen);
    app.session(Some(&chosen))
        .await
        .problem(ProblemCode::Unauthenticated);
}

#[tokio::test(start_paused = true)]
async fn a_failed_login_keeps_the_presented_session() {
    let app = App::new();
    let cookie = app.login().await;
    app.login_with("wrong password", Some(&cookie))
        .await
        .problem(ProblemCode::InvalidCredentials);
    assert_eq!(app.session(Some(&cookie)).await.status, StatusCode::OK);
}

// ---- Logout, expiry, restart, and the bound ----------------------------

#[tokio::test(start_paused = true)]
async fn logout_ends_the_session_and_removes_the_cookie() {
    let app = App::new();
    let cookie = app.login().await;
    let reply = app.logout(Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(
        reply.set_cookie(),
        Some("__Host-telemetry_session=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict")
    );
    // A stolen copy of the cookie stops working.
    let after = app.session(Some(&cookie)).await;
    after.problem(ProblemCode::Unauthenticated);
    // A 401 changes no cookie (see `a_late_401_never_removes_a_new_cookie`).
    assert_eq!(after.set_cookie(), None);
}

#[tokio::test(start_paused = true)]
async fn logout_without_a_session_is_204_too() {
    let app = App::new();
    let reply = app.logout(None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.set_cookie().unwrap().contains("Max-Age=0"));
}

#[tokio::test(start_paused = true)]
async fn logout_ends_only_its_own_session() {
    let app = App::new();
    let phone = app.login().await;
    let desktop = app.login().await;
    app.logout(Some(&phone)).await;
    assert_eq!(app.session(Some(&desktop)).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn an_unused_session_ends_after_seven_days() {
    let app = App::new();
    let cookie = app.login().await;
    tokio::time::advance((7 * DAY).checked_sub(Duration::from_secs(1)).unwrap()).await;
    assert_eq!(app.session(Some(&cookie)).await.status, StatusCode::OK);
    tokio::time::advance(7 * DAY).await;
    let reply = app.session(Some(&cookie)).await;
    reply.problem(ProblemCode::Unauthenticated);
    assert_eq!(reply.set_cookie(), None);
}

/// Two tabs share one cookie store. Tab A sends a request with the old
/// cookie; before its answer arrives, tab B logs in and the browser stores
/// the new cookie. The late 401 of tab A must not remove the new cookie,
/// so a 401 sets no cookie at all; only a login and a logout do.
#[tokio::test(start_paused = true)]
async fn a_late_401_never_removes_a_new_cookie() {
    let app = App::new();
    let old = app.login().await;
    app.logout(Some(&old)).await;
    // Tab B logs in (it presents the old cookie, which is dead).
    let new = app.login_with(PASSWORD, Some(&old)).await.session_pair();
    // The late answers to tab A, for the session read and for a protected
    // route, carry no Set-Cookie.
    let late_session = app.session(Some(&old)).await;
    late_session.problem(ProblemCode::Unauthenticated);
    assert_eq!(late_session.set_cookie(), None);
    let late_route = app
        .call(with_cookie(get_request("/api/v1/battery"), Some(&old)))
        .await;
    late_route.problem(ProblemCode::Unauthenticated);
    assert_eq!(late_route.set_cookie(), None);
    assert_eq!(app.session(Some(&new)).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn every_session_endpoint_refuses_two_session_cookies() {
    let app = App::new();
    let live = app.login().await;
    let other = format!("__Host-telemetry_session={}", "ef".repeat(32));
    let malformed = "__Host-telemetry_session=x".to_owned();
    for second in [&other, &malformed, &live] {
        let two = format!("{live}; {second}");
        app.session(Some(&two))
            .await
            .problem(ProblemCode::InvalidRequest);
        let reply = app.logout(Some(&two)).await;
        reply.problem(ProblemCode::InvalidRequest);
        assert_eq!(reply.set_cookie(), None);
        let mut login = login_request(&json!({ "password": PASSWORD }).to_string());
        login.headers_mut().insert(COOKIE, two.parse().unwrap());
        app.call(login).await.problem(ProblemCode::InvalidRequest);
    }
    // No request above reached the verifier or ended the live session.
    assert_eq!(app.verifier.calls(), 1);
    assert_eq!(app.session(Some(&live)).await.status, StatusCode::OK);
    // In two Cookie headers too.
    let request = HttpRequest::builder()
        .uri("/api/v1/session")
        .header(COOKIE, &live)
        .header(COOKIE, &other)
        .body(Body::empty())
        .unwrap();
    app.call(request).await.problem(ProblemCode::InvalidRequest);
}

#[tokio::test(start_paused = true)]
async fn a_session_in_use_ends_thirty_days_after_the_login() {
    let app = App::new();
    let cookie = app.login().await;
    for _ in 0..5 {
        tokio::time::advance(6 * DAY).await;
        // Day 6, 12, 18, 24, 30: the last one is the absolute end.
        let reply = app.session(Some(&cookie)).await;
        if reply.status != StatusCode::OK {
            reply.problem(ProblemCode::Unauthenticated);
            return;
        }
    }
    panic!("the session outlived 30 days");
}

#[tokio::test(start_paused = true)]
async fn a_restart_ends_every_session() {
    let verifier = Arc::new(FakeVerifier::default());
    let before = App::with(
        Arc::clone(&verifier),
        SessionLimits::OWNER,
        ThrottleLimits::DEFAULT,
    );
    let cookie = before.login().await;
    // A new process has a new state.
    let after = App::with(verifier, SessionLimits::OWNER, ThrottleLimits::DEFAULT);
    after
        .session(Some(&cookie))
        .await
        .problem(ProblemCode::Unauthenticated);
}

#[tokio::test(start_paused = true)]
async fn the_ninth_login_ends_the_oldest_of_eight_sessions() {
    let app = App::with(
        Arc::new(FakeVerifier::default()),
        SessionLimits::OWNER,
        ThrottleLimits {
            capacity: 100,
            refill: Duration::from_secs(1),
        },
    );
    let mut cookies = Vec::new();
    for _ in 0..9 {
        cookies.push(app.login().await);
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    app.session(Some(&cookies[0]))
        .await
        .problem(ProblemCode::Unauthenticated);
    for cookie in &cookies[1..] {
        assert_eq!(app.session(Some(cookie)).await.status, StatusCode::OK);
    }
}

// ---- CSRF ---------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_cross_origin_login_is_refused_before_any_verification() {
    let app = App::new();
    for origin in [Some("https://evil.example"), Some("null"), None] {
        let mut request = login_request(&json!({ "password": PASSWORD }).to_string());
        request.headers_mut().remove("origin");
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        let reply = app.call(request).await;
        reply.problem(ProblemCode::ForbiddenOrigin);
        assert_eq!(reply.set_cookie(), None, "{origin:?}");
    }
    assert_eq!(app.verifier.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_same_origin_login_without_origin_passes_on_fetch_metadata() {
    let app = App::new();
    let mut request = login_request(&json!({ "password": PASSWORD }).to_string());
    request.headers_mut().remove("origin");
    request
        .headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    assert_eq!(app.call(request).await.status, StatusCode::OK);
    let mut cross = login_request(&json!({ "password": PASSWORD }).to_string());
    cross.headers_mut().remove("origin");
    cross
        .headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    app.call(cross).await.problem(ProblemCode::ForbiddenOrigin);
}

#[tokio::test(start_paused = true)]
async fn a_cross_origin_logout_keeps_the_session() {
    let app = App::new();
    let cookie = app.login().await;
    let request = HttpRequest::builder()
        .method("DELETE")
        .uri("/api/v1/session")
        .header("origin", "https://evil.example")
        .header(COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let reply = app.call(request).await;
    reply.problem(ProblemCode::ForbiddenOrigin);
    assert_eq!(reply.set_cookie(), None);
    assert_eq!(app.session(Some(&cookie)).await.status, StatusCode::OK);
}

// ---- Request shape --------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_malformed_login_gets_its_problem_and_costs_no_throttle_token() {
    let app = App::new();
    let long = "x".repeat(1025);
    let large = format!("{{\"password\":\"{}\"}}", "x".repeat(LOGIN_BODY_LIMIT));
    let cases: Vec<(Option<&str>, String, ProblemCode)> = vec![
        (
            None,
            json!({ "password": PASSWORD }).to_string(),
            ProblemCode::UnsupportedMediaType,
        ),
        (
            Some("text/plain"),
            json!({ "password": PASSWORD }).to_string(),
            ProblemCode::UnsupportedMediaType,
        ),
        (
            Some("application/x-www-form-urlencoded"),
            "password=x".to_owned(),
            ProblemCode::UnsupportedMediaType,
        ),
        (
            Some("application/json"),
            large,
            ProblemCode::PayloadTooLarge,
        ),
        (
            Some("application/json"),
            "not json".to_owned(),
            ProblemCode::InvalidRequest,
        ),
        (
            Some("application/json"),
            json!([PASSWORD]).to_string(),
            ProblemCode::InvalidRequest,
        ),
        (
            Some("application/json"),
            json!({}).to_string(),
            ProblemCode::InvalidRequest,
        ),
        (
            Some("application/json"),
            json!({ "password": "" }).to_string(),
            ProblemCode::InvalidRequest,
        ),
        (
            Some("application/json"),
            json!({ "password": long }).to_string(),
            ProblemCode::InvalidRequest,
        ),
        (
            Some("application/json"),
            json!({ "password": 12 }).to_string(),
            ProblemCode::InvalidRequest,
        ),
        (
            Some("application/json"),
            json!({ "password": PASSWORD, "remember": true }).to_string(),
            ProblemCode::InvalidRequest,
        ),
    ];
    for (content_type, body, code) in cases {
        let mut request = login_request(&body);
        request.headers_mut().remove(CONTENT_TYPE);
        if let Some(content_type) = content_type {
            request
                .headers_mut()
                .insert(CONTENT_TYPE, content_type.parse().unwrap());
        }
        app.call(request).await.problem(code);
    }
    assert_eq!(app.verifier.calls(), 0);
    // All five tokens are still there.
    for _ in 0..5 {
        app.login_with("wrong password", None)
            .await
            .problem(ProblemCode::InvalidCredentials);
    }
    app.login_with(PASSWORD, None)
        .await
        .problem(ProblemCode::LoginThrottled);
}

/// A request body without a declared length: `chunks` frames of
/// `chunk` bytes. It counts the frames that the server reads.
struct Chunked {
    chunks: usize,
    chunk: usize,
    read: Arc<AtomicUsize>,
}

impl axum::body::HttpBody for Chunked {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        if this.chunks == 0 {
            return std::task::Poll::Ready(None);
        }
        this.chunks -= 1;
        this.read.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from(vec![
            b' ';
            this.chunk
        ])))))
    }
}

fn chunked_login(content_type: &str, chunks: usize, read: &Arc<AtomicUsize>) -> HttpRequest<Body> {
    HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/session")
        .header("origin", ORIGIN)
        .header(CONTENT_TYPE, content_type)
        .body(Body::new(Chunked {
            chunks,
            chunk: 1024,
            read: Arc::clone(read),
        }))
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_chunked_login_body_above_4_kib_stops_at_the_limit() {
    let app = App::new();
    // 8 KiB without Content-Length: above the login limit, below the
    // global 16 KiB limit.
    let read = Arc::new(AtomicUsize::new(0));
    app.call(chunked_login("application/json", 8, &read))
        .await
        .problem(ProblemCode::PayloadTooLarge);
    // The server read the frames up to the limit, not the whole body.
    assert_eq!(read.load(Ordering::SeqCst), 5);
    assert_eq!(app.verifier.calls(), 0);
    // A declared length above the limit is refused before any read.
    let mut declared = login_request(&json!({ "password": PASSWORD }).to_string());
    declared
        .headers_mut()
        .insert(axum::http::header::CONTENT_LENGTH, "5000".parse().unwrap());
    app.call(declared)
        .await
        .problem(ProblemCode::PayloadTooLarge);
}

#[tokio::test(start_paused = true)]
async fn a_login_with_another_media_type_is_refused_before_the_body_is_read() {
    let app = App::new();
    let read = Arc::new(AtomicUsize::new(0));
    app.call(chunked_login("text/plain", 8, &read))
        .await
        .problem(ProblemCode::UnsupportedMediaType);
    assert_eq!(read.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn json_with_a_charset_parameter_is_accepted() {
    let app = App::new();
    let mut request = login_request(&json!({ "password": PASSWORD }).to_string());
    request.headers_mut().insert(
        CONTENT_TYPE,
        "Application/JSON; charset=utf-8".parse().unwrap(),
    );
    assert_eq!(app.call(request).await.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn the_longest_password_reaches_the_verifier() {
    let app = App::new();
    app.login_with(&"x".repeat(1024), None)
        .await
        .problem(ProblemCode::InvalidCredentials);
    assert_eq!(app.verifier.calls(), 1);
}

// ---- Throttle -------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_sixth_attempt_is_throttled_with_retry_after() {
    let app = App::new();
    for _ in 0..5 {
        app.login_with("wrong password", None)
            .await
            .problem(ProblemCode::InvalidCredentials);
    }
    // Also the correct password waits: the limit is global.
    let reply = app.login_with(PASSWORD, None).await;
    let body = reply.problem(ProblemCode::LoginThrottled);
    assert_eq!(body["retry_after_seconds"], 12);
    assert_eq!(reply.headers[RETRY_AFTER], "12");
    assert_eq!(reply.set_cookie(), None);
    assert_eq!(app.verifier.calls(), 5);
    tokio::time::advance(Duration::from_secs(12)).await;
    assert_eq!(app.login_with(PASSWORD, None).await.status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_attempts_stay_inside_the_bucket_and_the_slot() {
    let app = Arc::new(App::new());
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..24 {
        let app = Arc::clone(&app);
        tasks.spawn(async move { app.login_with("wrong password", None).await.status });
    }
    let mut throttled = 0;
    let mut refused = 0;
    while let Some(status) = tasks.join_next().await {
        match status.unwrap() {
            StatusCode::TOO_MANY_REQUESTS => throttled += 1,
            StatusCode::UNAUTHORIZED => refused += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert!(refused <= 5, "{refused} verifications passed the bucket");
    assert_eq!(throttled + refused, 24);
    assert_eq!(app.verifier.calls(), refused);
    assert_eq!(app.verifier.most_at_once.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_attempt_during_a_verification_is_busy() {
    let verifier = Arc::new(FakeVerifier::gated());
    let app = Arc::new(App::with(
        Arc::clone(&verifier),
        SessionLimits::OWNER,
        ThrottleLimits::DEFAULT,
    ));
    let first = {
        let app = Arc::clone(&app);
        tokio::spawn(async move { app.login_with(PASSWORD, None).await })
    };
    verifier.wait_until_entered().await;
    let second = app.login_with(PASSWORD, None).await;
    let body = second.problem(ProblemCode::LoginThrottled);
    assert_eq!(body["retry_after_seconds"], 1);
    assert_eq!(second.headers[RETRY_AFTER], "1");
    verifier.release();
    assert_eq!(first.await.unwrap().status, StatusCode::OK);
    assert_eq!(verifier.calls(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_login_keeps_the_slot_until_its_verification_ends() {
    let verifier = Arc::new(FakeVerifier::gated());
    let app = Arc::new(App::with(
        Arc::clone(&verifier),
        SessionLimits::OWNER,
        ThrottleLimits::DEFAULT,
    ));
    let first = {
        let app = Arc::clone(&app);
        tokio::spawn(async move { app.login_with(PASSWORD, None).await })
    };
    verifier.wait_until_entered().await;
    // The client leaves: the request future is dropped.
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    // The verification still runs, so the slot stays taken.
    app.login_with(PASSWORD, None)
        .await
        .problem(ProblemCode::LoginThrottled);
    verifier.release();
    // The slot frees when the verification ends.
    let mut status = StatusCode::TOO_MANY_REQUESTS;
    for _ in 0..200 {
        status = app.login_with(PASSWORD, None).await.status;
        if status != StatusCode::TOO_MANY_REQUESTS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verifier.most_at_once.load(Ordering::SeqCst), 1);
}

// ---- The guard in front of other API routes ------------------------------

#[tokio::test(start_paused = true)]
async fn the_guard_runs_before_a_handler_at_the_battery_mount_point() {
    // `App` is the production router assembly with a counting route where
    // the battery API mounts; this test does not install the guard itself.
    let app = App::new();
    let calls = || app.battery_calls.load(Ordering::SeqCst);

    app.call(get_request("/api/v1/battery"))
        .await
        .problem(ProblemCode::Unauthenticated);
    let unknown = format!("__Host-telemetry_session={}", "cd".repeat(32));
    let reply = app
        .call(with_cookie(get_request("/api/v1/battery"), Some(&unknown)))
        .await;
    reply.problem(ProblemCode::Unauthenticated);
    assert_eq!(reply.set_cookie(), None);
    assert_eq!(calls(), 0);

    let cookie = app.login().await;
    let reply = app
        .call(with_cookie(get_request("/api/v1/battery"), Some(&cookie)))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(calls(), 1);

    // A state-changing request needs the origin also with a session.
    let post = HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/battery")
        .header(COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    app.call(post).await.problem(ProblemCode::ForbiddenOrigin);

    // Two session cookies are refused before any lookup, also when one of
    // them is valid.
    let two = format!("{cookie}; {unknown}");
    app.call(with_cookie(get_request("/api/v1/battery"), Some(&two)))
        .await
        .problem(ProblemCode::InvalidRequest);
    assert_eq!(calls(), 1);

    // After logout the same cookie is refused again.
    app.logout(Some(&cookie)).await;
    app.call(with_cookie(get_request("/api/v1/battery"), Some(&cookie)))
        .await
        .problem(ProblemCode::Unauthenticated);
    assert_eq!(calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn every_documented_api_route_outside_the_public_list_needs_a_session() {
    let app = App::new();
    let document = crate::openapi::documented_router().into_openapi();
    let mut public = Vec::new();
    let mut protected = 0_usize;
    for (path, item) in &document.paths.paths {
        let methods = [
            ("GET", item.get.is_some()),
            ("POST", item.post.is_some()),
            ("PUT", item.put.is_some()),
            ("PATCH", item.patch.is_some()),
            ("DELETE", item.delete.is_some()),
        ];
        if PUBLIC_ROUTES.contains(&path.as_str()) {
            public.push(path.clone());
            continue;
        }
        protected += 1;
        for (method, present) in methods {
            if !present {
                continue;
            }
            let request = HttpRequest::builder()
                .method(method)
                .uri(path.as_str())
                .header("origin", ORIGIN)
                .body(Body::empty())
                .unwrap();
            app.call(request)
                .await
                .problem(ProblemCode::Unauthenticated);
        }
    }
    public.sort();
    assert_eq!(public, ["/api/v1/session", "/health"]);
    // The loop above checks each private route of the contract, also
    // `/api/v1/battery`. The requests go to the counting route at the
    // battery mount point of the production assembly (`App`). Each route
    // of the battery API is private, so the walk must check at least as
    // many protected routes as the battery router has.
    let battery_routes = crate::battery::router().into_openapi().paths.paths.len();
    assert!(
        protected >= battery_routes,
        "{protected} protected routes, {battery_routes} battery routes"
    );
}

#[tokio::test(start_paused = true)]
async fn health_and_unknown_api_paths_keep_their_responses() {
    let app = App::new();
    assert_eq!(
        app.call(get_request("/health")).await.status,
        StatusCode::OK
    );
    app.call(get_request("/api/v1/no-such-route"))
        .await
        .problem(ProblemCode::NotFound);
}

// ---- Logs and error bodies ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn no_password_cookie_or_token_reaches_a_log_or_a_body() {
    let (capture, _guard) = capture::install();
    let app = App::new();
    let wrong = "wrong-secret-password-424242";
    let failed = app.login_with(wrong, None).await;
    let reply = app.login_with(PASSWORD, None).await;
    let cookie = reply.session_pair();
    let token = cookie.split_once('=').unwrap().1.to_owned();
    app.session(Some(&cookie)).await;
    app.logout(Some(&cookie)).await;
    let mut request = login_request(&json!({ "password": wrong }).to_string());
    request
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    let refused = app.call(request).await;

    let logs = capture.text();
    for secret in [PASSWORD, wrong, token.as_str()] {
        assert!(!logs.contains(secret), "the log holds a secret");
        for body in [&failed.body, &refused.body] {
            assert!(!String::from_utf8_lossy(body).contains(secret));
        }
    }
    let events: Vec<String> = capture
        .objects()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_owned))
        .collect();
    for expected in [
        "login_failed",
        "login_succeeded",
        "logout",
        "request_refused",
    ] {
        assert!(
            events.iter().any(|event| event == expected),
            "{expected}: {events:?}"
        );
    }
    for object in capture.objects() {
        for (key, _) in object.as_object().unwrap() {
            assert!(
                crate::logging::ALLOWED_FIELDS.contains(&key.as_str())
                    || crate::logging::RESERVED_KEYS.contains(&key.as_str()),
                "unexpected log field {key}"
            );
        }
    }
}

#[test]
fn debug_of_the_state_and_the_config_shows_no_secret() {
    let hash = password::testing::hash(PASSWORD, 19_456, 2, 1);
    let config = Config {
        password_hash: StoredHash::parse(&hash).unwrap(),
        origin: PublicOrigin::parse(ORIGIN).unwrap(),
    };
    let salt = hash.split('$').nth(4).unwrap();
    let output = hash.rsplit('$').next().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let state = runtime.block_on(async { State::new(&config) });
    for text in [format!("{config:?}"), format!("{state:?}")] {
        assert!(!text.contains(salt), "{text}");
        assert!(!text.contains(output), "{text}");
    }
    let password = Password(PASSWORD.to_owned());
    assert!(!format!("{password:?}").contains(PASSWORD));
}

// ---- Configuration -----------------------------------------------------------

#[test]
fn the_configuration_reads_and_checks_both_variables() {
    use std::collections::HashMap;
    use std::ffi::OsString;

    let hash = password::testing::hash(PASSWORD, 19_456, 2, 1);
    let read = |pairs: &[(&str, &str)]| {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(*value)))
            .collect();
        let lookup = move |name: &str| map.get(name).cloned();
        crate::config::with_vars(&lookup, Config::from_vars)
    };
    assert!(read(&[("WEB_PASSWORD_HASH", &hash), ("WEB_PUBLIC_ORIGIN", ORIGIN)]).is_ok());
    assert_eq!(
        read(&[("WEB_PUBLIC_ORIGIN", ORIGIN)]).unwrap_err(),
        ConfigError::Missing {
            name: "WEB_PASSWORD_HASH"
        }
    );
    assert_eq!(
        read(&[("WEB_PASSWORD_HASH", &hash)]).unwrap_err(),
        ConfigError::Missing {
            name: "WEB_PUBLIC_ORIGIN"
        }
    );
    assert_eq!(
        read(&[
            ("WEB_PASSWORD_HASH", "plain password"),
            ("WEB_PUBLIC_ORIGIN", ORIGIN)
        ])
        .unwrap_err()
        .name(),
        "WEB_PASSWORD_HASH"
    );
    assert_eq!(
        read(&[
            ("WEB_PASSWORD_HASH", &hash),
            ("WEB_PUBLIC_ORIGIN", "https://x.example/")
        ])
        .unwrap_err()
        .name(),
        "WEB_PUBLIC_ORIGIN"
    );
}

// ---- The contract -------------------------------------------------------------

fn committed_contract() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/openapi.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Checks a body against a component schema: required keys, no other
/// keys, and each string against the pattern of its `$ref` schema.
fn assert_session_schema(contract: &Value, body: &Value) {
    let schemas = &contract["components"]["schemas"];
    let schema = &schemas["Session"];
    let object = body.as_object().unwrap();
    let mut required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    required.sort_unstable();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, required);
    for (key, value) in object {
        let property = &schema["properties"][key];
        let target = property["$ref"].as_str().map_or(property, |reference| {
            &schemas[reference.trim_start_matches("#/components/schemas/")]
        });
        assert_eq!(target["type"], "string", "{key}");
        assert_eq!(target["format"], "date-time", "{key}");
        let text = value.as_str().unwrap();
        // The pattern of `wire::Instant`: digits, `T`, and `Z`.
        assert!(text.ends_with('Z') && text.len() >= 20, "{key}: {text}");
    }
}

#[tokio::test(start_paused = true)]
async fn session_responses_match_the_contract() {
    let contract = committed_contract();
    let operations = &contract["paths"]["/api/v1/session"];
    let app = App::new();

    let login = app.login_with(PASSWORD, None).await;
    assert!(operations["post"]["responses"]["200"].is_object());
    assert_session_schema(&contract, &login.json());
    let cookie = login.session_pair();
    let read = app.session(Some(&cookie)).await;
    assert!(operations["get"]["responses"]["200"].is_object());
    assert_session_schema(&contract, &read.json());

    let mut problems = vec![
        ("get", app.session(None).await),
        ("post", app.login_with("wrong password", None).await),
        ("post", {
            let mut request = login_request("{}");
            request.headers_mut().remove("origin");
            app.call(request).await
        }),
        ("post", app.call(login_request("[]")).await),
        ("post", {
            let mut request = login_request("{}");
            request.headers_mut().remove(CONTENT_TYPE);
            app.call(request).await
        }),
    ];
    // The guard answers of each session operation: two session cookies
    // (400) and, for DELETE, another origin (403).
    let two = format!("{cookie}; __Host-telemetry_session={}", "ef".repeat(32));
    let two_cookie_get = app.session(Some(&two)).await;
    let two_cookie_delete = app.logout(Some(&two)).await;
    let two_cookie_post = {
        let mut request = login_request(&json!({ "password": PASSWORD }).to_string());
        request.headers_mut().insert(COOKIE, two.parse().unwrap());
        app.call(request).await
    };
    for reply in [&two_cookie_get, &two_cookie_delete, &two_cookie_post] {
        reply.problem(ProblemCode::InvalidRequest);
    }
    problems.push(("get", two_cookie_get));
    problems.push(("delete", two_cookie_delete));
    problems.push(("post", two_cookie_post));
    problems.push(("delete", {
        let request = HttpRequest::builder()
            .method("DELETE")
            .uri("/api/v1/session")
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap();
        app.call(request).await
    }));
    for _ in 0..5 {
        problems.push(("post", app.login_with("wrong password", None).await));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (method, reply) in &problems {
        let status = reply.status.as_u16().to_string();
        let declared = &operations[*method]["responses"][&status];
        assert_eq!(
            declared["content"][PROBLEM_CONTENT_TYPE]["schema"]["$ref"],
            "#/components/schemas/Problem",
            "{method} {status}"
        );
        assert_eq!(reply.headers[CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
        seen.insert(format!("{method} {status}"));
    }
    for expected in [
        "get 400",
        "get 401",
        "post 400",
        "post 401",
        "post 403",
        "post 415",
        "post 429",
        "delete 400",
        "delete 403",
    ] {
        assert!(seen.contains(expected), "{expected}: {seen:?}");
    }

    let logout = app.logout(Some(&cookie)).await;
    assert_eq!(logout.status, StatusCode::NO_CONTENT);
    assert!(operations["delete"]["responses"]["204"].is_object());
    assert!(logout.body.is_empty());
}
