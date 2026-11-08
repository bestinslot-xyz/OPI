//! The runes module's routes, including the rune, holder, wallet and feed
//! routes of [`catalog`] and [`feeds`], which have no JavaScript
//! counterpart.
//!
//! Each other handler follows its JavaScript counterpart line by line, as
//! the brc20 routes do. get_block_height_of_db answers -1 when its query fails.
//! numeric columns are cast to text under the column's own name and decoded
//! with [`pg_json::text`]; numeric[] decodes to numbers, as node-pg's
//! array parser reads it with parseFloat.

use {
    super::ModuleRoutes,
    super::no_row,
    crate::{
        config::Module,
        db::Db,
        extract::{ClientIp, JsQuery},
        js::{JsObject, JsValue, sort_by_number},
        js_object,
        openapi::{Answer, DbHeight, ErrorBody, ListBlockHeight, Null, NumericText, QueryEcho},
        params,
        pg_json::{self, ToJs, oid},
        reply::{ApiError, Reply},
    },
    axum::extract::State,
    utoipa::ToSchema,
    utoipa_axum::{router::UtoipaMethodRouterExt, routes},
};

mod catalog;
mod feeds;

/// The 503 answer of a rune route before the runes module built its API
/// tables.
pub const TABLES_NOT_BUILT: &str =
    "the runes API tables are not built yet; start opi-indexer run once";

/// The state of the rune routes of [`catalog`] and [`feeds`]: the database
/// and whether the runes module's API tables and indexes
/// ([`opi_runes::holders`]) exist there, which the module builds at its
/// start. Until it first finds them all, each request checks with one query
/// of a `to_regclass` lookup per relation; from then on the answer is cached.
#[derive(Clone)]
pub(super) struct RunesTables {
    pub db: Db,
    built: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RunesTables {
    fn new(db: Db) -> Self {
        RunesTables {
            db,
            built: Default::default(),
        }
    }

    /// The 503 answer while the tables are missing, `None` once they exist.
    pub async fn unbuilt(&self) -> Result<Option<Reply>, ApiError> {
        use std::sync::atomic::Ordering;
        if self.built.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let relations: Vec<String> = ["runes_current_balances", "runes_holders"]
            .into_iter()
            .chain(opi_runes::holders::INDEXES.iter().map(|(name, _)| *name))
            .map(|name| format!("public.{name}"))
            .collect();
        let built = sqlx::query_scalar!(
            r#"select bool_and(to_regclass(r) is not null) as "built!" from unnest($1::text[]) r;"#,
            &relations,
        )
        .fetch_one(self.db.pool()?)
        .await?;
        if !built {
            return Ok(Some(Reply::error(503, TABLES_NOT_BUILT)));
        }
        self.built.store(true, Ordering::Relaxed);
        Ok(None)
    }
}

pub fn routes(db: &Db, _settings: &super::RouteSettings) -> ModuleRoutes {
    let tables = RunesTables::new(db.clone());
    ModuleRoutes::new()
        .routes(routes!(ip))
        .routes(routes!(block_height))
        .routes(routes!(balance_on_block))
        .routes(routes!(activity_on_block))
        .routes(routes!(get_current_balance_of_wallet))
        .routes(routes!(get_unspent_rune_outpoints_of_wallet))
        .routes(routes!(holders))
        .routes(routes!(get_hash_of_all_activity))
        .routes(routes!(event))
        .routes(routes!(catalog::rune_info).with_state(tables.clone()))
        .routes(routes!(catalog::runes).with_state(tables.clone()))
        .routes(routes!(catalog::rune_holders).with_state(tables.clone()))
        .routes(routes!(catalog::wallet_balances).with_state(tables.clone()))
        .routes(routes!(feeds::rune_events).with_state(tables.clone()))
        .routes(routes!(feeds::wallet_events).with_state(tables))
}

/// A runes route's answer: `error` null, the `result` and the database's
/// height.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesAnswer<T: ToSchema> {
    error: Null,
    result: T,
    db_block_height: DbHeight,
}

/// A wallet's balance of a rune. The columns come from tables without
/// NOT NULL constraints, so every field may be null.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesBalance {
    #[schema(nullable)]
    pkscript: String,
    #[schema(nullable)]
    wallet_addr: String,
    #[schema(nullable)]
    rune_id: String,
    #[schema(nullable)]
    rune_name: String,
    /// The sum of the wallet's outpoints' balances of the rune.
    #[schema(nullable)]
    total_balance: NumericText,
}

