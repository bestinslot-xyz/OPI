//! The document against the modules it covers, the OpenAPI 3.1 schema, the
//! parameter rules and the README. Every module route has an operation by
//! construction: [`ModuleRoutes`](crate::routes::ModuleRoutes) takes routes
//! only from `routes!`.

use {
    super::{ApiDoc, operations_mut},
    crate::{
        config::{MODULES, Module},
        routes::{
            self,
            paging::{
                DEFAULT_LIMIT, MAX_ADDRESS_LEN, MAX_BEFORE_ID, MAX_INSCRIPTION_ID_LEN, MAX_LIMIT,
                MAX_NAME_LEN, MAX_OFFSET, MAX_PKSCRIPT_LEN, MAX_PREFIX_DIGITS, MAX_TICKER_LEN,
                MAX_TYPE_LEN,
            },
        },
    },
    axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    },
    serde_json::{Value, json},
    std::collections::BTreeSet,
    tower::ServiceExt,
    utoipa::{OpenApi, openapi::OpenApi as Document},
};

/// The OpenAPI 3.1 schema (https://spec.openapis.org/oas/3.1/schema/2022-10-07).
const OAS_3_1_SCHEMA: &str = include_str!("oas-3.1-schema.json");

fn app(modules: &[Module]) -> (Router, Document) {
    routes::app(modules, vec![], &Default::default())
}

fn document(modules: &[Module]) -> Value {
    serde_json::to_value(app(modules).1).unwrap()
}

/// The path item keys that name an operation.
const OPERATION_KEYS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Each operation of `document` as (method, path, operation).
fn each_operation(document: &Value) -> Vec<(String, String, &Value)> {
    let mut found = Vec::new();
    for (path, item) in document["paths"].as_object().unwrap() {
        for (key, operation) in item.as_object().unwrap() {
            if OPERATION_KEYS.contains(&key.as_str()) {
                found.push((key.to_uppercase(), path.clone(), operation));
            }
        }
    }
    // a map's order depends on serde_json's preserve_order feature, which
    // another crate of the build may turn on
    found.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
    found
}

/// The README's route tables of `document`: one per tag, each operation
/// with its query parameters and its summary.
fn route_tables(document: &Document) -> String {
    let document = serde_json::to_value(document).unwrap();
    let mut out = String::new();
    for tag in document["tags"].as_array().unwrap() {
        let name = tag["name"].as_str().unwrap();
        out.push_str(&format!(
            "**{name}**: {}.\n\n| Route | Query parameters | Answers |\n| --- | --- | --- |\n",
            tag["description"].as_str().unwrap()
        ));
        for (method, path, operation) in each_operation(&document) {
            if operation["tags"][0] != name {
                continue;
            }
            let parameters: Vec<String> = operation["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|parameter| parameter["in"] == "query")
                .map(|parameter| format!("`{}`", parameter["name"].as_str().unwrap()))
                .collect();
            let summary = operation["summary"]
                .as_str()
                .unwrap()
                .replace('\n', " ")
                .replace('|', "\\|");
            out.push_str(&format!(
                "| `{method} {path}` | {} | {summary} |\n",
                parameters.join(", ")
            ));
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_document_lists_only_the_enabled_modules() {
    for module in MODULES {
        let document = app(&[module]).1;
        let prefix = format!("/v1/{}/", module.name());
        let paths: Vec<&String> = document.paths.paths.keys().collect();
        assert!(
            paths.iter().any(|path| path.starts_with(&prefix)),
            "{prefix}"
        );
        for path in paths {
            assert!(
                path.starts_with(&prefix)
                    || ["/v1/status", "/v1/openapi.json"].contains(&path.as_str()),
                "{path} with only {} enabled",
                module.name()
            );
        }
    }
}

#[tokio::test]
async fn openapi_json_serves_the_document() {
    let (router, document) = app(&[Module::Sns, Module::Bitmap]);
    let request = Request::builder()
        .uri("/v1/openapi.json")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let served: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(served, serde_json::to_value(document).unwrap());
}

/// Every schema object of `value`: the component schemas and those of the
/// parameters, responses and headers.
fn schemas<'a>(value: &'a Value, found: &mut Vec<(String, &'a Value)>, at: &str) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let at = format!("{at}/{key}");
                if key == "schema" {
                    found.push((at, child));
                } else if at.ends_with("/components/schemas") {
                    for (name, schema) in child.as_object().unwrap() {
                        found.push((format!("{at}/{name}"), schema));
                    }
                } else {
                    schemas(child, found, &at);
                }
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                schemas(child, found, &format!("{at}/{i}"));
            }
        }
        _ => {}
    }
}

#[test]
fn the_document_is_valid_openapi_3_1() {
    let schema: Value = serde_json::from_str(OAS_3_1_SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let document = document(&MODULES);
    let errors: Vec<String> = validator
        .iter_errors(&document)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");

    // The OpenAPI schema leaves schema objects to JSON Schema 2020-12.
    let mut found = Vec::new();
    schemas(&document, &mut found, "");
    assert!(found.len() > 100, "{}", found.len());
    for (at, schema) in found {
        if let Err(error) = jsonschema::meta::validate(schema) {
            panic!("{at}: {error}");
        }
    }
}

#[test]
fn every_operation_has_a_unique_id_a_summary_and_a_listed_tag() {
    let tags: BTreeSet<String> = ApiDoc::openapi()
        .tags
        .unwrap()
        .into_iter()
        .map(|tag| tag.name)
        .collect();
    let mut document = app(&MODULES).1;
    let mut ids = BTreeSet::new();
    for (path, _, operation) in operations_mut(&mut document) {
        assert_eq!(
            path,
            path.to_lowercase(),
            "the front door routes lowercase paths"
        );
        let id = operation.operation_id.clone().unwrap_or_default();
        assert!(ids.insert(id.clone()), "{path}: the id {id} is taken");
        assert!(
            operation.summary.as_deref().is_some_and(|s| !s.is_empty()),
            "{id} needs a doc comment"
        );
        let operation_tags = operation.tags.clone().unwrap_or_default();
        assert_eq!(operation_tags.len(), 1, "{id}");
        assert!(tags.contains(&operation_tags[0]), "{id}");
        for status in ["429", "500"] {
            assert!(
                operation.responses.responses.contains_key(status),
                "{id} {status}"
            );
        }
    }
}

/// The schema of `path`'s query or path parameter `name`, if it has one.
fn parameter<'a>(document: &'a Value, path: &str, name: &str) -> Option<&'a Value> {
    document["paths"][path]["get"]["parameters"]
        .as_array()?
        .iter()
        .find(|parameter| parameter["name"] == name)
        .map(|parameter| &parameter["schema"])
}

