//! The history feeds: `/v1/inscriptions/inscription/history` and
//! `/v1/inscriptions/wallet/history`.
//!
//! Both read `inscriptions_events`, one row per creation and transfer, and
//! answer `{"error":null,"result":...}`. A missing or invalid parameter
//! answers `400 {"error":"<reason>","result":null}` and an unknown
//! inscription `404 {"error":"inscription not found","result":null}`.
//!
//! An event is `{"id","block_height","block_time","event_type",
//! "old_outpoint","old_offset","old_pkscript","old_address","new_outpoint",
//! "new_offset","new_pkscript","new_address"}`: `event_type` is `creation`
//! or `transfer`, the `old_` fields are the location a transfer moved the
//! inscription from (null for a creation) and `block_time` the block's time
//! in seconds (null for a block whose hash the database no longer holds).
//! Addresses derive from the pkScripts and the database's network.
//!
//! The feeds page newest first by key: `limit` (default 100, at most 500)
//! and `before_id`, the `next_before_id` of the previous page. A page holds
//! the rows with an id below `before_id`, so pages never repeat or skip a
//! row when new blocks arrive. A database pruned with `OPI_PRUNE` holds
//! every event, so the feeds answer as on a full node; `block_time` is null
//! below the pruned window, whose block hashes are deleted.

use {
    super::{
        EntryRow, IndexedHeight, Inscription, InscriptionParams, InscriptionRef, NOT_FOUND,
        indexed_height, network_in, snapshot,
    },
    crate::{
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        openapi::{Answer, ErrorBody, LimitParam},
        reply::{ApiError, Reply},
        routes::{
            address::address_of,
            paging::{
                BadParam, DEFAULT_LIMIT, MAX_BEFORE_ID, MAX_LIMIT, Wallet, integer, split_page,
            },
        },
    },
    axum::extract::State,
    utoipa::{IntoParams, ToSchema},
};

/// The paging of a history feed.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct HistoryParams {
    /// The previous page's `next_before_id`: the page holds the rows with a
    /// lower id.
    #[param(minimum = 1, maximum = 999999999999999999_i64)]
    before_id: Option<i64>,
}

/// The `limit` and `before_id` of a feed request.
fn paging(query: &JsObject) -> Result<(i64, i64), BadParam> {
    let limit = integer(query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?;
    let before_id = integer(query, "before_id", MAX_BEFORE_ID, 1, MAX_BEFORE_ID)?;
    Ok((limit, before_id))
}

/// A creation or transfer of an inscription.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(in crate::routes) struct HistoryEvent {
    /// The event's id: its block height in the high 32 bits and its place in
    /// the block in the low 32.
    id: i64,
    block_height: i32,
    /// The block's time in seconds; null for a block whose hash the
    /// database no longer holds.
    #[schema(nullable)]
    block_time: i64,
    /// `creation` or `transfer`.
    event_type: String,
    /// The output a transfer moved the inscription from; null for a
    /// creation, as the other `old_` fields are.
    #[schema(nullable)]
    old_outpoint: String,
    #[schema(nullable)]
    old_offset: i64,
    #[schema(nullable)]
    old_pkscript: String,
    #[schema(nullable)]
    old_address: String,
    /// The output the event put the inscription on.
    new_outpoint: String,
    new_offset: i64,
    new_pkscript: String,
    /// The script's address; null for a script without one.
    #[schema(nullable)]
    new_address: String,
}

/// A location: an outpoint, the sat's offset in it and the output's script.
struct Location {
    outpoint: String,
    offset: i64,
    pkscript: Vec<u8>,
}

impl Location {
    fn of(
        outpoint: Option<String>,
        offset: Option<i64>,
        pkscript: Option<Vec<u8>>,
    ) -> Option<Location> {
        Some(Location {
            outpoint: outpoint?,
            offset: offset?,
            pkscript: pkscript?,
        })
    }
}

