//! The ticker routes: `/v1/brc20/ticker`, `/v1/brc20/ticker/by_contract`,
//! `/v1/brc20/tickers`, `/v1/brc20/ticker/holders` and
//! `/v1/brc20/wallet/balances`.
//!
//! They answer `{"error":null,"result":...}`. Amounts are raw integers as
//! strings: every BRC-20 and BRC2.0 amount carries 18 fractional digits
//! (1 token is `1000000000000000000`), so `decimals` is always
//! [`AMOUNT_DECIMALS`], the divisor's exponent, and `ticker_decimals` is the
//! ticker's deploy precision, the most fractional digits an amount of it may
//! have (null for a BRC2.0 balance of a ticker brc20 does not list). A
//! missing or invalid parameter answers
//! `400 {"error":"<reason>","result":null}` and an unknown ticker
//! `404 {"error":"ticker not found","result":null}`.
//!
//! `holders` counts the pkScripts with a positive BRC-20 balance, without
//! the BRC2.0 module's pkScript ([`BRC20_PROG_MODULE_PKSCRIPT`]), whose
//! balance is listed per EVM address on the BRC2.0 side. `prog_holders`
//! counts the EVM wallets with a positive BRC2.0 balance. A Bitcoin address
//! holding both kinds counts in both, and `sort=holders` ranks by the BRC-20
//! count.
//!
//! Tickers are matched as stored: brc20 keeps the lowercase form of each
//! ticker, and the BRC2.0 engine names its token contract after that same
//! lowercase ticker, so `brc20_prog_tickers.ticker` and the BRC2.0 balances'
//! `ticker` equal `brc20_tickers.tick`. A ticker parameter is lowercased the
//! same way.
//!
//! A holder is a Bitcoin address: its BRC-20 balance (by `wallet`) and the
//! BRC2.0 balance of its EVM address (mapped through `evm_addresses`) form
//! one row. A BRC-20 balance whose pkScript has no address is listed by its
//! pkScript, and an EVM address with no Bitcoin address (a contract, say) by
//! its EVM address. The BRC2.0 module's pkScript is left out of holders and
//! wallet balances, as it is of `holders`.
//!
//! Lists page with `offset` (default 0) and `limit` (default 100, at most
//! 500) in a fixed order that ends in a unique key, and report `has_more`.

use {
    super::block_height_of_db,
    crate::{
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        openapi::{Answer, DbHeight, ErrorBody, PageParams, with_default},
        reply::{ApiError, Reply},
    },
    axum::extract::State,
    utoipa::{
        IntoParams, ToSchema,
        openapi::{RefOr, schema::Schema},
    },
};

use crate::routes::paging::{
    BadParam, DEFAULT_LIMIT, MAX_ADDRESS_LEN, MAX_LIMIT, MAX_OFFSET, MAX_TICKER_LEN, integer,
    optional_text, prefix_pattern, single, split_page, text,
};

#[cfg(test)]
mod tests;

/// The fractional digits of every stored amount: an amount divided by
/// 10^18 is in tokens.
pub const AMOUNT_DECIMALS: i32 = 18;

/// The pkScript (`OP_RETURN "BRC20PROG"`) that holds the BRC-20 side of
/// every token deposited into BRC2.0, opi-brc20's
/// `BRC20_PROG_OP_RETURN_PKSCRIPT`. Its balance is the sum the BRC2.0
/// balances list per EVM address, so the BRC-20 side leaves it out.
pub const BRC20_PROG_MODULE_PKSCRIPT: &str = "6a09425243323050524f47";

/// A page of a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub offset: i64,
    pub limit: i64,
}

