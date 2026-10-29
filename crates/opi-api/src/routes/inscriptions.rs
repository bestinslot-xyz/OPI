//! The inscriptions module's routes: `/v1/inscriptions/inscription`,
//! `/v1/inscriptions/wallet`, `/v1/inscriptions/outpoint` and
//! `/v1/inscriptions/outpoints`.
//!
//! They answer `{"error":null,"result":...}`. A missing or invalid parameter
//! or body answers `400 {"error":"<reason>","result":null}` and an unknown
//! inscription `404 {"error":"inscription not found","result":null}`.
//!
//! They read `inscriptions_entries`, one row per bound inscription at its
//! current location, which the inscriptions module keeps per block and on
//! reorg. Unbound inscriptions have no row; a burned one keeps its OP_RETURN
//! output, with a null `address`. Every answer carries `indexed_height`, the
//! highest block the module's database holds, read in the same
//! `REPEATABLE READ READ ONLY` snapshot as the rows: the answer describes the
//! chain up to that block, so an output confirmed above it is unknown, not
//! clean.
//!
//! An inscription is `{"inscription_id","number","sequence_number",
//! "content_type","media_type","charms","cursed","outpoint","offset",
//! "pkscript","address","genesis_height","last_moved_height"}`:
//! `content_type` is the envelope's content type as inscribed, `media_type`
//! the same lowercased without parameters (both null without one), `charms`
//! ord's charm bitfield, `cursed` whether `number` is negative, `offset` the
//! sat's offset in the output, and `pkscript` the output's script in hex.

pub(super) mod outputs;

use {
    super::{
        ModuleRoutes,
        paging::{
            BadParam, DEFAULT_LIMIT, MAX_CONTENT_TYPE_LEN, MAX_CURSOR_LEN, MAX_LIMIT,
            MAX_OUTPOINTS, Wallet, integer, optional_text, prefix_pattern, single, split_page,
            text,
        },
    },
    crate::{
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue},
        js_object,
        openapi::{self, Answer, ErrorBody, LimitParam},
        reply::{ApiError, Reply},
    },
    axum::{
        body::Bytes,
        extract::{State, rejection::BytesRejection},
    },
    serde_json::json,
    sqlx::{PgConnection, Postgres, Transaction},
    std::collections::HashMap,
    utoipa::{IntoParams, PartialSchema, ToSchema, openapi::RefOr, openapi::schema::Schema},
    utoipa_axum::routes,
};

#[cfg(test)]
mod tests;

/// The answer to an unknown inscription.
pub const NOT_FOUND: &str = "inscription not found";

pub fn routes(_db: &Db, _settings: &super::RouteSettings) -> ModuleRoutes {
    ModuleRoutes::new()
        .routes(routes!(inscription))
        .routes(routes!(wallet))
        .routes(routes!(outpoint))
        .routes(routes!(outpoints))
}

/// The highest block a module's database holds: null before its first
/// block.
pub struct IndexedHeight;

impl PartialSchema for IndexedHeight {
    fn schema() -> RefOr<Schema> {
        openapi::json_schema(json!({
            "type": ["integer", "null"],
            "description": "The highest block the answer covers: the rows were read in one snapshot with it. An output confirmed above it is unknown, not clean. Null before the first block.",
        }))
    }
}

impl ToSchema for IndexedHeight {}

/// An inscription at its current location.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(super) struct Inscription {
    /// `<txid>i<index>`.
    inscription_id: String,
    /// The inscription number; negative for a cursed inscription.
    number: i64,
    /// ord's sequence number, the inscription's place in creation order.
    sequence_number: i64,
    /// The envelope's content type as inscribed; null without one.
    #[schema(nullable)]
    content_type: String,
    /// `content_type` lowercased without parameters; null without one.
    #[schema(nullable)]
    media_type: String,
    /// ord's charm bitfield.
    charms: i32,
    /// Whether `number` is negative.
    cursed: bool,
    /// The output holding the inscription, `<txid>:<vout>`.
    outpoint: String,
    /// The inscribed sat's offset in the output.
    offset: i64,
    /// The output's script, hex.
    pkscript: String,
    /// The script's address; null for a script without one, such as an
    /// OP_RETURN that burned the inscription.
    #[schema(nullable)]
    address: String,
    /// The block that created the inscription.
    genesis_height: i32,
    /// The block that last moved it, its genesis block before a move.
    last_moved_height: i32,
}