/// The event object of `id`, which moved an inscription from `old` (none
/// for a creation, `is_transfer` false) to `new`.
fn event_object(
    id: i64,
    block_time: Option<i64>,
    is_transfer: bool,
    old: Option<Location>,
    new: Location,
    network: &str,
) -> JsObject {
    let old = old.as_ref();
    js_object! {
        "id" => id as f64,
        "block_height" => (id >> 32) as f64,
        "block_time" => block_time.map(|t| t as f64),
        "event_type" => if is_transfer { "transfer" } else { "creation" },
        "old_outpoint" => old.map(|o| o.outpoint.clone()),
        "old_offset" => old.map(|o| o.offset as f64),
        "old_pkscript" => old.map(|o| hex::encode(&o.pkscript)),
        "old_address" => old.and_then(|o| address_of(&o.pkscript, network)),
        "new_outpoint" => new.outpoint,
        "new_offset" => new.offset as f64,
        "new_address" => address_of(&new.pkscript, network),
        "new_pkscript" => hex::encode(&new.pkscript),
    }
}

/// A page of an inscription's history.
#[derive(ToSchema)]
#[allow(dead_code)]
struct InscriptionHistory {
    inscription: Inscription,
    /// Newest first.
    events: Vec<HistoryEvent>,
    limit: i64,
    has_more: bool,
    /// The `before_id` of the next page, null on the last page.
    #[schema(nullable)]
    next_before_id: i64,
    indexed_height: IndexedHeight,
}

/// A page of an inscription's creation and transfers, newest first.
///
/// The inscription is named by its id or its number; an unknown or unbound
/// one answers 404.
#[utoipa::path(
    get,
    path = "/v1/inscriptions/inscription/history",
    tag = "inscriptions",
    params(InscriptionParams, HistoryParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<InscriptionHistory>),
        (status = 400, description = "A missing or invalid parameter, or both or neither of `id` and `number`", body = ErrorBody),
        (status = 404, description = "`inscription not found`", body = ErrorBody),
    ),
)]
pub(super) async fn inscription_history(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (wanted, (limit, before_id)) =
        match InscriptionRef::from_query(&query).and_then(|w| Ok((w, paging(&query)?))) {
            Ok(request) => request,
            Err(bad) => return Ok(bad.reply()),
        };
    let (id, number) = match &wanted {
        InscriptionRef::Id(id) => (Some(id.as_str()), None),
        InscriptionRef::Number(number) => (None, Some(*number)),
    };
    let mut tx = snapshot(&db).await?;
    let height = indexed_height(&mut tx).await?;
    let entry = sqlx::query_as!(
        EntryRow,
        r#"select e.sequence_number, e.inscription_id, e."number", e.content_type, e.media_type,
                e.charms, encode(e.txid, 'hex') || ':' || e.vout as "outpoint!", e."offset",
                e.pkscript, e.genesis_height, e.last_moved_height,
                n.network_type as "network!"
            from inscriptions_entries e
            cross join (select network_type from inscriptions_network_type limit 1) n
            where ($1::text is null or e.inscription_id = $1)
              and ($2::int8 is null or e."number" = $2);"#,
        id,
        number,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(entry) = entry else {
        tx.commit().await?;
        return Ok(Reply::error(404, NOT_FOUND));
    };
    // inscriptions_events_sequence_number_id_idx, read backwards from the
    // cursor; each transfer's old location is its prev_id event's
    let rows = sqlx::query!(
        r#"select v.id, v.prev_id,
                encode(v.txid, 'hex') || ':' || v.vout as "new_outpoint!",
                v."offset" as new_offset, v.pkscript as new_pkscript,
                encode(p.txid, 'hex') || ':' || p.vout as "old_outpoint?",
                p."offset" as "old_offset?", p.pkscript as "old_pkscript?",
                extract(epoch from h.block_time)::int8 as "block_time?"
            from inscriptions_events v
            left join inscriptions_events p on p.id = v.prev_id
            left join inscriptions_block_hashes h on h.block_height = (v.id >> 32)::int4
            where v.sequence_number = $1 and v.id < $2
            order by v.sequence_number desc, v.id desc
            limit $3;"#,
        entry.sequence_number,
        before_id,
        limit + 1,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let (rows, has_more) = split_page(rows, limit);
    let next_before_id = has_more
        .then(|| rows.last().map(|row| row.id as f64))
        .flatten();
    let network = entry.network.clone();
    let events: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            let old = Location::of(row.old_outpoint, row.old_offset, row.old_pkscript);
            let new = Location {
                outpoint: row.new_outpoint,
                offset: row.new_offset,
                pkscript: row.new_pkscript,
            };
            event_object(
                row.id,
                row.block_time,
                row.prev_id.is_some(),
                old,
                new,
                &network,
            )
            .into()
        })
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "inscription" => entry.into_object(),
            "events" => events,
            "limit" => limit as f64,
            "has_more" => has_more,
            "next_before_id" => next_before_id,
            "indexed_height" => height,
        },
    }))
}