impl Page {
    pub fn from_query(query: &JsObject) -> Result<Page, BadParam> {
        Ok(Page {
            offset: integer(query, "offset", 0, 0, MAX_OFFSET)?,
            limit: integer(query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
        })
    }

    /// The rows a query fetches: one past the page, which tells whether
    /// more follow.
    fn fetch(&self) -> i64 {
        self.limit + 1
    }

    /// The page's rows of `rows` (fetched with [`Page::fetch`]), and whether
    /// more follow.
    fn split<T>(&self, rows: Vec<T>) -> (Vec<T>, bool) {
        split_page(rows, self.limit)
    }

    /// The page fields of a list's result.
    fn fields(&self, result: &mut JsObject, has_more: bool) {
        result.set("offset", self.offset as f64);
        result.set("limit", self.limit as f64);
        result.set("has_more", has_more);
    }
}

/// The lowercase ticker of `key`, as brc20 stores tickers: at most
/// [`MAX_TICKER_LEN`] bytes, without NUL.
pub(super) fn ticker(query: &JsObject, key: &str) -> Result<String, BadParam> {
    Ok(text(query, key, MAX_TICKER_LEN)?.to_lowercase())
}

/// The order of `/v1/brc20/tickers`.
// Its schema names each order in snake case, as `TickerSort::name` does
// (a unit test compares them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TickerSort {
    Holders,
    ProgHolders,
    MaxSupply,
    MintProgress,
    DeployHeight,
}

impl TickerSort {
    const ALL: [TickerSort; 5] = [
        TickerSort::Holders,
        TickerSort::ProgHolders,
        TickerSort::MaxSupply,
        TickerSort::MintProgress,
        TickerSort::DeployHeight,
    ];

    /// The parameter value, which the query's ORDER BY also names.
    pub fn name(self) -> &'static str {
        match self {
            TickerSort::Holders => "holders",
            TickerSort::ProgHolders => "prog_holders",
            TickerSort::MaxSupply => "max_supply",
            TickerSort::MintProgress => "mint_progress",
            TickerSort::DeployHeight => "deploy_height",
        }
    }
}

/// What `/v1/brc20/tickers` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickersRequest {
    pub sort: TickerSort,
    pub ascending: bool,
    /// The LIKE pattern of the `search` prefix.
    pub pattern: Option<String>,
    pub page: Page,
}

impl TickersRequest {
    pub fn from_query(query: &JsObject) -> Result<TickersRequest, BadParam> {
        let sort = match single(query, "sort")? {
            None => TickerSort::Holders,
            Some(name) => TickerSort::ALL
                .into_iter()
                .find(|sort| sort.name() == name)
                .ok_or_else(|| {
                    let names: Vec<&str> = TickerSort::ALL.iter().map(|s| s.name()).collect();
                    BadParam(format!("sort must be one of {}", names.join(", ")))
                })?,
        };
        let ascending = match single(query, "order")? {
            None | Some("desc") => false,
            Some("asc") => true,
            Some(_) => return Err(BadParam("order must be asc or desc".into())),
        };
        let pattern = match optional_text(query, "search", MAX_TICKER_LEN)? {
            None | Some("") => None,
            Some(prefix) => Some(prefix_pattern(&prefix.to_lowercase())),
        };
        Ok(TickersRequest {
            sort,
            ascending,
            pattern,
            page: Page::from_query(query)?,
        })
    }
}

/// The `ticker` parameter.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub(super) struct TickerParam {
    /// The ticker, matched in lowercase.
    #[param(min_length = 1, max_length = 256)]
    ticker: String,
}

/// A ticker: its supply, mint progress and holder counts. Amounts are raw
/// integers as strings with `decimals` (18) fractional digits.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(super) struct Brc20Ticker {
    /// The ticker in lowercase.
    tick: String,
    /// The ticker as its deploy inscription spells it.
    original_tick: String,
    /// The fractional digits of every amount: always 18.
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    /// The deploy precision: the most fractional digits an amount may have.
    ticker_decimals: i32,
    max_supply: String,
    minted_supply: String,
    remaining_supply: String,
    burned_supply: String,
    limit_per_mint: String,
    is_self_mint: bool,
    deploy_inscription_id: String,
    deploy_block_height: i32,
    /// The pkScripts with a positive BRC-20 balance, without the BRC2.0
    /// module's pkScript.
    holders: i64,
    /// The EVM addresses with a positive BRC2.0 balance.
    prog_holders: i64,
}

