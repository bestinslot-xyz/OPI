//! The brc20 module's routes, including the
//! `/v1/brc20/explorer/...` routes for the BRC2.0 blockscout frontend and the
//! ticker routes of [`tickers`], the holders snapshot of [`snapshot`] and the
//! event feeds of [`events`], which have no JavaScript counterpart.
//!
//! Each other handler follows its JavaScript counterpart line by line: query
//! params keep their JavaScript values ([`JsValue`]) through the checks and
//! coercions the route applies, and become typed SQL params (with
//! Postgres's input rules, see [`params`]) where the route runs its query.
//! A result column that node-pg gives as text (numeric, jsonb) is cast to
//! text in SQL under the column's own name and decoded with
//! [`pg_json::text`].

use {
    super::ModuleRoutes,
    super::{no_row, tip_cache::TipCache},
    crate::{
        config::Module,
        db::Db,
        extract::{ClientIp, ExpressParams, JsQuery},
        js::{JsError, JsObject, JsValue, compare_strings, sort_by_number},
        js_object,
        openapi::{
            Answer, DbHeight, ErrorBody, HashBlockHeight, ListBlockHeight, Null, NumericText,
            ParityTicker, QueryEcho,
        },
        params,
        pg_json::{self, ToJs, oid},
        reply::{ApiError, Reply},
    },
    axum::extract::State,
    futures::future::BoxFuture,
    sha2::{Digest, Sha256},
    sqlx::{PgConnection, PgExecutor},
    utoipa::ToSchema,
    utoipa_axum::{router::UtoipaMethodRouterExt, routes},
};

pub(super) mod events;
pub(super) mod snapshot;
mod tickers;

#[cfg(test)]
mod tests;

const EVENT_SEPARATOR: &str = "|";

/// The explorer's page size (EXPLORER_PAGE_SIZE).
const EXPLORER_PAGE_SIZE: i64 = 50;

pub fn routes(db: &Db, settings: &super::RouteSettings) -> ModuleRoutes {
    ModuleRoutes::new()
        .routes(routes!(ip))
        .routes(routes!(db_version))
        .routes(routes!(event_hash_version))
        .routes(routes!(extras_block_height))
        .routes(routes!(block_height))
        .routes(routes!(balance_on_block))
        .routes(routes!(activity_on_block))
        .routes(routes!(bitcoin_rpc_results_on_block))
        .routes(routes!(get_current_balance_of_wallet))
        .routes(routes!(get_valid_tx_notes_of_wallet))
        .routes(routes!(get_valid_tx_notes_of_ticker))
        .routes(routes!(holders))
        .routes(routes!(get_hash_of_all_activity))
        .routes(
            routes!(get_hash_of_all_current_balances).with_state(TipCache::new(
                db.clone(),
                Module::Brc20,
                current_balances_answer,
            )),
        )
        .routes(routes!(event))
        .routes(routes!(explorer_btc_address))
        .routes(routes!(explorer_balances))
        .routes(routes!(explorer_address_tabs_counters))
        .routes(routes!(explorer_tokens))
        .routes(routes!(tickers::ticker_info))
        .routes(routes!(tickers::ticker_by_contract))
        .routes(routes!(tickers::tickers))
        .routes(routes!(tickers::holders))
        .routes(
            routes!(snapshot::snapshot).with_state(snapshot::Snapshots::new(
                db.clone(),
                settings.snapshot_timeout,
            )),
        )
        .routes(routes!(tickers::wallet_balances))
        .routes(routes!(events::ticker_events))
        .routes(routes!(events::wallet_events))
}

/// A brc20 event as the block and inscription routes list it: the stored
/// event object (its fields depend on the event type), with `event_type`
/// and `inscription_id` set on it.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20StoredEvent {
    /// The event type's name; absent for a type id `brc20_event_types`
    /// does not list.
    #[schema(required = false)]
    event_type: String,
    inscription_id: String,
}

/// A BRC-20 balance: overall and available amounts.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20Balance {
    overall_balance: NumericText,
    available_balance: NumericText,
}

/// A BRC-20 balance with the database's height.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20CurrentBalance {
    overall_balance: NumericText,
    available_balance: NumericText,
    block_height: DbHeight,
}

/// A cached bitcoin RPC call.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20RpcResult {
    method: String,
    /// The request's stored JSON.
    request: serde_json::Value,
    /// The response's stored JSON.
    response: serde_json::Value,
}

/// An unused transfer inscription of a wallet. `brc20_unused_tx_inscrs` has
/// no DDL in this repository, so every field may be null.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20WalletUnusedTx {
    #[schema(nullable)]
    tick: String,
    #[schema(nullable)]
    inscription_id: String,
    #[schema(nullable)]
    amount: NumericText,
    #[schema(nullable)]
    genesis_height: i32,
}

/// A wallet's unused transfer inscriptions.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20WalletUnusedTxes {
    unused_txes: Vec<Brc20WalletUnusedTx>,
    block_height: DbHeight,
}

/// An unused transfer inscription of a ticker. Every field may be null, as
/// for a wallet's.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20TickerUnusedTx {
    #[schema(nullable)]
    current_holder_pkscript: String,
    #[schema(nullable)]
    current_holder_wallet: String,
    #[schema(nullable)]
    inscription_id: String,
    #[schema(nullable)]
    amount: NumericText,
    #[schema(nullable)]
    genesis_height: i32,
}

/// A ticker's unused transfer inscriptions.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20TickerUnusedTxes {
    unused_txes: Vec<Brc20TickerUnusedTx>,
    block_height: DbHeight,
}