#[test]
fn parameter_bounds_are_the_routes_rules() {
    let document = document(&MODULES);
    let mut checked = 0;
    for (path, _) in document["paths"].as_object().unwrap() {
        let parameters = document["paths"][path]["get"]["parameters"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for parameter in parameters {
            let name = parameter["name"].as_str().unwrap();
            let schema = &parameter["schema"];
            let bounded = schema.get("maximum").is_some()
                || schema.get("maxLength").is_some()
                || schema.get("pattern").is_some();
            let expected = match (path.as_str(), name) {
                (_, "limit") => {
                    json!({"minimum": 1, "maximum": MAX_LIMIT, "default": DEFAULT_LIMIT})
                }
                (_, "offset") => json!({"minimum": 0, "maximum": MAX_OFFSET, "default": 0}),
                (_, "before_id") => json!({"minimum": 1, "maximum": MAX_BEFORE_ID}),
                ("/v1/bitmap/search", "after") | (_, "block_height") if bounded => {
                    json!({"minimum": 0, "maximum": i32::MAX})
                }
                ("/v1/bitmap/search", "prefix") => {
                    json!({"pattern": format!("^[0-9]{{1,{}}}$", MAX_PREFIX_DIGITS)})
                }
                ("/v1/sns/search", "prefix") => json!({"minLength": 1, "maxLength": MAX_NAME_LEN}),
                ("/v1/sns/search", "namespace" | "after") => json!({"maxLength": MAX_NAME_LEN}),
                (_, "ticker") if bounded => json!({"minLength": 1, "maxLength": MAX_TICKER_LEN}),
                (_, "search") => json!({"maxLength": MAX_TICKER_LEN}),
                (_, "type") => json!({"maxLength": MAX_TYPE_LEN}),
                ("/v1/brc20/wallet/events", "address") => json!({"maxLength": MAX_ADDRESS_LEN}),
                (_, "address") if bounded => json!({"minLength": 1, "maxLength": MAX_ADDRESS_LEN}),
                (_, "inscription_id") if bounded => {
                    json!({"minLength": 1, "maxLength": MAX_INSCRIPTION_ID_LEN})
                }
                (_, "pkscript") if bounded => {
                    json!({"maxLength": MAX_PKSCRIPT_LEN, "pattern": "^([0-9a-fA-F]{2})*$"})
                }
                (_, "contract_address") => json!({"pattern": "^0[xX][0-9a-fA-F]{40}$"}),
                _ => {
                    assert!(
                        !bounded,
                        "{path} {name}: a bound this test does not check: {schema}"
                    );
                    continue;
                }
            };
            for (key, value) in expected.as_object().unwrap() {
                assert_eq!(&schema[key], value, "{path} {name} {key}");
            }
            checked += 1;
        }
    }
    assert!(checked > 20, "{checked}");
    assert_eq!(
        parameter(&document, "/v1/brc20/tickers", "limit").unwrap()["maximum"],
        MAX_LIMIT
    );
}

#[test]
#[should_panic(expected = "utoipa dropped a keyword")]
fn a_written_schema_keeps_every_keyword() {
    super::json_schema(json!({"type": "string", "const": "x"}));
}

#[test]
fn parameter_defaults_match_their_schemas() {
    let document = document(&MODULES);
    let mut checked = 0;
    for (_, path, operation) in each_operation(&document) {
        for parameter in operation["parameters"].as_array().into_iter().flatten() {
            let schema = &parameter["schema"];
            if let Some(default) = schema.get("default") {
                assert!(
                    jsonschema::is_valid(schema, default),
                    "{path} {}: {schema}",
                    parameter["name"]
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 10, "{checked}");
}

/// The README's route tables, generated from the document.
const README_BEGIN: &str = "<!-- BEGIN route tables: generated from the OpenAPI document; OPI_UPDATE_README=1 cargo test -p opi-api readme rewrites them -->";
const README_END: &str = "<!-- END route tables -->";

#[test]
fn the_readme_route_tables_are_the_documents() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md");
    let readme = std::fs::read_to_string(path).unwrap();
    let (before, rest) = readme
        .split_once(README_BEGIN)
        .expect("README.md holds the route tables' BEGIN marker");
    let (current, after) = rest
        .split_once(README_END)
        .expect("README.md holds the route tables' END marker");
    let tables = format!("\n\n{}", route_tables(&app(&MODULES).1));
    if std::env::var_os("OPI_UPDATE_README").is_some() {
        std::fs::write(
            path,
            format!("{before}{README_BEGIN}{tables}{README_END}{after}"),
        )
        .unwrap();
        return;
    }
    assert!(
        current == tables,
        "README.md's route tables differ from the OpenAPI document; run `OPI_UPDATE_README=1 cargo test -p opi-api readme` to rewrite them"
    );
}