/// An inscription and the height the answer covers.
#[derive(ToSchema)]
#[allow(dead_code)]
struct InscriptionInfo {
    #[serde(flatten)]
    inscription: Inscription,
    indexed_height: IndexedHeight,
}

/// An `inscriptions_entries` row.
pub(super) struct EntryRow {
    pub sequence_number: i64,
    pub inscription_id: String,
    pub number: i64,
    pub content_type: Option<String>,
    pub media_type: Option<String>,
    pub charms: i32,
    pub outpoint: String,
    pub offset: i64,
    pub pkscript: String,
    pub address: Option<String>,
    pub genesis_height: i32,
    pub last_moved_height: i32,
}

impl EntryRow {
    pub fn into_object(self) -> JsObject {
        js_object! {
            "inscription_id" => self.inscription_id,
            "number" => self.number as f64,
            "sequence_number" => self.sequence_number as f64,
            "content_type" => self.content_type,
            "media_type" => self.media_type,
            "charms" => self.charms,
            "cursed" => self.number < 0,
            "outpoint" => self.outpoint,
            "offset" => self.offset as f64,
            "pkscript" => self.pkscript,
            "address" => self.address,
            "genesis_height" => self.genesis_height,
            "last_moved_height" => self.last_moved_height,
        }
    }
}

/// A read-only snapshot of a module database: the height and the rows of an
/// answer come from the same state.
pub(super) async fn snapshot(db: &Db) -> Result<Transaction<'static, Postgres>, ApiError> {
    Ok(db
        .pool()?
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?)
}

