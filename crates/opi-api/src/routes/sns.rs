//! The sns module's routes.
//!
//! Each handler follows its JavaScript counterpart line by line, as the
//! brc20 routes do. The JS routes have no catch of their own, so any failed
//! query, the latest-height one included, ends in the app's 500 answer.
//!
//! `/v1/sns/by_inscription` and `/v1/sns/search` have no JavaScript
//! counterpart. They answer `{"error":null,"result":...}`; a missing or
//! invalid parameter answers `400 {"error":"<reason>","result":null}` and an
//! unknown inscription `404 {"error":"sns name not found","result":null}`.
//!
//! A name is `{"inscription_id","inscription_number","name","namespace",
//! "block_height"}`: `namespace` is the stored `domain`, the part of the
//! name after its dot, and `block_height` is the block that registered it.
//! An inscription registers one name, so `by_inscription?inscription_id=`
//! answers that one.
//!
//! `search?prefix=&namespace=` lists the names that start with `prefix`,
//! bytewise ascending by name (the `C` collation, which a `COLLATE "C"`
//! index on `sns_names."name"` serves for the prefix match and the order),
//! optionally only those of one namespace. SNS stores names lowercased
//! (Unicode 17.0.0 full lowercase mapping, final sigma included), so the
//! prefix and namespace are lowercased with the indexer's own tables
//! (`opi_sns::unicode::to_lowercase`): `Sats` finds `sats.x`. A prefix has no
//! wildcards: `%`, `_` and `\` match themselves. A Greek capital sigma at
//! the end of a prefix lowercases to final sigma only inside a complete
//! name, so a prefix ending in one matches both the sigma and the final
//! sigma spelling, and a namespace is matched in the same two forms. The
//! list pages by key: `limit` (default 100, at most 500) and `after`, the
//! `next_after` of the previous page; a page holds the names after `after`.
//! `has_more` tells whether more follow and `next_after` is the name to ask
//! with (null on the last page). The result's `prefix` and `namespace` are
//! the lowercase forms the search used (the form without a final sigma
//! where two apply), and `namespace` is null when none was given.

use {
    super::ModuleRoutes,
    super::{
        no_row,
        paging::{
            BadParam, DEFAULT_LIMIT, MAX_INSCRIPTION_ID_LEN, MAX_LIMIT, MAX_NAME_LEN, integer,
            optional_text, prefix_pattern, split_page, text,
        },
    },
    crate::{
        config::Module,
        db::Db,
        extract::{ClientIp, JsQuery},
        js::{JsError, JsObject, JsValue},
        js_object,
        openapi::{Answer, DbHeight, ErrorBody, HashBlockHeight, LimitParam, QueryEcho},
        params,
        pg_json::ToJs,
        reply::{ApiError, Reply},
    },
    axum::extract::State,
    opi_sns::unicode::to_lowercase,
    sha2::{Digest, Sha256},
    utoipa::{IntoParams, ToSchema},
    utoipa_axum::routes,
};

const EVENT_SEPARATOR: &str = "|";

pub fn routes(_db: &Db, _settings: &super::RouteSettings) -> ModuleRoutes {
    ModuleRoutes::new()
        .routes(routes!(ip))
        .routes(routes!(block_height))
        .routes(routes!(get_hash_of_all_activity))
        .routes(routes!(get_hash_of_all_registered_names))
        .routes(routes!(get_info_of_sns))
        .routes(routes!(get_inscriptions_of_domain))
        .routes(routes!(get_registered_namespaces))
        .routes(routes!(by_inscription))
        .routes(routes!(search))
}

/// A block's event hashes.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SnsActivityHash {
    cumulative_event_hash: String,
    block_event_hash: String,
    indexer_version: String,
    /// The `block_height` parameter as given.
    block_height: QueryEcho,
}

/// The hash of every registered name.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SnsNamesHash {
    /// The sha256 hex of `name;domain;inscription_id;inscription_number`
    /// per name, by inscription number, joined by `|`.
    current_sns_names_hash: String,
    indexer_version: String,
    block_height: DbHeight,
}

/// A registered name's inscription and namespace.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SnsNameInfo {
    inscription_id: String,
    inscription_number: i32,
    /// The part of the name after its dot.
    domain: String,
    /// The `name` parameter as given.
    sns_name: QueryEcho,
}

/// A name of a namespace.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SnsDomainName {
    inscription_id: String,
    inscription_number: i32,
    sns_name: String,
}

/// A registered namespace.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SnsNamespace {
    inscription_id: String,
    inscription_number: i32,
    namespace: String,
}

/// A registered name.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SnsName {
    inscription_id: String,
    inscription_number: i32,
    name: String,
    /// The part of the name after its dot.
    namespace: String,
    /// The block that registered the name.
    block_height: i32,
}

