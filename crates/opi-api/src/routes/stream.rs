//! The push feed: `GET /v1/stream`, server-sent events of each block's
//! events, per module, once the block has the confirmations the subscriber
//! asks for, with a rollback when a reorg replaces a delivered block.
//!
//! The modules on the stream are brc20 (its events, in the event feeds'
//! form), bitmap (the bitmaps a block registered) and sns (the names a block
//! registered); each has a [`Provider`]. A module's [poller](poller) learns
//! of new blocks and reorgs from the module's block-hash table, which the
//! indexer writes as each block commits. Each subscriber runs a
//! [session], whose frames reach the client through a bounded
//! [body](body::SseBody).

mod body;
mod poller;
mod provider;
mod session;

#[cfg(test)]
mod tests;

pub use provider::Provider;

use {
    crate::{
        config::{Module, StreamSettings},
        db::Db,
        extract::JsQuery,
        js::JsObject,
        openapi::{self, ErrorBody, Null},
        reply::{ApiError, Reply},
        routes::{
            bitmap::Bitmap,
            brc20::events::Brc20FeedEvent,
            paging::{BadParam, MAX_MODULES_LEN, integer, optional_text, single},
            sns::SnsName,
        },
    },
    axum::{
        body::Body,
        extract::State,
        http::{HeaderMap, HeaderValue, header},
        response::{IntoResponse, Response},
    },
    serde_json::json,
    std::{collections::BTreeMap, sync::Arc},
    tokio::sync::{Semaphore, mpsc, oneshot, watch},
    utoipa::{
        IntoParams, ToSchema,
        openapi::{RefOr, schema::Schema},
    },
};

/// The answer of a stream request beyond STREAM_MAX_SUBSCRIBERS.
pub const TOO_MANY: &str = "too many stream subscribers";

/// The signal that ends every stream, at the API's shutdown.
#[derive(Clone)]
pub struct StreamStop(watch::Receiver<bool>);

/// The sender of a [`StreamStop`].
pub struct StreamStopper(watch::Sender<bool>);

impl StreamStopper {
    /// Ends every stream of the signal.
    pub fn stop(&self) {
        self.0.send_replace(true);
    }
}

impl StreamStop {
    pub fn new() -> (StreamStopper, StreamStop) {
        let (sender, receiver) = watch::channel(false);
        (StreamStopper(sender), StreamStop(receiver))
    }

    /// A signal that never fires.
    pub fn never() -> StreamStop {
        StreamStop(watch::channel(false).1)
    }

    pub fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }

    /// Completes once the signal has fired.
    pub async fn stopped(&mut self) {
        loop {
            if *self.0.borrow_and_update() {
                return;
            }
            if self.0.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

impl Default for StreamStop {
    fn default() -> Self {
        StreamStop::never()
    }
}

impl std::fmt::Debug for StreamStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StreamStop")
            .field(&self.is_stopped())
            .finish()
    }
}

/// The streams of a process: its settings, the enabled modules' databases,
/// the pollers and the subscriber cap.
#[derive(Clone)]
pub struct Streams(Arc<Inner>);

struct Inner {
    settings: StreamSettings,
    enabled: Vec<(Module, Db)>,
    pollers: Arc<poller::Pollers>,
    permits: Arc<Semaphore>,
    stop: StreamStop,
}

impl Streams {
    pub fn new(settings: StreamSettings, enabled: Vec<(Module, Db)>, stop: StreamStop) -> Streams {
        Streams(Arc::new(Inner {
            settings,
            enabled,
            pollers: poller::Pollers::new(settings.poll, stop.clone()),
            permits: Arc::new(Semaphore::new(settings.max_subscribers)),
            stop,
        }))
    }
}

/// A module on the stream.
#[derive(ToSchema)]
#[schema(rename_all = "lowercase")]
#[allow(dead_code)]
enum StreamModule {
    Bitmap,
    Brc20,
    Sns,
}

fn module_schema(name: &str) -> RefOr<Schema> {
    openapi::json_schema(json!({"type": "string", "enum": [name]}))
}

fn brc20_module() -> RefOr<Schema> {
    module_schema("brc20")
}

