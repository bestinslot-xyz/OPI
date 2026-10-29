//! Helpers the module route tests share: an app serving one module, a
//! request (whose answer is checked against the OpenAPI document), and the
//! live-database gate.

#![allow(dead_code)]

use {
    axum::{
        Router,
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::Request,
    },
    opi_api::{
        config::{MODULES, Module, load_config},
        db::Db,
        js::JsValue,
        router,
    },
    serde_json::{Value, json},
    sqlx::{
        PgPool,
        postgres::{PgConnectOptions, PgPoolOptions},
    },
    std::{
        collections::HashMap,
        net::SocketAddr,
        sync::{Arc, LazyLock, Mutex},
    },
    tower::ServiceExt,
};

/// The OpenAPI document of every module.
static DOCUMENT: LazyLock<Value> = LazyLock::new(|| {
    serde_json::to_value(opi_api::routes::app(&MODULES, vec![], &Default::default()).1).unwrap()
});

/// The validators of the answer schemas checked so far, by path template,
/// status and media type.
static VALIDATORS: LazyLock<Mutex<HashMap<String, Arc<jsonschema::Validator>>>> =
    LazyLock::new(Default::default);

/// The document's path template that `path` matches, if any: a `{name}`
/// segment matches any non-empty segment.
fn path_template(path: &str) -> Option<&'static str> {
    let segments: Vec<&str> = path.split('/').collect();
    DOCUMENT["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .find(|template| {
            let parts: Vec<&str> = template.split('/').collect();
            parts.len() == segments.len()
                && parts.iter().zip(&segments).all(|(part, segment)| {
                    part == segment || (part.starts_with('{') && !segment.is_empty())
                })
        })
}

/// Checks `answer` to a GET of `uri` against the OpenAPI document: the
/// operation lists its status and content type, and a JSON body validates
/// against that answer's schema. Two 404s are not checked: a path the
/// document has no template for (an unknown route) and a disabled module's
/// `"<module> is not enabled on this node"`, which the document leaves out
/// since it lists only the enabled modules. Any other answer on a path
/// without a template fails, so a mistyped test URI cannot skip the check.
pub fn check_against_document(uri: &str, answer: &Answer) {
    if answer.status == 404 {
        let disabled = serde_json::from_str::<Value>(&answer.body).is_ok_and(|body| {
            body["error"]
                .as_str()
                .is_some_and(|error| error.ends_with("is not enabled on this node"))
        });
        if disabled {
            return;
        }
    }
    let path = uri.split('?').next().unwrap().to_ascii_lowercase();
    let path = match path.strip_suffix('/') {
        Some(stripped) if !stripped.is_empty() => stripped.to_string(),
        _ => path,
    };
    let Some(template) = path_template(&path) else {
        assert_eq!(
            answer.status, 404,
            "{uri}: no path of the OpenAPI document matches it"
        );
        return;
    };
    let status = answer.status.to_string();
    let response = &DOCUMENT["paths"][template]["get"]["responses"][&status];
    assert!(
        response.is_object(),
        "{uri}: the document lists no {status} answer for {template}"
    );
    let media = answer
        .content_type
        .as_deref()
        .and_then(|value| value.split(';').next())
        .unwrap_or_default()
        .trim()
        .to_string();
    let schema = &response["content"][&media]["schema"];
    assert!(
        schema.is_object(),
        "{uri}: the document lists no {media} body for {template} {status}"
    );
    if media != "application/json" {
        return;
    }
    let key = format!("{template} {status} {media}");
    let validator = VALIDATORS
        .lock()
        .unwrap()
        .entry(key)
        .or_insert_with(|| {
            // the schema's `#/components/...` references resolve in this root
            let root = json!({"allOf": [schema], "components": DOCUMENT["components"]});
            Arc::new(jsonschema::validator_for(&root).unwrap())
        })
        .clone();
    let body: Value = serde_json::from_str(&answer.body)
        .unwrap_or_else(|error| panic!("{uri}: a JSON answer that does not parse: {error}"));
    let errors: Vec<String> = validator
        .iter_errors(&body)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{uri}: the {status} answer does not match the document's schema for {template}: {errors:#?}\n{}",
        answer.body
    );
}

pub const HTML: &str = "text/html; charset=utf-8";
pub const JSON: &str = "application/json; charset=utf-8";
pub const INTERNAL_ERROR: &str = r#"{"error":"internal error","result":null}"#;
pub const NOT_INDEXED: &str = r#"{"error":"block not indexed yet","result":null}"#;

