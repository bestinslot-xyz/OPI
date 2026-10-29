//! The bitmap module's routes.
//!
//! Each handler follows its JavaScript counterpart line by line, as the
//! brc20 routes do. The JS routes have no catch of their own, so any failed
//! query, the latest-height one included, ends in the app's 500 answer.
//!
//! `/v1/bitmap/by_inscription` and `/v1/bitmap/search` have no JavaScript
//! counterpart. They answer `{"error":null,"result":...}`; a missing or
//! invalid parameter answers `400 {"error":"<reason>","result":null}` and an
//! unknown inscription `404 {"error":"bitmap not found","result":null}`.
//!
//! A bitmap is `{"inscription_id","inscription_number","bitmap_number",
//! "block_height"}` (`block_height` is the block that registered it). An
//! inscription holds one bitmap string, so `by_inscription?inscription_id=`
//! answers the one bitmap it registered.
//!
//! `search?prefix=` lists the bitmaps whose number, written in decimal
//! digits, starts with `prefix` (1 to 10 digits; `1` lists 1, 10 to 19, 100
//! to 199 and so on), ascending by number. A bitmap number has no leading
//! zeros, so `0` lists bitmap 0 and any longer prefix starting with `0`
//! lists nothing. The prefix becomes one number range per digit count, each
//! read through the unique `bitmaps_bitmap_number_idx`. The list pages by
//! key: `limit` (default 100, at most 500) and `after`, the `next_after` of
//! the previous page; a page holds the bitmaps with a number above `after`.
//! `has_more` tells whether more follow and `next_after` is the number to
//! ask with (null on the last page).

use {
    super::ModuleRoutes,
    super::{
        no_row,
        paging::{
            BadParam, DEFAULT_LIMIT, MAX_INSCRIPTION_ID_LEN, MAX_LIMIT, integer, required, single,
            split_page, text,
        },
    },
    crate::{
        config::Module,
        db::Db,
        extract::{ClientIp, JsQuery},
        js::{JsObject, JsValue},
        js_object,
        openapi::{Answer, ErrorBody, HashBlockHeight, LimitParam, QueryEcho},
        params,
        reply::{ApiError, Reply},
    },
    anyhow::anyhow,
    axum::extract::State,
    utoipa::{IntoParams, ToSchema},
    utoipa_axum::routes,
};

/// The digits of the largest bitmap number, 2147483647.
pub const MAX_PREFIX_DIGITS: usize = 10;

pub fn routes(_db: &Db, _settings: &super::RouteSettings) -> ModuleRoutes {
    ModuleRoutes::new()
        .routes(routes!(ip))
        .routes(routes!(block_height))
        .routes(routes!(get_hash_of_all_activity))
        .routes(routes!(get_hash_of_all_bitmaps))
        .routes(routes!(get_inscription_id_of_bitmap))
        .routes(routes!(by_inscription))
        .routes(routes!(search))
}

/// A block's event hashes.
#[derive(ToSchema)]
#[allow(dead_code)]
struct BitmapActivityHash {
    cumulative_event_hash: String,
    block_event_hash: String,
    indexer_version: String,
    /// The `block_height` parameter as given.
    block_height: QueryEcho,
}

/// The inscription of a bitmap.
#[derive(ToSchema)]
#[allow(dead_code)]
struct BitmapInscription {
    inscription_id: String,
    /// The `bitmap_number` parameter as given.
    bitmap_number: QueryEcho,
}

/// A bitmap.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Bitmap {
    inscription_id: String,
    inscription_number: i32,
    bitmap_number: i32,
    /// The block that registered the bitmap.
    block_height: i32,
}

/// The parameters of `/v1/bitmap/search`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct SearchParams {
    /// 1 to 10 decimal digits: the bitmaps whose number starts with them.
    #[param(pattern = "^[0-9]{1,10}$")]
    prefix: String,
    /// The previous page's `next_after`: the page holds the bitmaps with a
    /// higher number.
    #[param(minimum = 0, maximum = 2147483647)]
    after: Option<i32>,
}

/// A page of bitmaps by number prefix.
#[derive(ToSchema)]
#[allow(dead_code)]
struct BitmapSearch {
    /// The `prefix` parameter.
    prefix: String,
    limit: i64,
    has_more: bool,
    /// The `after` of the next page, null on the last page.
    #[schema(nullable)]
    next_after: i32,
    /// Ascending by number.
    bitmaps: Vec<Bitmap>,
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

/// The client's IP address, as the API sees it through
/// API_TRUSTED_PROXY_CNT proxies.
#[utoipa::path(
    get,
    path = "/v1/bitmap/ip",
    tag = "bitmap",
    responses((status = 200, description = "The client's IP address", body = String, content_type = "text/html")),
)]
async fn ip(ClientIp(ip): ClientIp) -> Reply {
    Reply::send_str(ip)
}