fn bitmap_module() -> RefOr<Schema> {
    module_schema("bitmap")
}

fn sns_module() -> RefOr<Schema> {
    module_schema("sns")
}

/// A brc20 block: its events, in the event feeds' form, by id.
#[derive(ToSchema)]
#[allow(dead_code)]
struct StreamBrc20Block {
    #[schema(schema_with = brc20_module)]
    module: String,
    height: i32,
    hash: String,
    /// The block's time in seconds; null when the block's time is unknown.
    #[schema(nullable)]
    block_time: i64,
    /// The fractional digits of the events' raw amounts.
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    events: Vec<Brc20FeedEvent>,
}

/// A bitmap block: the bitmaps it registered, by id.
#[derive(ToSchema)]
#[allow(dead_code)]
struct StreamBitmapBlock {
    #[schema(schema_with = bitmap_module)]
    module: String,
    height: i32,
    hash: String,
    /// Null: the bitmap database stores no block times.
    block_time: Null,
    events: Vec<Bitmap>,
}

/// An sns block: the names it registered, by id.
#[derive(ToSchema)]
#[allow(dead_code)]
struct StreamSnsBlock {
    #[schema(schema_with = sns_module)]
    module: String,
    height: i32,
    hash: String,
    /// Null: the sns database stores no block times.
    block_time: Null,
    events: Vec<SnsName>,
}

/// The data of a `rollback` event: a reorg replaced the module's blocks
/// above `height`, the last block kept; the replacement blocks follow.
#[derive(ToSchema)]
#[allow(dead_code)]
struct StreamRollback {
    module: StreamModule,
    /// The last block kept.
    height: i32,
}

/// The data of a `heartbeat` event: each module's indexed tip, null before
/// its first block.
#[derive(ToSchema)]
#[allow(dead_code)]
struct StreamHeartbeat {
    tips: BTreeMap<String, Option<i32>>,
}

/// The data of an `error` event, the last of a stream that ends early.
#[derive(ToSchema)]
#[allow(dead_code)]
struct StreamError {
    error: String,
}

/// The named schemas of the stream's events, for the document of the
/// `enabled` modules: `StreamBlock` (one of the enabled modules' blocks),
/// `StreamRollback`, `StreamHeartbeat` and `StreamError`, and the schemas
/// they refer to.
pub fn components(enabled: &[Module]) -> Vec<(String, RefOr<Schema>)> {
    fn add<T: ToSchema>(schemas: &mut Vec<(String, RefOr<Schema>)>) -> String {
        let name = T::name().into_owned();
        schemas.push((name.clone(), T::schema()));
        T::schemas(schemas);
        name
    }
    let mut schemas = Vec::new();
    add::<StreamRollback>(&mut schemas);
    add::<StreamModule>(&mut schemas);
    add::<StreamHeartbeat>(&mut schemas);
    add::<StreamError>(&mut schemas);
    let mut blocks = Vec::new();
    for provider in enabled.iter().filter_map(|&m| Provider::of(m).ok()) {
        let block = match provider {
            Provider::Bitmap => {
                add::<Bitmap>(&mut schemas);
                add::<StreamBitmapBlock>(&mut schemas)
            }
            Provider::Brc20 => {
                add::<Brc20FeedEvent>(&mut schemas);
                add::<StreamBrc20Block>(&mut schemas)
            }
            Provider::Sns => {
                add::<SnsName>(&mut schemas);
                add::<StreamSnsBlock>(&mut schemas)
            }
        };
        blocks.push((provider, block));
    }
    blocks.sort();
    let one_of: Vec<_> = blocks
        .iter()
        .map(|(_, name)| json!({"$ref": format!("#/components/schemas/{name}")}))
        .collect();
    schemas.push((
        "StreamBlock".into(),
        openapi::json_schema(json!({
            "description": "The data of a `block` event: one block of one module, with its events.",
            "oneOf": one_of,
        })),
    ));
    schemas
}