/// The highest block of `inscriptions_block_hashes`, null for an empty
/// table.
pub(super) async fn indexed_height(conn: &mut PgConnection) -> Result<Option<i32>, ApiError> {
    Ok(
        sqlx::query_scalar!(r#"select max(block_height) as "h?" from inscriptions_block_hashes;"#)
            .fetch_one(conn)
            .await?,
    )
}

/// The inscriptions on `outpoints`, by outpoint, then offset, then sequence
/// number.
pub(super) async fn entries_on(
    conn: &mut PgConnection,
    outpoints: &[String],
) -> Result<Vec<EntryRow>, ApiError> {
    Ok(sqlx::query_as!(
        EntryRow,
        r#"select sequence_number, inscription_id, "number", content_type, media_type, charms,
                outpoint, "offset", pkscript, address, genesis_height, last_moved_height
            from inscriptions_entries
            where outpoint = any($1)
            order by outpoint, "offset", sequence_number;"#,
        outpoints,
    )
    .fetch_all(conn)
    .await?)
}

/// The inscriptions of each of `outpoints`, in its order (a repeated
/// outpoint answers each time), as `(outpoint, inscriptions)`.
pub(super) fn group_by_outpoint(
    outpoints: &[String],
    rows: Vec<EntryRow>,
) -> Vec<(String, Vec<JsValue>)> {
    let mut by_outpoint: HashMap<String, Vec<JsValue>> = HashMap::new();
    for row in rows {
        by_outpoint
            .entry(row.outpoint.clone())
            .or_default()
            .push(row.into_object().into());
    }
    outpoints
        .iter()
        .map(|outpoint| {
            let inscriptions = by_outpoint.get(outpoint).cloned().unwrap_or_default();
            (outpoint.clone(), inscriptions)
        })
        .collect()
}

fn is_hex_txid(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A u32 in decimal digits, written canonically.
fn parse_u32(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 10 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// An outpoint `<txid>:<vout>` (64 hex digits, a u32), lowercased and with
/// `vout` in canonical decimal, as the indexer stores it.
pub fn parse_outpoint(text: &str) -> Option<String> {
    let (txid, vout) = text.split_once(':')?;
    let vout = parse_u32(vout)?;
    is_hex_txid(txid).then(|| format!("{}:{vout}", txid.to_ascii_lowercase()))
}

/// An inscription id `<txid>i<index>` (64 hex digits, a u32), lowercased and
/// with `index` in canonical decimal, as the indexer stores it.
pub fn parse_inscription_id(text: &str) -> Option<String> {
    let txid = text.get(..64)?;
    let index = text.get(64..)?.strip_prefix('i')?;
    let index = parse_u32(index)?;
    is_hex_txid(txid).then(|| format!("{}i{index}", txid.to_ascii_lowercase()))
}

/// The outpoint the error message names.
const OUTPOINT_FORM: &str = "<txid>:<vout> (64 hex digits, a colon and a 32-bit output index)";

/// The `outpoints` of a batch body `{"outpoints":["<txid>:<vout>",...]}`:
/// 1 to [`MAX_OUTPOINTS`] outpoints, each read by [`parse_outpoint`].
pub fn outpoints_body(body: &[u8]) -> Result<Vec<String>, BadParam> {
    let shape = || {
        BadParam(format!(
            "the body must be JSON {{\"outpoints\":[...]}} with 1 to {MAX_OUTPOINTS} outpoints"
        ))
    };
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| shape())?;
    let list = value
        .get("outpoints")
        .and_then(serde_json::Value::as_array)
        .filter(|list| (1..=MAX_OUTPOINTS).contains(&list.len()))
        .ok_or_else(shape)?;
    list.iter()
        .enumerate()
        .map(|(i, item)| {
            item.as_str().and_then(parse_outpoint).ok_or_else(|| {
                BadParam(format!(
                    "outpoints[{i}] must be an outpoint {OUTPOINT_FORM}"
                ))
            })
        })
        .collect()
}

/// The body as a batch route reads it, or the answer to a body it cannot
/// read (one over the body limit, say).
pub(super) fn batch_outpoints(body: Result<Bytes, BytesRejection>) -> Result<Vec<String>, Reply> {
    let body = body
        .map_err(|rejection| Reply::error(rejection.status().as_u16(), &rejection.body_text()))?;
    outpoints_body(&body).map_err(BadParam::reply)
}

/// A batch body: up to 500 outpoints.
pub struct OutpointsBody;

impl PartialSchema for OutpointsBody {
    fn schema() -> RefOr<Schema> {
        openapi::json_schema(json!({
            "type": "object",
            "required": ["outpoints"],
            "properties": {
                "outpoints": {
                    "type": "array",
                    "description": "The outpoints, `<txid>:<vout>`; one may repeat.",
                    "minItems": 1,
                    "maxItems": MAX_OUTPOINTS,
                    "items": {"type": "string", "pattern": OUTPOINT_PATTERN},
                },
            },
        }))
    }
}

impl ToSchema for OutpointsBody {}

/// The pattern of an outpoint parameter.
pub const OUTPOINT_PATTERN: &str = "^[0-9a-fA-F]{64}:[0-9]{1,10}$";

/// A creation or transfer of an inscription, as the push feed carries it.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(in crate::routes) struct InscriptionsEvent {
    /// The event's id, which orders a block's events.
    id: i64,
    block_height: i32,
    /// `creation` or `transfer`.
    #[schema(schema_with = event_type_schema)]
    event_type: String,
    sequence_number: i64,
    inscription_id: String,
    /// The inscription number; null for an inscription the entries no
    /// longer hold.
    #[schema(nullable)]
    number: i64,
    /// Whether `number` is negative; null as `number` is.
    #[schema(nullable)]
    cursed: bool,
    /// The envelope's content type as inscribed; null without one.
    #[schema(nullable)]
    content_type: String,
    /// `content_type` lowercased without parameters; null without one.
    #[schema(nullable)]
    media_type: String,
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

fn event_type_schema() -> RefOr<Schema> {
    openapi::json_schema(json!({"type": "string", "enum": ["creation", "transfer"]}))
}

/// The lowest and the highest id of block `height`'s events, `None` for a
/// block without events. `id + 0` reads the extremes through the
/// block_height index instead of walking the primary key.
pub(in crate::routes) async fn block_event_ids(
    db: &Db,
    height: i32,
) -> Result<Option<(i64, i64)>, ApiError> {
    let row = sqlx::query!(
        r#"select min(id + 0) as "lo?", max(id + 0) as "hi?" from inscriptions_events where block_height = $1;"#,
        height,
    )
    .fetch_one(db.pool()?)
    .await?;
    Ok(row.lo.zip(row.hi))
}

/// Up to `limit` of block `height`'s creations and transfers with an id
/// above `after` and at most `last`, oldest first, each with its id.
pub(in crate::routes) async fn block_events(
    db: &Db,
    height: i32,
    after: i64,
    last: i64,
    limit: i64,
) -> Result<Vec<(i64, JsValue)>, ApiError> {
    let rows = sqlx::query!(
        r#"select v.id, v.block_height, v.sequence_number, v.inscription_id,
                e."number" as "number?", e.content_type as "content_type?",
                e.media_type as "media_type?",
                v.old_outpoint, v.old_offset, v.old_pkscript, v.old_address,
                v.new_outpoint, v.new_offset, v.new_pkscript, v.new_address
            from inscriptions_events v
            left join inscriptions_entries e on e.sequence_number = v.sequence_number
            where v.id > $2 and v.id <= $3 and v.block_height = $1
            order by v.id limit $4;"#,
        height,
        after,
        last,
        limit,
    )
    .fetch_all(db.pool()?)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let event_type = if row.old_outpoint.is_none() {
                "creation"
            } else {
                "transfer"
            };
            let event = js_object! {
                "id" => row.id as f64,
                "block_height" => row.block_height,
                "event_type" => event_type,
                "sequence_number" => row.sequence_number as f64,
                "inscription_id" => row.inscription_id,
                "number" => row.number.map(|n| n as f64),
                "cursed" => row.number.map(|n| n < 0),
                "content_type" => row.content_type,
                "media_type" => row.media_type,
                "old_outpoint" => row.old_outpoint,
                "old_offset" => row.old_offset.map(|n| n as f64),
                "old_pkscript" => row.old_pkscript,
                "old_address" => row.old_address,
                "new_outpoint" => row.new_outpoint,
                "new_offset" => row.new_offset as f64,
                "new_pkscript" => row.new_pkscript,
                "new_address" => row.new_address,
            };
            (row.id, JsValue::from(event))
        })
        .collect())
}

