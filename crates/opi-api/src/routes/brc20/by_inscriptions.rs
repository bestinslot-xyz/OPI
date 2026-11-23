//! `POST /v1/brc20/events_by_inscriptions`: the events of up to 500
//! inscription ids in one request.
//!
//! The body is `{"inscription_ids":["<txid>i<index>",...]}` with 1 to 500
//! ids (see [`inscription_ids_body`](crate::routes::inscriptions::inscription_ids_body)),
//! lowercased as the indexer stores them. The answer is
//! `{"error":null,"result":{"inscriptions":[{"inscription_id","events"}],
//! "block_height"}}`: one entry per requested id, in the request's order (a
//! repeated id answers each time), whose `events` are what
//! `/v1/brc20/event?inscription_id=` answers for that id: the union of the
//! id's `brc20_events` and `brc20_light_events` rows by id, each the stored
//! event object with `event_type` and `inscription_id` set. An id without
//! events has an empty list. `block_height` is the highest block of
//! `brc20_block_hashes`, read in the same `REPEATABLE READ READ ONLY`
//! snapshot as the events. A body the route cannot read answers
//! `400 {"error":"<reason>","result":null}`.

use {
    super::{Brc20StoredEvent, event_entry, event_type_names},
    crate::{
        db::Db,
        js::JsValue,
        js_object,
        openapi::{Answer, ErrorBody},
        reply::{ApiError, Reply},
        routes::inscriptions::{InscriptionIdsBody, batch_inscription_ids, snapshot},
    },
    axum::{
        body::Bytes,
        extract::{State, rejection::BytesRejection},
    },
    std::collections::HashMap,
    utoipa::ToSchema,
};

/// The events of one inscription id.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20InscriptionEvents {
    /// The requested id, lowercase.
    inscription_id: String,
    /// By event id, ascending; empty for an id without events.
    events: Vec<Brc20StoredEvent>,
}

/// The events of many inscription ids and the height the answer covers.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Brc20EventsByInscriptions {
    /// One entry per requested id, in the request's order.
    inscriptions: Vec<Brc20InscriptionEvents>,
    /// The highest block of `brc20_block_hashes`, read in one snapshot with
    /// the events; null before the first block.
    #[schema(nullable)]
    block_height: i32,
}

/// Every event of up to 500 inscription ids.
///
/// One entry per requested id, in the request's order; each id's `events`
/// are what `/v1/brc20/event` answers for it, an id without events has an
/// empty list.
#[utoipa::path(
    post,
    path = "/v1/brc20/events_by_inscriptions",
    tag = "brc20",
    request_body(content = InscriptionIdsBody, content_type = "application/json", description = "`{\"inscription_ids\":[\"<txid>i<index>\",...]}`, 1 to 500 inscription ids"),
    responses(
        (status = 200, description = "Each inscription's events", body = Answer<Brc20EventsByInscriptions>),
        (status = 400, description = "A body that is not `{\"inscription_ids\":[...]}` with 1 to 500 valid inscription ids", body = ErrorBody),
        (status = 413, description = "A body over 2 MB", body = ErrorBody),
    ),
)]
pub(super) async fn events_by_inscriptions(
    State(db): State<Db>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Reply, ApiError> {
    let ids = match batch_inscription_ids(body) {
        Ok(ids) => ids,
        Err(reply) => return Ok(reply),
    };
    let mut tx = snapshot(&db).await?;
    let height: Option<i32> =
        sqlx::query_scalar!(r#"select max(block_height) as "h?" from brc20_block_hashes;"#)
            .fetch_one(&mut *tx)
            .await?;
    let names = event_type_names(&mut *tx).await?;
    // brc20_events_inscription_id_event_type_idx and its light-events
    // counterpart; the union keeps its jsonb columns, so it removes the
    // duplicates `/v1/brc20/event`'s union does, and the text cast comes
    // after it
    let rows = sqlx::query!(
        r#"select u.event::text as "event", u.event_type as "event_type!", u.inscription_id as "inscription_id!"
            from (select id, event, event_type, inscription_id
                  from brc20_events
                  where inscription_id = any($1::text[])
                  union select id, event, event_type, inscription_id
                  from brc20_light_events
                  where inscription_id = any($1::text[])) u
            order by u.id asc;"#,
        &ids,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut by_id: HashMap<String, Vec<JsValue>> = HashMap::new();
    for row in rows {
        let entry = event_entry(
            &names,
            row.event.as_deref(),
            row.event_type,
            row.inscription_id.clone(),
        )?;
        by_id.entry(row.inscription_id).or_default().push(entry);
    }
    let inscriptions: Vec<JsValue> = ids
        .into_iter()
        .map(|id| {
            let events = by_id.get(&id).cloned().unwrap_or_default();
            js_object! {
                "inscription_id" => id,
                "events" => events,
            }
            .into()
        })
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "inscriptions" => inscriptions,
            "block_height" => height,
        },
    }))
}
