//! The event feeds: `/v1/brc20/ticker/events` and `/v1/brc20/wallet/events`.
//!
//! Both list the rows of `brc20_events` (and `brc20_light_events`, which the
//! indexer fills instead of it in light mode), newest first, and answer
//! `{"error":null,"result":...}`. A missing or invalid parameter answers
//! `400 {"error":"<reason>","result":null}` and an unknown ticker
//! `404 {"error":"ticker not found","result":null}`.
//!
//! An event is
//! `{"id","block_height","block_time","inscription_id","event_type","ticker_decimals","event"}`:
//! `id` orders the feed, `block_time` is the block's time in seconds (null
//! while the block's time is unknown, as for rows above the journal tip at
//! the indexer's start), `event_type` is the name from `brc20_event_types`
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
//! event when new events arrive, and a deep page costs what the first costs
//! (an offset would walk and discard every row before it). `has_more` tells
//! whether an older event exists and `next_before_id` is the id to ask with
//! (null on the last page). `type` keeps one event type, by its name.
//!
//! A ticker's events are those whose event object has `tick` equal to the
//! lowercased ticker. A wallet's events are those where the wallet is any
//! party of the event: a `*_pkScript` or `*_wallet` field of the object
//! (`deployer`, `minted`, `source`, `spent`, `predeployer`). `address` is a
//! Bitcoin address, or an EVM address whose Bitcoin address comes from
//! `evm_addresses` (an unmapped one has no events), and `pkscript` is hex.

use {
    super::{
        block_height_of_db,
        tickers::{
            AMOUNT_DECIMALS, BadParam, DEFAULT_LIMIT, MAX_LIMIT, integer, is_evm_address, single,
            ticker, ticker_decimals,
        },
    },
    crate::{
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        pg_json::{self, oid},
        reply::{ApiError, Reply},
    },
    axum::extract::State,
};

#[cfg(test)]
mod tests;

/// The largest `before_id` a request may name.
pub const MAX_BEFORE_ID: i64 = 999_999_999_999_999_999;

/// The longest `address` a request may name.
pub const MAX_ADDRESS_LEN: usize = 128;

/// The longest `pkscript` a request may name, in hex digits.
pub const MAX_PKSCRIPT_LEN: usize = 20_000;

/// The paging and type filter of a feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRequest {
    pub limit: i64,
    pub before_id: Option<i64>,
    /// The `type` parameter, a name of `brc20_event_types`.
    pub event_type: Option<String>,
}