/// A runes event of a block.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesBlockEvent {
    /// The event type's name; absent for a type id `runes_event_types`
    /// does not list.
    #[schema(required = false)]
    event_type: String,
    #[schema(nullable)]
    outpoint: String,
    #[schema(nullable)]
    pkscript: String,
    #[schema(nullable)]
    rune_id: String,
    #[schema(nullable)]
    amount: NumericText,
}

/// A runes event of a transaction.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesTxEvent {
    /// The event type's name; absent for a type id `runes_event_types`
    /// does not list.
    #[schema(required = false)]
    event_type: String,
    #[schema(nullable)]
    outpoint: String,
    #[schema(nullable)]
    pkscript: String,
    #[schema(nullable)]
    wallet_addr: String,
    #[schema(nullable)]
    rune_id: String,
    #[schema(nullable)]
    amount: NumericText,
}

/// An unspent outpoint holding runes.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesOutpoint {
    #[schema(nullable)]
    pkscript: String,
    #[schema(nullable)]
    wallet_addr: String,
    #[schema(nullable)]
    outpoint: String,
    /// The outpoint's rune ids.
    #[schema(nullable)]
    rune_ids: Vec<String>,
    /// The balance of each rune id, read as numbers (node-pg parses a
    /// numeric[] with parseFloat, so a large balance loses precision).
    #[schema(nullable)]
    balances: Vec<f64>,
}

/// A holder of a rune.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesHolder {
    #[schema(nullable)]
    pkscript: String,
    #[schema(nullable)]
    wallet_addr: String,
    #[schema(nullable)]
    rune_id: String,
    /// The sum of the holder's unspent outpoints' balances of the rune.
    #[schema(nullable)]
    total_balance: NumericText,
    /// The rune's name: the `rune_name` parameter in upper case, or the
    /// name of `rune_id`.
    rune_name: String,
}

/// A block's event hashes.
#[derive(ToSchema)]
#[allow(dead_code)]
struct RunesActivityHash {
    cumulative_event_hash: String,
    block_event_hash: String,
    indexer_version: String,
    /// The `block_height` parameter as given.
    block_height: QueryEcho,
}

/// get_block_height_of_db: the latest indexed height (a number, or null for
/// an empty table), or -1 when the query fails.
async fn block_height_of_db(db: &Db) -> JsValue {
    db.latest_block_height(Module::Runes)
        .await
        .unwrap_or_else(|err| {
            tracing::error!("runes block height: {err:#}");
            JsValue::from(-1)
        })
}

/// The event type names by id (`event_type_id_to_name`): a plain object
/// whose keys are the ids as strings.
async fn event_type_names(db: &Db) -> Result<JsObject, ApiError> {
    let rows = sqlx::query!("select event_type_name, event_type_id from runes_event_types;")
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

/// `event_type_id_to_name[row.event_type]`: undefined for an unknown id,
/// which JSON.stringify leaves out.
fn event_type_name(names: &JsObject, event_type: i32) -> Result<JsValue, ApiError> {
    Ok(names.get(&event_type.to_js().to_js_string()?).clone())
}

/// A balance row of balance_on_block and get_current_balance_of_wallet.
fn balance_entry(
    pkscript: Option<String>,
    wallet_addr: Option<String>,
    rune_id: Option<String>,
    rune_name: Option<String>,
    total_balance: Option<String>,
) -> Result<JsValue, ApiError> {
    Ok(js_object! {
        "pkscript" => pkscript.to_js(),
        "wallet_addr" => wallet_addr.to_js(),
        "rune_id" => rune_id.to_js(),
        "rune_name" => rune_name.to_js(),
        "total_balance" => pg_json::text(oid::NUMERIC, total_balance.as_deref())?,
    }
    .into())
}

/// The client's IP address, as the API sees it through
/// API_TRUSTED_PROXY_CNT proxies.
#[utoipa::path(
    get,
    path = "/v1/runes/ip",
    tag = "runes",
    responses((status = 200, description = "The client's IP address", body = String, content_type = "text/html")),
)]
async fn ip(ClientIp(ip): ClientIp) -> Reply {
    Reply::send_str(ip)
}