/// The app with only `module` enabled, reading from `db`.
pub fn app(module: Module, db: Db) -> Router {
    app_with_env(module, db, &[])
}

/// [`app`] with the settings `vars` adds to its environment.
pub fn app_with_env(module: Module, db: Db, vars: &[(&str, &str)]) -> Router {
    let database = format!("{}_DATABASE", module.db_prefix());
    let env: HashMap<String, String> = [
        ("OPI_MODULES".to_string(), module.name().to_string()),
        (database, "d".to_string()),
    ]
    .into_iter()
    .chain(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())))
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

/// The media type of server-sent events.
pub const EVENT_STREAM: &str = "text/event-stream";

/// The answer to a GET of `uri`, checked against the OpenAPI document. An
/// event stream's body is not read (it does not end): the answer's body is
/// empty, and dropping it closes the stream; [`Sse`] reads one.
pub async fn get(app: &Router, uri: &str) -> Answer {
    get_with(app, uri, &[]).await
}

/// [`get`] with the request headers `headers`.
pub async fn get_with(app: &Router, uri: &str, headers: &[(&str, &str)]) -> Answer {
    let response = request(app, uri, headers).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string());
    let body = if content_type.as_deref() == Some(EVENT_STREAM) {
        String::new()
    } else {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    };
    let answer = Answer {
        status,
        content_type,
        body,
    };
    check_against_document(uri, &answer);
    answer
}

async fn request(app: &Router, uri: &str, headers: &[(&str, &str)]) -> axum::response::Response {
    let mut request = Request::builder().uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut request = request.body(Body::empty()).unwrap();
    let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    app.clone().oneshot(request).await.unwrap()
}

/// A frame of an event stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub event: String,
    pub id: Option<String>,
    pub data: Value,
}

/// An open event stream of `/v1/stream`, whose frames are checked against
/// the document's schema of their event's data.
pub struct Sse {
    body: axum::body::BodyDataStream,
    buffer: String,
}

/// The document's schema of an event's data, by event name.
fn event_schema(event: &str) -> &'static str {
    match event {
        "block" => "StreamBlock",
        "rollback" => "StreamRollback",
        "heartbeat" => "StreamHeartbeat",
        "error" => "StreamError",
        other => panic!("an unknown stream event {other}"),
    }
}

impl Sse {
    /// Opens `uri` with the `Last-Event-ID` `last_event_id`; panics unless
    /// it answers a 200 event stream.
    pub async fn open(app: &Router, uri: &str, last_event_id: Option<&str>) -> Sse {
        let headers: Vec<(&str, &str)> = last_event_id
            .map(|id| ("last-event-id", id))
            .into_iter()
            .collect();
        let response = request(app, uri, &headers).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string());
        if status != 200 {
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            panic!("{uri}: {status} {}", String::from_utf8_lossy(&body));
        }
        let answer = Answer {
            status,
            content_type: content_type.clone(),
            body: String::new(),
        };
        check_against_document(uri, &answer);
        assert_eq!(content_type.as_deref(), Some(EVENT_STREAM), "{uri}");
        Sse {
            body: response.into_body().into_data_stream(),
            buffer: String::new(),
        }
    }

    /// The next frame within `wait`; `None` when the stream has ended, and
    /// a panic when no frame comes in time.
    pub async fn next_within(&mut self, wait: std::time::Duration) -> Option<Frame> {
        use futures::StreamExt;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let text: String = self.buffer.drain(..end + 2).collect();
                return Some(parse_frame(&text));
            }
            let chunk = tokio::time::timeout_at(deadline, self.body.next())
                .await
                .unwrap_or_else(|_| panic!("no frame within {wait:?}; buffered {:?}", self.buffer));
            match chunk {
                Some(chunk) => self
                    .buffer
                    .push_str(std::str::from_utf8(&chunk.unwrap()).unwrap()),
                None => {
                    assert!(self.buffer.is_empty(), "a cut frame {:?}", self.buffer);
                    return None;
                }
            }
        }
    }

    /// The next frame within 10 seconds.
    pub async fn next(&mut self) -> Option<Frame> {
        self.next_within(std::time::Duration::from_secs(10)).await
    }

    /// The next frame that is not a heartbeat.
    pub async fn next_message(&mut self) -> Option<Frame> {
        loop {
            let frame = self.next().await?;
            if frame.event != "heartbeat" {
                return Some(frame);
            }
        }
    }
}

