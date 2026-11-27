//! HTTP tests of the router: real requests through the real middleware,
//! with `oneshot` and no socket.
//!
//! Expected values come from the shared S1 interface (routes, headers,
//! limits, problem shape) and from the committed `contract/openapi.json`.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Json;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::rejection::JsonRejection;
use axum::http::header::{
    ACCEPT_ENCODING, ALLOW, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE,
    RETRY_AFTER, VARY,
};
use axum::http::{HeaderMap, Request as HttpRequest};
use axum::routing::{get, post};
use serde_json::{Map, Value};
use tower::ServiceExt;

use super::*;
use crate::logging::capture;
use crate::test_support::TempDir;

const VERSION: &str = "0123abc";
const INDEX: &str = "<!doctype html><meta name=\"telemetry-version\" content=\"__TELEMETRY_VERSION__\"><div id=\"root\"></div>";
const APP_JS: &[u8] = b"console.log(1);";
const APP_JS_GZ: &[u8] = b"\x1f\x8b fake gzip bytes";

struct Response3 {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Response3 {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap()
    }

    /// Asserts a problem response of `code` and returns its body.
    fn assert_problem(&self, code: ProblemCode) -> Value {
        assert_eq!(self.status, code.status(), "{}", self.text());
        assert_eq!(self.headers[CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
        assert_eq!(self.headers[CACHE_CONTROL], "no-store");
        let body = self.json();
        assert_eq!(body["code"], code.as_str());
        assert_eq!(body["status"], code.status().as_u16());
        body
    }
}

fn spa_files() -> TempDir {
    let dir = TempDir::new();
    dir.write("index.html", INDEX.as_bytes());
    dir.write("assets/app.js", APP_JS);
    dir.write("assets/app.js.gz", APP_JS_GZ);
    dir.write("assets/sub/inner.css", b"a{}");
    dir
}

/// Test-only routes of these tests: a slow route, a panic, and a JSON
/// body extractor with the problem mapping.
fn extra_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/test/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                "late"
            }),
        )
        .route(
            "/api/v1/test/panic",
            get(|| async {
                panic!("secret value 4242");
                #[expect(unreachable_code, reason = "the handler needs a return type")]
                ""
            }),
        )
        .route(
            "/api/v1/test/json",
            post(
                |body: Result<Json<Map<String, Value>>, JsonRejection>| async move {
                    match body {
                        Ok(Json(object)) => Json(object.len()).into_response(),
                        Err(rejection) => Problem::from(rejection).into_response(),
                    }
                },
            ),
        )
}

struct TestApp {
    _dir: TempDir,
    router: Router,
}

impl TestApp {
    fn new() -> Self {
        let dir = spa_files();
        let static_files = StaticFiles::load(dir.path()).unwrap();
        let state = AppState::new(Version::from_commit_sha(VERSION).unwrap());
        let router = build(state, &static_files, extra_routes());
        Self { _dir: dir, router }
    }

    async fn call(&self, request: HttpRequest<Body>) -> Response3 {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        Response3 {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, uri: &str) -> Response3 {
        self.call(request("GET", uri)).await
    }
}

fn request(method: &str, uri: &str) -> HttpRequest<Body> {
    HttpRequest::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn assert_security_headers(response: &Response3, case: &str) {
    for (name, value) in SECURITY_HEADERS {
        assert_eq!(
            response.headers.get(name).and_then(|v| v.to_str().ok()),
            Some(value),
            "{case}: {name}"
        );
    }
    assert!(response.headers.contains_key(CACHE_CONTROL), "{case}");
}

#[tokio::test]
async fn health_returns_the_status_and_the_version() {
    let app = TestApp::new();
    let response = app.get("/health").await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.headers[CONTENT_TYPE], "application/json");
    assert_eq!(response.headers[CACHE_CONTROL], "no-store");
    assert_eq!(
        response.json(),
        serde_json::json!({"status": "ok", "version": VERSION})
    );
    let head = app.call(request("HEAD", "/health")).await;
    assert_eq!(head.status, StatusCode::OK);
}

#[tokio::test]
async fn health_gives_unknown_without_a_commit_sha() {
    let dir = spa_files();
    let static_files = StaticFiles::load(dir.path()).unwrap();
    let router = router(AppState::new(Version::unknown()), &static_files);
    let response = router.oneshot(request("GET", "/health")).await.unwrap();
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["version"], "unknown");
}