/// The latest indexed block height.
///
/// `null` before the first block and `-1` when the query fails.
#[utoipa::path(
    get,
    path = "/v1/runes/block_height",
    tag = "runes",
    responses((status = 200, description = "The height as text, `null` or `-1`", body = String, content_type = "text/html")),
)]
async fn block_height(State(db): State<Db>) -> Result<Reply, ApiError> {
    let height = block_height_of_db(&db).await;
    Ok(Reply::send_str(height.to_js_string()?))
}

/// Every rune balance of a pkscript or address at the start of a block
/// height.
///
/// A non-empty `address` selects the wallet by address, otherwise by
/// `pkscript`.
#[utoipa::path(
    get,
    path = "/v1/runes/balance_on_block",
    tag = "runes",
    params(
        ("block_height" = Option<String>, Query, description = "The block height, an integer; the balances are those of the outpoints created below it and unspent at it. Text Postgres cannot read as an int4 answers 500. Left out, the list is empty."),
        ("pkscript" = Option<String>, Query, description = "The wallet's pkScript, hex; read when `address` is empty. Both left out, the list is empty."),
        ("address" = Option<String>, Query, description = "The wallet's address. Left out or empty, the route reads `pkscript`."),
    ),
    responses(
        (status = 200, description = "The balances, in no set order", body = RunesAnswer<Vec<RunesBalance>>),
        (status = 400, description = "`block not indexed yet` for a height above the indexed tip plus one", body = ErrorBody),
    ),
)]
async fn balance_on_block(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let block_height = query.get("block_height").clone();
    let pkscript = query.get("pkscript").clone().or("".into());
    let address = query.get("address").clone().or("".into());

    let current_block_height = block_height_of_db(&db).await;
    if block_height.gt(&current_block_height.add(&1.into())?)? {
        return Ok(Reply::error(400, "block not indexed yet"));
    }

    let rules = db.int_rules().await?;
    let pool = db.pool()?;
    // `address != ''` selects by `wallet_addr` instead of `pkscript`
    let rows: Vec<JsValue> = if !address.loose_eq(&"".into())? {
        let rows = sqlx::query!(
            r#"select pkscript as "pkscript?", wallet_addr as "wallet_addr?", rite.rune_id as "rune_id?", rite.rune_name as "rune_name?", sum(balance)::text as "total_balance"
                from runes_outpoint_to_balances rotb, unnest(rune_ids, balances) as u(rune_id, balance)
                left join runes_id_to_entry rite on rite.rune_id = u.rune_id
                where wallet_addr = $1 and block_height < $2 and (spent_block_height is null or spent_block_height >= $2)
                group by pkscript, wallet_addr, rite.rune_id, rite.rune_name;"#,
            params::text(&address)?,
            params::int4(rules, &block_height)?,
        )
        .fetch_all(pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                balance_entry(
                    r.pkscript,
                    r.wallet_addr,
                    r.rune_id,
                    r.rune_name,
                    r.total_balance,
                )
            })
            .collect::<Result<_, _>>()?
    } else {
        let rows = sqlx::query!(
            r#"select pkscript as "pkscript?", wallet_addr as "wallet_addr?", rite.rune_id as "rune_id?", rite.rune_name as "rune_name?", sum(balance)::text as "total_balance"
                from runes_outpoint_to_balances rotb, unnest(rune_ids, balances) as u(rune_id, balance)
                left join runes_id_to_entry rite on rite.rune_id = u.rune_id
                where pkscript = $1 and block_height < $2 and (spent_block_height is null or spent_block_height >= $2)
                group by pkscript, wallet_addr, rite.rune_id, rite.rune_name;"#,
            params::text(&pkscript)?,
            params::int4(rules, &block_height)?,
        )
        .fetch_all(pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                balance_entry(
                    r.pkscript,
                    r.wallet_addr,
                    r.rune_id,
                    r.rune_name,
                    r.total_balance,
                )
            })
            .collect::<Result<_, _>>()?
    };

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => rows,
        "db_block_height" => current_block_height,
    }))
}