/// The latest indexed block height.
///
/// `null` before the first block; a failed query answers 500.
#[utoipa::path(
    get,
    path = "/v1/bitmap/block_height",
    tag = "bitmap",
    responses((status = 200, description = "The height as text, or `null`", body = String, content_type = "text/html")),
)]
async fn block_height(State(db): State<Db>) -> Result<Reply, ApiError> {
    let height = block_height_of_db(&db).await?;
    Ok(Reply::send_str(height.to_js_string()?))
}

/// A block's event hashes, cumulative and its own.
///
/// A height the hash table has no row for answers 500.
#[utoipa::path(
    get,
    path = "/v1/bitmap/get_hash_of_all_activity",
    tag = "bitmap",
    params(
        HashBlockHeight,
    ),
    responses(
        (status = 200, description = "The block's hashes", body = Answer<BitmapActivityHash>),
        (status = 400, description = "`block not indexed yet` for a height above the indexed tip", body = ErrorBody),
    ),
)]
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

/// The hash of every bitmap; the route always fails.
///
/// The route reads the latest height and passes it as the only parameter of
/// a query that has no `$1`. Postgres refuses that bind, so after the
/// height query the route always fails with the 500 answer.
#[utoipa::path(
    get,
    path = "/v1/bitmap/get_hash_of_all_bitmaps",
    tag = "bitmap",
    responses((status = 500, description = "Always: `{\"error\":\"internal error\",\"result\":null}`", body = ErrorBody)),
)]
async fn get_hash_of_all_bitmaps(State(db): State<Db>) -> Result<Reply, ApiError> {
    block_height_of_db(&db).await?;
    Err(
        anyhow!("bind message supplies 1 parameters, but prepared statement \"\" requires 0")
            .into(),
    )
}

/// The inscription id of a bitmap number.
#[utoipa::path(
    get,
    path = "/v1/bitmap/get_inscription_id_of_bitmap",
    tag = "bitmap",
    params(
        ("bitmap_number" = String, Query, description = "The bitmap number, an integer. Text Postgres cannot read as an int4 answers 500. Left out, no bitmap matches (400)."),
    ),
    responses(
        (status = 200, description = "The bitmap's inscription", body = Answer<BitmapInscription>),
        (status = 400, description = "`bitmap not found`", body = ErrorBody),
    ),
)]
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

/// A bitmaps row as the new routes answer it.
struct BitmapRow {
    inscription_id: String,
    inscription_number: i32,
    bitmap_number: i32,
    block_height: i32,
}

impl BitmapRow {
    fn into_object(self) -> JsObject {
        js_object! {
            "inscription_id" => self.inscription_id,
            "inscription_number" => self.inscription_number,
            "bitmap_number" => self.bitmap_number,
            "block_height" => self.block_height,
        }
    }
}

/// The bitmap an inscription registered.
#[utoipa::path(
    get,
    path = "/v1/bitmap/by_inscription",
    tag = "bitmap",
    params(
        ("inscription_id" = String, Query, description = "The inscription id", min_length = 1, max_length = 128),
    ),
    responses(
        (status = 200, description = "The bitmap", body = Answer<Bitmap>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
        (status = 404, description = "`bitmap not found`", body = ErrorBody),
    ),
)]
async fn by_inscription(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let inscription_id = match text(&query, "inscription_id", MAX_INSCRIPTION_ID_LEN) {
        Ok(inscription_id) => inscription_id,
        Err(bad) => return Ok(bad.reply()),
    };
    let row = sqlx::query!(
        "select inscription_id, inscription_number, bitmap_number, block_height
            from bitmaps
            where inscription_id = $1
            order by bitmap_number
            limit 1;",
        inscription_id,
    )
    .fetch_optional(db.pool()?)
    .await?;
    let Some(row) = row else {
        return Ok(Reply::error(404, "bitmap not found"));
    };
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => BitmapRow {
            inscription_id: row.inscription_id,
            inscription_number: row.inscription_number,
            bitmap_number: row.bitmap_number,
            block_height: row.block_height,
        }
        .into_object(),
    }))
}

/// The inclusive number ranges `(low, high)` of the bitmap numbers whose
/// decimal text starts with `prefix`, one per digit count, ascending. A
/// number has no leading zeros, so a prefix `0` is the number 0 and a longer
/// prefix starting with `0` has no range.
pub fn prefix_ranges(prefix: &str) -> Result<Vec<(i32, i32)>, BadParam> {
    let invalid = || {
        BadParam(format!(
            "prefix must be 1 to {MAX_PREFIX_DIGITS} decimal digits"
        ))
    };
    if prefix.is_empty()
        || prefix.len() > MAX_PREFIX_DIGITS
        || !prefix.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let value: i64 = prefix.parse().map_err(|_| invalid())?;
    if prefix.starts_with('0') {
        return Ok(if prefix.len() == 1 {
            vec![(0, 0)]
        } else {
            Vec::new()
        });
    }
    let mut ranges = Vec::new();
    let mut scale = 1i64;
    for _ in prefix.len()..=MAX_PREFIX_DIGITS {
        let low = value * scale;
        let high = ((value + 1) * scale - 1).min(i32::MAX as i64);
        if low > i32::MAX as i64 {
            break;
        }
        ranges.push((low as i32, high as i32));
        scale *= 10;
    }
    Ok(ranges)
}