#[tokio::test]
async fn an_unknown_api_path_is_a_404_problem_for_every_method() {
    let app = TestApp::new();
    for (method, uri) in [
        ("GET", "/api"),
        ("GET", "/api/"),
        ("GET", "/api/v1"),
        ("GET", "/api/v1/unknown"),
        ("GET", "/api/v2/battery?range=7d"),
        ("POST", "/api/v1/session"),
        ("DELETE", "/api/anything/deeper"),
        ("HEAD", "/api/v1/unknown"),
    ] {
        let response = app.call(request(method, uri)).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(response.headers[CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
        if method != "HEAD" {
            response.assert_problem(ProblemCode::NotFound);
        }
    }
}

#[tokio::test]
async fn a_wrong_method_on_a_known_route_is_a_405_problem() {
    let app = TestApp::new();
    for method in ["POST", "PUT", "DELETE", "PATCH", "OPTIONS"] {
        let response = app.call(request(method, "/health")).await;
        response.assert_problem(ProblemCode::MethodNotAllowed);
        let allow = response.headers[ALLOW].to_str().unwrap();
        assert!(allow.contains("GET"), "{allow}");
    }
}

#[tokio::test]
async fn another_method_outside_the_api_is_a_405_problem() {
    let app = TestApp::new();
    for (method, uri) in [
        ("POST", "/"),
        ("PUT", "/battery/7d"),
        ("DELETE", "/assets/app.js"),
        ("POST", "/robots.txt"),
    ] {
        let response = app.call(request(method, uri)).await;
        response.assert_problem(ProblemCode::MethodNotAllowed);
        assert!(response.headers.contains_key(ALLOW), "{method} {uri}");
    }
}

#[tokio::test]
async fn a_declared_body_above_16_kib_is_a_413_problem() {
    let app = TestApp::new();
    let at_limit = HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/test/json")
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_LENGTH, BODY_LIMIT_BYTES)
        .body(Body::from(
            format!("{{\"a\":\"{}\"}}", "x".repeat(BODY_LIMIT_BYTES - 8)).into_bytes(),
        ))
        .unwrap();
    let response = app.call(at_limit).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text());

    let above = HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/session")
        .header(CONTENT_LENGTH, BODY_LIMIT_BYTES + 1)
        .body(Body::from(vec![b'x'; BODY_LIMIT_BYTES + 1]))
        .unwrap();
    let response = app.call(above).await;
    response.assert_problem(ProblemCode::PayloadTooLarge);
    assert_security_headers(&response, "413");
}

/// A body without a `Content-Length` header, as with chunked transfer:
/// the declared-length check cannot see it, so the extractor limit must
/// reject it.
#[tokio::test]
async fn a_body_without_length_above_16_kib_is_a_413_problem() {
    let app = TestApp::new();
    let body = format!("{{\"a\":\"{}\"}}", "x".repeat(BODY_LIMIT_BYTES));
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/test/json")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    assert!(!request.headers().contains_key(CONTENT_LENGTH));
    app.call(request)
        .await
        .assert_problem(ProblemCode::PayloadTooLarge);
}