/// A frame's fields, its data checked against its event's schema.
fn parse_frame(text: &str) -> Frame {
    let (mut event, mut id, mut data) = (None, None, None);
    for line in text.lines().filter(|line| !line.is_empty()) {
        let (field, value) = line.split_once(": ").unwrap_or((line, ""));
        match field {
            "event" => event = Some(value.to_string()),
            "id" => id = Some(value.to_string()),
            "data" => data = Some(value.to_string()),
            other => panic!("an unknown field {other} in {text:?}"),
        }
    }
    let event = event.unwrap_or_else(|| panic!("a frame without event: {text:?}"));
    let data: Value = serde_json::from_str(&data.expect("a frame with data")).unwrap();
    let name = event_schema(&event);
    let validator = VALIDATORS
        .lock()
        .unwrap()
        .entry(format!("event {name}"))
        .or_insert_with(|| {
            let root = json!({
                "$ref": format!("#/components/schemas/{name}"),
                "components": DOCUMENT["components"],
            });
            Arc::new(jsonschema::validator_for(&root).unwrap())
        })
        .clone();
    let errors: Vec<String> = validator
        .iter_errors(&data)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "a {event} frame that does not match {name}: {errors:#?}\n{data}"
    );
    Frame { event, id, data }
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
/// server's startup option `plan_cache_mode`, or `None` (the test is
/// skipped) when it is unset. The live tests only read.
pub fn live_pool(var: &str, max_connections: u32) -> Option<PgPool> {
    let Ok(url) = std::env::var(var) else {
        eprintln!("{var} is unset; skipping");
        return None;
    };
    let options = url
        .parse::<PgConnectOptions>()
        .unwrap()
        .options([("plan_cache_mode", opi_api::db::PLAN_CACHE_MODE)]);
    Some(
        PgPoolOptions::new()
            .max_connections(max_connections)
            .connect_lazy_with(options),
    )
}

/// `module`'s app on the database `var` names, and a pool on it.
pub fn live(module: Module, var: &str) -> Option<(Router, PgPool)> {
    let pool = live_pool(var, 2)?;
    Some((app(module, Db::from_pool(pool.clone())), pool))
}

/// Runs `body` with `module`'s app on a throwaway database on the local
/// Postgres (127.0.0.1:5432, the current user) built by running `scripts` in
/// order, created with `collation` (a LC_COLLATE, the template's when
/// `None`) and dropped even when `body` panics. Skips unless
/// `OPI_TEST_PG=1`.
pub async fn with_seeded<F, Fut>(
    module: Module,
    tag: &str,
    collation: Option<&str>,
    scripts: &[&str],
    body: F,
) where
    F: FnOnce(Router, PgPool) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    with_seeded_pool(module, tag, collation, scripts, 2, body).await;
}

/// [`with_seeded`] with a pool of `max_connections`.
pub async fn with_seeded_pool<F, Fut>(
    module: Module,
    tag: &str,
    collation: Option<&str>,
    scripts: &[&str],
    max_connections: u32,
    body: F,
) where
    F: FnOnce(Router, PgPool) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    use sqlx::Connection;
    if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
        eprintln!("OPI_TEST_PG is not 1; skipping");
        return;
    }
    let user = std::env::var("USER").unwrap();
    let options = || {
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username(&user)
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("opi_api_{tag}_{}_{nanos}", std::process::id());
    let mut root = sqlx::PgConnection::connect_with(&options().database("postgres"))
        .await
        .unwrap();
    let create = match collation {
        None => format!("CREATE DATABASE {name}"),
        Some(collation) => format!(
            "CREATE DATABASE {name} TEMPLATE template0 ENCODING 'UTF8' LC_COLLATE '{collation}' LC_CTYPE '{collation}'"
        ),
    };
    sqlx::query(sqlx::AssertSqlSafe(create))
        .execute(&mut root)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_lazy_with(
            options()
                .database(&name)
                .options([("plan_cache_mode", opi_api::db::PLAN_CACHE_MODE)]),
        );
    for script in scripts {
        sqlx::raw_sql(sqlx::AssertSqlSafe(script.to_string()))
            .execute(&pool)
            .await
            .unwrap();
    }
    let router = app(module, Db::from_pool(pool.clone()));
    let result = tokio::spawn(body(router, pool.clone())).await;
    pool.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&mut root)
    .await
    .unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
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
