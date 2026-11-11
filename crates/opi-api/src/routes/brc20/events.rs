//! The event feeds: `/v1/brc20/ticker/events`, `/v1/brc20/wallet/events`
//! and `/v1/brc20/deploys`.
//!
//! Each lists the rows of `brc20_events` (and `brc20_light_events`, which the
//! indexer fills instead of it in light mode), newest first, and answers
//! `{"error":null,"result":...}`. A missing or invalid parameter answers
//! `400 {"error":"<reason>","result":null}` and an unknown ticker
//! `404 {"error":"ticker not found","result":null}`.
//!
//! An event is
//! `{"id","block_height","block_time","inscription_id","event_type","ticker_decimals","event"}`:
//! `id` orders the feed, `block_time` is the block's time in seconds (null
//! when the block's time is unknown: a block indexed before block times were
//! stored that the journal did not hold at a start, or any such block in
//! light mode), `event_type` is the name from `brc20_event_types`
//! and `event` is the stored event object. Its amounts are raw integers as
//! strings with 18 fractional digits, so a result's `decimals` is
//! [`AMOUNT_DECIMALS`] and `ticker_decimals` is the deploy precision of the
//! event's ticker (null for an event without a ticker, such as a BRC2.0
//! call). BRC2.0 deposits are `transfer-transfer` events spent to the module
//! pkScript and withdrawals are `brc20prog-withdraw-*` events; they appear in
//! a wallet's feed through its source and spent parties like any event.
//!
//! The feeds page by key: `limit` (default 100, at most 500) and
//! `before_id`, the `next_before_id` of the previous page. A page holds the
//! events with an id below `before_id`, so a page never repeats or skips an
//! event when new events arrive (an offset would shift). `has_more` tells
//! whether an older event exists and `next_before_id` is the id to ask with
//! (null on the last page). `type` keeps one event type, by its name.
//! `category` keeps the events of one or more categories, comma-separated
//! ([`CATEGORIES`], `brc20_event_category` in opi-brc20's db_init.sql):
//!
//! - `deploy`: `deploy-inscribe` and `predeploy-inscribe`;
//! - `mint`: `mint-inscribe`;
//! - `transfer`: `transfer-inscribe`, and `transfer-transfer` not spent to
//!   the BRC2.0 module's pkScript;
//! - `wrap`: `transfer-transfer` spent to the BRC2.0 module's pkScript
//!   ([`BRC20_PROG_MODULE_PKSCRIPT`](super::tickers::BRC20_PROG_MODULE_PKSCRIPT),
//!   the same on every network), a deposit
//!   into BRC2.0 (or a burn, before BRC2.0 accepts the ticker);
//! - `unwrap`: `brc20prog-withdraw-inscribe` and
//!   `brc20prog-withdraw-transfer`, both steps of a withdrawal, so every
//!   event type has one category; `type=brc20prog-withdraw-transfer` with it
//!   keeps the withdrawals themselves;
//! - `brc20prog`: the BRC2.0 deploy, call and transact inscriptions and
//!   transfers.
//!
//! With both `type` and `category`, an event passes both.
//!
//! Every page costs the same at any depth: an index range scan from the
//! cursor. A ticker's events come from `(tick, id)` indexes, with one per
//! rare category (`deploy`, `wrap`, `unwrap`); a wallet's from
//! `brc20_event_parties`, keyed by `(pkscript, event_id)`; the network-wide
//! list from `(id)` indexes per rare category. A filter without its own
//! index (a ticker's or a wallet's `mint` page, say) reads the rows until
//! the page fills.
//!
//! A database uses one event table per mode (full mode writes `brc20_events`,
//! light mode `brc20_light_events`), which the cursor relies on: an id names
//! one event. A reorg rewinds the ids with the blocks, so a cursor taken
//! above a rewound tip can skip or repeat events of the rewound range.
//!
//! A ticker's events are those whose event object has `tick` equal to the
//! lowercased ticker. A wallet's events are those where the wallet is a
//! party of the event (`deployer`, `minted`, `source`, `spent`,
//! `predeployer`): for `pkscript`, the party's `*_pkScript` field is that
//! pkScript; for `address`, the party's `*_wallet` field is that address.
//! An address is read as the pkScript it encodes, whose parties' rows come
//! from `brc20_event_parties`, and an event of them passes when one of its
//! `*_wallet` fields is the address as given: the indexer writes each
//! wallet field from its party's pkScript, so the two name the same events,
//! and an address of another network, or not in the form the indexer
//! writes, names none. `address` is a Bitcoin address, or an EVM address
//! whose Bitcoin address comes from `evm_addresses` (an unmapped one has no
//! events), and `pkscript` is hex.