#[tokio::test]
async fn json_rejections_are_problems() {
    let app = TestApp::new();
    let cases = [
        (None, "{}", ProblemCode::UnsupportedMediaType),
        (Some("text/plain"), "{}", ProblemCode::UnsupportedMediaType),
        (Some("application/json"), "{", ProblemCode::InvalidRequest),
        (Some("application/json"), "[1]", ProblemCode::InvalidRequest),
    ];
    for (content_type, body, code) in cases {
        let mut builder = HttpRequest::builder()
            .method("POST")
            .uri("/api/v1/test/json");
        if let Some(content_type) = content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        let response = app.call(builder.body(Body::from(body)).unwrap()).await;
        let problem = response.assert_problem(code);
        assert!(!problem.to_string().contains("line"), "{problem}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_request_above_30_s_is_a_503_timeout_problem() {
    let app = TestApp::new();
    let started = tokio::time::Instant::now();
    let response = app.get("/api/v1/test/slow").await;
    assert_eq!(started.elapsed(), REQUEST_TIMEOUT);
    let body = response.assert_problem(ProblemCode::Timeout);
    assert_eq!(body["retry_after_seconds"], 5);
    assert_eq!(response.headers[RETRY_AFTER], "5");
    assert_security_headers(&response, "timeout");
}

#[tokio::test]
async fn a_panic_is_a_500_internal_error_problem() {
    let app = TestApp::new();
    let response = app.get("/api/v1/test/panic").await;
    let body = response.assert_problem(ProblemCode::InternalError);
    assert!(!response.text().contains("4242"), "{body}");
    assert!(!response.headers.contains_key(RETRY_AFTER));
    assert_security_headers(&response, "panic");
}

#[tokio::test]
async fn a_missing_asset_is_a_404_problem_not_the_index() {
    let app = TestApp::new();
    for uri in [
        "/assets/missing.js",
        "/assets/",
        "/assets",
        "/assets/sub",
        "/assets/sub/",
        "/assets/%2e%2e/index.html",
        "/assets/../index.html",
    ] {
        let response = app.get(uri).await;
        response.assert_problem(ProblemCode::NotFound);
        assert!(!response.text().contains("telemetry-version"), "{uri}");
        assert_ne!(
            response.headers[CACHE_CONTROL], ASSET_CACHE_CONTROL,
            "{uri}"
        );
        assert_security_headers(&response, uri);
    }
}

#[tokio::test]
async fn an_asset_is_served_with_gzip_and_a_long_cache_time() {
    let app = TestApp::new();
    let plain = app.get("/assets/app.js").await;
    assert_eq!(plain.status, StatusCode::OK);
    assert_eq!(plain.body.as_ref(), APP_JS);
    assert_eq!(plain.headers[CACHE_CONTROL], ASSET_CACHE_CONTROL);
    assert!(
        plain.headers[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    assert!(!plain.headers.contains_key(CONTENT_ENCODING));
    assert_security_headers(&plain, "asset");

    let gzip = app
        .call(
            HttpRequest::builder()
                .uri("/assets/app.js")
                .header(ACCEPT_ENCODING, "gzip, br")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(gzip.status, StatusCode::OK);
    assert_eq!(gzip.body.as_ref(), APP_JS_GZ);
    assert_eq!(gzip.headers[CONTENT_ENCODING], "gzip");
    // A shared cache must keep the two encodings apart.
    assert_eq!(gzip.headers[VARY], "accept-encoding");
    assert_eq!(plain.headers[VARY], "accept-encoding");
    assert_eq!(gzip.headers[CACHE_CONTROL], ASSET_CACHE_CONTROL);

    let nested = app.get("/assets/sub/inner.css").await;
    assert_eq!(nested.status, StatusCode::OK);
    assert!(
        nested.headers[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );
}

#[tokio::test]
async fn other_get_paths_give_the_index_with_the_version() {
    let app = TestApp::new();
    for uri in [
        "/",
        "/battery",
        "/battery/7d?x=1",
        "/login",
        "/index.html",
        "/apix",
    ] {
        for method in ["GET", "HEAD"] {
            let response = app.call(request(method, uri)).await;
            assert_eq!(response.status, StatusCode::OK, "{method} {uri}");
            assert_eq!(response.headers[CONTENT_TYPE], "text/html; charset=utf-8");
            assert_eq!(response.headers[CACHE_CONTROL], "no-store");
            assert_security_headers(&response, uri);
            if method == "GET" {
                let text = response.text();
                assert!(
                    text.contains("<meta name=\"telemetry-version\" content=\"0123abc\">"),
                    "{text}"
                );
                assert!(!text.contains(crate::config::VERSION_PLACEHOLDER));
            }
        }
    }
}

#[tokio::test]
async fn robots_txt_disallows_everything() {
    let app = TestApp::new();
    let response = app.get("/robots.txt").await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.text(), "User-agent: *\nDisallow: /\n");
    assert_eq!(response.headers[CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_security_headers(&response, "robots");
}

#[tokio::test]
async fn every_response_kind_has_the_security_headers() {
    let app = TestApp::new();
    let cases = [
        ("GET", "/health"),
        ("GET", "/api/v1/unknown"),
        ("POST", "/health"),
        ("POST", "/page"),
        ("GET", "/assets/app.js"),
        ("GET", "/assets/missing.js"),
        ("GET", "/"),
        ("HEAD", "/"),
        ("GET", "/robots.txt"),
        ("GET", "/api/v1/test/panic"),
    ];
    for (method, uri) in cases {
        let response = app.call(request(method, uri)).await;
        assert_security_headers(&response, &format!("{method} {uri}"));
    }
}

#[test]
fn the_content_security_policy_is_the_contract_value() {
    assert_eq!(
        CONTENT_SECURITY_POLICY,
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
         font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none'; \
         form-action 'self'; frame-ancestors 'none'"
    );
}

// --- Request log ---------------------------------------------------------

#[tokio::test]
async fn the_request_event_holds_the_route_template_only() {
    let app = TestApp::new();
    let (capture, guard) = capture::install();
    let request = HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/secret-path?token=secret-query")
        .header(COOKIE, "session=secret-cookie")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from("{\"password\":\"secret-body\"}"))
        .unwrap();
    let response = app.call(request).await;
    drop(guard);
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    let text = capture.text();
    for secret in [
        "secret-path",
        "secret-query",
        "secret-cookie",
        "secret-body",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
    let events: Vec<Value> = capture
        .objects()
        .into_iter()
        .filter(|event| event["message"] == "request")
        .collect();
    assert_eq!(events.len(), 1, "{text}");
    let event = &events[0];
    assert_eq!(event["method"], "POST");
    assert_eq!(event["route"], "/api/{*rest}");
    assert_eq!(event["status"], 404);
    assert!(event["latency_ms"].is_u64());
    let id = event["request_id"].as_str().unwrap();
    assert_eq!(id.len(), 16, "{id}");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    let keys: BTreeSet<&str> = event
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "latency_ms",
            "level",
            "message",
            "method",
            "request_id",
            "route",
            "status",
            "target",
            "timestamp"
        ])
    );
}

async fn logged_request(app: &TestApp, method: &str, uri: &str) -> Value {
    let (capture, guard) = capture::install();
    app.call(request(method, uri)).await;
    drop(guard);
    capture
        .objects()
        .into_iter()
        .find(|event| event["message"] == "request")
        .unwrap()
}

#[tokio::test]
async fn the_route_is_a_template_or_a_fixed_word() {
    let app = TestApp::new();
    let cases = [
        ("GET", "/health", "/health"),
        ("GET", "/battery/secret-deep-link", FALLBACK_ROUTE),
        ("GET", "/assets/secret-file.js", ASSETS_ROUTE),
        ("GET", "/assets", "/assets"),
        ("GET", "/api/v1/secret", "/api/{*rest}"),
        ("FOOBAR", "/health", "/health"),
    ];
    for (method, uri, route) in cases {
        let event = logged_request(&app, method, uri).await;
        assert_eq!(event["route"], route, "{method} {uri}");
        assert!(!event.to_string().contains("secret"), "{event}");
    }
    let event = logged_request(&app, "FOOBAR", "/health").await;
    assert_eq!(event["method"], "other");
    assert_eq!(event["status"], 405);
}

#[tokio::test]
async fn a_panicked_request_is_logged_without_a_request_id() {
    let app = TestApp::new();
    let (capture, guard) = capture::install();
    app.get("/api/v1/test/panic").await;
    drop(guard);
    let text = capture.text();
    assert!(!text.contains("4242"), "{text}");
    let objects = capture.objects();
    let event = objects.iter().find(|e| e["message"] == "request").unwrap();
    assert_eq!(event["status"], 500);
    assert_eq!(event["request_id"], NO_REQUEST_ID);
    assert!(objects.iter().any(|e| e["message"] == "request_panicked"));
}

#[test]
fn the_panic_hook_logs_the_position_only() {
    let (capture, guard) = capture::install();
    crate::logging::install_panic_hook();
    let result = std::panic::catch_unwind(|| panic!("secret panic value 777"));
    drop(std::panic::take_hook());
    drop(guard);
    assert!(result.is_err());
    let text = capture.text();
    assert!(!text.contains("777"), "{text}");
    let objects = capture.objects();
    assert_eq!(objects.len(), 1, "{text}");
    assert_eq!(objects[0]["message"], "panic");
    assert_eq!(objects[0]["level"], "ERROR");
    assert!(objects[0]["file"].as_str().unwrap().ends_with("tests.rs"));
    assert!(objects[0]["line"].is_u64());
}

#[test]
fn request_ids_are_hex_and_differ() {
    let first = RequestId::generate().unwrap();
    let second = RequestId::generate().unwrap();
    assert_ne!(first.as_str(), second.as_str());
    assert!(first.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn methods_map_to_a_closed_set() {
    assert_eq!(method_name(&Method::GET), "GET");
    assert_eq!(
        method_name(&Method::from_bytes(b"PROPFIND").unwrap()),
        "other"
    );
    assert_eq!(
        method_name(&Method::from_bytes(b"secret").unwrap()),
        "other"
    );
}

// --- Contract --------------------------------------------------------------

fn committed_contract() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/openapi.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_committed_contract_equals_a_fresh_export() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract/openapi.json");
    let committed = std::fs::read_to_string(path).unwrap();
    let fresh = crate::openapi::to_json().unwrap();
    assert!(
        committed == fresh,
        "contract/openapi.json is stale; run the export (mise run contract)"
    );
    assert!(fresh.ends_with("}\n"));
    assert_eq!(
        fresh,
        crate::openapi::to_json().unwrap(),
        "not deterministic"
    );
}

#[test]
fn the_contract_is_openapi_3_1_with_a_fixed_version() {
    let contract = committed_contract();
    assert_eq!(contract["openapi"], "3.1.0");
    assert_eq!(contract["info"]["version"], crate::openapi::API_VERSION);
    assert!(contract["info"].get("license").is_none());
}

#[test]
fn the_contract_problem_codes_equal_the_rust_set() {
    let contract = committed_contract();
    let listed: Vec<&str> = contract["components"]["schemas"]["ProblemCode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let rust: Vec<&str> = ProblemCode::ALL.iter().map(|code| code.as_str()).collect();
    assert_eq!(listed, rust);
}

#[test]
fn every_documented_error_response_is_a_problem() {
    let contract = committed_contract();
    for (path, item) in contract["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            for (status, response) in operation["responses"].as_object().unwrap() {
                let content = response["content"].as_object().unwrap();
                if status.starts_with('4') || status.starts_with('5') {
                    let keys: Vec<&String> = content.keys().collect();
                    assert_eq!(
                        keys,
                        ["application/problem+json"],
                        "{method} {path} {status}"
                    );
                    assert_eq!(
                        content["application/problem+json"]["schema"]["$ref"],
                        "#/components/schemas/Problem"
                    );
                }
            }
        }
    }
}

/// Checks that `body` has every required property of the component
/// `schema`, no other property than the listed ones, and the declared JSON
/// type of each value.
fn assert_matches_schema(contract: &Value, schema: &str, body: &Value) {
    let schemas = &contract["components"]["schemas"];
    let schema_value = &schemas[schema];
    let object = body.as_object().unwrap();
    let properties = schema_value["properties"].as_object().unwrap();
    for required in schema_value["required"].as_array().unwrap() {
        let required = required.as_str().unwrap();
        assert!(
            object.contains_key(required),
            "{schema}: missing {required} in {body}"
        );
    }
    for (key, value) in object {
        let property = properties
            .get(key)
            .unwrap_or_else(|| panic!("{schema}: undeclared {key} in {body}"));
        if let Some(reference) = property["$ref"].as_str() {
            let name = reference.trim_start_matches("#/components/schemas/");
            let target = &schemas[name];
            assert_eq!(target["type"], "string", "{name}");
            let allowed = target["enum"].as_array().unwrap();
            assert!(
                allowed.contains(value),
                "{schema}.{key}: {value} not in {name}"
            );
            continue;
        }
        match property["type"].as_str().unwrap() {
            "string" => assert!(value.is_string(), "{schema}.{key}: {value}"),
            "integer" => {
                let number = value
                    .as_i64()
                    .unwrap_or_else(|| panic!("{schema}.{key}: {value}"));
                if let Some(minimum) = property["minimum"].as_i64() {
                    assert!(number >= minimum, "{schema}.{key}: {number}");
                }
                if let Some(maximum) = property["maximum"].as_i64() {
                    assert!(number <= maximum, "{schema}.{key}: {number}");
                }
            }
            other => panic!("{schema}.{key}: unexpected schema type {other}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn real_responses_match_the_contract_schemas() {
    let contract = committed_contract();
    let app = TestApp::new();

    let health = app.get("/health").await;
    assert_matches_schema(&contract, "Health", &health.json());

    let mut problems = vec![
        app.get("/api/v1/unknown").await,
        app.call(request("POST", "/health")).await,
        app.get("/assets/missing.js").await,
        app.get("/api/v1/test/panic").await,
        app.get("/api/v1/test/slow").await,
        app.call(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/test/json")
                .header(CONTENT_LENGTH, BODY_LIMIT_BYTES + 1)
                .body(Body::empty())
                .unwrap(),
        )
        .await,
        app.call(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/test/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await,
        app.call(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/test/json")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await,
    ];
    let mut seen = BTreeSet::new();
    for response in problems.drain(..) {
        assert_eq!(response.headers[CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
        let body = response.json();
        assert_matches_schema(&contract, "Problem", &body);
        assert_eq!(body["status"], response.status.as_u16());
        let retryable = matches!(response.status.as_u16(), 429 | 503);
        assert_eq!(
            body.get("retry_after_seconds").is_some(),
            retryable,
            "{body}"
        );
        seen.insert(body["code"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        seen,
        BTreeSet::from(
            [
                "internal_error",
                "invalid_request",
                "method_not_allowed",
                "not_found",
                "payload_too_large",
                "timeout",
                "unsupported_media_type"
            ]
            .map(str::to_owned)
        )
    );

    // The codes that no S1 route can produce yet: their bodies from the
    // one mapping must also match the schema.
    for code in ProblemCode::ALL {
        let body = serde_json::to_value(Problem::new(code)).unwrap();
        assert_matches_schema(&contract, "Problem", &body);
    }
}

// --- Test-only routes ------------------------------------------------------

#[cfg(feature = "test-server")]
mod test_server {
    use axum::http::header::SET_COOKIE;

    use super::*;

    fn app() -> (TempDir, Router) {
        let dir = spa_files();
        let static_files = StaticFiles::load(dir.path()).unwrap();
        let router = test_server_router(
            AppState::new(Version::from_commit_sha(VERSION).unwrap()),
            &static_files,
        );
        (dir, router)
    }

    async fn check(router: &Router, cookie: Option<&str>) -> Value {
        let mut builder = HttpRequest::builder().uri("/test-only/e4/cookie/check");
        if let Some(cookie) = cookie {
            builder = builder.header(COOKIE, cookie);
        }
        let response = router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn the_set_route_sets_the_host_cookie() {
        let (_dir, router) = app();
        let response = router
            .oneshot(request("GET", "/test-only/e4/cookie/set"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers()[SET_COOKIE],
            "__Host-e4=1; Path=/; Secure; HttpOnly; SameSite=Strict"
        );
        assert_eq!(
            response.headers()["content-security-policy"],
            CONTENT_SECURITY_POLICY
        );
    }

    #[tokio::test]
    async fn the_check_route_reports_the_cookie() {
        let (_dir, router) = app();
        assert_eq!(
            check(&router, None).await,
            serde_json::json!({"present": false})
        );
        assert_eq!(
            check(&router, Some("__Host-e4=1")).await,
            serde_json::json!({"present": true})
        );
        assert_eq!(
            check(&router, Some("a=b; __Host-e4=1")).await,
            serde_json::json!({"present": true})
        );
        for other in ["__Host-e4=10", "e4=1", "__Host-e4=0", "x__Host-e4=1"] {
            assert_eq!(
                check(&router, Some(other)).await,
                serde_json::json!({"present": false}),
                "{other}"
            );
        }
    }

    #[tokio::test]
    async fn the_production_router_has_no_test_routes() {
        let dir = spa_files();
        let static_files = StaticFiles::load(dir.path()).unwrap();
        let production = router(
            AppState::new(Version::from_commit_sha(VERSION).unwrap()),
            &static_files,
        );
        let response = production
            .oneshot(request("GET", "/test-only/e4/cookie/set"))
            .await
            .unwrap();
        assert!(!response.headers().contains_key(SET_COOKIE));
        assert_eq!(response.headers()[CONTENT_TYPE], "text/html; charset=utf-8");
    }
}
