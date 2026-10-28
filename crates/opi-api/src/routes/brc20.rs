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
    super::no_row,
    crate::{
        config::Module,
        db::Db,
        extract::{ClientIp, ExpressParams, JsQuery},
        js::{JsError, JsObject, JsValue, compare_strings, sort_by_number},
        js_object, params,
        pg_json::{self, ToJs, oid},
        reply::{ApiError, Reply},
    },
    axum::{Router, extract::State, routing::get},
    sha2::{Digest, Sha256},
};

mod events;
mod snapshot;
mod tickers;

#[cfg(test)]
mod tests;

const EVENT_SEPARATOR: &str = "|";

/// The explorer's page size (EXPLORER_PAGE_SIZE).
const EXPLORER_PAGE_SIZE: i64 = 50;

pub fn routes(router: Router, db: Db, settings: &super::RouteSettings) -> Router {
    router.merge(
        Router::new()
            .route("/v1/brc20/ip", get(ip))
            .route("/v1/brc20/db_version", get(db_version))
            .route("/v1/brc20/event_hash_version", get(event_hash_version))
            .route("/v1/brc20/extras_block_height", get(extras_block_height))
            .route("/v1/brc20/block_height", get(block_height))
            .route("/v1/brc20/balance_on_block", get(balance_on_block))
            .route("/v1/brc20/activity_on_block", get(activity_on_block))
            .route(
                "/v1/brc20/bitcoin_rpc_results_on_block",
                get(bitcoin_rpc_results_on_block),
            )
            .route(
                "/v1/brc20/get_current_balance_of_wallet",
                get(get_current_balance_of_wallet),
            )
            .route(
                "/v1/brc20/get_valid_tx_notes_of_wallet",
                get(get_valid_tx_notes_of_wallet),
            )
            .route(
                "/v1/brc20/get_valid_tx_notes_of_ticker",
                get(get_valid_tx_notes_of_ticker),
            )
            .route("/v1/brc20/holders", get(holders))
            .route(
                "/v1/brc20/get_hash_of_all_activity",
                get(get_hash_of_all_activity),
            )
            .route(
                "/v1/brc20/get_hash_of_all_current_balances",
                get(get_hash_of_all_current_balances),
            )
            .route("/v1/brc20/event", get(event))
            .route(
                "/v1/brc20/explorer/btc_address/{evm_address}",
                get(explorer_btc_address),
            )
            .route(
                "/v1/brc20/explorer/balances/{evm_address}",
                get(explorer_balances),
            )
            .route(
                "/v1/brc20/explorer/address_tabs_counters/{evm_address}",
                get(explorer_address_tabs_counters),
            )
            .route("/v1/brc20/explorer/tokens", get(explorer_tokens))
            .route("/v1/brc20/ticker", get(tickers::ticker_info))
            .route(
                "/v1/brc20/ticker/by_contract",
                get(tickers::ticker_by_contract),
            )
            .route("/v1/brc20/tickers", get(tickers::tickers))
            .route("/v1/brc20/ticker/holders", get(tickers::holders))
            .route(
                "/v1/brc20/ticker/holders/snapshot",
                get(snapshot::snapshot).with_state(snapshot::Snapshots::new(
                    db.clone(),
                    settings.snapshot_timeout,
                )),
            )
            .route("/v1/brc20/wallet/balances", get(tickers::wallet_balances))
            .route("/v1/brc20/ticker/events", get(events::ticker_events))
            .route("/v1/brc20/wallet/events", get(events::wallet_events))
            .with_state(db),
    )
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

async fn ip(ClientIp(ip): ClientIp) -> Reply {
    Reply::send_str(ip)
}

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

async fn extras_block_height(State(db): State<Db>) -> Result<Reply, ApiError> {
    let height = extras_block_height_of_db(&db).await;
    Ok(Reply::send_str(height.to_js_string()?))
}

async fn block_height(State(db): State<Db>) -> Result<Reply, ApiError> {
    let height = block_height_of_db(&db).await;
    Ok(Reply::send_str(height.to_js_string()?))
}

/// A ticker's balance of a pkscript at the start of a block height.
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

async fn indexer_version(db: &Db) -> Result<String, ApiError> {
    let rows = sqlx::query_scalar!("select indexer_version from brc20_indexer_version;")
        .fetch_all(db.pool()?)
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("indexer_version"))?)
}

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
    let indexer_version = indexer_version(&db).await?;

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

async fn get_hash_of_all_current_balances(State(db): State<Db>) -> Result<Reply, ApiError> {
    let current_block_height = block_height_of_db(&db).await;
    let rows = sqlx::query!(
        r#"select pkscript, tick, overall_balance::text as "overall_balance", available_balance::text as "available_balance"
            from brc20_current_balances
            order by pkscript asc, tick asc;"#
    )
    .fetch_all(db.pool()?)
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
    let indexer_version = indexer_version(&db).await?;

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "current_balances_hash" => hash_hex,
            "indexer_version" => indexer_version,
            "block_height" => current_block_height,
        },
    }))
}

/// Every event of an inscription id.
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

async fn explorer_btc_address(
    State(db): State<Db>,
    ExpressParams(path): ExpressParams,
) -> Result<Reply, ApiError> {
    let btc_address = btc_address_of_evm_address(&db, path.get("evm_address")).await?;
    Ok(Reply::send_json(js_object! {
        "btc_address" => btc_address,
    }))
}

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