/// A holder's current balance.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20Holder {
    pkscript: String,
    #[schema(nullable)]
    wallet: String,
    overall_balance: NumericText,
    available_balance: NumericText,
}

/// A ticker's holders, under the key `unused_txes`.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20Holders {
    /// The holders with a nonzero balance, largest first.
    unused_txes: Vec<Brc20Holder>,
    block_height: DbHeight,
}

/// A block's event hashes.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ActivityHash {
    cumulative_event_hash: String,
    block_event_hash: String,
    cumulative_trace_hash: String,
    block_trace_hash: String,
    indexer_version: String,
    /// The `block_height` parameter as given.
    block_height: QueryEcho,
}

/// The hash of every current balance.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20BalancesHash {
    /// The sha256 hex of every nonzero balance as
    /// `pkscript;tick;overall;available`, in pkscript then tick order,
    /// joined by `|`.
    current_balances_hash: String,
    indexer_version: String,
    block_height: DbHeight,
}

/// The BTC address of an EVM address.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ExplorerBtcAddress {
    /// Null for an EVM address `evm_addresses` does not map.
    #[schema(nullable)]
    btc_address: String,
}

/// A token balance as the explorer shows it.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ExplorerBalance {
    /// The ticker.
    external_token_name: String,
    /// The overall balance.
    external_value: NumericText,
    /// The ticker's deploy precision as text, `"null"` for a ticker
    /// `brc20_tickers` does not list.
    external_decimals: String,
}

/// An EVM address's token balances.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ExplorerBalances {
    /// Largest first; empty for an unmapped EVM address.
    items: Vec<Brc20ExplorerBalance>,
}

/// The explorer's address tab counters; only `token_balances_count` counts.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ExplorerCounters {
    #[schema(minimum = 0, maximum = 0)]
    internal_transactions_count: i64,
    #[schema(minimum = 0, maximum = 0)]
    logs_count: i64,
    /// The tickers with a positive balance.
    token_balances_count: i64,
    #[schema(minimum = 0, maximum = 0)]
    token_transfers_count: i64,
    #[schema(minimum = 0, maximum = 0)]
    transactions_count: i64,
    #[schema(minimum = 0, maximum = 0)]
    validations_count: i64,
    #[schema(minimum = 0, maximum = 0)]
    withdrawals_count: i64,
}

/// A ticker as the explorer's token list shows it.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ExplorerToken {
    /// Always empty.
    address: String,
    /// Always `ERC-20`.
    r#type: String,
    symbol: String,
    name: String,
    /// The ticker's deploy precision as text.
    decimals: String,
    /// The holders with a positive balance, as text.
    holders: String,
    exchange_rate: Null,
    /// The minted supply as text.
    total_supply: String,
    icon_url: Null,
    circulating_market_cap: Null,
}

/// The parameters of the explorer's next token page.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ExplorerNextPage {
    /// The page's last holder count, as text.
    #[schema(nullable)]
    holder_count: String,
    /// The next page's offset.
    items_count: f64,
    /// The page's last ticker.
    name: String,
    market_cap: Null,
}

/// A page of the explorer's token list.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ExplorerTokens {
    items: Vec<Brc20ExplorerToken>,
    /// Null on the last page.
    #[schema(nullable)]
    next_page_params: Brc20ExplorerNextPage,
}

/// get_block_height_of_db: the latest indexed height (a number, or null for
/// an empty table), or -1 when the query fails.
async fn block_height_of_db(db: &Db) -> JsValue {
    db.latest_block_height(Module::Brc20)
        .await
        .unwrap_or_else(|err| {
            tracing::error!("brc20 block height: {err:#}");
            JsValue::from(-1)
        })
}

/// get_extras_block_height_of_db: as [`block_height_of_db`], for
/// brc20_extras_block_hashes.
async fn extras_block_height_of_db(db: &Db) -> JsValue {
    let height = async {
        let height: Option<i32> = sqlx::query_scalar!(
            "SELECT max(block_height) as max_block_height FROM brc20_extras_block_hashes;"
        )
        .fetch_one(db.pool()?)
        .await?;
        anyhow::Ok(height.to_js())
    };
    height.await.unwrap_or_else(|err| {
        tracing::error!("brc20 extras block height: {err:#}");
        JsValue::from(-1)
    })
}

/// The event type names by id (`event_type_id_to_name`): a plain object
/// whose keys are the ids as strings.
async fn event_type_names(db: &Db) -> Result<JsObject, ApiError> {
    let rows = sqlx::query!("select event_type_name, event_type_id from brc20_event_types;")
        .fetch_all(db.pool()?)
        .await?;
    let mut names = JsObject::new();
    for row in rows {
        names.set(
            row.event_type_id.to_js().to_js_string()?,
            row.event_type_name,
        );
    }
    Ok(names)
}

/// `event.event_type = ...; event.inscription_id = ...` on an event's
/// parsed jsonb. Assigning a property of null throws; on other primitives
/// it does nothing (the route runs in sloppy mode), and on an array it adds
/// a named property, which JSON.stringify leaves out.
fn add_event_fields(
    event: &mut JsValue,
    event_type: JsValue,
    inscription_id: JsValue,
) -> Result<(), JsError> {
    match event {
        JsValue::Object(object) => {
            object.set("event_type", event_type);
            object.set("inscription_id", inscription_id);
            Ok(())
        }
        JsValue::Null | JsValue::Undefined => Err(JsError::type_error(format!(
            "Cannot set properties of {} (setting 'event_type')",
            event.to_js_string()?
        ))),
        _ => Ok(()),
    }
}