/// The parameters of `/v1/inscriptions/inscription`: exactly one of `id`
/// and `number`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct InscriptionParams {
    /// The inscription id, `<txid>i<index>`.
    #[param(pattern = "^[0-9a-fA-F]{64}i[0-9]{1,10}$")]
    id: Option<String>,
    /// The inscription number; negative for a cursed inscription.
    #[param(minimum = -2147483648, maximum = 2147483647)]
    number: Option<i64>,
}

/// The inscription a request names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InscriptionRef {
    Id(String),
    Number(i64),
}

impl InscriptionRef {
    /// Exactly one of `id` and `number`.
    pub fn from_query(query: &JsObject) -> Result<InscriptionRef, BadParam> {
        let id = single(query, "id")?.filter(|value| !value.is_empty());
        let number = single(query, "number")?.filter(|value| !value.is_empty());
        match (id, number) {
            (None, None) => Err(BadParam("id or number is required".into())),
            (Some(_), Some(_)) => Err(BadParam("give id or number, not both".into())),
            (Some(id), None) => parse_inscription_id(id).map(InscriptionRef::Id).ok_or_else(|| {
                BadParam(
                    "id must be an inscription id <txid>i<index> (64 hex digits, `i` and a 32-bit index)"
                        .into(),
                )
            }),
            (None, Some(number)) => {
                let digits = number.strip_prefix('-').unwrap_or(number);
                let parsed = (!digits.is_empty()
                    && digits.len() <= 10
                    && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| number.parse::<i64>().ok())
                .flatten()
                .filter(|n| i32::try_from(*n).is_ok());
                parsed.map(InscriptionRef::Number).ok_or_else(|| {
                    BadParam("number must be an integer from -2147483648 to 2147483647".into())
                })
            }
        }
    }
}