/// The parameters of `/v1/sns/search`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct SearchParams {
    /// The names' start, lowercased as SNS stores names; `%`, `_` and `\`
    /// match themselves.
    #[param(min_length = 1, max_length = 2048)]
    prefix: String,
    /// Keeps the names of one namespace, lowercased the same way.
    #[param(max_length = 2048)]
    namespace: Option<String>,
    /// The previous page's `next_after`: the page holds the names after it.
    #[param(max_length = 2048)]
    after: Option<String>,
}

/// A page of names by prefix.
#[derive(ToSchema)]
#[allow(dead_code)]
struct SnsSearch {
    /// The lowercase prefix the search used.
    prefix: String,
    /// The lowercase namespace the search used, null without one.
    #[schema(nullable)]
    namespace: String,
    limit: i64,
    has_more: bool,
    /// The `after` of the next page, null on the last page.
    #[schema(nullable)]
    next_after: String,
    /// Bytewise ascending by name.
    names: Vec<SnsName>,
}

/// get_block_height_of_db: the latest indexed height (a number, or null for
/// an empty table). A failed query is the route's failure.
async fn block_height_of_db(db: &Db) -> Result<JsValue, ApiError> {
    Ok(db.latest_block_height(Module::Sns).await?)
}

async fn indexer_version(db: &Db) -> Result<String, ApiError> {
    let rows = sqlx::query_scalar!("select indexer_version from sns_names_indexer_version;")
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
    path = "/v1/sns/ip",
    tag = "sns",
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
    path = "/v1/sns/block_height",
    tag = "sns",
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
    path = "/v1/sns/get_hash_of_all_activity",
    tag = "sns",
    params(
        HashBlockHeight,
    ),
    responses(
        (status = 200, description = "The block's hashes", body = Answer<SnsActivityHash>),
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
            from sns_names_cumulative_event_hashes
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

/// An sns_names row as get_hash_of_all_registered_names reads it.
struct NameRow {
    name: JsValue,
    domain: JsValue,
    inscription_id: JsValue,
    inscription_number: JsValue,
}

/// The sha256 hex of `name;domain;inscription_id;inscription_number` per
/// row, in the rows' order, joined by `|`.
fn registered_names_hash(rows: &[NameRow]) -> Result<String, JsError> {
    let mut whole_str = String::new();
    for row in rows {
        for (i, field) in [
            &row.name,
            &row.domain,
            &row.inscription_id,
            &row.inscription_number,
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                whole_str.push(';');
            }
            whole_str.push_str(&field.to_js_string()?);
        }
        whole_str.push_str(EVENT_SEPARATOR);
    }
    // whole_str.slice(0, -1): the last separator
    whole_str.pop();
    Ok(format!("{:x}", Sha256::digest(whole_str.as_bytes())))
}

/// The hash of every registered name.
///
/// The route reads and hashes the whole `sns_names` table.
#[utoipa::path(
    get,
    path = "/v1/sns/get_hash_of_all_registered_names",
    tag = "sns",
    responses((status = 200, description = "The hash", body = Answer<SnsNamesHash>)),
)]
async fn get_hash_of_all_registered_names(State(db): State<Db>) -> Result<Reply, ApiError> {
    let current_block_height = block_height_of_db(&db).await?;
    let rows = sqlx::query!(
        r#"select "name", domain, inscription_id, inscription_number
            from sns_names
            order by inscription_number asc;"#
    )
    .fetch_all(db.pool()?)
    .await?;
    let rows: Vec<NameRow> = rows
        .into_iter()
        .map(|row| NameRow {
            name: row.name.into(),
            domain: row.domain.into(),
            inscription_id: row.inscription_id.into(),
            inscription_number: row.inscription_number.to_js(),
        })
        .collect();
    // the whole table is joined and hashed in memory, off the async workers
    let hash_hex = tokio::task::spawn_blocking(move || registered_names_hash(&rows)).await??;
    let indexer_version = indexer_version(&db).await?;

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "current_sns_names_hash" => hash_hex,
            "indexer_version" => indexer_version,
            "block_height" => current_block_height,
        },
    }))
}

/// The inscription and namespace of a registered name.
#[utoipa::path(
    get,
    path = "/v1/sns/get_info_of_sns",
    tag = "sns",
    params(
        ("name" = String, Query, description = "The name, matched as stored (lowercase). Left out, no name matches (400)."),
    ),
    responses(
        (status = 200, description = "The name's inscription", body = Answer<SnsNameInfo>),
        (status = 400, description = "`sns name not found`", body = ErrorBody),
    ),
)]
async fn get_info_of_sns(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let name = query.get("name").clone();

    let rows = sqlx::query!(
        r#"select inscription_id, inscription_number, domain
            from sns_names
            where "name" = $1;"#,
        params::text(&name)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(Reply::error(400, "sns name not found"));
    };

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "inscription_id" => row.inscription_id,
            "inscription_number" => row.inscription_number.to_js(),
            "domain" => row.domain,
            "sns_name" => name,
        },
    }))
}