/// An events query's row, shaped as the route's result entry.
fn event_entry(
    names: &JsObject,
    event: Option<&str>,
    event_type: i32,
    inscription_id: String,
) -> Result<JsValue, ApiError> {
    let mut entry = pg_json::text(oid::JSONB, event)?;
    let event_type = names.get(&event_type.to_js().to_js_string()?).clone();
    add_event_fields(&mut entry, event_type, inscription_id.into())?;
    Ok(entry)
}

/// The client's IP address, as the API sees it through
/// API_TRUSTED_PROXY_CNT proxies.
#[utoipa::path(
    get,
    path = "/v1/brc20/ip",
    tag = "brc20",
    responses((status = 200, description = "The client's IP address", body = String, content_type = "text/html")),
)]
async fn ip(ClientIp(ip): ClientIp) -> Reply {
    Reply::send_str(ip)
}

/// The database's schema version.
///
/// A database without a `brc20_indexer_version` row answers 500.
#[utoipa::path(
    get,
    path = "/v1/brc20/db_version",
    tag = "brc20",
    responses((status = 200, description = "The version number as text", body = String, content_type = "text/html")),
)]
async fn db_version(State(db): State<Db>) -> Result<Reply, ApiError> {
    let rows = sqlx::query_scalar!("SELECT db_version FROM brc20_indexer_version;")
        .fetch_all(db.pool()?)
        .await?;
    let version = rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("db_version"))?;
    Ok(Reply::send_str(version.to_js().to_js_string()?))
}

/// The version of the event hash format.
///
/// A database without a `brc20_indexer_version` row answers 500.
#[utoipa::path(
    get,
    path = "/v1/brc20/event_hash_version",
    tag = "brc20",
    responses((status = 200, description = "The version number as text", body = String, content_type = "text/html")),
)]
async fn event_hash_version(State(db): State<Db>) -> Result<Reply, ApiError> {
    let rows = sqlx::query_scalar!("SELECT event_hash_version FROM brc20_indexer_version;")
        .fetch_all(db.pool()?)
        .await?;
    let version = rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("event_hash_version"))?;
    Ok(Reply::send_str(version.to_js().to_js_string()?))
}

/// The latest block height of `brc20_extras_block_hashes`.
///
/// `null` for an empty table and `-1` when the query fails (as while the
/// table does not exist).
#[utoipa::path(
    get,
    path = "/v1/brc20/extras_block_height",
    tag = "brc20",
    responses((status = 200, description = "The height as text, `null` or `-1`", body = String, content_type = "text/html")),
)]
async fn extras_block_height(State(db): State<Db>) -> Result<Reply, ApiError> {
    let height = extras_block_height_of_db(&db).await;
    Ok(Reply::send_str(height.to_js_string()?))
}

/// The latest indexed block height.
///
/// `null` before the first block and `-1` when the query fails.
#[utoipa::path(
    get,
    path = "/v1/brc20/block_height",
    tag = "brc20",
    responses((status = 200, description = "The height as text, `null` or `-1`", body = String, content_type = "text/html")),
)]
async fn block_height(State(db): State<Db>) -> Result<Reply, ApiError> {
    let height = block_height_of_db(&db).await;
    Ok(Reply::send_str(height.to_js_string()?))
}

/// A ticker's balance of a pkscript at the start of a block height.
#[utoipa::path(
    get,
    path = "/v1/brc20/balance_on_block",
    tag = "brc20",
    params(
        ("block_height" = String, Query, description = "The block height, an integer; the balance is the latest one below it. Text Postgres cannot read as an int4 answers 500. Left out, no balance matches (400)."),
        ("pkscript" = String, Query, description = "The wallet's pkScript, hex. Left out, no balance matches (400)."),
        ParityTicker,
    ),
    responses(
        (status = 200, description = "The balance", body = Answer<Brc20Balance>),
        (status = 400, description = "`block not indexed yet` for a height above the indexed tip plus one, `no balance found` when no balance row matches", body = ErrorBody),
    ),
)]
async fn balance_on_block(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let block_height = query.get("block_height").clone();
    let pkscript = query.get("pkscript").clone();
    let tick = query.get("ticker").to_lower_case()?;

    let current_block_height = block_height_of_db(&db).await;
    if block_height.gt(&current_block_height.add(&1.into())?)? {
        return Ok(Reply::error(400, "block not indexed yet"));
    }

    let rules = db.int_rules().await?;
    let rows = sqlx::query!(
        r#"select overall_balance::text as "overall_balance", available_balance::text as "available_balance"
            from brc20_historic_balances
            where block_height < $1
              and pkscript = $2
              and tick = $3
            order by id desc
            limit 1;"#,
        params::int4(rules, &block_height)?,
        params::text(&pkscript)?,
        params::text(&tick.into())?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(Reply::error(400, "no balance found"));
    };
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "overall_balance" => pg_json::text(oid::NUMERIC, row.overall_balance.as_deref())?,
            "available_balance" => pg_json::text(oid::NUMERIC, row.available_balance.as_deref())?,
        },
    }))
}