/// One inscription's metadata and current location.
///
/// The inscription is named by its id or its number. An unbound inscription
/// has no location and answers 404, as an unknown one does.
#[utoipa::path(
    get,
    path = "/v1/inscriptions/inscription",
    tag = "inscriptions",
    params(InscriptionParams),
    responses(
        (status = 200, description = "The inscription", body = Answer<InscriptionInfo>),
        (status = 400, description = "A missing or invalid parameter, or both or neither of `id` and `number`", body = ErrorBody),
        (status = 404, description = "`inscription not found`", body = ErrorBody),
    ),
)]
async fn inscription(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let wanted = match InscriptionRef::from_query(&query) {
        Ok(wanted) => wanted,
        Err(bad) => return Ok(bad.reply()),
    };
    let (id, number) = match &wanted {
        InscriptionRef::Id(id) => (Some(id.as_str()), None),
        InscriptionRef::Number(number) => (None, Some(*number)),
    };
    let mut tx = snapshot(&db).await?;
    let height = indexed_height(&mut tx).await?;
    // inscriptions_entries_inscription_id_idx or _number_idx: each
    // execution's custom plan folds the condition that does not apply
    let row = sqlx::query_as!(
        EntryRow,
        r#"select sequence_number, inscription_id, "number", content_type, media_type, charms,
                outpoint, "offset", pkscript, address, genesis_height, last_moved_height
            from inscriptions_entries
            where ($1::text is null or inscription_id = $1)
              and ($2::int8 is null or "number" = $2);"#,
        id,
        number,
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let Some(row) = row else {
        return Ok(Reply::error(404, NOT_FOUND));
    };
    let mut result = row.into_object();
    result.set("indexed_height", height);
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => result,
    }))
}

/// The wallet and filters of `/v1/inscriptions/wallet`.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct WalletParams {
    /// The wallet's address.
    #[param(max_length = 128)]
    address: Option<String>,
    /// The wallet's pkScript, hex.
    #[param(max_length = 20000, pattern = "^([0-9a-fA-F]{2})*$")]
    pkscript: Option<String>,
    /// Keeps the inscriptions of a media type, compared lowercased and
    /// without parameters: exactly (`text/plain`), or by prefix when it ends
    /// with `/` (`image/`).
    #[param(max_length = 256)]
    content_type: Option<String>,
    /// Keeps the inscriptions created at this height or above.
    #[param(minimum = 0, maximum = 2147483647)]
    from_height: Option<i64>,
    /// Keeps the inscriptions created at this height or below.
    #[param(minimum = 0, maximum = 2147483647)]
    to_height: Option<i64>,
    /// `true` keeps the cursed inscriptions (negative numbers), `false` the
    /// others.
    cursed: Option<bool>,
    /// The previous page's `next_after`: the page holds the inscriptions
    /// after it.
    #[param(max_length = 256)]
    after: Option<String>,
}

/// The content type filter of `/v1/inscriptions/wallet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentTypeFilter {
    /// The media type, exactly.
    Exact(String),
    /// The media types that start with it, which ends with `/`.
    Prefix(String),
}

impl ContentTypeFilter {
    /// The filter `content_type` names: lowercased, without parameters, a
    /// prefix when it ends with `/`.
    pub fn parse(text: &str) -> Result<ContentTypeFilter, BadParam> {
        let media = text
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if media.is_empty() || media.chars().any(char::is_whitespace) {
            return Err(BadParam(
                "content_type must be a media type (text/plain) or a prefix ending with / (image/)"
                    .into(),
            ));
        }
        Ok(if media.ends_with('/') {
            ContentTypeFilter::Prefix(media)
        } else {
            ContentTypeFilter::Exact(media)
        })
    }

