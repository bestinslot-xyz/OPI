//! The bitmap module's routes (api/routes/bitmap.js).
//!
//! Each handler follows its JavaScript counterpart line by line, as the
//! brc20 routes do. The JS routes have no catch of their own, so any failed
//! query, the latest-height one included, ends in the app's 500 answer.

use {
    super::no_row,
    crate::{
        config::Module,
        db::Db,
        extract::{ClientIp, JsQuery},
        js::JsValue,
        js_object, params,
        reply::{ApiError, Reply},
    },
    anyhow::anyhow,
    axum::{Router, extract::State, routing::get},
};

pub fn routes(router: Router, db: Db) -> Router {
    router.merge(
        Router::new()
            .route("/v1/bitmap/ip", get(ip))
            .route("/v1/bitmap/block_height", get(block_height))
            .route(
                "/v1/bitmap/get_hash_of_all_activity",
                get(get_hash_of_all_activity),
            )
            .route(
                "/v1/bitmap/get_hash_of_all_bitmaps",
                get(get_hash_of_all_bitmaps),
            )
            .route(
                "/v1/bitmap/get_inscription_id_of_bitmap",
                get(get_inscription_id_of_bitmap),
            )
            .with_state(db),
    )
}

/// get_block_height_of_db: the latest indexed height (a number, or null for
/// an empty table). A failed query is the route's failure.
async fn block_height_of_db(db: &Db) -> Result<JsValue, ApiError> {
    Ok(db.latest_block_height(Module::Bitmap).await?)
}

async fn indexer_version(db: &Db) -> Result<String, ApiError> {
    let rows = sqlx::query_scalar!("select indexer_version from bitmap_indexer_version;")
        .fetch_all(db.pool()?)
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("indexer_version"))?)
}

async fn ip(ClientIp(ip): ClientIp) -> Reply {
    Reply::send_str(ip)
}

async fn block_height(State(db): State<Db>) -> Result<Reply, ApiError> {
    let height = block_height_of_db(&db).await?;
    Ok(Reply::send_str(height.to_js_string()?))
}

async fn get_hash_of_all_activity(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let block_height = query.get("block_height").clone();

    let current_block_height = block_height_of_db(&db).await?;
    if block_height.gt(&current_block_height)? {
        return Ok(Reply::error(400, "block not indexed yet"));
    }

    let rules = db.int_rules().await?;
    let rows = sqlx::query!(
        "select cumulative_event_hash, block_event_hash
            from bitmap_cumulative_event_hashes
            where block_height = $1;",
        params::int4(rules, &block_height)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    // the version query runs before `res.rows[0]` is read
    let indexer_version = indexer_version(&db).await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| no_row("cumulative_event_hash"))?;

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

/// The route reads the latest height and passes it as the only parameter of
/// a query that has no `$1`. Postgres refuses that bind, so after the
/// height query the route always fails with the 500 answer.
async fn get_hash_of_all_bitmaps(State(db): State<Db>) -> Result<Reply, ApiError> {
    block_height_of_db(&db).await?;
    Err(
        anyhow!("bind message supplies 1 parameters, but prepared statement \"\" requires 0")
            .into(),
    )
}

async fn get_inscription_id_of_bitmap(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let bitmap_number = query.get("bitmap_number").clone();

    let rules = db.int_rules().await?;
    let rows = sqlx::query_scalar!(
        "select inscription_id
            from bitmaps
            where bitmap_number = $1;",
        params::int4(rules, &bitmap_number)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let Some(inscription_id) = rows.into_iter().next() else {
        return Ok(Reply::error(400, "bitmap not found"));
    };

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "inscription_id" => inscription_id,
            "bitmap_number" => bitmap_number,
        },
    }))
}