/// Every brc20 event of a block height.
///
/// The events of `brc20_events` and `brc20_light_events`, by id.
#[utoipa::path(
    get,
    path = "/v1/brc20/activity_on_block",
    tag = "brc20",
    params(
        ListBlockHeight,
    ),
    responses(
        (status = 200, description = "The block's events", body = Answer<Vec<Brc20StoredEvent>>),
        (status = 400, description = "`block not indexed yet` for a height above the indexed tip", body = ErrorBody),
    ),
)]
async fn activity_on_block(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let block_height = query.get("block_height").clone();

    let current_block_height = block_height_of_db(&db).await;
    if block_height.gt(&current_block_height)? {
        return Ok(Reply::error(400, "block not indexed yet"));
    }

    let names = event_type_names(&db).await?;
    let rules = db.int_rules().await?;
    // the union keeps its jsonb columns, so it removes the same duplicates
    // the route's union does; the text cast comes after it
    let rows = sqlx::query!(
        r#"select u.event::text as "event", u.event_type as "event_type!", u.inscription_id as "inscription_id!"
            from (select id, event, event_type, inscription_id
                  from brc20_events
                  where block_height = $1
                  union select id, event, event_type, inscription_id
                  from brc20_light_events
                  where block_height = $1) u
            order by u.id asc;"#,
        params::int4(rules, &block_height)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(event_entry(
            &names,
            row.event.as_deref(),
            row.event_type,
            row.inscription_id,
        )?);
    }
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// Every cached bitcoin RPC result of a block height.
#[utoipa::path(
    get,
    path = "/v1/brc20/bitcoin_rpc_results_on_block",
    tag = "brc20",
    params(
        ListBlockHeight,
    ),
    responses(
        (status = 200, description = "The block's RPC results, by id", body = Answer<Vec<Brc20RpcResult>>),
        (status = 400, description = "`block not indexed yet` for a height above the indexed tip", body = ErrorBody),
    ),
)]
async fn bitcoin_rpc_results_on_block(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let block_height = query.get("block_height").clone();

    let current_block_height = block_height_of_db(&db).await;
    if block_height.gt(&current_block_height)? {
        return Ok(Reply::error(400, "block not indexed yet"));
    }

    let rules = db.int_rules().await?;
    let rows = sqlx::query!(
        r#"select
            method, request::text as "request", response::text as "response"
            from brc20_bitcoin_rpc_result_cache
            where block_height = $1
            order by id asc;"#,
        params::int4(rules, &block_height)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(JsValue::from(js_object! {
            "method" => row.method,
            "request" => pg_json::text(oid::JSONB, row.request.as_deref())?,
            "response" => pg_json::text(oid::JSONB, row.response.as_deref())?,
        }));
    }
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// A wallet's current balance of a ticker.
///
/// A non-empty `address` selects the wallet by address, otherwise by
/// `pkscript`.
#[utoipa::path(
    get,
    path = "/v1/brc20/get_current_balance_of_wallet",
    tag = "brc20",
    params(
        ("address" = Option<String>, Query, description = "The wallet's address. Left out or empty, the route reads `pkscript`."),
        ("pkscript" = Option<String>, Query, description = "The wallet's pkScript, hex; read when `address` is empty. Both left out, the route matches the empty pkScript and answers 400."),
        ParityTicker,
    ),
    responses(
        (status = 200, description = "The balance", body = Answer<Brc20CurrentBalance>),
        (status = 400, description = "`no balance found`", body = ErrorBody),
    ),
)]
async fn get_current_balance_of_wallet(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let address = query.get("address").clone().or("".into());
    let pkscript = query.get("pkscript").clone().or("".into());
    let tick = query.get("ticker").to_lower_case()?;

    let current_block_height = block_height_of_db(&db).await;
    let tick = params::text(&tick.into())?;
    let pool = db.pool()?;
    // `address != ''` swaps the query's first `pkscript` for `wallet`
    let row = if !address.loose_eq(&"".into())? {
        sqlx::query!(
            r#"select overall_balance::text as "overall_balance", available_balance::text as "available_balance"
                from brc20_current_balances
                where wallet = $1
                  and tick = $2
                limit 1;"#,
            params::text(&address)?,
            tick,
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .next()
        .map(|row| (row.overall_balance, row.available_balance))
    } else {
        sqlx::query!(
            r#"select overall_balance::text as "overall_balance", available_balance::text as "available_balance"
                from brc20_current_balances
                where pkscript = $1
                  and tick = $2
                limit 1;"#,
            params::text(&pkscript)?,
            tick,
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .next()
        .map(|row| (row.overall_balance, row.available_balance))
    };
    let Some((overall_balance, available_balance)) = row else {
        return Ok(Reply::error(400, "no balance found"));
    };
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "overall_balance" => pg_json::text(oid::NUMERIC, overall_balance.as_deref())?,
            "available_balance" => pg_json::text(oid::NUMERIC, available_balance.as_deref())?,
            "block_height" => current_block_height,
        },
    }))
}

/// A brc20_unused_tx_inscrs row as get_valid_tx_notes_of_wallet reads it:
/// tick, inscription_id, amount (as text) and genesis_height. No
/// repository holds the table's DDL, so every column may be NULL.
type UnusedTxRow = (Option<String>, Option<String>, Option<String>, Option<i32>);