use {
    super::{
        block_height_of_db,
        tickers::{AMOUNT_DECIMALS, TickerParam, is_evm_address, ticker, ticker_decimals},
    },
    crate::{
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        openapi::{Answer, DbHeight, ErrorBody, LimitParam},
        pg_json::{self, oid},
        reply::{ApiError, Reply},
        routes::paging::{
            BadParam, DEFAULT_LIMIT, MAX_BEFORE_ID, MAX_CATEGORY_LEN, MAX_LIMIT, MAX_TYPE_LEN,
            Wallet, integer, optional_text, single, split_page,
        },
    },
    axum::extract::State,
    std::str::FromStr,
    utoipa::{IntoParams, ToSchema},
};

#[cfg(test)]
mod tests;

/// The fractional digits of an event's amounts.
pub(in crate::routes) const EVENT_DECIMALS: i32 = AMOUNT_DECIMALS;

/// The event categories, in the order a request's list is kept in.
pub const CATEGORIES: [&str; 6] = ["deploy", "mint", "transfer", "wrap", "unwrap", "brc20prog"];

/// The paging, type and category filters of a feed.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct FeedParams {
    /// Keeps one event type, by its `brc20_event_types` name.
    #[param(max_length = 256)]
    r#type: Option<String>,
    /// Keeps the events of these categories, comma-separated: `deploy`,
    /// `mint`, `transfer`, `wrap`, `unwrap`, `brc20prog`.
    #[param(
        max_length = 64,
        pattern = "^((deploy|mint|transfer|wrap|unwrap|brc20prog)(,(deploy|mint|transfer|wrap|unwrap|brc20prog))*)?$"
    )]
    category: Option<String>,
    /// The previous page's `next_before_id`: the page holds the events
    /// with a lower id.
    #[param(minimum = 1, maximum = 999999999999999999_i64)]
    before_id: Option<i64>,
}

/// An event of a feed.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(in crate::routes) struct Brc20FeedEvent {
    /// The event's id, which orders the feed.
    id: i64,
    block_height: i32,
    /// The block's time in seconds; null when the block's time is unknown.
    #[schema(nullable)]
    block_time: i64,
    inscription_id: String,
    /// The event type's name.
    #[schema(nullable)]
    event_type: String,
    /// The deploy precision of the event's ticker; null for an event
    /// without a ticker.
    #[schema(nullable)]
    ticker_decimals: i32,
    /// The stored event object; its amounts are raw integers as strings
    /// with 18 fractional digits.
    #[schema(value_type = Object)]
    event: serde_json::Value,
}

/// A page of a ticker's events.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20TickerEvents {
    /// The lowercase ticker.
    ticker: String,
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    ticker_decimals: i32,
    /// The `type` filter, null without one.
    #[schema(nullable)]
    event_type: String,
    /// Newest first.
    events: Vec<Brc20FeedEvent>,
    limit: i64,
    /// Whether older events exist.
    has_more: bool,
    /// The `before_id` of the next page, null on the last page.
    #[schema(nullable)]
    next_before_id: i64,
    block_height: DbHeight,
}

