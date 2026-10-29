//! The rune routes: `/v1/runes/rune`, `/v1/runes/runes`,
//! `/v1/runes/rune/holders` and `/v1/runes/wallet/balances`.
//!
//! They answer `{"error":null,"result":...}`. A missing or invalid parameter
//! answers `400 {"error":"<reason>","result":null}` and an unknown rune
//! `404 {"error":"rune not found","result":null}`.
//!
//! A rune is named by its id (`block:tx`) or its name, spaced (`•` or `.`)
//! or not, in any case, read with ord's parsers ([`opi_runes::names`]). A
//! spaced name must carry the rune's own spacers. Amounts are raw integers
//! as strings with the rune's `divisibility` fractional digits.
//!
//! Holders and balances come from the runes module's rollup:
//! `runes_current_balances` (per rune and pkScript, the sum over unspent
//! outpoints, positive only) and `runes_holders` (its rows per rune), which
//! the module keeps per block and on reorg ([`opi_runes::holders`]).
//!
//! Lists page by key: `limit` (default 100, at most 500) and `after`, the
//! `next_after` of the previous page, an opaque cursor naming the last row's
//! place in the order. A page holds the rows after it, so pages never repeat
//! or skip a row whose place does not change. `has_more` tells whether more
//! follow and `next_after` is null on the last page.

use {
    super::block_height_of_db,
    crate::{
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        openapi::{Answer, DbHeight, ErrorBody, LimitParam, with_default},
        reply::{ApiError, Reply},
        routes::paging::{
            BadParam, DEFAULT_LIMIT, MAX_CURSOR_LEN, MAX_LIMIT, MAX_RUNE_LEN, Wallet, integer,
            optional_text, prefix_pattern, single, split_page, text,
        },
    },
    axum::extract::State,
    opi_runes::names::{RuneRef, name_prefix, parse_rune, spaced_name, symbol_char},
    utoipa::{
        IntoParams, ToSchema,
        openapi::{RefOr, schema::Schema},
    },
};

#[cfg(test)]
mod tests;

/// The `rune` parameter.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub(super) struct RuneParam {
    /// The rune: its id (`block:tx`), or its name, spaced (`•` or `.`) or
    /// not, in any case. A spaced name must carry the rune's spacers.
    #[param(min_length = 1, max_length = 128)]
    rune: String,
}

/// The rune `rune` names, as a lookup.
pub(super) fn rune_param(query: &JsObject) -> Result<RuneRef, BadParam> {
    parse_rune(text(query, "rune", MAX_RUNE_LEN)?).ok_or_else(|| {
        BadParam(
            "rune must be a rune id (block:tx) or a rune name (letters A-Z and spacers)".into(),
        )
    })
}

/// The `limit` and `after` of a list paged by key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPage<C> {
    pub limit: i64,
    pub after: Option<C>,
}

impl<C> KeyPage<C> {
    /// `limit`, and `after` read by `cursor` (`None` for a cursor it
    /// refuses).
    fn from_query(query: &JsObject, cursor: fn(&str) -> Option<C>) -> Result<Self, BadParam> {
        let limit = integer(query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?;
        let after = match optional_text(query, "after", MAX_CURSOR_LEN)? {
            None | Some("") => None,
            Some(text) => Some(cursor(text).ok_or_else(|| {
                BadParam("after must be the next_after of a previous page".into())
            })?),
        };
        Ok(KeyPage { limit, after })
    }
}

/// The `after` parameter of a list paged by key.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct AfterParam {
    /// The previous page's `next_after`: the page holds the rows after it.
    #[param(max_length = 256)]
    after: Option<String>,
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.len() <= 80 && text.bytes().all(|b| b.is_ascii_digit())
}

/// A numeric as Postgres prints a sort key: an integer, `-1`, or a decimal
/// fraction.
fn is_number(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    match unsigned.split_once('.') {
        None => is_digits(unsigned),
        Some((whole, fraction)) => is_digits(whole) && is_digits(fraction),
    }
}

/// The order of `/v1/runes/runes`.
// Its schema names each order in snake case, as `RuneSort::name` does (a
// unit test compares them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuneSort {
    Holders,
    Supply,
    MintProgress,
    EtchingHeight,
}

impl RuneSort {
    pub const ALL: [RuneSort; 4] = [
        RuneSort::Holders,
        RuneSort::Supply,
        RuneSort::MintProgress,
        RuneSort::EtchingHeight,
    ];