/// A wallet's unused transfer inscriptions.
///
/// A non-empty `address` selects the wallet by address, otherwise by
/// `pkscript`. The route reads `brc20_unused_tx_inscrs`, which neither the
/// indexer nor `brc20 reindex-extras` creates; it answers 500 until the
/// table exists.
#[utoipa::path(
    get,
    path = "/v1/brc20/get_valid_tx_notes_of_wallet",
    tag = "brc20",
    params(
        ("address" = Option<String>, Query, description = "The wallet's address. Left out or empty, the route reads `pkscript`."),
        ("pkscript" = Option<String>, Query, description = "The wallet's pkScript, hex; read when `address` is empty. Both left out, the route matches the empty pkScript and answers 400."),
    ),
    responses(
        (status = 200, description = "The inscriptions, by ticker", body = Answer<Brc20WalletUnusedTxes>),
        (status = 400, description = "`no unused tx found`", body = ErrorBody),
    ),
)]
async fn get_valid_tx_notes_of_wallet(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let address = query.get("address").clone().or("".into());
    let pkscript = query.get("pkscript").clone().or("".into());

    let current_block_height = block_height_of_db(&db).await;
    let pool = db.pool()?;
    // `address != ''` swaps the query's first `pkscript` for `wallet`
    let rows: Vec<UnusedTxRow> = if !address.loose_eq(&"".into())? {
        sqlx::query!(
            r#"select tick as "tick?", inscription_id as "inscription_id?", amount::text as "amount", block_height as "genesis_height?"
                from brc20_unused_tx_inscrs
                where current_holder_wallet = $1
                order by tick asc;"#,
            params::text(&address)?,
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| (r.tick, r.inscription_id, r.amount, r.genesis_height))
        .collect()
    } else {
        sqlx::query!(
            r#"select tick as "tick?", inscription_id as "inscription_id?", amount::text as "amount", block_height as "genesis_height?"
                from brc20_unused_tx_inscrs
                where current_holder_pkscript = $1
                order by tick asc;"#,
            params::text(&pkscript)?,
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| (r.tick, r.inscription_id, r.amount, r.genesis_height))
        .collect()
    };
    if rows.is_empty() {
        return Ok(Reply::error(400, "no unused tx found"));
    }
    let mut unused_txes = Vec::with_capacity(rows.len());
    for (tick, inscription_id, amount, genesis_height) in rows {
        unused_txes.push(JsValue::from(js_object! {
            "tick" => tick.to_js(),
            "inscription_id" => inscription_id.to_js(),
            "amount" => pg_json::text(oid::NUMERIC, amount.as_deref())?,
            "genesis_height" => genesis_height.to_js(),
        }));
    }
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "unused_txes" => unused_txes,
            "block_height" => current_block_height,
        },
    }))
}

/// A ticker's unused transfer inscriptions.
///
/// The route reads `brc20_unused_tx_inscrs`, which neither the indexer nor
/// `brc20 reindex-extras` creates; it answers 500 until the table exists.
#[utoipa::path(
    get,
    path = "/v1/brc20/get_valid_tx_notes_of_ticker",
    tag = "brc20",
    params(
        ParityTicker,
    ),
    responses(
        (status = 200, description = "The inscriptions, by holder pkScript", body = Answer<Brc20TickerUnusedTxes>),
        (status = 400, description = "`no unused tx found`", body = ErrorBody),
    ),
)]
async fn get_valid_tx_notes_of_ticker(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let tick = JsValue::from(query.get("ticker").to_lower_case()?).or("".into());

    let current_block_height = block_height_of_db(&db).await;
    let rows = sqlx::query!(
        r#"select current_holder_pkscript as "current_holder_pkscript?", current_holder_wallet as "current_holder_wallet?", inscription_id as "inscription_id?", amount::text as "amount", block_height as "genesis_height?"
            from brc20_unused_tx_inscrs
            where tick = $1
            order by current_holder_pkscript asc;"#,
        params::text(&tick)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    if rows.is_empty() {
        return Ok(Reply::error(400, "no unused tx found"));
    }
    let mut unused_txes = Vec::with_capacity(rows.len());
    for row in rows {
        unused_txes.push(JsValue::from(js_object! {
            "current_holder_pkscript" => row.current_holder_pkscript.to_js(),
            "current_holder_wallet" => row.current_holder_wallet.to_js(),
            "inscription_id" => row.inscription_id.to_js(),
            "amount" => pg_json::text(oid::NUMERIC, row.amount.as_deref())?,
            "genesis_height" => row.genesis_height.to_js(),
        }));
    }
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "unused_txes" => unused_txes,
            "block_height" => current_block_height,
        },
    }))
}

/// `rows.sort((a, b) => parseInt(b.overall_balance) - parseInt(a.overall_balance))`,
/// then `rows.filter((row) => parseInt(row.overall_balance) != 0)`.
fn order_holders(rows: Vec<JsObject>) -> Result<Vec<JsObject>, JsError> {
    let mut keyed = Vec::with_capacity(rows.len());
    for row in rows {
        keyed.push((row.get("overall_balance").parse_int()?, row));
    }
    sort_by_number(&mut keyed, |a, b| b.0 - a.0);
    Ok(keyed
        .into_iter()
        .filter(|(balance, _)| *balance != 0.0)
        .map(|(_, row)| row)
        .collect())
}

