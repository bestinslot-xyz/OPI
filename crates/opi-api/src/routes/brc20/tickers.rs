//! The ticker routes: `/v1/brc20/ticker`, `/v1/brc20/tickers`,
//! `/v1/brc20/ticker/holders` and `/v1/brc20/wallet/balances`.
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
        reply::{ApiError, Reply},
    },
    axum::extract::State,
};

pub use crate::routes::paging::{
    BadParam, DEFAULT_LIMIT, MAX_LIMIT, MAX_OFFSET, integer, prefix_pattern, required, single,
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
    fn split<T>(&self, mut rows: Vec<T>) -> (Vec<T>, bool) {
        let has_more = rows.len() as i64 > self.limit;
        rows.truncate(self.limit as usize);
        (rows, has_more)
    }

    /// The page fields of a list's result.
    fn fields(&self, result: &mut JsObject, has_more: bool) {
        result.set("offset", self.offset as f64);
        result.set("limit", self.limit as f64);
        result.set("has_more", has_more);
    }
}

/// The lowercase ticker of `key`, as brc20 stores tickers.
pub(super) fn ticker(query: &JsObject, key: &str) -> Result<String, BadParam> {
    Ok(required(query, key)?.to_lowercase())
}

/// The order of `/v1/brc20/tickers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        let pattern = match single(query, "search")? {
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

/// `/v1/brc20/ticker?ticker=`: a ticker's supply, mint progress, holder
/// counts and BRC2.0 token contract (null before its first deposit).
pub async fn ticker_info(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let tick = match ticker(&query, "ticker") {
        Ok(tick) => tick,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let row = sqlx::query!(
        r#"select t.tick, t.original_tick, t.decimals,
                t.max_supply::text as "max_supply!",
                (t.max_supply - t.remaining_supply)::text as "minted_supply!",
                t.remaining_supply::text as "remaining_supply!",
                t.burned_supply::text as "burned_supply!",
                t.limit_per_mint::text as "limit_per_mint!",
                t.is_self_mint, t.deploy_inscription_id, t.block_height,
                coalesce(h.holders, 0) as "holders!",
                coalesce(h.prog_holders, 0) as "prog_holders!",
                (select p.contract_address from brc20_prog_tickers p
                  where p.ticker = t.tick
                  order by p.block_height, p.contract_address
                  limit 1) as "prog_contract_address?"
            from brc20_tickers t
            left join brc20_ticker_holders h on h.tick = t.tick
            where t.tick = $1;"#,
        tick,
    )
    .fetch_optional(db.pool()?)
    .await?;
    let Some(row) = row else {
        return Ok(Reply::error(404, "ticker not found"));
    };
    let mut result = TickerRow {
        tick: row.tick,
        original_tick: row.original_tick,
        ticker_decimals: row.decimals,
        max_supply: row.max_supply,
        minted_supply: row.minted_supply,
        remaining_supply: row.remaining_supply,
        burned_supply: row.burned_supply,
        limit_per_mint: row.limit_per_mint,
        is_self_mint: row.is_self_mint,
        deploy_inscription_id: row.deploy_inscription_id,
        deploy_block_height: row.block_height,
        holders: row.holders,
        prog_holders: row.prog_holders,
    }
    .into_object();
    result.set("prog_contract_address", row.prog_contract_address);
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// `/v1/brc20/tickers?sort=&order=&search=&offset=&limit=`: tickers by
/// `holders` (the default), `prog_holders`, `max_supply`, `mint_progress`
/// (minted over max supply) or `deploy_height`, descending unless
/// `order=asc`, then by ticker; `search` keeps the tickers that start with
/// it.
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
    let rows = sqlx::query!(
        r#"select t.tick, t.original_tick, t.decimals,
                t.max_supply::text as "max_supply!",
                (t.max_supply - t.remaining_supply)::text as "minted_supply!",
                t.remaining_supply::text as "remaining_supply!",
                t.burned_supply::text as "burned_supply!",
                t.limit_per_mint::text as "limit_per_mint!",
                t.is_self_mint, t.deploy_inscription_id, t.block_height,
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
        .map(|row| {
            TickerRow {
                tick: row.tick,
                original_tick: row.original_tick,
                ticker_decimals: row.decimals,
                max_supply: row.max_supply,
                minted_supply: row.minted_supply,
                remaining_supply: row.remaining_supply,
                burned_supply: row.burned_supply,
                limit_per_mint: row.limit_per_mint,
                is_self_mint: row.is_self_mint,
                deploy_inscription_id: row.deploy_inscription_id,
                deploy_block_height: row.block_height,
                holders: row.holders,
                prog_holders: row.prog_holders,
            }
            .into_object()
            .into()
        })
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

/// `/v1/brc20/ticker/holders?ticker=&offset=&limit=`: the holders of a
/// ticker by their BRC-20 balance plus their BRC2.0 balance, largest first,
/// then by holder.
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
    // once, and (holder, pkscript, evm_address) is a unique key.
    let rows = sqlx::query!(
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
        .map(|row| {
            js_object! {
                "address" => row.address,
                "pkscript" => row.pkscript,
                "evm_address" => row.evm_address,
                "balance" => row.balance,
                "available_balance" => row.available_balance,
                "prog_balance" => row.prog_balance,
                "total_balance" => row.total_balance,
            }
            .into()
        })
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

/// `/v1/brc20/wallet/balances?address=&offset=&limit=`: every ticker a
/// wallet holds, its BRC-20 and BRC2.0 balances side by side, by ticker.
/// `address` is a Bitcoin address, whose EVM address comes from
/// `evm_addresses`, or an EVM address, whose Bitcoin address comes from the
/// same table when it has one.
pub async fn wallet_balances(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (address, page) = match required(&query, "address")
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