/// A ticker with its BRC2.0 token contract and the database's height.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20TickerInfo {
    #[serde(flatten)]
    ticker: Brc20Ticker,
    /// The BRC2.0 token contract, null before the ticker's first deposit.
    #[schema(nullable)]
    prog_contract_address: String,
    block_height: DbHeight,
}

/// The ticker of a BRC2.0 token contract.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20ContractTicker {
    #[serde(flatten)]
    ticker: Brc20Ticker,
    /// The queried contract, in lowercase.
    prog_contract_address: String,
    /// The ticker hash the contract's `BRC20Created` log registered.
    prog_ticker_hash: String,
    block_height: DbHeight,
}

/// A page of tickers.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20Tickers {
    sort: TickerSort,
    order: OrderName,
    tickers: Vec<Brc20Ticker>,
    offset: i64,
    limit: i64,
    has_more: bool,
    block_height: DbHeight,
}

/// A list's direction.
#[derive(ToSchema)]
#[allow(dead_code)]
#[serde(rename_all = "snake_case")]
enum OrderName {
    Desc,
    Asc,
}

fn sort_schema() -> RefOr<Schema> {
    with_default::<TickerSort>("holders")
}

fn order_schema() -> RefOr<Schema> {
    with_default::<OrderName>("desc")
}

/// The parameters of `/v1/brc20/tickers`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct TickersParams {
    /// The order: `holders`, `prog_holders`, `max_supply`, `mint_progress`
    /// (minted over max supply) or `deploy_height`; ties order by ticker.
    #[param(schema_with = sort_schema)]
    sort: Option<TickerSort>,
    /// The direction.
    #[param(schema_with = order_schema)]
    order: Option<OrderName>,
    /// Keeps the tickers that start with it, in lowercase.
    #[param(max_length = 256)]
    search: Option<String>,
}

/// A holder of a ticker: a Bitcoin address with its BRC-20 balance and the
/// BRC2.0 balance of its EVM address, a pkScript without an address, or an
/// EVM address without a Bitcoin address.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(super) struct Brc20TickerHolder {
    /// The Bitcoin address; null for a pkScript without an address or an
    /// EVM address `evm_addresses` does not map.
    #[schema(nullable)]
    address: String,
    /// The pkScript of the BRC-20 balance, null without one.
    #[schema(nullable)]
    pkscript: String,
    /// The EVM address of the BRC2.0 balance, null without one.
    #[schema(nullable)]
    evm_address: String,
    /// The BRC-20 overall balance.
    balance: String,
    /// The BRC-20 available balance.
    available_balance: String,
    /// The BRC2.0 balance.
    prog_balance: String,
    /// `balance` plus `prog_balance`.
    total_balance: String,
}

/// A page of a ticker's holders.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20TickerHolders {
    /// The lowercase ticker.
    ticker: String,
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    ticker_decimals: i32,
    holders: Vec<Brc20TickerHolder>,
    offset: i64,
    limit: i64,
    has_more: bool,
    block_height: DbHeight,
}

/// A wallet's balance of one ticker.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20WalletBalance {
    tick: String,
    #[schema(minimum = 18, maximum = 18)]
    decimals: i32,
    /// Null for a BRC2.0 balance of a ticker brc20 does not list.
    #[schema(nullable)]
    ticker_decimals: i32,
    balance: String,
    available_balance: String,
    prog_balance: String,
    total_balance: String,
}

/// A page of a wallet's balances.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20WalletBalances {
    /// The Bitcoin address: the one asked for, or an EVM address's mapped
    /// one (null when unmapped).
    #[schema(nullable)]
    address: String,
    /// The EVM address: the one asked for in lowercase, or a Bitcoin
    /// address's mapped one (null when unmapped).
    #[schema(nullable)]
    evm_address: String,
    balances: Vec<Brc20WalletBalance>,
    offset: i64,
    limit: i64,
    has_more: bool,
    block_height: DbHeight,
}

/// A brc20_tickers row with its holder counts.
struct TickerRow {
    tick: String,
    original_tick: String,
    ticker_decimals: i32,
    max_supply: String,
    minted_supply: String,
    remaining_supply: String,
    burned_supply: String,
    limit_per_mint: String,
    is_self_mint: bool,
    deploy_inscription_id: String,
    deploy_block_height: i32,
    holders: i64,
    prog_holders: i64,
}