/// A ticker's holders and their current balances.
///
/// The holders with a nonzero balance, largest first, listed under the key
/// `unused_txes`. A ticker without balance rows answers 400.
#[utoipa::path(
    get,
    path = "/v1/brc20/holders",
    tag = "brc20",
    params(
        ParityTicker,
    ),
    responses(
        (status = 200, description = "The holders", body = Answer<Brc20Holders>),
        (status = 400, description = "`no unused tx found` for a ticker without balance rows", body = ErrorBody),
    ),
)]
async fn holders(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let tick = JsValue::from(query.get("ticker").to_lower_case()?).or("".into());

    let current_block_height = block_height_of_db(&db).await;
    // the order names the table's column: a bare `overall_balance` would
    // name the text output column
    let rows = sqlx::query!(
        r#"select pkscript, wallet, overall_balance::text as "overall_balance", available_balance::text as "available_balance"
            from brc20_current_balances
            where tick = $1
            order by brc20_current_balances.overall_balance asc;"#,
        params::text(&tick)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    if rows.is_empty() {
        return Ok(Reply::error(400, "no unused tx found"));
    }
    let mut objects = Vec::with_capacity(rows.len());
    for row in rows {
        objects.push(js_object! {
            "pkscript" => row.pkscript,
            "wallet" => row.wallet.to_js(),
            "overall_balance" => pg_json::text(oid::NUMERIC, row.overall_balance.as_deref())?,
            "available_balance" => pg_json::text(oid::NUMERIC, row.available_balance.as_deref())?,
        });
    }
    // a rune with many holders sorts a large set, off the async workers
    let rows: Vec<JsValue> = tokio::task::spawn_blocking(move || order_holders(objects))
        .await??
        .into_iter()
        .map(JsValue::from)
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "unused_txes" => rows,
            "block_height" => current_block_height,
        },
    }))
}

async fn indexer_version<'e>(executor: impl PgExecutor<'e>) -> Result<String, ApiError> {
    let rows = sqlx::query_scalar!("select indexer_version from brc20_indexer_version;")
        .fetch_all(executor)
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("indexer_version"))?)
}

/// A block's event and trace hashes, cumulative and its own.
///
/// A height the hash table has no row for answers 500.
#[utoipa::path(
    get,
    path = "/v1/brc20/get_hash_of_all_activity",
    tag = "brc20",
    params(
        HashBlockHeight,
    ),
    responses(
        (status = 200, description = "The block's hashes", body = Answer<Brc20ActivityHash>),
        (status = 400, description = "`block not indexed yet` for a height above the indexed tip", body = ErrorBody),
    ),
)]
async fn get_hash_of_all_activity(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let block_height = query.get("block_height").clone();

    let current_block_height = block_height_of_db(&db).await;
    if block_height.gt(&current_block_height)? {
        return Ok(Reply::error(400, "block not indexed yet"));
    }

    let rules = db.int_rules().await?;
    let rows = sqlx::query!(
        "select cumulative_event_hash, block_event_hash, cumulative_trace_hash, block_trace_hash
            from brc20_cumulative_event_hashes
            where block_height = $1;",
        params::int4(rules, &block_height)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("cumulative_event_hash"))?;
    let indexer_version = indexer_version(db.pool()?).await?;

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "cumulative_event_hash" => row.cumulative_event_hash,
            "block_event_hash" => row.block_event_hash,
            "cumulative_trace_hash" => row.cumulative_trace_hash,
            "block_trace_hash" => row.block_trace_hash,
            "indexer_version" => indexer_version,
            "block_height" => block_height,
        },
    }))
}

/// A brc20_current_balances row as get_hash_of_all_current_balances reads
/// it.
struct BalanceRow {
    pkscript: String,
    tick: String,
    overall_balance: JsValue,
    available_balance: JsValue,
}

/// The sha256 hex of every nonzero balance, in JavaScript string order of
/// pkscript then tick, as `pkscript;tick;overall;available` joined by `|`.
fn current_balances_hash(mut rows: Vec<BalanceRow>) -> Result<String, JsError> {
    rows.sort_by(|a, b| {
        compare_strings(&a.pkscript, &b.pkscript).then_with(|| compare_strings(&a.tick, &b.tick))
    });
    let mut whole_str = String::new();
    for row in &rows {
        if row.overall_balance.parse_int()? != 0.0 {
            whole_str.push_str(&row.pkscript);
            whole_str.push(';');
            whole_str.push_str(&row.tick);
            whole_str.push(';');
            whole_str.push_str(&row.overall_balance.to_js_string()?);
            whole_str.push(';');
            whole_str.push_str(&row.available_balance.to_js_string()?);
            whole_str.push_str(EVENT_SEPARATOR);
        }
    }
    // whole_str.slice(0, -1): the last separator
    whole_str.pop();
    Ok(format!("{:x}", Sha256::digest(whole_str.as_bytes())))
}

/// The hash of every current balance.
///
/// The route reads, sorts and hashes the whole `brc20_current_balances`
/// table once per indexed block and serves that answer until the tip moves.
#[utoipa::path(
    get,
    path = "/v1/brc20/get_hash_of_all_current_balances",
    tag = "brc20",
    responses((status = 200, description = "The hash", body = Answer<Brc20BalancesHash>)),
)]
async fn get_hash_of_all_current_balances(
    State(cache): State<TipCache>,
) -> Result<Reply, ApiError> {
    cache.answer().await
}

/// The hash answer at `height`. The indexer commits a block's current
/// balances before its block hash, so an answer that reads a balance
/// written above `height` is not settled.
fn current_balances_answer(
    conn: &mut PgConnection,
    height: Option<i32>,
) -> BoxFuture<'_, Result<(Reply, bool), ApiError>> {
    Box::pin(async move {
        let ahead = sqlx::query_scalar!(
            r#"select exists(select 1 from brc20_current_balances
                where block_height > coalesce($1::int4, -1)) as "ahead!";"#,
            height,
        )
        .fetch_one(&mut *conn)
        .await?;
        let reply = current_balances_reply(conn, height).await?;
        Ok((reply, !ahead))
    })
}