/// The wallet of `/v1/inscriptions/wallet/history`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct WalletHistoryParams {
    /// The wallet's address.
    #[param(max_length = 128)]
    address: Option<String>,
    /// The wallet's pkScript, hex.
    #[param(max_length = 20000, pattern = "^([0-9a-fA-F]{2})*$")]
    pkscript: Option<String>,
}

/// An inscription as a wallet's history names it.
#[derive(ToSchema)]
#[allow(dead_code)]
struct HistoryInscription {
    inscription_id: String,
    number: i64,
    sequence_number: i64,
    #[schema(nullable)]
    content_type: String,
    #[schema(nullable)]
    media_type: String,
    cursed: bool,
}

/// One stay of an inscription in the wallet.
#[derive(ToSchema)]
#[allow(dead_code)]
struct WalletStay {
    inscription: HistoryInscription,
    /// The creation or transfer that put the inscription on the wallet.
    received: HistoryEvent,
    /// The transfer that moved it on next (to another wallet or within this
    /// one); null while the wallet holds it.
    #[schema(nullable)]
    sent: HistoryEvent,
}

/// A page of a wallet's inscription history.
#[derive(ToSchema)]
#[allow(dead_code)]
struct WalletHistory {
    /// The address asked for; null for a `pkscript` request.
    #[schema(nullable)]
    address: String,
    /// The lowercase pkScript asked for; null for an `address` request.
    #[schema(nullable)]
    pkscript: String,
    /// Newest receipt first.
    history: Vec<WalletStay>,
    limit: i64,
    has_more: bool,
    /// The `before_id` of the next page, null on the last page.
    #[schema(nullable)]
    next_before_id: i64,
    indexed_height: IndexedHeight,
}