impl TickerRow {
    fn into_object(self) -> JsObject {
        js_object! {
            "tick" => self.tick,
            "original_tick" => self.original_tick,
            "decimals" => AMOUNT_DECIMALS,
            "ticker_decimals" => self.ticker_decimals,
            "max_supply" => self.max_supply,
            "minted_supply" => self.minted_supply,
            "remaining_supply" => self.remaining_supply,
            "burned_supply" => self.burned_supply,
            "limit_per_mint" => self.limit_per_mint,
            "is_self_mint" => self.is_self_mint,
            "deploy_inscription_id" => self.deploy_inscription_id,
            "deploy_block_height" => self.deploy_block_height,
            "holders" => self.holders as f64,
            "prog_holders" => self.prog_holders as f64,
        }
    }
}

/// The brc20_tickers row of `tick` with its holder counts, or `None` when
/// brc20 has no such ticker.
async fn ticker_row(db: &Db, tick: &str) -> Result<Option<TickerRow>, ApiError> {
    Ok(sqlx::query_as!(
        TickerRow,
        r#"select t.tick, t.original_tick, t.decimals as ticker_decimals,
                t.max_supply::text as "max_supply!",
                (t.max_supply - t.remaining_supply)::text as "minted_supply!",
                t.remaining_supply::text as "remaining_supply!",
                t.burned_supply::text as "burned_supply!",
                t.limit_per_mint::text as "limit_per_mint!",
                t.is_self_mint, t.deploy_inscription_id,
                t.block_height as deploy_block_height,
                coalesce(h.holders, 0) as "holders!",
                coalesce(h.prog_holders, 0) as "prog_holders!"
            from brc20_tickers t
            left join brc20_ticker_holders h on h.tick = t.tick
            where t.tick = $1;"#,
        tick,
    )
    .fetch_optional(db.pool()?)
    .await?)
}