/// Every runes event of a block height.
#[utoipa::path(
    get,
    path = "/v1/runes/activity_on_block",
    tag = "runes",
    params(
        ListBlockHeight,
    ),
    responses(
        (status = 200, description = "The block's events, by id", body = RunesAnswer<Vec<RunesBlockEvent>>),
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
    let rows = sqlx::query!(
        r#"select event_type, outpoint as "outpoint?", pkscript as "pkscript?", rune_id as "rune_id?", amount::text as "amount"
            from runes_events re
            where block_height = $1
            order by id asc;"#,
        params::int4(rules, &block_height)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(JsValue::from(js_object! {
            "event_type" => event_type_name(&names, row.event_type)?,
            "outpoint" => row.outpoint.to_js(),
            "pkscript" => row.pkscript.to_js(),
            "rune_id" => row.rune_id.to_js(),
            "amount" => pg_json::text(oid::NUMERIC, row.amount.as_deref())?,
        }));
    }

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
        "db_block_height" => current_block_height,
    }))
}

/// Every current rune balance of a wallet.
///
/// A non-empty `address` selects the wallet by address, otherwise by
/// `pkscript`.
#[utoipa::path(
    get,
    path = "/v1/runes/get_current_balance_of_wallet",
    tag = "runes",
    params(
        ("address" = Option<String>, Query, description = "The wallet's address. Left out or empty, the route reads `pkscript`."),
        ("pkscript" = Option<String>, Query, description = "The wallet's pkScript, hex; read when `address` is empty. Both left out, the list is empty."),
    ),
    responses((status = 200, description = "The balances, in no set order", body = RunesAnswer<Vec<RunesBalance>>)),
)]
async fn get_current_balance_of_wallet(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let address = query.get("address").clone().or("".into());
    let pkscript = query.get("pkscript").clone().or("".into());
    // `address != ''` selects by `wallet_addr` instead of `pkscript`
    let by_wallet = !address.loose_eq(&"".into())?;

    let current_block_height = block_height_of_db(&db).await;
    let pool = db.pool()?;
    let rows: Vec<JsValue> = if by_wallet {
        let rows = sqlx::query!(
            r#"select pkscript as "pkscript?", wallet_addr as "wallet_addr?", rite.rune_id as "rune_id?", rite.rune_name as "rune_name?", sum(balance)::text as "total_balance"
                from runes_outpoint_to_balances rotb, unnest(rune_ids, balances) as u(rune_id, balance)
                left join runes_id_to_entry rite on rite.rune_id = u.rune_id
                where wallet_addr = $1 and spent = false
                group by pkscript, wallet_addr, rite.rune_id, rite.rune_name;"#,
            params::text(&address)?,
        )
        .fetch_all(pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                balance_entry(
                    r.pkscript,
                    r.wallet_addr,
                    r.rune_id,
                    r.rune_name,
                    r.total_balance,
                )
            })
            .collect::<Result<_, _>>()?
    } else {
        let rows = sqlx::query!(
            r#"select pkscript as "pkscript?", wallet_addr as "wallet_addr?", rite.rune_id as "rune_id?", rite.rune_name as "rune_name?", sum(balance)::text as "total_balance"
                from runes_outpoint_to_balances rotb, unnest(rune_ids, balances) as u(rune_id, balance)
                left join runes_id_to_entry rite on rite.rune_id = u.rune_id
                where pkscript = $1 and spent = false
                group by pkscript, wallet_addr, rite.rune_id, rite.rune_name;"#,
            params::text(&pkscript)?,
        )
        .fetch_all(pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                balance_entry(
                    r.pkscript,
                    r.wallet_addr,
                    r.rune_id,
                    r.rune_name,
                    r.total_balance,
                )
            })
            .collect::<Result<_, _>>()?
    };

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => rows,
        "db_block_height" => current_block_height,
    }))
}

/// An unspent outpoint row of get_unspent_rune_outpoints_of_wallet.
fn outpoint_entry(
    pkscript: Option<String>,
    wallet_addr: Option<String>,
    outpoint: Option<String>,
    rune_ids: Option<String>,
    balances: Option<String>,
) -> Result<JsValue, ApiError> {
    Ok(js_object! {
        "pkscript" => pkscript.to_js(),
        "wallet_addr" => wallet_addr.to_js(),
        "outpoint" => outpoint.to_js(),
        "rune_ids" => pg_json::text(oid::TEXT_ARRAY,rune_ids.as_deref())?,
        "balances" => pg_json::text(oid::NUMERIC_ARRAY, balances.as_deref())?,
    }
    .into())
}