async fn current_balances_reply(
    conn: &mut PgConnection,
    height: Option<i32>,
) -> Result<Reply, ApiError> {
    let rows = sqlx::query!(
        r#"select pkscript, tick, overall_balance::text as "overall_balance", available_balance::text as "available_balance"
            from brc20_current_balances
            order by pkscript asc, tick asc;"#
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut balances = Vec::with_capacity(rows.len());
    for row in rows {
        balances.push(BalanceRow {
            pkscript: row.pkscript,
            tick: row.tick,
            overall_balance: pg_json::text(oid::NUMERIC, row.overall_balance.as_deref())?,
            available_balance: pg_json::text(oid::NUMERIC, row.available_balance.as_deref())?,
        });
    }
    // the whole table is sorted and hashed in memory, off the async workers
    let hash_hex = tokio::task::spawn_blocking(move || current_balances_hash(balances)).await??;
    let indexer_version = indexer_version(&mut *conn).await?;

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "current_balances_hash" => hash_hex,
            "indexer_version" => indexer_version,
            "block_height" => height.to_js(),
        },
    }))
}

/// Every event of an inscription id.
///
/// The events of `brc20_events` and `brc20_light_events`, by id.
#[utoipa::path(
    get,
    path = "/v1/brc20/event",
    tag = "brc20",
    params(
        ("inscription_id" = String, Query, description = "The inscription id. Left out or empty, the route answers 400."),
    ),
    responses(
        (status = 200, description = "The inscription's events", body = Answer<Vec<Brc20StoredEvent>>),
        (status = 400, description = "`inscription_id is required` when it is left out or empty", body = ErrorBody),
    ),
)]
async fn event(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let names = event_type_names(&db).await?;

    let inscription_id = query.get("inscription_id").clone();
    if !inscription_id.truthy() {
        return Ok(Reply::error(400, "inscription_id is required"));
    }

    // the union keeps its jsonb columns, so it removes the same duplicates
    // the route's union does; the text cast comes after it
    let rows = sqlx::query!(
        r#"select u.event::text as "event", u.event_type as "event_type!", u.inscription_id as "inscription_id!"
            from (select id, event, event_type, inscription_id
                  from brc20_events
                  where inscription_id = $1
                  union select id, event, event_type, inscription_id
                  from brc20_light_events
                  where inscription_id = $1) u
            order by u.id asc;"#,
        params::text(&inscription_id)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(event_entry(
            &names,
            row.event.as_deref(),
            row.event_type,
            row.inscription_id,
        )?);
    }
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// get_btc_address_of_evm_address: the BTC address evm_addresses maps an
/// EVM address to, or null when no row matches or the row's address is
/// NULL (no repository holds the table's DDL).
async fn btc_address_of_evm_address(
    db: &Db,
    evm_address: &JsValue,
) -> Result<Option<String>, ApiError> {
    let rows = sqlx::query_scalar!(
        r#"select btc_address as "btc_address?" from evm_addresses where evm_address = lower($1);"#,
        params::text(evm_address)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    Ok(rows.into_iter().next().flatten())
}

/// The BTC address an EVM address maps to, for the BRC2.0 explorer.
#[utoipa::path(
    get,
    path = "/v1/brc20/explorer/btc_address/{evm_address}",
    tag = "brc20",
    params(
        ("evm_address" = String, Path, description = "The EVM address, matched in lowercase. A segment that does not percent-decode answers 500."),
    ),
    responses((status = 200, description = "The BTC address", body = Brc20ExplorerBtcAddress)),
)]
async fn explorer_btc_address(
    State(db): State<Db>,
    ExpressParams(path): ExpressParams,
) -> Result<Reply, ApiError> {
    let btc_address = btc_address_of_evm_address(&db, path.get("evm_address")).await?;
    Ok(Reply::send_json(js_object! {
        "btc_address" => btc_address,
    }))
}

/// The BRC-20 balances of an EVM address's BTC address, for the BRC2.0
/// explorer.
#[utoipa::path(
    get,
    path = "/v1/brc20/explorer/balances/{evm_address}",
    tag = "brc20",
    params(
        ("evm_address" = String, Path, description = "The EVM address, matched in lowercase. A segment that does not percent-decode answers 500."),
    ),
    responses((status = 200, description = "The positive balances", body = Brc20ExplorerBalances)),
)]
async fn explorer_balances(
    State(db): State<Db>,
    ExpressParams(path): ExpressParams,
) -> Result<Reply, ApiError> {
    let Some(btc_address) = btc_address_of_evm_address(&db, path.get("evm_address")).await? else {
        return Ok(Reply::send_json(js_object! {
            "items" => Vec::<JsValue>::new(),
        }));
    };
    let rows = sqlx::query!(
        r#"select bcb.tick, bcb.overall_balance::text as "overall_balance", bt.decimals as "decimals?"
            from brc20_current_balances bcb
            left join brc20_tickers bt on bcb.tick = bt.tick
            where bcb.wallet = $1 and bcb.overall_balance > 0
            order by bcb.overall_balance desc;"#,
        params::text(&btc_address.into())?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(JsValue::from(js_object! {
            "external_token_name" => row.tick,
            "external_value" => pg_json::text(oid::NUMERIC, row.overall_balance.as_deref())?,
            "external_decimals" => row.decimals.to_js().to_js_string()?,
        }));
    }
    Ok(Reply::send_json(js_object! { "items" => items }))
}