    /// The filter as the result echoes it.
    pub fn text(&self) -> &str {
        match self {
            ContentTypeFilter::Exact(text) | ContentTypeFilter::Prefix(text) => text,
        }
    }
}

/// What `/v1/inscriptions/wallet` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletRequest {
    pub wallet: Wallet,
    pub content_type: Option<ContentTypeFilter>,
    pub from_height: Option<i32>,
    pub to_height: Option<i32>,
    pub cursed: Option<bool>,
    pub limit: i64,
    /// The sequence number the page starts below.
    pub after: Option<i64>,
}

impl WalletRequest {
    pub fn from_query(query: &JsObject) -> Result<WalletRequest, BadParam> {
        let wallet = Wallet::from_query(query)?;
        let content_type = match optional_text(query, "content_type", MAX_CONTENT_TYPE_LEN)? {
            None | Some("") => None,
            Some(text) => Some(ContentTypeFilter::parse(text)?),
        };
        let height = |key: &str| -> Result<Option<i32>, BadParam> {
            match single(query, key)? {
                None => Ok(None),
                Some(_) => Ok(Some(integer(query, key, 0, 0, i64::from(i32::MAX))? as i32)),
            }
        };
        let from_height = height("from_height")?;
        let to_height = height("to_height")?;
        if let (Some(from), Some(to)) = (from_height, to_height)
            && from > to
        {
            return Err(BadParam("from_height must not be above to_height".into()));
        }
        let cursed = match single(query, "cursed")? {
            None => None,
            Some("true") => Some(true),
            Some("false") => Some(false),
            Some(_) => return Err(BadParam("cursed must be true or false".into())),
        };
        let after = match optional_text(query, "after", MAX_CURSOR_LEN)? {
            None | Some("") => None,
            Some(text) => Some(
                (text.len() <= 18 && text.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| text.parse::<i64>().ok())
                    .flatten()
                    .ok_or_else(|| {
                        BadParam("after must be the next_after of a previous page".into())
                    })?,
            ),
        };
        Ok(WalletRequest {
            wallet,
            content_type,
            from_height,
            to_height,
            cursed,
            limit: integer(query, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
            after,
        })
    }
}

/// A page of a wallet's inscriptions.
#[derive(ToSchema)]
#[allow(dead_code)]
struct InscriptionsWallet {
    /// The address asked for; null for a `pkscript` request.
    #[schema(nullable)]
    address: String,
    /// The lowercase pkScript asked for; null for an `address` request.
    #[schema(nullable)]
    pkscript: String,
    /// The `content_type` filter as applied (lowercased, without
    /// parameters), null without one.
    #[schema(nullable)]
    content_type: String,
    /// The `from_height` filter, null without one.
    #[schema(nullable)]
    from_height: i32,
    /// The `to_height` filter, null without one.
    #[schema(nullable)]
    to_height: i32,
    /// The `cursed` filter, null without one.
    #[schema(nullable)]
    cursed: bool,
    /// Newest first, by sequence number.
    inscriptions: Vec<Inscription>,
    limit: i64,
    has_more: bool,
    /// The `after` of the next page, null on the last page.
    #[schema(nullable)]
    next_after: String,
    indexed_height: IndexedHeight,
}

/// A page of the inscriptions a wallet holds, newest first.
///
/// The inscriptions on the wallet's outputs, by `address` or `pkscript`,
/// by sequence number, descending, optionally of a content type, a genesis
/// height range or cursed or not. The list pages by key, so pages never
/// repeat or skip an inscription that stays in the wallet. A wallet the
/// database does not know answers an empty list.
#[utoipa::path(
    get,
    path = "/v1/inscriptions/wallet",
    tag = "inscriptions",
    params(WalletParams, LimitParam),
    responses(
        (status = 200, description = "The page", body = Answer<InscriptionsWallet>),
        (status = 400, description = "A missing or invalid parameter, or both or neither of `address` and `pkscript`", body = ErrorBody),
    ),
)]
async fn wallet(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let request = match WalletRequest::from_query(&query) {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply()),
    };
    let (exact, prefix) = match &request.content_type {
        None => (None, None),
        Some(ContentTypeFilter::Exact(media)) => (Some(media.clone()), None),
        Some(ContentTypeFilter::Prefix(media)) => (None, Some(prefix_pattern(media))),
    };
    let after = request.after.unwrap_or(i64::MAX);
    let from_height = request.from_height.unwrap_or(0);
    let to_height = request.to_height.unwrap_or(i32::MAX);
    let fetch = request.limit + 1;
    let mut tx = snapshot(&db).await?;
    let height = indexed_height(&mut tx).await?;
    // inscriptions_entries_address_seq_idx or _pkscript_seq_idx, read
    // backwards from the cursor. The wallet is bounded as a (column,
    // sequence_number) range and the order names the column, so the planner
    // reads the composite index instead of walking the primary key.
    let (address, pkscript, rows) = match request.wallet.clone() {
        Wallet::Address(address) => {
            let rows = sqlx::query_as!(
                EntryRow,
                r#"select sequence_number, inscription_id, "number", content_type, media_type,
                        charms, outpoint, "offset", pkscript, address, genesis_height,
                        last_moved_height
                    from inscriptions_entries
                    where (address, sequence_number) < ($1, $2)
                      and (address, sequence_number) > ($1, -1)
                      and ($3::text is null or media_type = $3)
                      and ($4::text is null or media_type like $4)
                      and genesis_height between $5 and $6
                      and ($7::bool is null or ("number" < 0) = $7)
                    order by address desc, sequence_number desc
                    limit $8;"#,
                address,
                after,
                exact,
                prefix,
                from_height,
                to_height,
                request.cursed,
                fetch,
            )
            .fetch_all(&mut *tx)
            .await?;
            (Some(address), None, rows)
        }
        Wallet::PkScript(pkscript) => {
            let rows = sqlx::query_as!(
                EntryRow,
                r#"select sequence_number, inscription_id, "number", content_type, media_type,
                        charms, outpoint, "offset", pkscript, address, genesis_height,
                        last_moved_height
                    from inscriptions_entries
                    where (pkscript, sequence_number) < ($1, $2)
                      and (pkscript, sequence_number) > ($1, -1)
                      and ($3::text is null or media_type = $3)
                      and ($4::text is null or media_type like $4)
                      and genesis_height between $5 and $6
                      and ($7::bool is null or ("number" < 0) = $7)
                    order by pkscript desc, sequence_number desc
                    limit $8;"#,
                pkscript,
                after,
                exact,
                prefix,
                from_height,
                to_height,
                request.cursed,
                fetch,
            )
            .fetch_all(&mut *tx)
            .await?;
            (None, Some(pkscript), rows)
        }
    };
    tx.commit().await?;
    let (rows, has_more) = split_page(rows, request.limit);
    let next_after = if has_more {
        rows.last().map(|row| row.sequence_number.to_string())
    } else {
        None
    };
    let inscriptions: Vec<JsValue> = rows
        .into_iter()
        .map(|row| row.into_object().into())
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "address" => address,
            "pkscript" => pkscript,
            "content_type" => request.content_type.as_ref().map(|c| c.text().to_string()),
            "from_height" => request.from_height,
            "to_height" => request.to_height,
            "cursed" => request.cursed,
            "inscriptions" => inscriptions,
            "limit" => request.limit as f64,
            "has_more" => has_more,
            "next_after" => next_after,
            "indexed_height" => height,
        },
    }))
}