    /// The parameter value, which the query's sort key also names.
    pub fn name(self) -> &'static str {
        match self {
            RuneSort::Holders => "holders",
            RuneSort::Supply => "supply",
            RuneSort::MintProgress => "mint_progress",
            RuneSort::EtchingHeight => "etching_height",
        }
    }
}

/// A list's direction.
#[derive(ToSchema)]
#[allow(dead_code)]
#[serde(rename_all = "snake_case")]
enum RunesOrder {
    Desc,
    Asc,
}

fn sort_schema() -> RefOr<Schema> {
    with_default::<RuneSort>("holders")
}

fn order_schema() -> RefOr<Schema> {
    with_default::<RunesOrder>("desc")
}

/// The parameters of `/v1/runes/runes`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct RunesParams {
    /// The order: `holders`, `supply` (premine plus minted), `mint_progress`
    /// (mints over the mint cap; runes without a cap sort below every other)
    /// or `etching_height`; ties order by rune number in the same direction.
    #[param(schema_with = sort_schema)]
    sort: Option<RuneSort>,
    /// The direction.
    #[param(schema_with = order_schema)]
    order: Option<RunesOrder>,
    /// Keeps the runes whose name starts with it, in any case. Written
    /// without spacers it matches the letters (`UNCOMMONG`); with spacers
    /// it matches the spaced name (`UNCOMMON•G`).
    #[param(max_length = 128)]
    search: Option<String>,
}

/// The `/v1/runes/runes` cursor: the last row's sort key and rune number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunesCursor {
    pub key: String,
    pub number: String,
}

fn runes_cursor(text: &str) -> Option<RunesCursor> {
    let (key, number) = text.rsplit_once(':')?;
    (is_number(key) && is_digits(number)).then(|| RunesCursor {
        key: key.into(),
        number: number.into(),
    })
}

/// What `/v1/runes/runes` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunesRequest {
    pub sort: RuneSort,
    pub ascending: bool,
    /// The LIKE pattern of the search's letters, the spacer mask and the
    /// spacer bits under it.
    pub search: Option<(String, i64, i64)>,
    pub page: KeyPage<RunesCursor>,
}

impl RunesRequest {
    pub fn from_query(query: &JsObject) -> Result<RunesRequest, BadParam> {
        let sort = match single(query, "sort")? {
            None => RuneSort::Holders,
            Some(name) => RuneSort::ALL
                .into_iter()
                .find(|sort| sort.name() == name)
                .ok_or_else(|| {
                    let names: Vec<&str> = RuneSort::ALL.iter().map(|s| s.name()).collect();
                    BadParam(format!("sort must be one of {}", names.join(", ")))
                })?,
        };
        let ascending = match single(query, "order")? {
            None | Some("desc") => false,
            Some("asc") => true,
            Some(_) => return Err(BadParam("order must be asc or desc".into())),
        };
        let search = match optional_text(query, "search", MAX_RUNE_LEN)? {
            None | Some("") => None,
            Some(prefix) => {
                let prefix = name_prefix(prefix).ok_or_else(|| {
                    BadParam("search must be rune name letters A-Z, with spacers between".into())
                })?;
                Some((prefix_pattern(&prefix.letters), prefix.mask, prefix.spacers))
            }
        };
        Ok(RunesRequest {
            sort,
            ascending,
            search,
            page: KeyPage::from_query(query, runes_cursor)?,
        })
    }
}

/// A rune's mint terms; each bound is null when the etching set none.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesTerms {
    /// The amount each mint makes.
    #[schema(nullable)]
    amount: String,
    /// The most mints.
    #[schema(nullable)]
    cap: String,
    /// The first height a mint may be in.
    #[schema(nullable)]
    height_start: i64,
    /// The height mints end before.
    #[schema(nullable)]
    height_end: i64,
    /// The first height a mint may be in, counted from the etching block.
    #[schema(nullable)]
    offset_start: i64,
    /// The height mints end before, counted from the etching block.
    #[schema(nullable)]
    offset_end: i64,
}