/// The parameters of `/v1/stream`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct StreamParams {
    /// Comma-separated modules: `brc20`, `bitmap`, `sns`. Left out, the
    /// stream carries every enabled module on the stream. runes is not on
    /// the stream yet.
    #[param(max_length = 64)]
    modules: Option<String>,
    /// The confirmations a block waits for: block H is sent once the
    /// module's indexed tip reaches H + confirmations - 1, so 1 sends each
    /// block as soon as it is indexed. At most STREAM_MAX_CONFIRMATIONS
    /// (default 100). Left out, STREAM_DEFAULT_CONFIRMATIONS (default 1).
    #[param(minimum = 1)]
    confirmations: Option<i64>,
    /// The first height to send, replaying the stored blocks from there in
    /// order; at least each module's oldest kept block. Left out, each
    /// module starts with its first block not yet due, or, for a module
    /// with no block yet, its first block.
    #[param(minimum = 0, maximum = 2147483647)]
    from_height: Option<i64>,
}

/// Where a stream starts.
struct Start {
    modules: Vec<Provider>,
    confirmations: i32,
    from_height: Option<i32>,
    /// The `Last-Event-ID`: module, height and hash.
    resume: Option<(Provider, i32, String)>,
}

fn parse(
    query: &JsObject,
    headers: &HeaderMap,
    enabled: &[Module],
    settings: &StreamSettings,
) -> Result<Start, BadParam> {
    let mut modules = Vec::new();
    let names = optional_text(query, "modules", MAX_MODULES_LEN)?.unwrap_or_default();
    for name in names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let Some(module) = crate::config::MODULES
            .into_iter()
            .find(|module| module.name() == name)
        else {
            return Err(BadParam(format!("modules names an unknown module: {name}")));
        };
        if !enabled.contains(&module) {
            return Err(BadParam(format!("{name} is not enabled on this node")));
        }
        let provider = Provider::of(module).map_err(BadParam)?;
        if !modules.contains(&provider) {
            modules.push(provider);
        }
    }
    if modules.is_empty() {
        modules = enabled
            .iter()
            .filter_map(|&module| Provider::of(module).ok())
            .collect();
        if modules.is_empty() {
            return Err(BadParam("no enabled module is on the stream".into()));
        }
    }
    modules.sort();
    let confirmations = integer(
        query,
        "confirmations",
        i64::from(settings.default_confirmations),
        1,
        i64::from(settings.max_confirmations),
    )? as i32;
    let from_height = match single(query, "from_height")? {
        None => None,
        Some(_) => Some(integer(query, "from_height", 0, 0, i64::from(i32::MAX))? as i32),
    };
    let resume = match headers.get("last-event-id") {
        None => None,
        Some(value) => {
            let bad = || {
                BadParam(
                    "Last-Event-ID must be <module>:<height>:<hash> of a module of the stream"
                        .into(),
                )
            };
            let value = value.to_str().map_err(|_| bad())?;
            let mut parts = value.splitn(3, ':');
            let (Some(name), Some(height), Some(hash)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(bad());
            };
            let provider = modules
                .iter()
                .copied()
                .find(|provider| provider.name() == name)
                .ok_or_else(bad)?;
            if height.is_empty() || height.len() > 10 || !height.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(bad());
            }
            let height: i32 = height.parse().map_err(|_| bad())?;
            if hash.is_empty() || hash.contains('\0') {
                return Err(bad());
            }
            Some((provider, height, hash.to_string()))
        }
    };
    Ok(Start {
        modules,
        confirmations,
        from_height,
        resume,
    })
}