/// The `outpoint` parameter.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct OutpointParam {
    /// The output, `<txid>:<vout>`.
    #[param(pattern = "^[0-9a-fA-F]{64}:[0-9]{1,10}$")]
    outpoint: String,
}

/// The inscriptions on an output.
#[derive(ToSchema)]
#[allow(dead_code)]
pub(super) struct OutpointInscriptions {
    /// The outpoint, lowercase.
    outpoint: String,
    /// By offset, then sequence number; empty for an output without
    /// inscriptions.
    inscriptions: Vec<Inscription>,
}

/// The inscriptions on an output and the height the answer covers.
#[derive(ToSchema)]
#[allow(dead_code)]
struct InscriptionsOutpoint {
    #[serde(flatten)]
    output: OutpointInscriptions,
    indexed_height: IndexedHeight,
}

/// The inscriptions on many outputs and the height the answer covers.
#[derive(ToSchema)]
#[allow(dead_code)]
struct InscriptionsOutpoints {
    /// One entry per requested outpoint, in the request's order.
    outpoints: Vec<OutpointInscriptions>,
    indexed_height: IndexedHeight,
}

/// The inscriptions an output holds, with their offsets.
///
/// An output without inscriptions, spent or never created answers an empty
/// list; one confirmed above `indexed_height` is unknown, not clean.
#[utoipa::path(
    get,
    path = "/v1/inscriptions/outpoint",
    tag = "inscriptions",
    params(OutpointParam),
    responses(
        (status = 200, description = "The output's inscriptions", body = Answer<InscriptionsOutpoint>),
        (status = 400, description = "A missing or invalid parameter", body = ErrorBody),
    ),
)]
async fn outpoint(State(db): State<Db>, JsQuery(query): JsQuery) -> Result<Reply, ApiError> {
    let outpoint = match text(&query, "outpoint", 128).and_then(|text| {
        parse_outpoint(text)
            .ok_or_else(|| BadParam(format!("outpoint must be an outpoint {OUTPOINT_FORM}")))
    }) {
        Ok(outpoint) => outpoint,
        Err(bad) => return Ok(bad.reply()),
    };
    let mut tx = snapshot(&db).await?;
    let height = indexed_height(&mut tx).await?;
    let rows = entries_on(&mut tx, std::slice::from_ref(&outpoint)).await?;
    tx.commit().await?;
    let inscriptions: Vec<JsValue> = rows
        .into_iter()
        .map(|row| row.into_object().into())
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "outpoint" => outpoint,
            "inscriptions" => inscriptions,
            "indexed_height" => height,
        },
    }))
}