/// A rune: its etching, supply, mint terms and progress, and holder count.
/// Amounts are raw integers as strings with `divisibility` fractional
/// digits.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesRune {
    /// The rune id, `block:tx`.
    rune_id: String,
    /// The name's letters, as stored.
    rune_name: String,
    /// The name with its spacers (`•`), as ord displays it.
    spaced_rune_name: String,
    /// The rune's number, its place in etching order, as a string.
    number: String,
    /// The fractional digits of every amount of the rune.
    divisibility: i32,
    /// The currency symbol, null without one.
    #[schema(nullable)]
    symbol: String,
    /// ord's spacer bitfield: bit `i` puts a spacer after letter `i`.
    spacers: i64,
    /// The etching transaction.
    etching: String,
    etching_block_height: i32,
    /// The etching block's time in seconds.
    timestamp: i64,
    turbo: bool,
    premine: String,
    mints: String,
    burned: String,
    /// The premine plus the minted amount (`mints` times `terms.amount`);
    /// burns are not subtracted.
    supply: String,
    /// The premine plus `terms.cap` times `terms.amount`.
    max_supply: String,
    /// Null for a rune without mint terms.
    #[schema(nullable)]
    terms: RunesTerms,
    /// `mints` over `terms.cap`, from 0 to 1; null without a cap above 0.
    #[schema(nullable)]
    mint_progress: f64,
    /// The pkScripts with a positive balance over unspent outpoints.
    holders: i64,
}

/// A rune and the database's height.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesRuneInfo {
    #[serde(flatten)]
    rune: RunesRune,
    block_height: DbHeight,
}

/// A page of runes.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesList {
    sort: RuneSort,
    order: RunesOrder,
    runes: Vec<RunesRune>,
    limit: i64,
    has_more: bool,
    /// The `after` of the next page, null on the last page.
    #[schema(nullable)]
    next_after: String,
    block_height: DbHeight,
}

/// A runes_id_to_entry row with its supply, mint progress and holder count.
pub(super) struct RuneRow {
    pub rune_id: String,
    pub rune_name: String,
    pub spacers: i64,
    pub number: String,
    pub divisibility: i32,
    pub symbol: Option<String>,
    pub etching: String,
    pub genesis_height: i32,
    pub timestamp: i64,
    pub turbo: bool,
    pub premine: String,
    pub mints: String,
    pub burned: String,
    pub supply: String,
    pub max_supply: String,
    pub terms_amount: Option<String>,
    pub terms_cap: Option<String>,
    pub terms_height_l: Option<i64>,
    pub terms_height_h: Option<i64>,
    pub terms_offset_l: Option<i64>,
    pub terms_offset_h: Option<i64>,
    pub mint_progress: Option<f64>,
    pub holders: i64,
    pub sort_key: Option<String>,
}

impl RuneRow {
    fn into_object(self) -> JsObject {
        let has_terms = self.terms_amount.is_some()
            || self.terms_cap.is_some()
            || self.terms_height_l.is_some()
            || self.terms_height_h.is_some()
            || self.terms_offset_l.is_some()
            || self.terms_offset_h.is_some();
        let terms = has_terms.then(|| {
            js_object! {
                "amount" => self.terms_amount,
                "cap" => self.terms_cap,
                "height_start" => self.terms_height_l.map(|v| v as f64),
                "height_end" => self.terms_height_h.map(|v| v as f64),
                "offset_start" => self.terms_offset_l.map(|v| v as f64),
                "offset_end" => self.terms_offset_h.map(|v| v as f64),
            }
        });
        let spaced = spaced_name(&self.rune_name, self.spacers);
        js_object! {
            "rune_id" => self.rune_id,
            "rune_name" => self.rune_name,
            "spaced_rune_name" => spaced,
            "number" => self.number,
            "divisibility" => self.divisibility,
            "symbol" => self.symbol.as_deref().and_then(symbol_char).map(String::from),
            "spacers" => self.spacers as f64,
            "etching" => self.etching,
            "etching_block_height" => self.genesis_height,
            "timestamp" => self.timestamp as f64,
            "turbo" => self.turbo,
            "premine" => self.premine,
            "mints" => self.mints,
            "burned" => self.burned,
            "supply" => self.supply,
            "max_supply" => self.max_supply,
            "terms" => terms,
            "mint_progress" => self.mint_progress,
            "holders" => self.holders as f64,
        }
    }
}