/// A page of a wallet's events.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20WalletEvents {
    /// The address asked for, or an EVM address's mapped Bitcoin address;
    /// null for a `pkscript` request.
    #[schema(nullable)]
    address: String,
    /// The lowercase pkScript asked for; null for an `address` request.
    #[schema(nullable)]
    pkscript: String,
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    /// The `type` filter, null without one.
    #[schema(nullable)]
    event_type: String,
    /// Newest first.
    events: Vec<Brc20FeedEvent>,
    limit: i64,
    /// Whether older events exist.
    has_more: bool,
    /// The `before_id` of the next page, null on the last page.
    #[schema(nullable)]
    next_before_id: i64,
    block_height: DbHeight,
}

/// A page of the network's events of some categories.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20CategoryEvents {
    /// The categories listed, in [`CATEGORIES`] order; `["deploy"]` without
    /// a `category` parameter.
    category: Vec<String>,
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    /// The `type` filter, null without one.
    #[schema(nullable)]
    event_type: String,
    /// Newest first.
    events: Vec<Brc20FeedEvent>,
    limit: i64,
    /// Whether older events exist.
    has_more: bool,
    /// The `before_id` of the next page, null on the last page.
    #[schema(nullable)]
    next_before_id: i64,
    block_height: DbHeight,
}

/// The wallet of a wallet route: exactly one of `address` and `pkscript`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub(super) struct WalletParams {
    /// A Bitcoin address, or an EVM address that answers as its mapped
    /// Bitcoin address.
    #[param(max_length = 128)]
    address: Option<String>,
    /// A pkScript, hex.
    #[param(max_length = 20000, pattern = "^([0-9a-fA-F]{2})*$")]
    pkscript: Option<String>,
}

/// The paging, type and category filters of a feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRequest {
    pub limit: i64,
    pub before_id: Option<i64>,
    /// The `type` parameter, a name of `brc20_event_types`.
    pub event_type: Option<String>,
    /// The `category` parameter, in [`CATEGORIES`] order without repeats.
    pub categories: Option<Vec<String>>,
}

