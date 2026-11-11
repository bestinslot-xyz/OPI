//! `/v1/brc20/wallet/balance_history?address=|pkscript=&ticker=`: a
//! wallet's balance of one ticker after each change, newest first.
//!
//! The rows are those of `brc20_historic_balances` for the wallet's
//! pkScript and the ticker: one per balance change, written by the event
//! that made it. `address` and `pkscript` name the wallet as on
//! `/v1/brc20/wallet/events`: an address is read as the pkScript it encodes,
//! and a row passes when its `wallet` is the address as given; an EVM
//! address answers as its mapped Bitcoin address, and an unmapped one has no
//! rows. Amounts are raw integers as strings with `decimals` (18) fractional
//! digits, and `ticker_decimals` is the ticker's deploy precision. An
//! unknown ticker answers `404 {"error":"ticker not found","result":null}`.
//!
//! The list pages by key as the event feeds do: `limit` (default 100, at
//! most 500) and `before_id`, the `next_before_id` of the previous page,
//! with `has_more`. Row ids grow with the block height (a reorg rewinds the
//! id sequence with the blocks), so the rows below `before_id` are those
//! at or below its block; each page is a backward range scan of the
//! `(pkscript, tick, block_height)` index from that block.

use {
    super::{
        block_height_of_db,
        events::{WalletParams, wallet_key},
        tickers::{AMOUNT_DECIMALS, TickerParam, ticker, ticker_decimals},
    },
    crate::{
        db::Db,
        extract::JsQuery,
        js::JsValue,
        js_object,
        openapi::{Answer, DbHeight, ErrorBody, LimitParam},
        reply::{ApiError, Reply},
        routes::paging::{
            BadParam, DEFAULT_LIMIT, MAX_BEFORE_ID, MAX_LIMIT, Wallet, integer, single, split_page,
        },
    },
    axum::extract::State,
    utoipa::{IntoParams, ToSchema},
};

/// The paging of the balance history.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct HistoryParams {
    /// The previous page's `next_before_id`: the page holds the rows with a
    /// lower id.
    #[param(minimum = 1, maximum = 999999999999999999_i64)]
    before_id: Option<i64>,
}

/// A balance after one change.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20BalanceChange {
    /// The row's id, which orders the list.
    id: i64,
    block_height: i32,
    /// The block's time in seconds; null when the block's time is unknown.
    #[schema(nullable)]
    block_time: i64,
    /// The balance after the change, raw.
    overall_balance: String,
    /// The part of it not locked in transfer inscriptions, raw.
    available_balance: String,
}

/// A page of a wallet's balance history.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20BalanceHistory {
    /// The address asked for, or an EVM address's mapped Bitcoin address;
    /// null for a `pkscript` request.
    #[schema(nullable)]
    address: String,
    /// The lowercase pkScript asked for; null for an `address` request.
    #[schema(nullable)]
    pkscript: String,
    /// The lowercase ticker.
    ticker: String,
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    ticker_decimals: i32,
    /// Newest first.
    balances: Vec<Brc20BalanceChange>,
    limit: i64,
    /// Whether older rows exist.
    has_more: bool,
    /// The `before_id` of the next page, null on the last page.
    #[schema(nullable)]
    next_before_id: i64,
    block_height: DbHeight,
}

/// The request of the balance history.
struct HistoryRequest {
    wallet: Wallet,
    tick: String,
    limit: i64,
    before_id: Option<i64>,
}

impl HistoryRequest {
    fn from_query(query: &crate::js::JsObject) -> Result<HistoryRequest, BadParam> {
        let wallet = Wallet::from_query(query)?;
        let tick = ticker(query, "ticker")?;
        let before_id = match single(query, "before_id")? {
            None => None,
            Some(_) => Some(integer(query, "before_id", 0, 1, MAX_BEFORE_ID)?),
        };
        Ok(HistoryRequest {
            wallet,
            tick,
            limit: integer(query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
            before_id,
        })
    }
}

/// A wallet's balance of one ticker after each change, newest first.
///
/// One row per change, with the block and its time. The list pages by key.
#[utoipa::path(
    get,
    path = "/v1/brc20/wallet/balance_history",
    tag = "brc20",
    params(WalletParams, TickerParam, HistoryParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<Brc20BalanceHistory>),
        (status = 400, description = "A missing or invalid parameter, or both or neither of `address` and `pkscript`", body = ErrorBody),
        (status = 404, description = "`ticker not found`", body = ErrorBody),
    ),
)]
pub async fn balance_history(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let request = match HistoryRequest::from_query(&query) {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let Some(deploy_decimals) = ticker_decimals(&db, &request.tick).await? else {
        return Ok(Reply::error(404, "ticker not found"));
    };
    let (address, pkscript, key) = wallet_key(&db, request.wallet).await?;
    let rows = match key {
        None => Vec::new(),
        Some(key) => {
            // The cursor's block bounds the index scan; a cursor whose row a
            // reorg removed bounds it by the id alone.
            let cursor_height = match request.before_id {
                None => None,
                Some(id) => Some(
                    sqlx::query_scalar!(
                        "select block_height from brc20_historic_balances where id = $1;",
                        id
                    )
                    .fetch_optional(db.pool()?)
                    .await?
                    .unwrap_or(i32::MAX),
                ),
            };
            sqlx::query!(
                r#"select b.id as "id!", b.block_height as "block_height!", h.block_time as "block_time?",
                        b.overall_balance::text as "overall_balance!",
                        b.available_balance::text as "available_balance!"
                    from brc20_historic_balances b
                    left join brc20_block_hashes h on h.block_height = b.block_height
                    where b.pkscript = $1 and b.tick = $2
                      and ($3::text is null or b.wallet = $3)
                      and ($4::int8 is null or (b.block_height <= $5 and b.id < $4))
                    order by b.block_height desc, b.id desc
                    limit $6;"#,
                key.pkscript,
                request.tick,
                key.wallet,
                request.before_id,
                cursor_height,
                request.limit + 1,
            )
            .fetch_all(db.pool()?)
            .await?
        }
    };
    let (rows, has_more) = split_page(rows, request.limit);
    let next_before_id = if has_more {
        rows.last().map(|row| row.id as f64)
    } else {
        None
    };
    let balances: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            js_object! {
                "id" => row.id as f64,
                "block_height" => row.block_height,
                "block_time" => row.block_time.map(|time| time as f64),
                "overall_balance" => row.overall_balance,
                "available_balance" => row.available_balance,
            }
            .into()
        })
        .collect();
    let mut result = js_object! {
        "address" => address,
        "pkscript" => pkscript,
        "ticker" => request.tick,
        "decimals" => AMOUNT_DECIMALS,
        "ticker_decimals" => deploy_decimals,
        "balances" => balances,
    };
    result.set("limit", request.limit as f64);
    result.set("has_more", has_more);
    result.set("next_before_id", next_before_id);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}