/// The rune `rune` names, or `None` when the database has no such rune.
pub(super) async fn find_rune(db: &Db, rune: &RuneRef) -> Result<Option<RuneRow>, ApiError> {
    let (rune_id, rune_name, spacers) = match rune {
        RuneRef::Id(id) => (Some(id.as_str()), None, None),
        RuneRef::Name { name, spacers } => (None, Some(name.as_str()), *spacers),
    };
    let row = sqlx::query_as!(
        RuneRow,
        r#"select e.rune_id, e.rune_name, e.spacers, e."number"::text as "number!",
                e.divisibility, e.symbol, e.etching, e.genesis_height,
                extract(epoch from e."timestamp")::int8 as "timestamp!", e.turbo,
                e.premine::text as "premine!", e.mints::text as "mints!",
                e.burned::text as "burned!",
                (e.premine + e.mints * coalesce(e.terms_amount, 0))::text as "supply!",
                (e.premine + coalesce(e.terms_cap, 0) * coalesce(e.terms_amount, 0))::text as "max_supply!",
                e.terms_amount::text as terms_amount, e.terms_cap::text as terms_cap,
                e.terms_height_l, e.terms_height_h, e.terms_offset_l, e.terms_offset_h,
                case when e.terms_cap > 0 then (e.mints / e.terms_cap)::float8 end as mint_progress,
                coalesce(h.holders, 0) as "holders!",
                null::text as sort_key
            from runes_id_to_entry e
            left join runes_holders h on h.rune_id = e.rune_id
            where ($1::text is null or e.rune_id = $1)
              and ($2::text is null or e.rune_name = $2);"#,
        rune_id,
        rune_name,
    )
    .fetch_optional(db.pool()?)
    .await?;
    Ok(row.filter(|row| spacers.is_none_or(|spacers| i64::from(spacers) == row.spacers)))
}

/// A rune's etching, supply, mint terms and progress, and holder count.
///
/// The rune is named by its id or its name, spaced or not, in any case.
#[utoipa::path(
    get,
    path = "/v1/runes/rune",
    tag = "runes",
    params(RuneParam),
    responses(
        (status = 200, description = "The rune", body = Answer<RunesRuneInfo>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
        (status = 404, description = "`rune not found`", body = ErrorBody),
    ),
)]
pub async fn rune_info(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let rune = match rune_param(&query) {
        Ok(rune) => rune,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let Some(row) = find_rune(&db, &rune).await? else {
        return Ok(Reply::error(404, "rune not found"));
    };
    let mut result = row.into_object();
    result.set("block_height", current_block_height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// A page of runes, by holders or another order.
///
/// Runes by `holders` (the default), `supply`, `mint_progress` or
/// `etching_height`, descending unless `order=asc`, then by rune number;
/// `search` keeps the runes whose name starts with it.
#[utoipa::path(
    get,
    path = "/v1/runes/runes",
    tag = "runes",
    params(RunesParams, AfterParam, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<RunesList>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
    ),
)]
pub async fn runes(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let request = match RunesRequest::from_query(&query) {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let direction = if request.ascending { "asc" } else { "desc" };
    let (pattern, mask, spacers) = match &request.search {
        Some((pattern, mask, spacers)) => (Some(pattern.as_str()), *mask, *spacers),
        None => (None, 0, 0),
    };
    let (after_key, after_number) = match &request.page.after {
        Some(cursor) => (Some(cursor.key.as_str()), Some(cursor.number.as_str())),
        None => (None, None),
    };
    // One static query: the sort and direction are whitelisted values bound
    // as parameters, and each connection's custom plans fold the CASEs that
    // do not apply. The search uses runes_id_to_entry_rune_name_pattern_idx.
    // Every order sorts the matching runes, which a page reads whole.
    let rows = sqlx::query_as!(
        RuneRow,
        r#"with r as (
                select e.*, coalesce(h.holders, 0) as holder_count,
                    case $2::text
                        when 'holders' then coalesce(h.holders, 0)::numeric
                        when 'supply' then e.premine + e.mints * coalesce(e.terms_amount, 0)
                        when 'mint_progress' then
                            case when e.terms_cap > 0 then e.mints / e.terms_cap else -1 end
                        else e.genesis_height::numeric
                    end as sort_key
                from runes_id_to_entry e
                left join runes_holders h on h.rune_id = e.rune_id
                where ($1::text is null or (e.rune_name like $1 and (e.spacers & $6::int8) = $7::int8))
            )
            select r.rune_id, r.rune_name, r.spacers, r."number"::text as "number!",
                r.divisibility, r.symbol, r.etching, r.genesis_height,
                extract(epoch from r."timestamp")::int8 as "timestamp!", r.turbo,
                r.premine::text as "premine!", r.mints::text as "mints!",
                r.burned::text as "burned!",
                (r.premine + r.mints * coalesce(r.terms_amount, 0))::text as "supply!",
                (r.premine + coalesce(r.terms_cap, 0) * coalesce(r.terms_amount, 0))::text as "max_supply!",
                r.terms_amount::text as terms_amount, r.terms_cap::text as terms_cap,
                r.terms_height_l, r.terms_height_h, r.terms_offset_l, r.terms_offset_h,
                case when r.terms_cap > 0 then (r.mints / r.terms_cap)::float8 end as mint_progress,
                r.holder_count as "holders!",
                r.sort_key::text as sort_key
            from r
            where $4::text is null
               or ($3::text = 'desc' and (r.sort_key, r."number") < ($4::text::numeric, $5::text::numeric))
               or ($3::text = 'asc' and (r.sort_key, r."number") > ($4::text::numeric, $5::text::numeric))
            order by
                case when $3::text = 'asc' then r.sort_key end asc,
                case when $3::text = 'asc' then r."number" end asc,
                r.sort_key desc,
                r."number" desc
            limit $8;"#,
        pattern,
        request.sort.name(),
        direction,
        after_key,
        after_number,
        mask,
        spacers,
        request.page.limit + 1,
    )
    .fetch_all(db.pool()?)
    .await?;
    let (rows, has_more) = split_page(rows, request.page.limit);
    let next_after = if has_more {
        rows.last()
            .map(|row| format!("{}:{}", row.sort_key.as_deref().unwrap_or(""), row.number))
    } else {
        None
    };
    let runes: Vec<JsValue> = rows
        .into_iter()
        .map(|row| row.into_object().into())
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "sort" => request.sort.name(),
            "order" => direction,
            "runes" => runes,
            "limit" => request.page.limit as f64,
            "has_more" => has_more,
            "next_after" => next_after,
            "block_height" => current_block_height,
        },
    }))
}