/// Every name of a namespace, with its inscription.
#[utoipa::path(
    get,
    path = "/v1/sns/get_inscriptions_of_domain",
    tag = "sns",
    params(
        ("domain" = String, Query, description = "The namespace, matched as stored (lowercase). Left out, the list is empty."),
    ),
    responses((status = 200, description = "The names, in no set order", body = Answer<Vec<SnsDomainName>>)),
)]
async fn get_inscriptions_of_domain(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let domain = query.get("domain").clone();

    let rows = sqlx::query!(
        r#"select inscription_id, inscription_number, "name" as sns_name
            from sns_names
            where domain = $1;"#,
        params::text(&domain)?,
    )
    .fetch_all(db.pool()?)
    .await?;
    let result: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            js_object! {
                "inscription_id" => row.inscription_id,
                "inscription_number" => row.inscription_number.to_js(),
                "sns_name" => row.sns_name,
            }
            .into()
        })
        .collect();

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// Every registered namespace, with its inscription.
#[utoipa::path(
    get,
    path = "/v1/sns/get_registered_namespaces",
    tag = "sns",
    responses((status = 200, description = "The namespaces, in no set order", body = Answer<Vec<SnsNamespace>>)),
)]
async fn get_registered_namespaces(State(db): State<Db>) -> Result<Reply, ApiError> {
    let rows = sqlx::query!(
        r#"select inscription_id, inscription_number, "namespace"
            from sns_namespaces;"#
    )
    .fetch_all(db.pool()?)
    .await?;
    let result: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            js_object! {
                "inscription_id" => row.inscription_id,
                "inscription_number" => row.inscription_number.to_js(),
                "namespace" => row.namespace,
            }
            .into()
        })
        .collect();

    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// An sns_names row as the new routes answer it.
struct SnsRow {
    inscription_id: String,
    inscription_number: i32,
    name: String,
    namespace: String,
    block_height: i32,
}

impl SnsRow {
    fn into_object(self) -> JsObject {
        js_object! {
            "inscription_id" => self.inscription_id,
            "inscription_number" => self.inscription_number,
            "name" => self.name,
            "namespace" => self.namespace,
            "block_height" => self.block_height,
        }
    }
}

/// The name an inscription registered.
#[utoipa::path(
    get,
    path = "/v1/sns/by_inscription",
    tag = "sns",
    params(
        ("inscription_id" = String, Query, description = "The inscription id", min_length = 1, max_length = 128),
    ),
    responses(
        (status = 200, description = "The name", body = Answer<SnsName>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
        (status = 404, description = "`sns name not found`", body = ErrorBody),
    ),
)]
async fn by_inscription(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let inscription_id = match text(&query, "inscription_id", MAX_INSCRIPTION_ID_LEN) {
        Ok(inscription_id) => inscription_id,
        Err(bad) => return Ok(bad.reply()),
    };
    let row = sqlx::query!(
        r#"select inscription_id, inscription_number, "name", domain, block_height
            from sns_names
            where inscription_id = $1
            order by id
            limit 1;"#,
        inscription_id,
    )
    .fetch_optional(db.pool()?)
    .await?;
    let Some(row) = row else {
        return Ok(Reply::error(404, "sns name not found"));
    };
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => SnsRow {
            inscription_id: row.inscription_id,
            inscription_number: row.inscription_number,
            name: row.name,
            namespace: row.domain,
            block_height: row.block_height,
        }
        .into_object(),
    }))
}

/// The lowercase forms a stored name can take for the first characters
/// `prefix` of a request. A Greek capital sigma lowercases to final sigma
/// when no cased letter follows it, so a prefix ending in one is a name's
/// start in both spellings; the second form lowercases the prefix as if a
/// letter followed.
pub fn name_prefix_forms(prefix: &str) -> Vec<String> {
    let closed = to_lowercase(prefix);
    let mut open = to_lowercase(&format!("{prefix}a"));
    open.pop();
    forms(closed, open)
}

/// The lowercase forms a stored namespace can take for `namespace`: alone,
/// and after a cased letter and a dot as in a name, where a final sigma
/// depends on the text before the dot.
pub fn namespace_forms(namespace: &str) -> Vec<String> {
    let alone = to_lowercase(namespace);
    let after_name = to_lowercase(&format!("a.{namespace}"))[2..].to_string();
    forms(alone, after_name)
}

