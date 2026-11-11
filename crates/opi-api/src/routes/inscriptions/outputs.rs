//! `POST /v1/outputs`: the inscriptions and runes on up to 500 outputs, one
//! answer across the inscriptions and runes modules, for a wallet choosing
//! the outputs it may spend.
//!
//! The route is registered whatever the enabled modules; without both
//! inscriptions and runes it answers
//! `404 {"error":"<module> is not enabled on this node","result":null}`,
//! since an output holding what a missing module tracks would look clean.
//! Each module's rows and height come from one snapshot of its database, and
//! `indexed_height` is the lower of the two heights: an output confirmed
//! above it is unknown, not clean.

use {
    super::{
        IndexedHeight, Inscription, OutpointsBody, batch_outpoints, entries_on, group_by_outpoint,
        indexed_height, snapshot,
    },
    crate::{
        config::Module,
        db::Db,
        js::JsValue,
        js_object,
        openapi::{Answer, ErrorBody},
        reply::{ApiError, Reply},
    },
    axum::{
        body::Bytes,
        extract::{State, rejection::BytesRejection},
    },
    opi_runes::names::{spaced_name, symbol_char},
    sqlx::PgConnection,
    std::collections::HashMap,
    utoipa::ToSchema,
};

/// The databases `/v1/outputs` reads: each of the two modules' when it is
/// enabled.
#[derive(Clone)]
pub struct Outputs {
    pub inscriptions: Option<Db>,
    pub runes: Option<Db>,
}

/// A rune balance of an output.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(in crate::routes) struct OutputRune {
    /// The rune id, `block:tx`.
    rune_id: String,
    /// The name's letters; null for a rune `runes_id_to_entry` does not
    /// list.
    #[schema(nullable)]
    rune_name: String,
    /// The name with its spacers (`•`); null as `rune_name` is.
    #[schema(nullable)]
    spaced_rune_name: String,
    /// The fractional digits of `amount`; null as `rune_name` is.
    #[schema(nullable)]
    divisibility: i32,
    /// The currency symbol, null without one.
    #[schema(nullable)]
    symbol: String,
    /// The output's balance of the rune, a raw integer as a string.
    amount: String,
}

/// The inscriptions and runes on an output.
#[derive(ToSchema)]
#[allow(dead_code)]
struct Output {
    /// The outpoint, lowercase.
    outpoint: String,
    /// By offset, then sequence number; empty for an output without
    /// inscriptions.
    inscriptions: Vec<Inscription>,
    /// In the order the output holds them; empty for an output without
    /// runes.
    runes: Vec<OutputRune>,
}

/// Each module's indexed height.
#[derive(ToSchema)]
#[allow(dead_code)]
struct OutputsHeights {
    inscriptions: IndexedHeight,
    runes: IndexedHeight,
}

/// The inscriptions and runes on many outputs.
#[derive(ToSchema)]
#[allow(dead_code)]
struct OutputsResult {
    /// One entry per requested outpoint, in the request's order.
    outputs: Vec<Output>,
    /// The lower of the two modules' heights, null while either has no
    /// block: an output confirmed above it is unknown, not clean.
    indexed_height: IndexedHeight,
    heights: OutputsHeights,
}

/// An unspent output's rune balance.
pub(in crate::routes) struct RuneRow {
    pub outpoint: String,
    rune_id: String,
    rune_name: Option<String>,
    spacers: Option<i64>,
    divisibility: Option<i32>,
    symbol: Option<String>,
    amount: String,
}

impl RuneRow {
    /// The balance as an [`OutputRune`].
    pub fn into_json(self) -> JsValue {
        let spaced = self
            .rune_name
            .as_deref()
            .map(|name| spaced_name(name, self.spacers.unwrap_or(0)));
        js_object! {
            "rune_id" => self.rune_id,
            "rune_name" => self.rune_name,
            "spaced_rune_name" => spaced,
            "divisibility" => self.divisibility,
            "symbol" => self.symbol.as_deref().and_then(symbol_char).map(String::from),
            "amount" => self.amount,
        }
        .into()
    }
}