impl FeedRequest {
    pub fn from_query(query: &JsObject) -> Result<FeedRequest, BadParam> {
        let before_id = match single(query, "before_id")? {
            None => None,
            Some(_) => Some(integer(query, "before_id", 0, 1, MAX_BEFORE_ID)?),
        };
        let event_type = match single(query, "type")? {
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

/// The wallet a feed lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wallet {
    Address(String),
    PkScript(String),
}

impl Wallet {
    /// Exactly one of `address` and `pkscript`; a pkScript is lowercased hex.
    pub fn from_query(query: &JsObject) -> Result<Wallet, BadParam> {
        let address = single(query, "address")?.filter(|value| !value.is_empty());
        let pkscript = single(query, "pkscript")?.filter(|value| !value.is_empty());
        match (address, pkscript) {
            (None, None) => Err(BadParam("address or pkscript is required".into())),
            (Some(_), Some(_)) => Err(BadParam("give address or pkscript, not both".into())),
            (Some(address), None) => {
                if address.len() > MAX_ADDRESS_LEN {
                    return Err(BadParam(format!(
                        "address must be at most {MAX_ADDRESS_LEN} characters"
                    )));
                }
                Ok(Wallet::Address(address.to_string()))
            }
            (None, Some(pkscript)) => {
                if pkscript.len() > MAX_PKSCRIPT_LEN
                    || pkscript.len() % 2 != 0
                    || !pkscript.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(BadParam(format!(
                        "pkscript must be hex of at most {MAX_PKSCRIPT_LEN} digits"
                    )));
                }
                Ok(Wallet::PkScript(pkscript.to_ascii_lowercase()))
            }
        }
    }
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
fn page_fields(result: &mut JsObject, mut rows: Vec<FeedRow>, limit: i64) -> Result<(), ApiError> {
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_before_id = if has_more {
        rows.last().map(|row| row.id as f64)
    } else {
        None
    };
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        events.push(JsValue::from(js_object! {
            "id" => row.id as f64,
            "block_height" => row.block_height,
            "block_time" => row.block_time.map(|time| time as f64),
            "inscription_id" => row.inscription_id,
            "event_type" => row.event_type,
            "ticker_decimals" => row.ticker_decimals,
            "event" => pg_json::text(oid::JSONB, Some(&row.event))?,
        }));
    }
    result.set("events", events);
    result.set("limit", limit as f64);
    result.set("has_more", has_more);
    result.set("next_before_id", next_before_id);
    Ok(())
}

/// `/v1/brc20/ticker/events?ticker=&type=&before_id=&limit=`: the events of
/// a ticker, newest first.
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
    // one past the page; the union keeps the newest of both.
    let rows = sqlx::query!(
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
                   and ($3::int8 is null or id < $3)
                 order by id desc limit $4)
                union all
                (select id, block_height, event_type, inscription_id, event
                 from brc20_light_events
                 where event ->> 'tick' = $1
                   and ($2::int4 is null or event_type = $2)
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
    )
    .fetch_all(db.pool()?)
    .await?;
    let rows = rows
        .into_iter()
        .map(|row| FeedRow {
            id: row.id,
            block_height: row.block_height,
            block_time: row.block_time,
            inscription_id: row.inscription_id,
            event_type: row.event_type,
            ticker_decimals: row.ticker_decimals,
            event: row.event,
        })
        .collect();
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

/// `/v1/brc20/wallet/events?address=|pkscript=&type=&before_id=&limit=`: the
/// events a wallet takes part in, newest first.
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
    // The key every party field of an event is matched with: a Bitcoin
    // address (an EVM address's mapped one) or the pkScript.
    let (address, pkscript, key) = match wallet {
        Wallet::Address(address) if is_evm_address(&address) => {
            let evm_address = address.to_lowercase();
            let mapped = sqlx::query_scalar!(
                "select btc_address from evm_addresses where evm_address = $1;",
                evm_address
            )
            .fetch_optional(db.pool()?)
            .await?;
            let key = mapped.clone();
            (mapped, None, key)
        }
        Wallet::Address(address) => (Some(address.clone()), None, Some(address)),
        Wallet::PkScript(pkscript) => (None, Some(pkscript.clone()), Some(pkscript)),
    };
    let rows = match key {
        None => Vec::new(),
        // Each table is read through its GIN index on the parties array: the
        // matching rows come from a bitmap scan, newest first up to one
        // past the page. The array's spelling is the indexes' (opi-brc20's
        // `event_feed_indexes`), which a unit test compares. Ordering by
        // `id + 0` keeps the planner from walking the primary key and
        // filtering: it estimates a wallet's matches from a default
        // selectivity and so takes that walk for a rare wallet, reading the
        // whole table, while the index serves every wallet in time
        // proportional to its own events.
        Some(key) => sqlx::query!(
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
                 where array_remove(ARRAY[nullif(("event" ->> 'deployer_pkScript'), ''), nullif(("event" ->> 'minted_pkScript'), ''), nullif(("event" ->> 'source_pkScript'), ''), nullif(("event" ->> 'spent_pkScript'), ''), nullif(("event" ->> 'predeployer_pkScript'), ''), nullif(("event" ->> 'deployer_wallet'), ''), nullif(("event" ->> 'minted_wallet'), ''), nullif(("event" ->> 'source_wallet'), ''), nullif(("event" ->> 'spent_wallet'), ''), nullif(("event" ->> 'predeployer_wallet'), '')], NULL) && ARRAY[$1::text]
                   and ($2::int4 is null or event_type = $2)
                   and ($3::int8 is null or id < $3)
                 order by id + 0 desc limit $4)
                union all
                (select id, block_height, event_type, inscription_id, event
                 from brc20_light_events
                 where array_remove(ARRAY[nullif(("event" ->> 'deployer_pkScript'), ''), nullif(("event" ->> 'minted_pkScript'), ''), nullif(("event" ->> 'source_pkScript'), ''), nullif(("event" ->> 'spent_pkScript'), ''), nullif(("event" ->> 'predeployer_pkScript'), ''), nullif(("event" ->> 'deployer_wallet'), ''), nullif(("event" ->> 'minted_wallet'), ''), nullif(("event" ->> 'source_wallet'), ''), nullif(("event" ->> 'spent_wallet'), ''), nullif(("event" ->> 'predeployer_wallet'), '')], NULL) && ARRAY[$1::text]
                   and ($2::int4 is null or event_type = $2)
                   and ($3::int8 is null or id < $3)
                 order by id + 0 desc limit $4)
            ) u
            left join brc20_block_hashes h on h.block_height = u.block_height
            order by u.id desc
            limit $4;"#,
            key,
            event_type,
            request.before_id,
            request.limit + 1,
        )
        .fetch_all(db.pool()?)
        .await?
        .into_iter()
        .map(|row| FeedRow {
            id: row.id,
            block_height: row.block_height,
            block_time: row.block_time,
            inscription_id: row.inscription_id,
            event_type: row.event_type,
            ticker_decimals: row.ticker_decimals,
            event: row.event,
        })
        .collect(),
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
