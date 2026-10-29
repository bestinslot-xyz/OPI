//! The runes event feeds: `/v1/runes/rune/events` and
//! `/v1/runes/wallet/events`.
//!
//! Both list rows of `runes_events`, newest first, and answer
//! `{"error":null,"result":...}`. A missing or invalid parameter answers
//! `400 {"error":"<reason>","result":null}` and an unknown rune
//! `404 {"error":"rune not found","result":null}`.
//!
//! An event is `{"id","block_height","block_time","event_type","txid",
//! "outpoint","pkscript","address","rune_id","rune_name","spaced_rune_name",
//! "divisibility","amount"}`: `id` orders the feed, `block_time` is the
//! block's time in seconds from `runes_block_hashes` (null for a height
//! pruning removed), `event_type` is the name from `runes_event_types` and
//! `amount` is a raw integer as a string with `divisibility` fractional
//! digits. `outpoint`, `pkscript` and `address` are set for `input` and
//! `output` events and null otherwise.
//!
//! The feeds page by key, as the brc20 feeds do: `limit` (default 100, at
//! most 500) and `before_id`, the `next_before_id` of the previous page. A
//! page holds the events with an id below `before_id`, so pages never
//! repeat or skip an event as new ones arrive. Each feed reads a
//! `(column, id)` index backwards from the cursor, so a page costs the same
//! at any depth. A reorg rewinds the ids with the blocks, so a cursor taken
//! above a rewound tip can skip or repeat events of the rewound range.
//!
//! A rune's events are those of its id. A wallet's events are those whose
//! outpoint the wallet owns: its `input` and `output` events, by `address`
//! or by `pkscript` (hex).

use {
    super::{
        RunesTables, block_height_of_db,
        catalog::{RuneParam, WalletParams, find_rune, rune_param},
    },
    crate::{
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        openapi::{Answer, DbHeight, ErrorBody, LimitParam},
        reply::{ApiError, Reply},
        routes::paging::{
            BadParam, DEFAULT_LIMIT, MAX_BEFORE_ID, MAX_LIMIT, MAX_TYPE_LEN, Wallet, integer,
            optional_text, single, split_page,
        },
    },
    axum::extract::State,
    opi_runes::names::spaced_name,
    utoipa::{IntoParams, ToSchema},
};

/// The paging and type filter of a feed.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct FeedParams {
    /// Keeps one event type, by its `runes_event_types` name (`input`,
    /// `new-allocation`, `mint`, `output`, `burn`).
    #[param(max_length = 256)]
    r#type: Option<String>,
    /// The previous page's `next_before_id`: the page holds the events
    /// with a lower id.
    #[param(minimum = 1, maximum = 999999999999999999_i64)]
    before_id: Option<i64>,
}

/// An event of a runes feed.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesFeedEvent {
    /// The event's id, which orders the feed.
    id: i64,
    block_height: i32,
    /// The block's time in seconds; null for a height pruning removed.
    #[schema(nullable)]
    block_time: i64,
    /// The event type's name.
    #[schema(nullable)]
    event_type: String,
    txid: String,
    /// The outpoint of an `input` or `output` event, null otherwise.
    #[schema(nullable)]
    outpoint: String,
    /// The outpoint's pkScript, null without an outpoint.
    #[schema(nullable)]
    pkscript: String,
    /// The pkScript's address, null without one.
    #[schema(nullable)]
    address: String,
    rune_id: String,
    #[schema(nullable)]
    rune_name: String,
    #[schema(nullable)]
    spaced_rune_name: String,
    /// The fractional digits of `amount`.
    #[schema(nullable)]
    divisibility: i32,
    amount: String,
}

/// A page of a rune's events.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesRuneEvents {
    rune_id: String,
    rune_name: String,
    spaced_rune_name: String,
    divisibility: i32,
    /// The `type` filter, null without one.
    #[schema(nullable)]
    event_type: String,
    /// Newest first.
    events: Vec<RunesFeedEvent>,
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
struct RunesWalletEvents {
    /// The address asked for; null for a `pkscript` request.
    #[schema(nullable)]
    address: String,
    /// The lowercase pkScript asked for; null for an `address` request.
    #[schema(nullable)]
    pkscript: String,
    /// The `type` filter, null without one.
    #[schema(nullable)]
    event_type: String,
    /// Newest first.
    events: Vec<RunesFeedEvent>,
    limit: i64,
    /// Whether older events exist.
    has_more: bool,
    /// The `before_id` of the next page, null on the last page.
    #[schema(nullable)]
    next_before_id: i64,
    block_height: DbHeight,
}

/// The paging and type filter of a feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRequest {
    pub limit: i64,
    pub before_id: Option<i64>,
    /// The `type` parameter, a name of `runes_event_types`.
    pub event_type: Option<String>,
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
        })
    }
}