/// A page of the inscriptions a wallet received, newest first, each with
/// the transfer that moved it on.
///
/// Each row is one stay: the creation or transfer that put an inscription
/// on the wallet's pkScript and the inscription's next transfer, null while
/// the wallet holds it. A wallet the database does not know answers an
/// empty list. Rows order by the receiving event's id, which `before_id`
/// pages by.
#[utoipa::path(
    get,
    path = "/v1/inscriptions/wallet/history",
    tag = "inscriptions",
    params(WalletHistoryParams, HistoryParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<WalletHistory>),
        (status = 400, description = "A missing or invalid parameter, or both or neither of `address` and `pkscript`", body = ErrorBody),
    ),
)]
pub(super) async fn wallet_history(
    State(db): State<Db>,
    JsQuery(query): JsQuery,
) -> Result<Reply, ApiError> {
    let (wallet, (limit, before_id)) =
        match Wallet::from_query(&query).and_then(|w| Ok((w, paging(&query)?))) {
            Ok(request) => request,
            Err(bad) => return Ok(bad.reply()),
        };
    let mut tx = snapshot(&db).await?;
    let height = indexed_height(&mut tx).await?;
    let network = network_in(&mut tx).await?;
    // inscriptions_events_pkscript_id_idx, read backwards from the cursor;
    // each receipt's next transfer through _sequence_number_id_idx
    let rows = match wallet.pkscript_bytes(&network) {
        None => Vec::new(),
        Some(script) => {
            sqlx::query!(
                r#"select r.id, r.prev_id,
                        encode(r.txid, 'hex') || ':' || r.vout as "r_outpoint!",
                        r."offset" as r_offset, r.pkscript as r_pkscript,
                        encode(rp.txid, 'hex') || ':' || rp.vout as "rp_outpoint?",
                        rp."offset" as "rp_offset?", rp.pkscript as "rp_pkscript?",
                        extract(epoch from rh.block_time)::int8 as "r_time?",
                        s.id as "s_id?",
                        encode(s.txid, 'hex') || ':' || s.vout as "s_outpoint?",
                        s."offset" as "s_offset?", s.pkscript as "s_pkscript?",
                        extract(epoch from sh.block_time)::int8 as "s_time?",
                        e.sequence_number, e.inscription_id, e."number",
                        e.content_type as "content_type?", e.media_type as "media_type?"
                    from inscriptions_events r
                    join inscriptions_entries e on e.sequence_number = r.sequence_number
                    left join inscriptions_events rp on rp.id = r.prev_id
                    left join lateral (
                        select n.id, n.txid, n.vout, n."offset", n.pkscript
                        from inscriptions_events n
                        where n.sequence_number = r.sequence_number and n.id > r.id
                        order by n.sequence_number, n.id
                        limit 1
                    ) s on true
                    left join inscriptions_block_hashes rh
                        on rh.block_height = (r.id >> 32)::int4
                    left join inscriptions_block_hashes sh
                        on sh.block_height = (s.id >> 32)::int4
                    where r.pkscript = $1 and r.id < $2
                    order by r.pkscript desc, r.id desc
                    limit $3;"#,
                script,
                before_id,
                limit + 1,
            )
            .fetch_all(&mut *tx)
            .await?
        }
    };
    tx.commit().await?;
    let (rows, has_more) = split_page(rows, limit);
    let next_before_id = has_more
        .then(|| rows.last().map(|row| row.id as f64))
        .flatten();
    let history: Vec<JsValue> = rows
        .into_iter()
        .map(|row| {
            let received_at = Location {
                outpoint: row.r_outpoint,
                offset: row.r_offset,
                pkscript: row.r_pkscript,
            };
            let sent = row.s_id.and_then(|s_id| {
                let to = Location::of(row.s_outpoint, row.s_offset, row.s_pkscript)?;
                let from = Location {
                    outpoint: received_at.outpoint.clone(),
                    offset: received_at.offset,
                    pkscript: received_at.pkscript.clone(),
                };
                Some(event_object(
                    s_id,
                    row.s_time,
                    true,
                    Some(from),
                    to,
                    &network,
                ))
            });
            let from = Location::of(row.rp_outpoint, row.rp_offset, row.rp_pkscript);
            let received = event_object(
                row.id,
                row.r_time,
                row.prev_id.is_some(),
                from,
                received_at,
                &network,
            );
            js_object! {
                "inscription" => js_object! {
                    "inscription_id" => row.inscription_id,
                    "number" => row.number as f64,
                    "sequence_number" => row.sequence_number as f64,
                    "content_type" => row.content_type,
                    "media_type" => row.media_type,
                    "cursed" => row.number < 0,
                },
                "received" => received,
                "sent" => sent,
            }
            .into()
        })
        .collect();
    let (address, pkscript) = match wallet {
        Wallet::Address(address) => (Some(address), None),
        Wallet::PkScript(pkscript) => (None, Some(pkscript)),
    };
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "address" => address,
            "pkscript" => pkscript,
            "history" => history,
            "limit" => limit as f64,
            "has_more" => has_more,
            "next_before_id" => next_before_id,
            "indexed_height" => height,
        },
    }))
}