/// A holder of a rune.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesRuneHolder {
    /// The pkScript's address; null for a script without one.
    #[schema(nullable)]
    address: String,
    pkscript: String,
    /// The sum of the pkScript's unspent outpoints' balances of the rune.
    balance: String,
}

/// A page of a rune's holders.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesRuneHolders {
    rune_id: String,
    rune_name: String,
    spaced_rune_name: String,
    divisibility: i32,
    /// Largest balance first, then by pkScript, descending.
    holders: Vec<RunesRuneHolder>,
    limit: i64,
    has_more: bool,
    /// The `after` of the next page, null on the last page.
    #[schema(nullable)]
    next_after: String,
    block_height: DbHeight,
}

/// The `/v1/runes/rune/holders` cursor: the last row's balance and
/// pkScript.
fn holders_cursor(text: &str) -> Option<(String, String)> {
    let (balance, pkscript) = text.split_once(':')?;
    let hex = pkscript.len() % 2 == 0 && pkscript.bytes().all(|b| b.is_ascii_hexdigit());
    (is_digits(balance) && hex).then(|| (balance.into(), pkscript.to_ascii_lowercase()))
}

/// A page of a rune's holders, largest balance first.
///
/// A holder is a pkScript with a positive balance of the rune over its
/// unspent outpoints; ties order by pkScript, descending.
#[utoipa::path(
    get,
    path = "/v1/runes/rune/holders",
    tag = "runes",
    params(RuneParam, AfterParam, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<RunesRuneHolders>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
        (status = 404, description = "`rune not found`", body = ErrorBody),
    ),
)]
pub async fn rune_holders(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (rune, page) = match rune_param(&query)
        .and_then(|rune| Ok((rune, KeyPage::from_query(&query, holders_cursor)?)))
    {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let Some(rune) = find_rune(&db, &rune).await? else {
        return Ok(Reply::error(404, "rune not found"));
    };
    let (after_balance, after_pkscript) = match &page.after {
        Some((balance, pkscript)) => (Some(balance.as_str()), Some(pkscript.as_str())),
        None => (None, None),
    };
    // A backward range scan of runes_current_balances_rune_id_balance_idx
    // from the cursor: a page costs the same at any depth.
    let rows = sqlx::query!(
        r#"select c.wallet_addr as "wallet_addr?", c.pkscript, c.balance::text as "balance!"
            from runes_current_balances c
            where c.rune_id = $1
              and ($2::text is null or (c.balance, c.pkscript) < ($2::text::numeric, $3::text))
            order by c.balance desc, c.pkscript desc
            limit $4;"#,
        rune.rune_id,
        after_balance,
        after_pkscript,
        page.limit + 1,
    )
    .fetch_all(db.pool()?)
    .await?;
    let (rows, has_more) = split_page(rows, page.limit);
    let next_after = if has_more {
        rows.last()
            .map(|row| format!("{}:{}", row.balance, row.pkscript))
    } else {
        None
    };
    let holders: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            js_object! {
                "address" => row.wallet_addr,
                "pkscript" => row.pkscript,
                "balance" => row.balance,
            }
            .into()
        })
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "rune_id" => rune.rune_id,
            "rune_name" => rune.rune_name.clone(),
            "spaced_rune_name" => spaced_name(&rune.rune_name, rune.spacers),
            "divisibility" => rune.divisibility,
            "holders" => holders,
            "limit" => page.limit as f64,
            "has_more" => has_more,
            "next_after" => next_after,
            "block_height" => current_block_height,
        },
    }))
}

