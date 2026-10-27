//! Helpers the module route tests share: an app serving one module, a
//! request, and the live-database gate.

#![allow(dead_code)]

use {
    axum::{
        Router,
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::Request,
    },
    opi_api::{
        config::{Module, load_config},
        db::Db,
        js::JsValue,
        router,
    },
    sqlx::{PgPool, postgres::PgPoolOptions},
    std::{collections::HashMap, net::SocketAddr},
    tower::ServiceExt,
};

pub const HTML: &str = "text/html; charset=utf-8";
pub const JSON: &str = "application/json; charset=utf-8";
pub const INTERNAL_ERROR: &str = r#"{"error":"internal error","result":null}"#;
pub const NOT_INDEXED: &str = r#"{"error":"block not indexed yet","result":null}"#;

/// The app with only `module` enabled, reading from `db`.
pub fn app(module: Module, db: Db) -> Router {
    let database = format!("{}_DATABASE", module.db_prefix());
    let env: HashMap<String, String> = [
        ("OPI_MODULES".to_string(), module.name().to_string()),
        (database, "d".to_string()),
    ]
    .into_iter()
    .collect();
    let config = load_config(|key| env.get(key).cloned()).unwrap();
    router(&config, vec![(module, db)])
}

/// The app over a database double whose latest height is `value`.
pub fn height(module: Module, value: JsValue) -> Router {
    app(module, Db::fake(move |_| Ok(value.clone())))
}

/// The app over a database double whose every query fails.
pub fn down(module: Module) -> Router {
    app(module, Db::fake(|_| anyhow::bail!("connection refused")))
}

pub struct Answer {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}

pub async fn get(app: &Router, uri: &str) -> Answer {
    let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string());
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Answer {
        status,
        content_type,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

pub async fn expect(app: &Router, uri: &str, status: u16, content_type: &str, body: &str) {
    let answer = get(app, uri).await;
    assert_eq!(answer.status, status, "{uri}: {}", answer.body);
    assert_eq!(answer.content_type.as_deref(), Some(content_type), "{uri}");
    assert_eq!(answer.body, body, "{uri}");
}

/// finalhandler's 404 page for a GET of `path`.
pub fn not_found_page(path: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot GET {path}</pre>\n</body>\n</html>\n"
    )
}

/// A pool on the database the environment variable `var` names, with the
/// server's session set up as [`opi_api::db::start_session`] does, or
/// `None` (the test is skipped) when it is unset. The live tests only read.
pub fn live_pool(var: &str, max_connections: u32) -> Option<PgPool> {
    let Ok(url) = std::env::var(var) else {
        eprintln!("{var} is unset; skipping");
        return None;
    };
    Some(
        PgPoolOptions::new()
            .max_connections(max_connections)
            .after_connect(|connection, _| Box::pin(opi_api::db::start_session(connection)))
            .connect_lazy(&url)
            .unwrap(),
    )
}

/// `module`'s app on the database `var` names, and a pool on it.
pub fn live(module: Module, var: &str) -> Option<(Router, PgPool)> {
    let pool = live_pool(var, 2)?;
    Some((app(module, Db::from_pool(pool.clone())), pool))
}

/// `text` for a query string, with every byte but ASCII letters and digits
/// percent-encoded.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `JSON.stringify` of a string.
pub fn js_string(text: &str) -> String {
    opi_api::js::json_stringify(&JsValue::from(text)).unwrap()
}