/// Every unspent outpoint of a wallet that holds runes.
///
/// A non-empty `address` selects the wallet by address, otherwise by
/// `pkscript`.
#[utoipa::path(
    get,
    path = "/v1/runes/get_unspent_rune_outpoints_of_wallet",
    tag = "runes",
    params(
        ("address" = Option<String>, Query, description = "The wallet's address. Left out or empty, the route reads `pkscript`."),
        ("pkscript" = Option<String>, Query, description = "The wallet's pkScript, hex; read when `address` is empty. Both left out, the list is empty."),
    ),
    responses((status = 200, description = "The outpoints, in no set order", body = RunesAnswer<Vec<RunesOutpoint>>)),
)]
async fn get_unspent_rune_outpoints_of_wallet(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let address = query.get("address").clone().or("".into());
    let pkscript = query.get("pkscript").clone().or("".into());
    // `address != ''` selects by `wallet_addr` instead of `pkscript`
    let by_wallet = !address.loose_eq(&"".into())?;

    let current_block_height = block_height_of_db(&db).await;
    let pool = db.pool()?;
    let rows: Vec<JsValue> = if by_wallet {
        let rows = sqlx::query!(
            r#"select pkscript as "pkscript?", wallet_addr as "wallet_addr?", rotb.outpoint as "outpoint?", rotb.rune_ids::text as "rune_ids", rotb.balances::text as "balances"
                from runes_outpoint_to_balances rotb
                where wallet_addr = $1 and spent = false;"#,
            params::text(&address)?,
        )
        .fetch_all(pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                outpoint_entry(
                    r.pkscript,
                    r.wallet_addr,
                    r.outpoint,
                    r.rune_ids,
                    r.balances,
                )
            })
            .collect::<Result<_, _>>()?
    } else {
        let rows = sqlx::query!(
            r#"select pkscript as "pkscript?", wallet_addr as "wallet_addr?", rotb.outpoint as "outpoint?", rotb.rune_ids::text as "rune_ids", rotb.balances::text as "balances"
                from runes_outpoint_to_balances rotb
                where pkscript = $1 and spent = false;"#,
            params::text(&pkscript)?,
        )
        .fetch_all(pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                outpoint_entry(
                    r.pkscript,
                    r.wallet_addr,
                    r.outpoint,
                    r.rune_ids,
                    r.balances,
                )
            })
            .collect::<Result<_, _>>()?
    };

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => rows,
        "db_block_height" => current_block_height,
    }))
}

/// `rows.sort((a, b) => parseInt(b.total_balance) - parseInt(a.total_balance))`.
fn order_holders(rows: Vec<JsObject>) -> Result<Vec<JsObject>, ApiError> {
    let mut keyed = Vec::with_capacity(rows.len());
    for row in rows {
        keyed.push((row.get("total_balance").parse_int()?, row));
    }
    sort_by_number(&mut keyed, |a, b| b.0 - a.0);
    Ok(keyed.into_iter().map(|(_, row)| row).collect())
}