/// Server-sent events of each block's events, with rollbacks on reorgs.
///
/// One `block` event per block and module,
/// `{"module","height","hash","block_time","events":[...]}` (schema
/// `StreamBlock`), with the SSE id `<module>:<height>:<hash>`. brc20's
/// events take the event feeds' form, with raw amounts of `decimals`
/// fractional digits; bitmap's are the bitmaps the block registered and
/// sns's the names. A block is sent once its module's indexed tip reaches
/// its height plus `confirmations` less one. The blocks of all the stream's
/// modules are sent by height, then module name, so a module behind the
/// others holds them back.
///
/// When a reorg replaces a block the stream has sent, a `rollback` event
/// `{"module","height"}` (`StreamRollback`) names the last block kept, and
/// the replacement blocks follow. A `heartbeat` event `{"tips":{...}}`
/// (`StreamHeartbeat`) comes every STREAM_HEARTBEAT_SECS while no block is
/// due. A stream whose client cannot take a message for that long ends with
/// an `error` event `{"error"}` (`StreamError`); so does one whose query
/// fails.
///
/// The `Last-Event-ID` header resumes after the block it names, and sends a
/// rollback first when that block is no longer stored. It takes precedence
/// over `from_height`.
#[utoipa::path(
    get,
    path = "/v1/stream",
    tag = "stream",
    params(
        StreamParams,
        ("Last-Event-ID" = Option<String>, Header, description = "The id of the last block received, `<module>:<height>:<hash>`: the stream resumes after it. Left out, the stream starts at `from_height`."),
    ),
    responses(
        (status = 200, description = "The stream: `block` events (data: `StreamBlock`), `rollback` events (`StreamRollback`), `heartbeat` events (`StreamHeartbeat`) and a last `error` event (`StreamError`) on an early end. A block's SSE id is `<module>:<height>:<hash>`.", content_type = "text/event-stream", body = String),
        (status = 400, description = "An invalid parameter or `Last-Event-ID`, a module that is unknown, disabled or not on the stream, or a start below a module's oldest kept block", body = ErrorBody),
        (status = 503, description = "`too many stream subscribers`: STREAM_MAX_SUBSCRIBERS streams are open", body = ErrorBody),
    ),
)]
pub(super) async fn stream(
    State(streams): State<Streams>,
    JsQuery(query): JsQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let inner = &streams.0;
    let enabled: Vec<Module> = inner.enabled.iter().map(|(module, _)| *module).collect();
    let start = match parse(&query, &headers, &enabled, &inner.settings) {
        Ok(start) => start,
        Err(bad) => return Ok(bad.reply().into_response()),
    };
    let Ok(permit) = inner.permits.clone().try_acquire_owned() else {
        return Ok(Reply::error(503, TOO_MANY).into_response());
    };
    let gauge = opi_core::metrics::global().stream_subscriber();
    let db_of = |module: Module| {
        inner
            .enabled
            .iter()
            .find(|(m, _)| *m == module)
            .map(|(_, db)| db.clone())
            .expect("an enabled module")
    };
    let mut cursors = Vec::with_capacity(start.modules.len());
    for &provider in &start.modules {
        let db = db_of(provider.module());
        // subscribe first, so no change after the reads below is missed
        let watch = inner.pollers.watch(provider, &db);
        let tip = provider.tip(&db).await?.map(|(height, _)| height);
        let oldest = provider.oldest(&db).await?;
        let (next, last) = match &start.resume {
            Some((module, height, hash)) if *module == provider => {
                (Some(height + 1), Some((*height, hash.clone())))
            }
            // the blocks before the resumed one in the stream's order
            Some((module, height, _)) if provider < *module => (Some(height + 1), None),
            Some((_, height, _)) => (Some(*height), None),
            None => match start.from_height {
                Some(from) => (Some(from), None),
                None => (
                    tip.map(|tip| (tip - start.confirmations + 2).max(oldest.unwrap_or(0))),
                    None,
                ),
            },
        };
        if let (Some(next), Some(oldest)) = (next, oldest)
            && next < oldest
            && last.is_none()
        {
            let what = if start.resume.is_some() {
                "Last-Event-ID"
            } else {
                "from_height"
            };
            return Ok(Reply::error(
                400,
                &format!(
                    "{what} is below {oldest}, the oldest block {} keeps",
                    provider.name()
                ),
            )
            .into_response());
        }
        cursors.push(session::Cursor {
            provider,
            db,
            watch,
            next,
            last,
            dirty: true,
        });
    }
    let (chunks, receiver) = mpsc::channel(inner.settings.buffer);
    let (end, ended) = oneshot::channel();
    let session = session::Session {
        settings: inner.settings,
        confirmations: start.confirmations,
        cursors,
        chunks,
        end,
        stop: inner.stop.clone(),
        permit,
        gauge,
    };
    tokio::spawn(session.run());
    let body = body::SseBody {
        chunks: receiver,
        end: Some(ended),
        done: false,
    };
    let mut response = Body::from_stream(body).into_response();
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    // nginx passes the stream on as it comes
    response_headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}