impl FeedRequest {
    pub fn from_query(query: &JsObject) -> Result<FeedRequest, BadParam> {
        let before_id = match single(query, "before_id")? {
            None => None,
            Some(_) => Some(integer(query, "before_id", 0, 1, MAX_BEFORE_ID)?),
        };
        let event_type = match optional_text(query, "type", MAX_TYPE_LEN)? {
            None | Some("") => None,
            Some(name) => Some(name.to_string()),
        };
        Ok(FeedRequest {
            limit: integer(query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
            before_id,
            event_type,
            categories: categories(query)?,
        })
    }
}

/// The `category` list: `None` without one (or an empty one), the named
/// categories in [`CATEGORIES`] order otherwise.
pub fn categories(query: &JsObject) -> Result<Option<Vec<String>>, BadParam> {
    let list = match optional_text(query, "category", MAX_CATEGORY_LEN)? {
        None | Some("") => return Ok(None),
        Some(list) => list,
    };
    let names: Vec<&str> = list.split(',').collect();
    if names.iter().any(|name| !CATEGORIES.contains(name)) {
        return Err(BadParam(format!(
            "category must be one or more of {}, separated by commas",
            CATEGORIES.join(", ")
        )));
    }
    Ok(Some(
        CATEGORIES
            .iter()
            .filter(|category| names.contains(category))
            .map(|category| category.to_string())
            .collect(),
    ))
}

/// A feed row.
struct FeedRow {
    id: i64,
    block_height: i32,
    block_time: Option<i64>,
    inscription_id: String,
    event_type: Option<String>,
    ticker_decimals: Option<i32>,
    event: String,
}

/// The `type` filter as an event type id: `None` for no filter, `Err` for a
/// name `brc20_event_types` does not list.
async fn event_type_id(
    db: &Db,
    name: Option<&str>,
) -> Result<Result<Option<i32>, BadParam>, ApiError> {
    let Some(name) = name else {
        return Ok(Ok(None));
    };
    let id = sqlx::query_scalar!(
        "select event_type_id from brc20_event_types where event_type_name = $1 limit 1;",
        name
    )
    .fetch_optional(db.pool()?)
    .await?;
    if let Some(id) = id {
        return Ok(Ok(Some(id)));
    }
    let names = sqlx::query_scalar!(
        "select event_type_name from brc20_event_types order by event_type_id;"
    )
    .fetch_all(db.pool()?)
    .await?;
    Ok(Err(BadParam(format!(
        "type must be one of {}",
        names.join(", ")
    ))))
}

/// The feed fields of a result: `events` (the page's rows), the page's
/// `limit`, `has_more` and `next_before_id`.
fn page_fields(result: &mut JsObject, rows: Vec<FeedRow>, limit: i64) -> Result<(), ApiError> {
    let (rows, has_more) = split_page(rows, limit);
    let next_before_id = if has_more {
        rows.last().map(|row| row.id as f64)
    } else {
        None
    };
    let events = rows
        .into_iter()
        .map(FeedRow::into_event)
        .collect::<Result<Vec<_>, _>>()?;
    result.set("events", events);
    result.set("limit", limit as f64);
    result.set("has_more", has_more);
    result.set("next_before_id", next_before_id);
    Ok(())
}

impl FeedRow {
    /// The row as a feed event.
    fn into_event(self) -> Result<JsValue, ApiError> {
        Ok(JsValue::from(js_object! {
            "id" => self.id as f64,
            "block_height" => self.block_height,
            "block_time" => self.block_time.map(|time| time as f64),
            "inscription_id" => self.inscription_id,
            "event_type" => self.event_type,
            "ticker_decimals" => self.ticker_decimals,
            "event" => pg_json::text(oid::JSONB, Some(&self.event))?,
        }))
    }
}

/// The wallet a wallet route reads: the pkScript (lowercase hex) of its
/// rows, and for an `address` request the address an event's `*_wallet`
/// field (or a balance row's `wallet`) must name.
pub(super) struct WalletKey {
    pub(super) pkscript: String,
    pub(super) wallet: Option<String>,
}

/// The echoed `address` and `pkscript` of a wallet request, and the key its
/// rows are read by: `None` for an EVM address without a Bitcoin address or
/// an address that encodes no pkScript, which have no rows.
pub(super) async fn wallet_key(
    db: &Db,
    wallet: Wallet,
) -> Result<(Option<String>, Option<String>, Option<WalletKey>), ApiError> {
    Ok(match wallet {
        Wallet::Address(address) if is_evm_address(&address) => {
            let evm_address = address.to_lowercase();
            let mapped = sqlx::query_scalar!(
                "select btc_address from evm_addresses where evm_address = $1;",
                evm_address
            )
            .fetch_optional(db.pool()?)
            .await?;
            // a mapped EVM address answers as its Bitcoin address, an
            // unmapped one (no rows) as the address asked for
            let key = mapped.as_deref().and_then(address_key);
            (Some(mapped.unwrap_or(address)), None, key)
        }
        Wallet::Address(address) => {
            let key = address_key(&address);
            (Some(address), None, key)
        }
        Wallet::PkScript(pkscript) => {
            let key = WalletKey {
                pkscript: pkscript.clone(),
                wallet: None,
            };
            (None, Some(pkscript), Some(key))
        }
    })
}

/// The key of a Bitcoin address: the pkScript it encodes, on any network
/// (the wallet match keeps the database's), with the address itself.
fn address_key(address: &str) -> Option<WalletKey> {
    let parsed = bitcoin::Address::from_str(address).ok()?;
    Some(WalletKey {
        pkscript: parsed.assume_checked().script_pubkey().to_hex_string(),
        wallet: Some(address.to_string()),
    })
}

/// The lowest and the highest id of block `height`'s events, over both
/// event tables, or `None` for a block without events. A block's events
/// take consecutive ids, since the indexer writes blocks in order and a
/// reorg rewinds the id sequences with the blocks.
pub(in crate::routes) async fn block_event_ids(
    db: &Db,
    height: i32,
) -> Result<Option<(i64, i64)>, ApiError> {
    // `id + 0` keeps the planner from walking the primary key for the
    // extremes and filtering on the height, which reads every event below
    // the block; the block_height index reads only the block's rows.
    let row = sqlx::query!(
        r#"select min(u.lo) as "lo?", max(u.hi) as "hi?" from (
                (select min(id + 0) as lo, max(id + 0) as hi from brc20_events where block_height = $1)
                union all
                (select min(id + 0) as lo, max(id + 0) as hi from brc20_light_events where block_height = $1)
            ) u;"#,
        height,
    )
    .fetch_one(db.pool()?)
    .await?;
    Ok(row.lo.zip(row.hi))
}