fn forms(first: String, second: String) -> Vec<String> {
    if first == second {
        vec![first]
    } else {
        vec![first, second]
    }
}

/// A page of the names that start with a prefix.
///
/// Bytewise ascending by name, of `namespace` when given. The prefix and
/// namespace are lowercased as SNS stores names (`Sats` finds `sats.x`);
/// the result's `prefix` and `namespace` are those lowercase forms. The
/// list pages by key.
#[utoipa::path(
    get,
    path = "/v1/sns/search",
    tag = "sns",
    params(SearchParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<SnsSearch>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
    ),
)]
async fn search(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let request = (|| -> Result<_, BadParam> {
        let prefix = text(&query, "prefix", MAX_NAME_LEN)?;
        let namespace = optional_text(&query, "namespace", MAX_NAME_LEN)?.filter(|n| !n.is_empty());
        let after = optional_text(&query, "after", MAX_NAME_LEN)?.filter(|a| !a.is_empty());
        let limit = integer(&query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?;
        Ok((prefix, namespace, after, limit))
    })();
    let (prefix, namespace, after, limit) = match request {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let prefix_forms = name_prefix_forms(prefix);
    let patterns: Vec<String> = prefix_forms
        .iter()
        .map(|form| prefix_pattern(form))
        .collect();
    let first = patterns[0].as_str();
    let second = patterns.get(1).map_or(first, String::as_str);
    let namespaces = namespace.map(namespace_forms);
    // Both the prefix match and the order use sns_names_name_c_idx, a btree
    // on "name" in the C collation; a keyset cursor compares in that order.
    let rows = sqlx::query!(
        r#"select inscription_id, inscription_number, "name", domain, block_height
            from sns_names
            where ("name" collate "C" like $1 or "name" collate "C" like $2)
                and ($3::text[] is null or domain = any($3))
                and ($4::text is null or "name" collate "C" > $4)
            order by "name" collate "C"
            limit $5;"#,
        first,
        second,
        namespaces.as_deref(),
        after,
        limit + 1,
    )
    .fetch_all(db.pool()?)
    .await?;
    let (rows, has_more) = split_page(rows, limit);
    let next_after = if has_more {
        rows.last().map(|row| row.name.clone())
    } else {
        None
    };
    let names: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            SnsRow {
                inscription_id: row.inscription_id,
                inscription_number: row.inscription_number,
                name: row.name,
                namespace: row.domain,
                block_height: row.block_height,
            }
            .into_object()
            .into()
        })
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "prefix" => prefix_forms[0].as_str(),
            "namespace" => namespaces.as_ref().map(|forms| forms[0].as_str()),
            "limit" => limit as f64,
            "has_more" => has_more,
            "next_after" => next_after,
            "names" => names,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_lowercase_like_the_indexer() {
        assert_eq!(name_prefix_forms("Sats"), ["sats"]);
        assert_eq!(name_prefix_forms("ÉCOLE"), ["école"]);
        // full mapping: U+0130 is `i` plus a combining dot
        assert_eq!(name_prefix_forms("İ"), ["i\u{307}"]);
        // a sigma after a letter is final alone and medial in a longer name
        assert_eq!(name_prefix_forms("aΣ"), ["aς", "aσ"]);
        assert_eq!(name_prefix_forms("Σa"), ["σa"]);
        assert_eq!(name_prefix_forms("aΣb"), ["aσb"]);
        assert_eq!(name_prefix_forms("%_\\"), ["%_\\"]);
    }

    #[test]
    fn namespaces_lowercase_like_a_names_tail() {
        assert_eq!(namespace_forms("SATS"), ["sats"]);
        assert_eq!(namespace_forms("Σ"), ["σ", "ς"]);
        assert_eq!(namespace_forms("1Σ"), ["1σ"]);
    }

    fn row(name: &str, domain: &str, inscription_id: &str, number: i32) -> NameRow {
        NameRow {
            name: name.into(),
            domain: domain.into(),
            inscription_id: inscription_id.into(),
            inscription_number: number.to_js(),
        }
    }

    #[test]
    fn the_names_hash_is_nodes() {
        // node: crypto.createHash('sha256').update(whole_str).digest('hex')
        let rows = [row("a.sats", "sats", "i0", -3), row("é.x", "x", "id", 10)];
        assert_eq!(
            registered_names_hash(&rows).unwrap(),
            "ff20dce73eb943d38f3d5c821ea0f5b23dbbfda17d8981fb678f2e02453942e7"
        );
        assert_eq!(
            registered_names_hash(&[]).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