/// The wallet of a wallet route: exactly one of `address` and `pkscript`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub(super) struct WalletParams {
    /// The wallet's address.
    #[param(max_length = 128)]
    address: Option<String>,
    /// The wallet's pkScript, hex.
    #[param(max_length = 20000, pattern = "^([0-9a-fA-F]{2})*$")]
    pkscript: Option<String>,
}

/// A wallet's balance of one rune.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesWalletBalance {
    rune_id: String,
    rune_name: String,
    spaced_rune_name: String,
    /// The rune's number, its place in etching order, as a string.
    number: String,
    divisibility: i32,
    #[schema(nullable)]
    symbol: String,
    /// The sum of the wallet's unspent outpoints' balances of the rune.
    balance: String,
}

/// A page of a wallet's rune balances.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesWalletBalances {
    /// The address asked for; null for a `pkscript` request.
    #[schema(nullable)]
    address: String,
    /// The lowercase pkScript asked for; null for an `address` request.
    #[schema(nullable)]
    pkscript: String,
    /// By rune number.
    balances: Vec<RunesWalletBalance>,
    limit: i64,
    has_more: bool,
    /// The `after` of the next page, null on the last page.
    #[schema(nullable)]
    next_after: String,
    block_height: DbHeight,
}

fn number_cursor(text: &str) -> Option<String> {
    is_digits(text).then(|| text.to_string())
}

/// A page of the runes a wallet holds, with its balances.
///
/// One row per rune with a positive balance over the wallet's unspent
/// outpoints, by rune number. A wallet the database does not know answers
/// an empty list.
#[utoipa::path(
    get,
    path = "/v1/runes/wallet/balances",
    tag = "runes",
    params(WalletParams, AfterParam, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<RunesWalletBalances>),
        (status = 400, description = "A missing or invalid parameter, or both or neither of `address` and `pkscript`", body = ErrorBody),
    ),
)]
pub async fn wallet_balances(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (wallet, page) = match Wallet::from_query(&query)
        .and_then(|wallet| Ok((wallet, KeyPage::from_query(&query, number_cursor)?)))
    {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let current_block_height = block_height_of_db(&db).await;
    let (address, pkscript) = match wallet {
        Wallet::Address(address) => (Some(address), None),
        Wallet::PkScript(pkscript) => (None, Some(pkscript)),
    };
    // The wallet's rows come through the pkscript or wallet_addr index of
    // runes_current_balances; a page sorts the wallet's runes.
    let rows = sqlx::query!(
        r#"select c.rune_id, e.rune_name, e.spacers, e."number"::text as "number!",
                e.divisibility, e.symbol, c.balance::text as "balance!"
            from runes_current_balances c
            join runes_id_to_entry e on e.rune_id = c.rune_id
            where ($1::text is null or c.pkscript = $1)
              and ($2::text is null or c.wallet_addr = $2)
              and ($3::text is null or e."number" > $3::text::numeric)
            order by e."number" asc
            limit $4;"#,
        pkscript,
        address,
        page.after,
        page.limit + 1,
    )
    .fetch_all(db.pool()?)
    .await?;
    let (rows, has_more) = split_page(rows, page.limit);
    let next_after = if has_more {
        rows.last().map(|row| row.number.clone())
    } else {
        None
    };
    let balances: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            let spaced = spaced_name(&row.rune_name, row.spacers);
            js_object! {
                "rune_id" => row.rune_id,
                "rune_name" => row.rune_name,
                "spaced_rune_name" => spaced,
                "number" => row.number,
                "divisibility" => row.divisibility,
                "symbol" => row.symbol.as_deref().and_then(symbol_char).map(String::from),
                "balance" => row.balance,
            }
            .into()
        })
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "address" => address,
            "pkscript" => pkscript,
            "balances" => balances,
            "limit" => page.limit as f64,
            "has_more" => has_more,
            "next_after" => next_after,
            "block_height" => current_block_height,
        },
    }))
}