/// Up to `limit` of block `height`'s events with an id above `after` and at
/// most `last` (see [`block_event_ids`]), oldest first, each with its id, in
/// the feed routes' event form.
pub(in crate::routes) async fn block_events(
    db: &Db,
    height: i32,
    after: i64,
    last: i64,
    limit: i64,
) -> Result<Vec<(i64, JsValue)>, ApiError> {
    // Each table is read through its primary key from the cursor, which the
    // id range keeps inside the block.
    let rows = sqlx::query_as!(
        FeedRow,
        r#"select u.id as "id!", u.block_height as "block_height!", h.block_time as "block_time?",
                u.inscription_id as "inscription_id!",
                (select e.event_type_name from brc20_event_types e
                  where e.event_type_id = u.event_type limit 1) as "event_type?",
                (select k.decimals from brc20_tickers k
                  where k.tick = u.event ->> 'tick') as "ticker_decimals?",
                u.event::text as "event!"
            from (
                (select id, block_height, event_type, inscription_id, event
                 from brc20_events
                 where id > $2 and id <= $3 and block_height = $1
                 order by id limit $4)
                union all
                (select id, block_height, event_type, inscription_id, event
                 from brc20_light_events
                 where id > $2 and id <= $3 and block_height = $1
                 order by id limit $4)
            ) u
            left join brc20_block_hashes h on h.block_height = u.block_height
            order by u.id
            limit $4;"#,
        height,
        after,
        last,
        limit,
    )
    .fetch_all(db.pool()?)
    .await?;
    rows.into_iter()
        .map(|row| Ok((row.id, row.into_event()?)))
        .collect()
}