/// The address tab counters of an EVM address, for the BRC2.0 explorer.
#[utoipa::path(
    get,
    path = "/v1/brc20/explorer/address_tabs_counters/{evm_address}",
    tag = "brc20",
    params(
        ("evm_address" = String, Path, description = "The EVM address, matched in lowercase. A segment that does not percent-decode answers 500."),
    ),
    responses((status = 200, description = "The counters", body = Brc20ExplorerCounters)),
)]
async fn explorer_address_tabs_counters(
    State(db): State<Db>,
    ExpressParams(path): ExpressParams,
) -> Result<Reply, ApiError> {
    let mut token_balances_count = JsValue::from(0);
    if let Some(btc_address) = btc_address_of_evm_address(&db, path.get("evm_address")).await? {
        let rows = sqlx::query_scalar!(
            "select count(*) as brc20_count
                from brc20_current_balances
                where wallet = $1 and overall_balance > 0;",
            params::text(&btc_address.into())?,
        )
        .fetch_all(db.pool()?)
        .await?;
        let count = rows
            .into_iter()
            .next()
            .ok_or_else(|| no_row("brc20_count"))?;
        token_balances_count = JsValue::Number(count.to_js().parse_int()?);
    }
    Ok(Reply::send_json(js_object! {
        "internal_transactions_count" => 0,
        "logs_count" => 0,
        "token_balances_count" => token_balances_count,
        "token_transfers_count" => 0,
        "transactions_count" => 0,
        "validations_count" => 0,
        "withdrawals_count" => 0,
    }))
}

/// The tokens page a request names: the `q` filter, the offset
/// (`parseInt(items_count || '0') || 0`) and whether the order is `asc`.
fn explorer_tokens_page(query: &JsObject) -> Result<(String, f64, bool), JsError> {
    let q = match query.get("q") {
        JsValue::String(q) => q.clone(),
        _ => String::new(),
    };
    let offset = JsValue::Number(
        query
            .get("items_count")
            .clone()
            .or("0".into())
            .parse_int()?,
    )
    .or(0.into())
    .to_number()?;
    let ascending = query.get("order").to_js_string()?.to_lowercase() == "asc";
    Ok((q, offset, ascending))
}

/// A page of tickers by holder count, for the BRC2.0 explorer.
///
/// Pages of 50 tickers with a positive balance, by holder count then ticker.
#[utoipa::path(
    get,
    path = "/v1/brc20/explorer/tokens",
    tag = "brc20",
    params(
        ("q" = Option<String>, Query, description = "Keeps the tickers that contain it, in any case. Left out, every ticker is listed."),
        ("items_count" = Option<String>, Query, description = "The tickers to skip, read with parseInt; left out or unreadable, 0"),
        ("order" = Option<String>, Query, description = "`asc` (in any case) for the fewest holders first; anything else, or the parameter left out, lists the most first"),
    ),
    responses((status = 200, description = "The page", body = Brc20ExplorerTokens)),
)]
async fn explorer_tokens(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let (q, offset, ascending) = explorer_tokens_page(&query)?;
    let rules = db.int_rules().await?;
    let pattern = params::text(&format!("%{q}%").into())?;
    let limit = Some(EXPLORER_PAGE_SIZE + 1);
    let offset_param = params::int8(rules, &offset.into())?;
    let pool = db.pool()?;
    // the route interpolates the whitelisted direction; each one is its
    // own query here
    let rows: Vec<(String, i32, Option<String>, Option<i64>)> = if ascending {
        sqlx::query!(
            r#"select bt.tick, bt.decimals, (bt.max_supply - bt.remaining_supply)::text as "total_supply", count(*) as holder_count
                from brc20_tickers bt
                join brc20_current_balances bcb on bt.tick = bcb.tick
                where bt.tick ilike $1 and bcb.overall_balance > 0
                group by 1, 2, 3
                order by holder_count asc, bt.tick asc
                limit $2 offset $3;"#,
            pattern,
            limit,
            offset_param,
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| (r.tick, r.decimals, r.total_supply, r.holder_count))
        .collect()
    } else {
        sqlx::query!(
            r#"select bt.tick, bt.decimals, (bt.max_supply - bt.remaining_supply)::text as "total_supply", count(*) as holder_count
                from brc20_tickers bt
                join brc20_current_balances bcb on bt.tick = bcb.tick
                where bt.tick ilike $1 and bcb.overall_balance > 0
                group by 1, 2, 3
                order by holder_count desc, bt.tick asc
                limit $2 offset $3;"#,
            pattern,
            limit,
            offset_param,
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| (r.tick, r.decimals, r.total_supply, r.holder_count))
        .collect()
    };
    let has_next = rows.len() as i64 > EXPLORER_PAGE_SIZE;
    let page = &rows[..rows.len().min(EXPLORER_PAGE_SIZE as usize)];
    let next_page_params = match page.last() {
        Some((tick, _, _, holder_count)) if has_next => JsValue::from(js_object! {
            "holder_count" => holder_count.to_js(),
            "items_count" => offset + EXPLORER_PAGE_SIZE as f64,
            "name" => tick.as_str(),
            "market_cap" => JsValue::Null,
        }),
        _ => JsValue::Null,
    };
    let mut items = Vec::with_capacity(page.len());
    for (tick, decimals, total_supply, holder_count) in page {
        items.push(JsValue::from(js_object! {
            "address" => "",
            "type" => "ERC-20",
            "symbol" => tick.as_str(),
            "name" => tick.as_str(),
            "decimals" => decimals.to_js().to_js_string()?,
            "holders" => holder_count.to_js().to_js_string()?,
            "exchange_rate" => JsValue::Null,
            "total_supply" => pg_json::text(oid::NUMERIC, total_supply.as_deref())?.to_js_string()?,
            "icon_url" => JsValue::Null,
            "circulating_market_cap" => JsValue::Null,
        }));
    }
    Ok(Reply::send_json(js_object! {
        "items" => items,
        "next_page_params" => next_page_params,
    }))
}