/// A page of the bitmaps whose number starts with a prefix.
///
/// Ascending by number: `1` lists 1, 10 to 19, 100 to 199 and so on. A
/// bitmap number has no leading zeros, so `0` lists bitmap 0 and a longer
/// prefix starting with `0` lists nothing. The list pages by key.
#[utoipa::path(
    get,
    path = "/v1/bitmap/search",
    tag = "bitmap",
    params(SearchParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<BitmapSearch>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
    ),
)]
async fn search(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let request = (|| -> Result<_, BadParam> {
        let prefix = required(&query, "prefix")?;
        let ranges = prefix_ranges(prefix)?;
        let after = match single(&query, "after")? {
            None => None,
            Some(_) => Some(integer(&query, "after", 0, 0, i32::MAX as i64)? as i32),
        };
        let limit = integer(&query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?;
        Ok((prefix, ranges, after, limit))
    })();
    let (prefix, ranges, after, limit) = match request {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let lows: Vec<i32> = ranges.iter().map(|range| range.0).collect();
    let highs: Vec<i32> = ranges.iter().map(|range| range.1).collect();
    // Each range is an index range scan of the unique bitmap_number index,
    // cut at the page size; the ranges ascend, so the outer sort merges at
    // most 10 short lists. One row past the page tells whether more follow.
    let rows = sqlx::query!(
        r#"select b.inscription_id as "inscription_id!", b.inscription_number as "inscription_number!",
                b.bitmap_number as "bitmap_number!", b.block_height as "block_height!"
            from unnest($1::int4[], $2::int4[]) as r(low, high)
            cross join lateral (
                select inscription_id, inscription_number, bitmap_number, block_height
                from bitmaps
                where bitmap_number between r.low and r.high
                    and ($3::int4 is null or bitmap_number > $3)
                order by bitmap_number
                limit $4
            ) b
            order by b.bitmap_number
            limit $4;"#,
        &lows,
        &highs,
        after,
        limit + 1,
    )
    .fetch_all(db.pool()?)
    .await?;
    let (rows, has_more) = split_page(rows, limit);
    let next_after = if has_more {
        rows.last().map(|row| row.bitmap_number)
    } else {
        None
    };
    let bitmaps: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            BitmapRow {
                inscription_id: row.inscription_id,
                inscription_number: row.inscription_number,
                bitmap_number: row.bitmap_number,
                block_height: row.block_height,
            }
            .into_object()
            .into()
        })
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "prefix" => prefix,
            "limit" => limit as f64,
            "has_more" => has_more,
            "next_after" => next_after,
            "bitmaps" => bitmaps,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_is_one_range_per_digit_count() {
        assert_eq!(
            prefix_ranges("12").unwrap(),
            vec![
                (12, 12),
                (120, 129),
                (1_200, 1_299),
                (12_000, 12_999),
                (120_000, 129_999),
                (1_200_000, 1_299_999),
                (12_000_000, 12_999_999),
                (120_000_000, 129_999_999),
                (1_200_000_000, 1_299_999_999),
            ]
        );
        assert_eq!(
            prefix_ranges("9").unwrap().last(),
            Some(&(900_000_000, 999_999_999))
        );
    }

    #[test]
    fn the_largest_numbers_are_cut_at_the_int4_maximum() {
        assert_eq!(
            prefix_ranges("2").unwrap().last(),
            Some(&(2_000_000_000, i32::MAX))
        );
        // 3,000,000,000 and up do not fit an int4
        assert_eq!(
            prefix_ranges("3").unwrap().last(),
            Some(&(300_000_000, 399_999_999))
        );
        assert_eq!(
            prefix_ranges("2147483647").unwrap(),
            vec![(i32::MAX, i32::MAX)]
        );
        assert_eq!(prefix_ranges("2147483648").unwrap(), vec![]);
    }

    #[test]
    fn leading_zeros_match_nothing_but_zero() {
        assert_eq!(prefix_ranges("0").unwrap(), vec![(0, 0)]);
        assert_eq!(prefix_ranges("00").unwrap(), vec![]);
        assert_eq!(prefix_ranges("012").unwrap(), vec![]);
    }

    #[test]
    fn a_prefix_is_one_to_ten_digits() {
        let invalid = BadParam("prefix must be 1 to 10 decimal digits".into());
        for prefix in ["", "1a", "-1", "1.5", " 1", "12345678901", "١٢"] {
            assert_eq!(
                prefix_ranges(prefix),
                Err(BadParam(invalid.0.clone())),
                "{prefix}"
            );
        }
    }
}