/// A page of a ticker's events, newest first.
///
/// The events whose event object has `tick` equal to the lowercased ticker,
/// of the `type` and the `category` list when given. The feed pages by key,
/// so pages neither repeat nor skip events as new ones arrive.
#[utoipa::path(
    get,
    path = "/v1/brc20/ticker/events",
    tag = "brc20",
    params(TickerParam, FeedParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<Brc20TickerEvents>),
        (status = 400, description = "A missing or invalid parameter, or a `type` `brc20_event_types` does not list", body = ErrorBody),
        (status = 404, description = "`ticker not found`", body = ErrorBody),
    ),
)]
pub async fn ticker_events(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (tick, request) = match ticker(&query, "ticker")
        .and_then(|tick| Ok((tick, FeedRequest::from_query(&query)?)))
    {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let Some(deploy_decimals) = ticker_decimals(&db, &tick).await? else {
        return Ok(Reply::error(404, "ticker not found"));
    };
    let event_type = match event_type_id(&db, request.event_type.as_deref()).await? {
        Ok(id) => id,
        Err(bad) => return Ok(bad.reply()),
    };
    // Each table is read through its (tick, id) index, newest first, up to
    // one past the page; the union keeps the newest of both. With a single
    // category the planner's custom plan reduces the category filter to the
    // predicate of that category's partial (tick, id) index.
    let rows = sqlx::query_as!(
        FeedRow,
        r#"select u.id as "id!", u.block_height as "block_height!", h.block_time as "block_time?",
                u.inscription_id as "inscription_id!",
                (select e.event_type_name from brc20_event_types e
                  where e.event_type_id = u.event_type limit 1) as "event_type?",
                (select k.decimals from brc20_tickers k
                  where k.tick = u.event ->> 'tick') as "ticker_decimals?",
                u.event::text as "event!"
            from (
                (select id, block_height, event_type, inscription_id, event
                 from brc20_events
                 where event ->> 'tick' = $1
                   and ($2::int4 is null or event_type = $2)
                   and ($5::text[] is null or brc20_event_category(event_type, event) = any($5))
                   and ($3::int8 is null or id < $3)
                 order by id desc limit $4)
                union all
                (select id, block_height, event_type, inscription_id, event
                 from brc20_light_events
                 where event ->> 'tick' = $1
                   and ($2::int4 is null or event_type = $2)
                   and ($5::text[] is null or brc20_event_category(event_type, event) = any($5))
                   and ($3::int8 is null or id < $3)
                 order by id desc limit $4)
            ) u
            left join brc20_block_hashes h on h.block_height = u.block_height
            order by u.id desc
            limit $4;"#,
        tick,
        event_type,
        request.before_id,
        request.limit + 1,
        request.categories.as_deref() as Option<&[String]>,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = js_object! {
        "ticker" => tick,
        "decimals" => AMOUNT_DECIMALS,
        "ticker_decimals" => deploy_decimals,
        "event_type" => request.event_type,
    };
    page_fields(&mut result, rows, request.limit)?;
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// A page of the events a wallet takes part in, newest first.
///
/// The events where the wallet is a party (`deployer`, `minted`, `source`,
/// `spent`, `predeployer`): by its `*_pkScript` field for `pkscript`, by its
/// `*_wallet` field for `address`, of the `type` and the `category` list
/// when given. An unmapped EVM address has no events. The feed pages by key,
/// as the ticker feed does.
#[utoipa::path(
    get,
    path = "/v1/brc20/wallet/events",
    tag = "brc20",
    params(WalletParams, FeedParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<Brc20WalletEvents>),
        (status = 400, description = "A missing or invalid parameter, both or neither of `address` and `pkscript`, or a `type` `brc20_event_types` does not list", body = ErrorBody),
    ),
)]
pub async fn wallet_events(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (wallet, request) = match Wallet::from_query(&query)
        .and_then(|wallet| Ok((wallet, FeedRequest::from_query(&query)?)))
    {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let event_type = match event_type_id(&db, request.event_type.as_deref()).await? {
        Ok(id) => id,
        Err(bad) => return Ok(bad.reply()),
    };
    let (address, pkscript, key) = wallet_key(&db, wallet).await?;
    let rows = match key {
        None => Vec::new(),
        // Each table is read through brc20_event_parties' (pkscript,
        // event_id) key backwards from the cursor, joined to the events by
        // id, up to one past the page; the filters apply to the joined rows.
        Some(key) => sqlx::query_as!(
            FeedRow,
            r#"select u.id as "id!", u.block_height as "block_height!", h.block_time as "block_time?",
                u.inscription_id as "inscription_id!",
                (select e.event_type_name from brc20_event_types e
                  where e.event_type_id = u.event_type limit 1) as "event_type?",
                (select k.decimals from brc20_tickers k
                  where k.tick = u.event ->> 'tick') as "ticker_decimals?",
                u.event::text as "event!"
            from (
                (select e.id, e.block_height, e.event_type, e.inscription_id, e.event
                 from brc20_event_parties p
                 join brc20_events e on e.id = p.event_id
                 where p.pkscript = decode($1, 'hex')
                   and ($3::int8 is null or p.event_id < $3)
                   and ($2::int4 is null or e.event_type = $2)
                   and ($5::text[] is null or brc20_event_category(e.event_type, e.event) = any($5))
                   and ($6::text is null or $6 in (e.event ->> 'deployer_wallet', e.event ->> 'minted_wallet',
                        e.event ->> 'source_wallet', e.event ->> 'spent_wallet', e.event ->> 'predeployer_wallet'))
                 order by p.event_id desc limit $4)
                union all
                (select e.id, e.block_height, e.event_type, e.inscription_id, e.event
                 from brc20_event_parties p
                 join brc20_light_events e on e.id = p.event_id
                 where p.pkscript = decode($1, 'hex')
                   and ($3::int8 is null or p.event_id < $3)
                   and ($2::int4 is null or e.event_type = $2)
                   and ($5::text[] is null or brc20_event_category(e.event_type, e.event) = any($5))
                   and ($6::text is null or $6 in (e.event ->> 'deployer_wallet', e.event ->> 'minted_wallet',
                        e.event ->> 'source_wallet', e.event ->> 'spent_wallet', e.event ->> 'predeployer_wallet'))
                 order by p.event_id desc limit $4)
            ) u
            left join brc20_block_hashes h on h.block_height = u.block_height
            order by u.id desc
            limit $4;"#,
            key.pkscript,
            event_type,
            request.before_id,
            request.limit + 1,
            request.categories.as_deref() as Option<&[String]>,
            key.wallet,
        )
        .fetch_all(db.pool()?)
        .await?,
    };
    let mut result = js_object! {
        "address" => address,
        "pkscript" => pkscript,
        "decimals" => AMOUNT_DECIMALS,
        "event_type" => request.event_type,
    };
    page_fields(&mut result, rows, request.limit)?;
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// A page of the network's events of some categories, newest first.
///
/// Every deploy and predeploy inscription without a `category` parameter;
/// with one, the events of the listed categories (every wrap, say), of the
/// `type` when given. The feed pages by key, as the ticker feed does.
#[utoipa::path(
    get,
    path = "/v1/brc20/deploys",
    tag = "brc20",
    params(FeedParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<Brc20CategoryEvents>),
        (status = 400, description = "A missing or invalid parameter, or a `type` `brc20_event_types` does not list", body = ErrorBody),
    ),
)]
pub async fn deploys(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let request = match FeedRequest::from_query(&query) {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let event_type = match event_type_id(&db, request.event_type.as_deref()).await? {
        Ok(id) => id,
        Err(bad) => return Ok(bad.reply()),
    };
    let categories = request
        .categories
        .clone()
        .unwrap_or_else(|| vec![CATEGORIES[0].to_string()]);
    // Each table is read through the (id) index of the category, which a
    // single category's custom plan selects (`deploy`, `wrap`, `unwrap` and
    // `brc20prog` have one), newest first up to one past the page.
    let rows = sqlx::query_as!(
        FeedRow,
        r#"select u.id as "id!", u.block_height as "block_height!", h.block_time as "block_time?",
                u.inscription_id as "inscription_id!",
                (select e.event_type_name from brc20_event_types e
                  where e.event_type_id = u.event_type limit 1) as "event_type?",
                (select k.decimals from brc20_tickers k
                  where k.tick = u.event ->> 'tick') as "ticker_decimals?",
                u.event::text as "event!"
            from (
                (select id, block_height, event_type, inscription_id, event
                 from brc20_events
                 where brc20_event_category(event_type, event) = any($1)
                   and ($2::int4 is null or event_type = $2)
                   and ($3::int8 is null or id < $3)
                 order by id desc limit $4)
                union all
                (select id, block_height, event_type, inscription_id, event
                 from brc20_light_events
                 where brc20_event_category(event_type, event) = any($1)
                   and ($2::int4 is null or event_type = $2)
                   and ($3::int8 is null or id < $3)
                 order by id desc limit $4)
            ) u
            left join brc20_block_hashes h on h.block_height = u.block_height
            order by u.id desc
            limit $4;"#,
        &categories,
        event_type,
        request.before_id,
        request.limit + 1,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = js_object! {
        "category" => categories.into_iter().map(JsValue::from).collect::<Vec<_>>(),
        "decimals" => AMOUNT_DECIMALS,
        "event_type" => request.event_type,
    };
    page_fields(&mut result, rows, request.limit)?;
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}
