//! The sns module's routes (api/routes/sns.js).
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
        js::{JsError, JsValue},
        js_object, params,
        pg_json::ToJs,
        reply::{ApiError, Reply},
    },
    axum::{Router, extract::State, routing::get},
    sha2::{Digest, Sha256},
};

const EVENT_SEPARATOR: &str = "|";

pub fn routes(router: Router, db: Db) -> Router {
    router.merge(
        Router::new()
            .route("/v1/sns/ip", get(ip))
            .route("/v1/sns/block_height", get(block_height))
            .route(
                "/v1/sns/get_hash_of_all_activity",
                get(get_hash_of_all_activity),
            )
            .route(
                "/v1/sns/get_hash_of_all_registered_names",
                get(get_hash_of_all_registered_names),
            )
            .route("/v1/sns/get_info_of_sns", get(get_info_of_sns))
            .route(
                "/v1/sns/get_inscriptions_of_domain",
                get(get_inscriptions_of_domain),
            )
            .route(
                "/v1/sns/get_registered_namespaces",
                get(get_registered_namespaces),
            )
            .with_state(db),
    )
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
    let hash_hex = registered_names_hash(&rows)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