/// A feed row.
struct FeedRow {
    id: i64,
    block_height: i32,
    block_time: Option<i64>,
    event_type: Option<String>,
    txid: String,
    outpoint: Option<String>,
    pkscript: Option<String>,
    wallet_addr: Option<String>,
    rune_id: String,
    rune_name: Option<String>,
    spacers: Option<i64>,
    divisibility: Option<i32>,
    amount: String,
}

/// The `type` filter as an event type id: `None` for no filter, `Err` for a
/// name `runes_event_types` does not list.
async fn event_type_id(
    db: &Db,
    name: Option<&str>,
) -> Result<Result<Option<i32>, BadParam>, ApiError> {
    let Some(name) = name else {
        return Ok(Ok(None));
    };
    let id = sqlx::query_scalar!(
        "select event_type_id from runes_event_types where event_type_name = $1 limit 1;",
        name
    )
    .fetch_optional(db.pool()?)
    .await?;
    if let Some(id) = id {
        return Ok(Ok(Some(id)));
    }
    let names = sqlx::query_scalar!(
        "select event_type_name from runes_event_types order by event_type_id;"
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
fn page_fields(result: &mut JsObject, rows: Vec<FeedRow>, limit: i64) {
    let (rows, has_more) = split_page(rows, limit);
    let next_before_id = if has_more {
        rows.last().map(|row| row.id as f64)
    } else {
        None
    };
    let events: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            let spaced = row
                .rune_name
                .as_deref()
                .map(|name| spaced_name(name, row.spacers.unwrap_or(0)));
            js_object! {
                "id" => row.id as f64,
                "block_height" => row.block_height,
                "block_time" => row.block_time.map(|time| time as f64),
                "event_type" => row.event_type,
                "txid" => row.txid,
                "outpoint" => row.outpoint,
                "pkscript" => row.pkscript,
                "address" => row.wallet_addr,
                "rune_id" => row.rune_id,
                "rune_name" => row.rune_name,
                "spaced_rune_name" => spaced,
                "divisibility" => row.divisibility,
                "amount" => row.amount,
            }
            .into()
        })
        .collect();
    result.set("events", events);
    result.set("limit", limit as f64);
    result.set("has_more", has_more);
    result.set("next_before_id", next_before_id);
}