/// Every holder of a rune, largest balance first.
///
/// The rune is named by `rune_name` or `rune_id`; the route looks up the
/// other one.
#[utoipa::path(
    get,
    path = "/v1/runes/holders",
    tag = "runes",
    params(
        ("rune_name" = Option<String>, Query, description = "The rune's name, matched in upper case. Left out or empty, the route reads `rune_id`."),
        ("rune_id" = Option<String>, Query, description = "The rune's id; read when `rune_name` is empty. Both left out, no rune matches (400)."),
    ),
    responses(
        (status = 200, description = "The holders", body = RunesAnswer<Vec<RunesHolder>>),
        (status = 400, description = "`rune not found`", body = ErrorBody),
    ),
)]
async fn holders(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let mut rune_name = JsValue::from(query.get("rune_name").to_upper_case()?).or("".into());
    let mut rune_id = query.get("rune_id").clone().or("".into());

    let current_block_height = block_height_of_db(&db).await;
    let pool = db.pool()?;
    if rune_name.loose_eq(&"".into())? {
        let rows = sqlx::query_scalar!(
            "select rune_name from runes_id_to_entry where rune_id = $1;",
            params::text(&rune_id)?,
        )
        .fetch_all(pool)
        .await?;
        let Some(name) = rows.into_iter().next() else {
            return Ok(Reply::error(400, "rune not found"));
        };
        rune_name = name.into();
    }
    if rune_id.loose_eq(&"".into())? {
        let rows = sqlx::query_scalar!(
            "select rune_id from runes_id_to_entry where rune_name = $1;",
            params::text(&rune_name)?,
        )
        .fetch_all(pool)
        .await?;
        let Some(id) = rows.into_iter().next() else {
            return Ok(Reply::error(400, "rune not found"));
        };
        rune_id = id.into();
    }

    // the order names the sum: a bare `total_balance` would name the text
    // output column
    let rows = sqlx::query!(
        r#"select pkscript as "pkscript?", wallet_addr as "wallet_addr?", rune_id as "rune_id?", sum(balance)::text as "total_balance"
            from runes_outpoint_to_balances rotb, unnest(rune_ids, balances) as u(rune_id, balance)
            where rune_id = $1 and spent = false
            group by pkscript, wallet_addr, rune_id
            order by sum(balance) desc;"#,
        params::text(&rune_id)?,
    )
    .fetch_all(pool)
    .await?;
    let mut objects = Vec::with_capacity(rows.len());
    for row in rows {
        objects.push(js_object! {
            "pkscript" => row.pkscript.to_js(),
            "wallet_addr" => row.wallet_addr.to_js(),
            "rune_id" => row.rune_id.to_js(),
            "total_balance" => pg_json::text(oid::NUMERIC, row.total_balance.as_deref())?,
        });
    }
    // a rune with many holders sorts a large set, off the async workers
    let rows: Vec<JsValue> = tokio::task::spawn_blocking(move || order_holders(objects))
        .await??
        .into_iter()
        .map(|mut row| {
            row.set("rune_name", rune_name.clone());
            JsValue::from(row)
        })
        .collect();

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => rows,
        "db_block_height" => current_block_height,
    }))
}

async fn indexer_version(db: &Db) -> Result<String, ApiError> {
    let rows = sqlx::query_scalar!("select indexer_version from runes_indexer_version;")
        .fetch_all(db.pool()?)
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("indexer_version"))?)
}

/// A block's event hashes, cumulative and its own.
#[utoipa::path(
    get,
    path = "/v1/runes/get_hash_of_all_activity",
    tag = "runes",
    params(
        ("block_height" = String, Query, description = "The block height, an integer. Text Postgres cannot read as an int4 answers 500. Left out, no hash row matches (400 `block not indexed`)."),
    ),
    responses(
        (status = 200, description = "The block's hashes", body = Answer<RunesActivityHash>),
        (status = 400, description = "`block not indexed yet` for a height above the indexed tip, `block not indexed` for a height without a hash row", body = ErrorBody),
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
        "select cumulative_event_hash, block_event_hash
            from runes_cumulative_event_hashes
            where block_height = $1;",
        params::int4(rules, &block_height)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(Reply::error(400, "block not indexed"));
    };
    let indexer_version = indexer_version(&db).await?;

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "cumulative_event_hash" => row.cumulative_event_hash,
            "block_event_hash" => row.block_event_hash,
            "indexer_version" => indexer_version,
            "block_height" => block_height,
        },
    }))
}

/// Every runes event of a transaction id.
#[utoipa::path(
    get,
    path = "/v1/runes/event",
    tag = "runes",
    params(
        ("transaction_id" = String, Query, description = "The transaction id. Left out or empty, the route answers 400."),
    ),
    responses(
        (status = 200, description = "The transaction's events, by id", body = Answer<Vec<RunesTxEvent>>),
        (status = 400, description = "`transaction_id is required` when it is left out or empty", body = ErrorBody),
    ),
)]
async fn event(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let names = event_type_names(&db).await?;

    let transaction_id = query.get("transaction_id").clone();
    if !transaction_id.truthy() {
        return Ok(Reply::error(400, "transaction_id is required"));
    }

    let rows = sqlx::query!(
        r#"select event_type, outpoint as "outpoint?", pkscript as "pkscript?", wallet_addr as "wallet_addr?", rune_id as "rune_id?", amount::text as "amount"
            from runes_events re
            where txid = $1
            order by id asc;"#,
        params::text(&transaction_id)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(JsValue::from(js_object! {
            "event_type" => event_type_name(&names, row.event_type)?,
            "outpoint" => row.outpoint.to_js(),
            "pkscript" => row.pkscript.to_js(),
            "wallet_addr" => row.wallet_addr.to_js(),
            "rune_id" => row.rune_id.to_js(),
            "amount" => pg_json::text(oid::NUMERIC, row.amount.as_deref())?,
        }));
    }

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}