/// The highest block of `runes_block_hashes`, null for an empty table.
pub(in crate::routes) async fn runes_height(
    conn: &mut PgConnection,
) -> Result<Option<i32>, ApiError> {
    Ok(
        sqlx::query_scalar!(r#"select max(block_height) as "h?" from runes_block_hashes;"#)
            .fetch_one(conn)
            .await?,
    )
}

/// The rune balances of the unspent ones of `outpoints`, by outpoint, each
/// output's balances in its own order.
pub(in crate::routes) async fn runes_on(
    conn: &mut PgConnection,
    outpoints: &[String],
) -> Result<Vec<RuneRow>, ApiError> {
    // the primary key of runes_outpoint_to_balances, (txid, vout)
    let wanted = opi_runes::OutpointColumns::lookup(outpoints);
    Ok(sqlx::query_as!(
        RuneRow,
        r#"select encode(o.txid, 'hex') || ':' || o.vout as "outpoint!",
                u.rune_id as "rune_id!", e.rune_name as "rune_name?",
                e.spacers as "spacers?", e.divisibility as "divisibility?",
                e.symbol as "symbol?", u.balance::text as "amount!"
            from (select distinct * from unnest($1::bytea[], $2::int4[])) as w(txid, vout)
            join runes_outpoint_to_balances o on o.txid = w.txid and o.vout = w.vout
            cross join lateral unnest(o.rune_ids, o.balances)
                with ordinality as u(rune_id, balance, n)
            left join runes_id_to_entry e on e.rune_id = u.rune_id
            where o.spent_block_height is null
            order by o.txid, o.vout, u.n;"#,
        &wanted.txids,
        &wanted.vouts,
    )
    .fetch_all(conn)
    .await?)
}

/// The inscriptions and runes on up to 500 outputs.
///
/// Per requested outpoint, in the request's order, its inscriptions (as
/// `/v1/inscriptions/outpoints` lists them) and its rune balances; an
/// output holding neither has two empty lists. It needs both the
/// inscriptions and the runes module. An output confirmed above
/// `indexed_height` is unknown, not clean.
#[utoipa::path(
    post,
    path = "/v1/outputs",
    tag = "outputs",
    request_body(content = OutpointsBody, content_type = "application/json", description = "`{\"outpoints\":[\"<txid>:<vout>\",...]}`, 1 to 500 outpoints"),
    responses(
        (status = 200, description = "Each output's inscriptions and runes", body = Answer<OutputsResult>),
        (status = 400, description = "A body that is not `{\"outpoints\":[...]}` with 1 to 500 valid outpoints", body = ErrorBody),
        (status = 404, description = "`inscriptions is not enabled on this node` or `runes is not enabled on this node`: the route needs both modules", body = ErrorBody),
        (status = 413, description = "A body over 2 MB", body = ErrorBody),
    ),
)]
pub async fn outputs(
    State(outputs): State<Outputs>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Reply, ApiError> {
    let (inscriptions_db, runes_db) = match (&outputs.inscriptions, &outputs.runes) {
        (Some(inscriptions), Some(runes)) => (inscriptions, runes),
        (None, _) => return Ok(not_enabled(Module::Inscriptions)),
        (_, None) => return Ok(not_enabled(Module::Runes)),
    };
    let outpoints = match batch_outpoints(body) {
        Ok(outpoints) => outpoints,
        Err(reply) => return Ok(reply),
    };
    let ((inscriptions_height, entries), (runes_height, runes)) = futures::try_join!(
        async {
            let mut tx = snapshot(inscriptions_db).await?;
            let height = indexed_height(&mut tx).await?;
            let rows = entries_on(&mut tx, &outpoints).await?;
            tx.commit().await?;
            Ok::<_, ApiError>((height, rows))
        },
        async {
            let mut tx = snapshot(runes_db).await?;
            let height = runes_height(&mut tx).await?;
            let rows = runes_on(&mut tx, &outpoints).await?;
            tx.commit().await?;
            Ok::<_, ApiError>((height, rows))
        },
    )?;
    let mut runes_of: HashMap<String, Vec<JsValue>> = HashMap::new();
    for row in runes {
        runes_of
            .entry(row.outpoint.clone())
            .or_default()
            .push(row.into_json());
    }
    let entries: Vec<JsValue> = group_by_outpoint(&outpoints, entries)
        .into_iter()
        .map(|(outpoint, inscriptions)| {
            let runes = runes_of.get(&outpoint).cloned().unwrap_or_default();
            js_object! {
                "outpoint" => outpoint,
                "inscriptions" => inscriptions,
                "runes" => runes,
            }
            .into()
        })
        .collect();
    let indexed_height = inscriptions_height.zip(runes_height).map(|(a, b)| a.min(b));
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "outputs" => entries,
            "indexed_height" => indexed_height,
            "heights" => js_object! {
                "inscriptions" => inscriptions_height,
                "runes" => runes_height,
            },
        },
    }))
}

/// The answer without `module`.
pub(in crate::routes) fn not_enabled(module: Module) -> Reply {
    Reply::error(
        404,
        &format!("{} is not enabled on this node", module.name()),
    )
}