/// A page of a rune's events, newest first.
///
/// The feed pages by key, so pages neither repeat nor skip events as new
/// ones arrive.
#[utoipa::path(
    get,
    path = "/v1/runes/rune/events",
    tag = "runes",
    params(RuneParam, FeedParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<RunesRuneEvents>),
        (status = 400, description = "A missing or invalid parameter, or a `type` `runes_event_types` does not list", body = ErrorBody),
        (status = 404, description = "`rune not found`", body = ErrorBody),
        (status = 503, description = "`the runes API tables are not built yet; start opi-indexer run once`: the runes module has not built its tables and indexes in this database", body = ErrorBody),
    ),
)]
pub async fn rune_events(
    State(tables): State<RunesTables>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (rune, request) =
        match rune_param(&query).and_then(|rune| Ok((rune, FeedRequest::from_query(&query)?))) {
            Ok(request) => request,
            Err(bad) => return Ok(bad.reply()),
        };
    if let Some(reply) = tables.unbuilt().await? {
        return Ok(reply);
    }
    let db = tables.db.clone();
    let current_block_height = block_height_of_db(&db).await;
    let Some(rune) = find_rune(&db, &rune).await? else {
        return Ok(Reply::error(404, "rune not found"));
    };
    let event_type = match event_type_id(&db, request.event_type.as_deref()).await? {
        Ok(id) => id,
        Err(bad) => return Ok(bad.reply()),
    };
    // runes_events_rune_id_id_idx, read backwards from the cursor up to one
    // past the page. The rune is bounded as a (rune_id, id) range and the
    // order names rune_id: with `rune_id = $1` the order's rune_id is
    // redundant, so the planner may walk the primary key backwards instead,
    // which reads every newer event of every rune before an old rune's first.
    let rows = sqlx::query_as!(
        FeedRow,
        r#"select r.id, r.block_height, extract(epoch from h.block_time)::int8 as "block_time?",
                (select t.event_type_name from runes_event_types t
                  where t.event_type_id = r.event_type limit 1) as "event_type?",
                r.txid, r.outpoint, r.pkscript, r.wallet_addr, r.rune_id,
                e.rune_name as "rune_name?", e.spacers as "spacers?",
                e.divisibility as "divisibility?", r.amount::text as "amount!"
            from (
                select * from runes_events
                where (rune_id, id) < ($1, $3) and (rune_id, id) > ($1, -1)
                  and ($2::int4 is null or event_type = $2)
                order by rune_id desc, id desc limit $4
            ) r
            left join runes_block_hashes h on h.block_height = r.block_height
            left join runes_id_to_entry e on e.rune_id = r.rune_id
            order by r.id desc;"#,
        rune.rune_id,
        event_type,
        request.before_id.unwrap_or(i64::MAX),
        request.limit + 1,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = js_object! {
        "rune_id" => rune.rune_id,
        "rune_name" => rune.rune_name.clone(),
        "spaced_rune_name" => spaced_name(&rune.rune_name, rune.spacers),
        "divisibility" => rune.divisibility,
        "event_type" => request.event_type,
    };
    page_fields(&mut result, rows, request.limit);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// A page of a wallet's runes events, newest first.
///
/// The `input` and `output` events of the outpoints the wallet owns, named
/// by `address` or `pkscript`. The feed pages by key, as the rune feed
/// does.
#[utoipa::path(
    get,
    path = "/v1/runes/wallet/events",
    tag = "runes",
    params(WalletParams, FeedParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<RunesWalletEvents>),
        (status = 400, description = "A missing or invalid parameter, both or neither of `address` and `pkscript`, or a `type` `runes_event_types` does not list", body = ErrorBody),
        (status = 503, description = "`the runes API tables are not built yet; start opi-indexer run once`: the runes module has not built its tables and indexes in this database", body = ErrorBody),
    ),
)]
pub async fn wallet_events(
    State(tables): State<RunesTables>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (wallet, request) = match Wallet::from_query(&query)
        .and_then(|wallet| Ok((wallet, FeedRequest::from_query(&query)?)))
    {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    if let Some(reply) = tables.unbuilt().await? {
        return Ok(reply);
    }
    let db = tables.db.clone();
    let current_block_height = block_height_of_db(&db).await;
    let event_type = match event_type_id(&db, request.event_type.as_deref()).await? {
        Ok(id) => id,
        Err(bad) => return Ok(bad.reply()),
    };
    let before_id = request.before_id.unwrap_or(i64::MAX);
    let fetch = request.limit + 1;
    // runes_events_wallet_addr_id_idx or runes_events_pkscript_id_idx, read
    // backwards from the cursor, bounded as a (column, id) range as the rune
    // feed is.
    let (address, pkscript, rows) = match wallet {
        Wallet::Address(address) => {
            let rows = sqlx::query_as!(
                FeedRow,
                r#"select r.id, r.block_height, extract(epoch from h.block_time)::int8 as "block_time?",
                    (select t.event_type_name from runes_event_types t
                      where t.event_type_id = r.event_type limit 1) as "event_type?",
                    r.txid, r.outpoint, r.pkscript, r.wallet_addr, r.rune_id,
                    e.rune_name as "rune_name?", e.spacers as "spacers?",
                    e.divisibility as "divisibility?", r.amount::text as "amount!"
                from (
                    select * from runes_events
                    where (wallet_addr, id) < ($1, $3) and (wallet_addr, id) > ($1, -1)
                      and ($2::int4 is null or event_type = $2)
                    order by wallet_addr desc, id desc limit $4
                ) r
                left join runes_block_hashes h on h.block_height = r.block_height
                left join runes_id_to_entry e on e.rune_id = r.rune_id
                order by r.id desc;"#,
                address,
                event_type,
                before_id,
                fetch,
            )
            .fetch_all(db.pool()?)
            .await?;
            (Some(address), None, rows)
        }
        Wallet::PkScript(pkscript) => {
            let rows = sqlx::query_as!(
                FeedRow,
                r#"select r.id, r.block_height, extract(epoch from h.block_time)::int8 as "block_time?",
                    (select t.event_type_name from runes_event_types t
                      where t.event_type_id = r.event_type limit 1) as "event_type?",
                    r.txid, r.outpoint, r.pkscript, r.wallet_addr, r.rune_id,
                    e.rune_name as "rune_name?", e.spacers as "spacers?",
                    e.divisibility as "divisibility?", r.amount::text as "amount!"
                from (
                    select * from runes_events
                    where (pkscript, id) < ($1, $3) and (pkscript, id) > ($1, -1)
                      and ($2::int4 is null or event_type = $2)
                    order by pkscript desc, id desc limit $4
                ) r
                left join runes_block_hashes h on h.block_height = r.block_height
                left join runes_id_to_entry e on e.rune_id = r.rune_id
                order by r.id desc;"#,
                pkscript,
                event_type,
                before_id,
                fetch,
            )
            .fetch_all(db.pool()?)
            .await?;
            (None, Some(pkscript), rows)
        }
    };
    let mut result = js_object! {
        "address" => address,
        "pkscript" => pkscript,
        "event_type" => request.event_type,
    };
    page_fields(&mut result, rows, request.limit);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}