/// A ticker's supply, mint progress, holder counts and BRC2.0 token
/// contract.
#[utoipa::path(
    get,
    path = "/v1/brc20/ticker",
    tag = "brc20",
    params(TickerParam),
    responses(
        (status = 200, description = "The ticker", body = Answer<Brc20TickerInfo>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
        (status = 404, description = "`ticker not found`", body = ErrorBody),
    ),
)]
pub async fn ticker_info(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let tick = match ticker(&query, "ticker") {
        Ok(tick) => tick,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let Some(row) = ticker_row(&db, &tick).await? else {
        return Ok(Reply::error(404, "ticker not found"));
    };
    let prog_contract_address = sqlx::query_scalar!(
        "select contract_address from brc20_prog_tickers
            where ticker = $1
            order by block_height, contract_address
            limit 1;",
        tick,
    )
    .fetch_optional(db.pool()?)
    .await?;
    let mut result = row.into_object();
    result.set("prog_contract_address", prog_contract_address);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// The lowercase EVM address of `key`, as the engine logs addresses.
pub(super) fn evm_address(query: &JsObject, key: &str) -> Result<String, BadParam> {
    let address = text(query, key, MAX_ADDRESS_LEN)?;
    if !is_evm_address(address) {
        return Err(BadParam(format!(
            "{key} must be an EVM address: 0x and 40 hex digits"
        )));
    }
    Ok(address.to_lowercase())
}

/// The ticker a BRC2.0 token contract carries.
///
/// The ticker as `/v1/brc20/ticker` answers it, with `prog_contract_address`
/// the queried contract and `prog_ticker_hash` the ticker hash its
/// `BRC20Created` log registered.
#[utoipa::path(
    get,
    path = "/v1/brc20/ticker/by_contract",
    tag = "brc20",
    params(
        ("contract_address" = String, Query, description = "The token contract: `0x` and 40 hex digits, in any case", pattern = "^0[xX][0-9a-fA-F]{40}$"),
    ),
    responses(
        (status = 200, description = "The ticker", body = Answer<Brc20ContractTicker>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
        (status = 404, description = "`contract not found`, or `ticker not found` for a contract of a ticker brc20 does not list", body = ErrorBody),
    ),
)]
pub async fn ticker_by_contract(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let contract_address = match evm_address(&query, "contract_address") {
        Ok(address) => address,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let contract = sqlx::query!(
        "select ticker, ticker_hash from brc20_prog_tickers where contract_address = $1;",
        contract_address,
    )
    .fetch_optional(db.pool()?)
    .await?;
    let Some(contract) = contract else {
        return Ok(Reply::error(404, "contract not found"));
    };
    let Some(row) = ticker_row(&db, &contract.ticker).await? else {
        return Ok(Reply::error(404, "ticker not found"));
    };
    let mut result = row.into_object();
    result.set("prog_contract_address", contract_address);
    result.set("prog_ticker_hash", contract.ticker_hash);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// A page of tickers, by holders or another order.
///
/// Tickers by `holders` (the default), `prog_holders`, `max_supply`,
/// `mint_progress` (minted over max supply) or `deploy_height`, descending
/// unless `order=asc`, then by ticker; `search` keeps the tickers that
/// start with it.
#[utoipa::path(
    get,
    path = "/v1/brc20/tickers",
    tag = "brc20",
    params(TickersParams, PageParams),
    responses(
        (status = 200, description = "The page", body = Answer<Brc20Tickers>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
    ),
)]
pub async fn tickers(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let request = match TickersRequest::from_query(&query) {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let direction = if request.ascending { "asc" } else { "desc" };
    // One static query: the sort and direction are whitelisted values bound
    // as parameters, and each connection's custom plans fold the CASEs that
    // do not apply. The prefix match uses brc20_tickers_tick_pattern_idx.
    let rows = sqlx::query_as!(
        TickerRow,
        r#"select t.tick, t.original_tick, t.decimals as ticker_decimals,
                t.max_supply::text as "max_supply!",
                (t.max_supply - t.remaining_supply)::text as "minted_supply!",
                t.remaining_supply::text as "remaining_supply!",
                t.burned_supply::text as "burned_supply!",
                t.limit_per_mint::text as "limit_per_mint!",
                t.is_self_mint, t.deploy_inscription_id,
                t.block_height as deploy_block_height,
                coalesce(h.holders, 0) as "holders!",
                coalesce(h.prog_holders, 0) as "prog_holders!"
            from brc20_tickers t
            left join brc20_ticker_holders h on h.tick = t.tick
            where ($1::text is null or t.tick like $1)
            order by
                case when $2::text = 'holders' and $3::text = 'asc' then coalesce(h.holders, 0) end asc,
                case when $2::text = 'holders' and $3::text = 'desc' then coalesce(h.holders, 0) end desc,
                case when $2::text = 'prog_holders' and $3::text = 'asc' then coalesce(h.prog_holders, 0) end asc,
                case when $2::text = 'prog_holders' and $3::text = 'desc' then coalesce(h.prog_holders, 0) end desc,
                case when $2::text = 'max_supply' and $3::text = 'asc' then t.max_supply end asc,
                case when $2::text = 'max_supply' and $3::text = 'desc' then t.max_supply end desc,
                case when $2::text = 'mint_progress' and $3::text = 'asc' then
                    case when t.max_supply = 0 then 0 else (t.max_supply - t.remaining_supply) / t.max_supply end
                end asc,
                case when $2::text = 'mint_progress' and $3::text = 'desc' then
                    case when t.max_supply = 0 then 0 else (t.max_supply - t.remaining_supply) / t.max_supply end
                end desc,
                case when $2::text = 'deploy_height' and $3::text = 'asc' then t.block_height end asc,
                case when $2::text = 'deploy_height' and $3::text = 'desc' then t.block_height end desc,
                t.tick asc
            limit $4 offset $5;"#,
        request.pattern,
        request.sort.name(),
        direction,
        request.page.fetch(),
        request.page.offset,
    )
    .fetch_all(db.pool()?)
    .await?;
    let (rows, has_more) = request.page.split(rows);
    let tickers: Vec<JsValue> = rows
        .into_iter()
        .map(|row| row.into_object().into())
        .collect();
    let mut result = js_object! {
        "sort" => request.sort.name(),
        "order" => direction,
        "tickers" => tickers,
    };
    request.page.fields(&mut result, has_more);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// The deploy precision of a ticker, or `None` when brc20 has no such
/// ticker.
pub(super) async fn ticker_decimals(db: &Db, tick: &str) -> Result<Option<i32>, ApiError> {
    Ok(
        sqlx::query_scalar!("select decimals from brc20_tickers where tick = $1;", tick)
            .fetch_optional(db.pool()?)
            .await?,
    )
}

/// A holder of a ticker: a Bitcoin address with its BRC-20 balance and the
/// BRC2.0 balance of its EVM address, a pkScript without an address, or an
/// EVM address without a Bitcoin address. `/v1/brc20/ticker/holders` and
/// its snapshot list these rows.
pub(super) struct HolderRow {
    pub address: Option<String>,
    pub pkscript: Option<String>,
    pub evm_address: Option<String>,
    pub balance: String,
    pub available_balance: String,
    pub prog_balance: String,
    pub total_balance: String,
}

impl HolderRow {
    pub fn into_object(self) -> JsObject {
        js_object! {
            "address" => self.address,
            "pkscript" => self.pkscript,
            "evm_address" => self.evm_address,
            "balance" => self.balance,
            "available_balance" => self.available_balance,
            "prog_balance" => self.prog_balance,
            "total_balance" => self.total_balance,
        }
    }
}

/// A page of a ticker's holders, BRC-20 and BRC2.0 balances merged.
///
/// The holders by their BRC-20 balance plus their BRC2.0 balance, largest
/// first, then by holder. The BRC2.0 module's pkScript is left out.
#[utoipa::path(
    get,
    path = "/v1/brc20/ticker/holders",
    tag = "brc20",
    params(TickerParam, PageParams),
    responses(
        (status = 200, description = "The page", body = Answer<Brc20TickerHolders>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
        (status = 404, description = "`ticker not found`", body = ErrorBody),
    ),
)]
pub async fn holders(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let (tick, page) =
        match ticker(&query, "ticker").and_then(|tick| Ok((tick, Page::from_query(&query)?))) {
            Ok(request) => request,
            Err(bad) => return Ok(bad.reply()),
        };
    let current_block_height = block_height_of_db(&db).await;
    let Some(deploy_decimals) = ticker_decimals(&db, &tick).await? else {
        return Ok(Reply::error(404, "ticker not found"));
    };
    // Each side reads the ticker's positive balances through its partial
    // index; a BRC-20 wallet and an evm_addresses Bitcoin address are each
    // unique, so the full join pairs each holder's two balances at most
    // once, and (holder, pkscript, evm_address) is a unique key. The
    // snapshot route merges and orders with the same `m` and select.
    let rows = sqlx::query_as!(
        HolderRow,
        r#"with b as (
                select pkscript, nullif(wallet, '') as wallet, overall_balance, available_balance
                from brc20_current_balances
                where tick = $1 and overall_balance > 0 and pkscript <> $2
            ), p as (
                select c.wallet as evm_address, e.btc_address, c.amount
                from brc20_prog_current_balances c
                left join evm_addresses e on e.evm_address = c.wallet
                where c.ticker = $1 and c.amount > 0
            ), m as (
                select coalesce(b.wallet, p.btc_address) as address,
                       b.pkscript, p.evm_address,
                       coalesce(b.overall_balance, 0) as balance,
                       coalesce(b.available_balance, 0) as available_balance,
                       coalesce(p.amount, 0) as prog_balance
                from b full join p on b.wallet = p.btc_address
            )
            select address as "address?", pkscript as "pkscript?", evm_address as "evm_address?",
                   balance::text as "balance!",
                   available_balance::text as "available_balance!",
                   prog_balance::text as "prog_balance!",
                   (balance + prog_balance)::text as "total_balance!"
            from m
            order by balance + prog_balance desc,
                     coalesce(address, pkscript, evm_address) asc,
                     pkscript asc nulls first,
                     evm_address asc nulls first
            limit $3 offset $4;"#,
        tick,
        BRC20_PROG_MODULE_PKSCRIPT,
        page.fetch(),
        page.offset,
    )
    .fetch_all(db.pool()?)
    .await?;
    let (rows, has_more) = page.split(rows);
    let holders: Vec<JsValue> = rows
        .into_iter()
        .map(|row| row.into_object().into())
        .collect();
    let mut result = js_object! {
        "ticker" => tick,
        "decimals" => AMOUNT_DECIMALS,
        "ticker_decimals" => deploy_decimals,
        "holders" => holders,
    };
    page.fields(&mut result, has_more);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// Whether `address` is an EVM address: `0x` and 40 hex digits.
pub fn is_evm_address(address: &str) -> bool {
    address.len() == 42
        && (address.starts_with("0x") || address.starts_with("0X"))
        && address[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// A page of a wallet's tickers, BRC-20 and BRC2.0 balances side by side.
///
/// One row per ticker with a positive balance on either side, by ticker.
/// `address` is a Bitcoin address, whose EVM address comes from
/// `evm_addresses`, or an EVM address, whose Bitcoin address comes from the
/// same table when it has one. A wallet the database does not know answers
/// an empty list.
#[utoipa::path(
    get,
    path = "/v1/brc20/wallet/balances",
    tag = "brc20",
    params(
        ("address" = String, Query, description = "A Bitcoin address, or an EVM address (`0x` and 40 hex digits, in any case)", min_length = 1, max_length = 128),
        PageParams,
    ),
    responses(
        (status = 200, description = "The page", body = Answer<Brc20WalletBalances>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
    ),
)]
pub async fn wallet_balances(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (address, page) = match text(&query, "address", MAX_ADDRESS_LEN)
        .and_then(|address| Ok((address, Page::from_query(&query)?)))
    {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let pool = db.pool()?;
    let (btc_address, evm_address) = if is_evm_address(address) {
        let evm_address = address.to_lowercase();
        let btc_address = sqlx::query_scalar!(
            "select btc_address from evm_addresses where evm_address = $1;",
            evm_address
        )
        .fetch_optional(pool)
        .await?;
        (btc_address, Some(evm_address))
    } else {
        let evm_address = sqlx::query_scalar!(
            "select evm_address from evm_addresses where btc_address = $1;",
            address
        )
        .fetch_optional(pool)
        .await?;
        (Some(address.to_string()), evm_address)
    };
    let rows = sqlx::query!(
        r#"with b as (
                select tick, overall_balance, available_balance
                from brc20_current_balances
                where wallet = $1 and overall_balance > 0 and pkscript <> $5
            ), p as (
                select ticker as tick, amount
                from brc20_prog_current_balances
                where wallet = $2 and amount > 0
            )
            select coalesce(b.tick, p.tick) as "tick!",
                   t.decimals as "decimals?",
                   coalesce(b.overall_balance, 0)::text as "balance!",
                   coalesce(b.available_balance, 0)::text as "available_balance!",
                   coalesce(p.amount, 0)::text as "prog_balance!",
                   (coalesce(b.overall_balance, 0) + coalesce(p.amount, 0))::text as "total_balance!"
            from b full join p on b.tick = p.tick
            left join brc20_tickers t on t.tick = coalesce(b.tick, p.tick)
            order by 1 asc
            limit $3 offset $4;"#,
        btc_address,
        evm_address,
        page.fetch(),
        page.offset,
        BRC20_PROG_MODULE_PKSCRIPT,
    )
    .fetch_all(pool)
    .await?;
    let (rows, has_more) = page.split(rows);
    let balances: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            js_object! {
                "tick" => row.tick,
                "decimals" => AMOUNT_DECIMALS,
                "ticker_decimals" => row.decimals,
                "balance" => row.balance,
                "available_balance" => row.available_balance,
                "prog_balance" => row.prog_balance,
                "total_balance" => row.total_balance,
            }
            .into()
        })
        .collect();
    let mut result = js_object! {
        "address" => btc_address,
        "evm_address" => evm_address,
        "balances" => balances,
    };
    page.fields(&mut result, has_more);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}
