//! The responses of the session guard in the `OpenAPI` document.
//!
//! The guard (`crate::auth::protect`) answers before a handler runs, so
//! the `#[utoipa::path]` of a handler does not show these answers.
//! [`declare_guard_responses`] adds them to every operation under `/api/`
//! of the document, from the same rules as the guard:
//!
//! - 400 `invalid_request`: the request carries more than one session
//!   cookie. Every API operation.
//! - 401 `unauthenticated`: no valid session. Every API operation outside
//!   [`crate::auth::PUBLIC_ROUTES`].
//! - 403 `forbidden_origin`: the origin rule. Every API operation with a
//!   method other than `GET` and `HEAD`.
//!
//! `crate::openapi` calls it once for the whole document, so a route of a
//! later slice gets these responses without a declaration of its own. An
//! operation that declares one of these statuses itself keeps its own
//! text.

use utoipa::openapi::content::ContentBuilder;
use utoipa::openapi::path::Operation;
use utoipa::openapi::response::ResponseBuilder;
use utoipa::openapi::{OpenApi, Ref, RefOr};

use super::PUBLIC_ROUTES;
use crate::problem::PROBLEM_CONTENT_TYPE;

/// The text of the 400 that the guard adds.
pub const TWO_COOKIES: &str =
    "The request is not valid, or it carries more than one session cookie.";
/// The text of the 401 that the guard adds.
pub const NO_SESSION: &str = "The request has no valid session.";
/// The text of the 403 that the guard adds.
pub const WRONG_ORIGIN: &str = "The request did not come from the application origin.";

/// The statuses that the guard can give for an operation.
#[must_use]
pub fn guard_statuses(path: &str, method: &str) -> Vec<&'static str> {
    if !path.starts_with("/api/") {
        return Vec::new();
    }
    let mut statuses = vec!["400"];
    if !PUBLIC_ROUTES.contains(&path) {
        statuses.push("401");
    }
    if method != "get" && method != "head" {
        statuses.push("403");
    }
    statuses
}

/// Adds the guard responses to every API operation of `document`.
pub fn declare_guard_responses(document: &mut OpenApi) {
    for (path, item) in &mut document.paths.paths {
        let operations: [(&str, &mut Option<Operation>); 8] = [
            ("get", &mut item.get),
            ("put", &mut item.put),
            ("post", &mut item.post),
            ("delete", &mut item.delete),
            ("options", &mut item.options),
            ("head", &mut item.head),
            ("patch", &mut item.patch),
            ("trace", &mut item.trace),
        ];
        for (method, operation) in operations {
            let Some(operation) = operation else {
                continue;
            };
            for status in guard_statuses(path, method) {
                let text = match status {
                    "400" => TWO_COOKIES,
                    "401" => NO_SESSION,
                    _ => WRONG_ORIGIN,
                };
                operation
                    .responses
                    .responses
                    .entry(status.to_owned())
                    .or_insert_with(|| problem_response(text));
            }
        }
    }
}

fn problem_response(description: &str) -> RefOr<utoipa::openapi::Response> {
    let content = ContentBuilder::new()
        .schema(Some(Ref::from_schema_name("Problem")))
        .build();
    RefOr::T(
        ResponseBuilder::new()
            .description(description)
            .content(PROBLEM_CONTENT_TYPE, content)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statuses_follow_the_rules_of_the_guard() {
        assert_eq!(guard_statuses("/health", "get"), Vec::<&str>::new());
        assert_eq!(guard_statuses("/api/v1/session", "get"), ["400"]);
        assert_eq!(guard_statuses("/api/v1/session", "post"), ["400", "403"]);
        assert_eq!(guard_statuses("/api/v1/session", "delete"), ["400", "403"]);
        assert_eq!(guard_statuses("/api/v1/battery", "get"), ["400", "401"]);
        assert_eq!(
            guard_statuses("/api/v1/battery", "post"),
            ["400", "401", "403"]
        );
    }

    /// Every API operation of the document, also a route that a later
    /// slice adds, declares each status that the guard can give, as a
    /// problem.
    #[test]
    fn every_api_operation_of_the_document_declares_the_guard_statuses() {
        let document = crate::openapi::documented_router().into_openapi();
        let json = serde_json::to_value(&document).unwrap();
        let mut checked = 0;
        for (path, item) in json["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                for status in guard_statuses(path, method) {
                    let response = &operation["responses"][status];
                    assert_eq!(
                        response["content"][PROBLEM_CONTENT_TYPE]["schema"]["$ref"],
                        "#/components/schemas/Problem",
                        "{method} {path} {status}"
                    );
                    checked += 1;
                }
            }
        }
        // GET, POST, and DELETE /api/v1/session today: 400 each, 403 for
        // POST and DELETE.
        assert!(checked >= 5, "{checked}");
    }

    #[test]
    fn a_route_without_declarations_gets_them() {
        let slice = utoipa_axum::router::OpenApiRouter::<crate::app::AppState>::new()
            .routes(utoipa_axum::routes!(undocumented_probe));
        let document = crate::openapi::documented_router_with(slice).into_openapi();
        let json = serde_json::to_value(&document).unwrap();
        let responses = &json["paths"]["/api/v1/probe"]["get"]["responses"];
        for status in ["400", "401"] {
            assert_eq!(
                responses[status]["content"][PROBLEM_CONTENT_TYPE]["schema"]["$ref"],
                "#/components/schemas/Problem",
                "{status}"
            );
        }
        assert!(responses.get("403").is_none());
    }

    /// A probe route that declares only its success.
    #[utoipa::path(get, path = "/probe", responses((status = 200, description = "ok")))]
    async fn undocumented_probe() {}
}