/// The inscriptions on up to 500 outputs, with their offsets.
///
/// One entry per requested outpoint, in the request's order; an output
/// without inscriptions has an empty list. An output confirmed above
/// `indexed_height` is unknown, not clean. `/v1/outputs` adds each output's
/// runes.
#[utoipa::path(
    post,
    path = "/v1/inscriptions/outpoints",
    tag = "inscriptions",
    request_body(content = OutpointsBody, content_type = "application/json", description = "`{\"outpoints\":[\"<txid>:<vout>\",...]}`, 1 to 500 outpoints"),
    responses(
        (status = 200, description = "Each output's inscriptions", body = Answer<InscriptionsOutpoints>),
        (status = 400, description = "A body that is not `{\"outpoints\":[...]}` with 1 to 500 valid outpoints", body = ErrorBody),
        (status = 413, description = "A body over 2 MB", body = ErrorBody),
    ),
)]
async fn outpoints(
    State(db): State<Db>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Reply, ApiError> {
    let outpoints = match batch_outpoints(body) {
        Ok(outpoints) => outpoints,
        Err(reply) => return Ok(reply),
    };
    let mut tx = snapshot(&db).await?;
    let height = indexed_height(&mut tx).await?;
    let rows = entries_on(&mut tx, &outpoints).await?;
    tx.commit().await?;
    let entries: Vec<JsValue> = group_by_outpoint(&outpoints, rows)
        .into_iter()
        .map(|(outpoint, inscriptions)| {
            js_object! {
                "outpoint" => outpoint,
                "inscriptions" => inscriptions,
            }
            .into()
        })
        .collect();
    Ok(Reply::send_json(js_object! {
        "error" => JsValue::Null,
        "result" => js_object! {
            "outpoints" => entries,
            "indexed_height" => height,
        },
    }))
}
