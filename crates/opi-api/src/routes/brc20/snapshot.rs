//! `/v1/brc20/ticker/holders/snapshot?ticker=&block_height=&format=`: every
//! holder of a ticker at the end of a block, in one streamed answer.
//!
//! A holder's balance at height H is its latest row at or below H in
//! `brc20_historic_balances` (BRC-20, by pkScript) and
//! `brc20_prog_historical_balances` (BRC2.0, by EVM address). The rows merge
//! and order as `/v1/brc20/ticker/holders` does ([`HolderRow`]), so the
//! snapshot at the indexed tip lists the holders route's rows, all of them.
//! `block_height` defaults to the tip; a height above the tip or below the
//! ticker's deploy height answers 400.
//!
//! `format=json` (the default) answers
//! `{"error":null,"result":{"ticker","block_height","decimals",
//! "ticker_decimals","holders":[...]}}`, unpaged. `format=csv` answers a
//! header row and one row per holder in [`CSV_COLUMNS`] order, RFC 4180
//! quoted, as an attachment named `<ticker>-<height>.csv`.
//!
//! Rows stream from Postgres to the client through a bounded channel, in
//! chunks of about [`CHUNK_BYTES`], so a snapshot holds a few chunks in
//! memory whatever its size. A failure before the first chunk answers 500;
//! a failure after it ends the body early and is logged. At most
//! [`MAX_RUNNING`] snapshots run at once; another answers 503 until one
//! ends. A snapshot holds a pool connection while it streams.

use {
    super::tickers::{AMOUNT_DECIMALS, BRC20_PROG_MODULE_PKSCRIPT, HolderRow, ticker},
    crate::{
        config::Module,
        db::Db,
        extract::JsQuery,
        js::{JsObject, JsValue, json_stringify},
        reply::{self, ApiError, Reply},
        routes::paging::{BadParam, integer, single},
    },
    axum::{
        body::{Body, Bytes},
        extract::State,
        http::{HeaderValue, StatusCode, header},
        response::{IntoResponse, Response},
    },
    futures::StreamExt,
    std::{fmt::Write, sync::Arc},
    tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};

#[cfg(test)]
mod tests;

/// The most snapshots one app streams at once.
pub const MAX_RUNNING: usize = 4;

/// The body size at which a chunk is sent.
const CHUNK_BYTES: usize = 64 * 1024;

/// The chunks a snapshot buffers ahead of the client.
const CHANNEL_CHUNKS: usize = 4;

/// The CSV header row.
pub const CSV_COLUMNS: [&str; 7] = [
    "address",
    "evm_address",
    "pkscript",
    "brc20_balance",
    "brc20_available_balance",
    "prog_balance",
    "total_balance",
];

pub const CSV: &str = "text/csv; charset=utf-8";

/// The route's state: the database and the permits of running snapshots.
#[derive(Clone)]
pub struct Snapshots {
    db: Db,
    running: Arc<Semaphore>,
}

impl Snapshots {
    pub fn new(db: Db) -> Snapshots {
        Snapshots {
            db,
            running: Arc::new(Semaphore::new(MAX_RUNNING)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Csv,
}

/// What a snapshot request names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRequest {
    pub tick: String,
    /// `None` for the indexed tip.
    pub block_height: Option<i32>,
    pub format: Format,
}

impl SnapshotRequest {
    pub fn from_query(query: &JsObject) -> Result<SnapshotRequest, BadParam> {
        let tick = ticker(query, "ticker")?;
        let block_height = match single(query, "block_height")? {
            None => None,
            Some(_) => Some(integer(query, "block_height", 0, 0, i32::MAX.into())? as i32),
        };
        let format = match single(query, "format")? {
            None | Some("json") => Format::Json,
            Some("csv") => Format::Csv,
            Some(_) => return Err(BadParam("format must be json or csv".into())),
        };
        Ok(SnapshotRequest {
            tick,
            block_height,
            format,
        })
    }
}

/// A snapshot the route has checked and streams.
struct Plan {
    tick: String,
    block_height: i32,
    ticker_decimals: i32,
    format: Format,
}

/// A body chunk, or the failure that ends the body.
type Chunk = Result<Bytes, anyhow::Error>;

pub async fn snapshot(
    State(snapshots): State<Snapshots>,
    JsQuery(query): JsQuery,
) -> Result<Response, ApiError> {
    let request = match SnapshotRequest::from_query(&query) {
        Ok(request) => request,
        Err(bad) => return Ok(bad.reply().into_response()),
    };
    let Ok(permit) = snapshots.running.clone().try_acquire_owned() else {
        return Ok(Reply::error(503, "too many snapshots running, retry later").into_response());
    };
    let db = &snapshots.db;
    let tip = match db.latest_block_height(Module::Brc20).await? {
        JsValue::Number(height) => Some(height as i32),
        _ => None,
    };
    let block_height = match (request.block_height, tip) {
        (Some(height), Some(tip)) if height <= tip => height,
        (None, Some(tip)) => tip,
        _ => {
            return Ok(Reply::error(400, "block_height is above the indexed tip").into_response());
        }
    };
    let ticker = sqlx::query!(
        "select decimals, block_height from brc20_tickers where tick = $1;",
        request.tick
    )
    .fetch_optional(db.pool()?)
    .await?;
    let Some(ticker) = ticker else {
        return Ok(Reply::error(404, "ticker not found").into_response());
    };
    if block_height < ticker.block_height {
        return Ok(
            Reply::error(400, "block_height is below the ticker's deploy height").into_response(),
        );
    }
    let plan = Plan {
        tick: request.tick,
        block_height,
        ticker_decimals: ticker.decimals,
        format: request.format,
    };
    let headers = headers(&plan);

    let permit = Arc::new(permit);
    let (sender, mut receiver) = mpsc::channel(CHANNEL_CHUNKS);
    tokio::spawn(stream_rows(
        db.pool()?.clone(),
        plan,
        sender,
        permit.clone(),
    ));
    // The answer waits for the first chunk, so a query that fails before
    // any row answers 500.
    let first = match receiver.recv().await {
        Some(Ok(first)) => first,
        Some(Err(error)) => return Err(error.into()),
        None => {
            return Err(anyhow::anyhow!("the snapshot stream ended before its first chunk").into());
        }
    };
    let rest = futures::stream::unfold((receiver, permit), |(mut receiver, permit)| async move {
        let chunk = receiver.recv().await?;
        Some((chunk, (receiver, permit)))
    });
    let body = Body::from_stream(futures::stream::once(async { Ok(first) }).chain(rest));
    let mut response = body.into_response();
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().extend(headers);
    Ok(response)
}

fn headers(plan: &Plan) -> Vec<(header::HeaderName, HeaderValue)> {
    match plan.format {
        Format::Json => vec![(header::CONTENT_TYPE, HeaderValue::from_static(reply::JSON))],
        Format::Csv => vec![
            (header::CONTENT_TYPE, HeaderValue::from_static(CSV)),
            (
                header::CONTENT_DISPOSITION,
                content_disposition(&format!("{}-{}.csv", plan.tick, plan.block_height)),
            ),
        ],
    }
}

/// `attachment; filename="<name>"`. A name with a character outside
/// printable ASCII, a `"` or a `\` gets `_` for it in `filename` and its
/// UTF-8 form, percent-encoded, in `filename*` (RFC 6266).
pub fn content_disposition(name: &str) -> HeaderValue {
    let plain = |c: char| c == ' ' || (c.is_ascii_graphic() && c != '"' && c != '\\');
    let fallback: String = name
        .chars()
        .map(|c| if plain(c) { c } else { '_' })
        .collect();
    let mut value = format!("attachment; filename=\"{fallback}\"");
    if fallback != name {
        value.push_str("; filename*=UTF-8''");
        for b in name.bytes() {
            if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
                value.push(b as char);
            } else {
                write!(value, "%{b:02X}").expect("writing to a String");
            }
        }
    }
    HeaderValue::from_str(&value).expect("printable ASCII")
}

/// One RFC 4180 field: quoted when it holds a comma, a quote, CR or LF,
/// with each quote doubled.
pub fn push_csv_field(out: &mut String, field: &str) {
    if field.contains([',', '"', '\r', '\n']) {
        out.push('"');
        out.push_str(&field.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(field);
    }
}

/// One CSV line of `fields`, CRLF-terminated.
pub fn push_csv_line<'a>(out: &mut String, fields: impl IntoIterator<Item = &'a str>) {
    for (i, field) in fields.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_csv_field(out, field);
    }
    out.push_str("\r\n");
}

fn push_csv_row(out: &mut String, row: &HolderRow) {
    push_csv_line(
        out,
        [
            row.address.as_deref().unwrap_or(""),
            row.evm_address.as_deref().unwrap_or(""),
            row.pkscript.as_deref().unwrap_or(""),
            &row.balance,
            &row.available_balance,
            &row.prog_balance,
            &row.total_balance,
        ],
    );
}

/// The JSON answer up to its first holder.
fn json_head(plan: &Plan) -> String {
    let ticker = json_stringify(&JsValue::from(plan.tick.as_str())).unwrap_or_default();
    format!(
        r#"{{"error":null,"result":{{"ticker":{ticker},"block_height":{},"decimals":{AMOUNT_DECIMALS},"ticker_decimals":{},"holders":["#,
        plan.block_height, plan.ticker_decimals
    )
}

/// Runs the snapshot query and sends its rows to `sender` in chunks. It
/// stops when the client is gone (the receiver is dropped). `permit` is
/// held until both this and the body are done.
async fn stream_rows(
    pool: sqlx::PgPool,
    plan: Plan,
    sender: mpsc::Sender<Chunk>,
    permit: Arc<OwnedSemaphorePermit>,
) {
    let _permit = permit;
    let mut out = String::with_capacity(CHUNK_BYTES * 2);
    match plan.format {
        Format::Json => out.push_str(&json_head(&plan)),
        Format::Csv => push_csv_line(&mut out, CSV_COLUMNS),
    }
    // Each side takes, per holder, its latest row at or below the height,
    // then keeps the positive balances; `m` and the select are those of
    // the holders route.
    let mut rows = sqlx::query_as!(
        HolderRow,
        r#"with bl as (
                select distinct on (pkscript) pkscript, wallet, overall_balance, available_balance
                from brc20_historic_balances
                where tick = $1 and block_height <= $3 and pkscript <> $2
                order by pkscript, block_height desc, id desc
            ), b as (
                select pkscript, nullif(wallet, '') as wallet, overall_balance, available_balance
                from bl
                where overall_balance > 0
            ), pl as (
                select distinct on (wallet) wallet, amount
                from brc20_prog_historical_balances
                where ticker = $1 and block_height <= $3
                order by wallet, block_height desc
            ), p as (
                select pl.wallet as evm_address, e.btc_address, pl.amount
                from pl
                left join evm_addresses e on e.evm_address = pl.wallet
                where pl.amount > 0
            ), m as (
                select coalesce(b.wallet, p.btc_address) as address,
                       b.pkscript, p.evm_address,
                       coalesce(b.overall_balance, 0) as balance,
                       coalesce(b.available_balance, 0) as available_balance,
                       coalesce(p.amount, 0) as prog_balance
                from b full join p on b.wallet = p.btc_address
            )
            select address as "address?", pkscript as "pkscript?", evm_address as "evm_address?",
                   balance::text as "balance!",
                   available_balance::text as "available_balance!",
                   prog_balance::text as "prog_balance!",
                   (balance + prog_balance)::text as "total_balance!"
            from m
            order by balance + prog_balance desc,
                     coalesce(address, pkscript, evm_address) asc,
                     pkscript asc nulls first,
                     evm_address asc nulls first;"#,
        plan.tick,
        BRC20_PROG_MODULE_PKSCRIPT,
        plan.block_height,
    )
    .fetch(&pool);
    let mut sent = false;
    let mut first_row = true;
    loop {
        match rows.next().await {
            Some(Ok(row)) => {
                match plan.format {
                    Format::Json => {
                        if !first_row {
                            out.push(',');
                        }
                        out.push_str(
                            &json_stringify(&JsValue::from(row.into_object())).unwrap_or_default(),
                        );
                    }
                    Format::Csv => push_csv_row(&mut out, &row),
                }
                first_row = false;
                if out.len() >= CHUNK_BYTES {
                    let chunk = Bytes::from(std::mem::replace(
                        &mut out,
                        String::with_capacity(CHUNK_BYTES * 2),
                    ));
                    if sender.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                    sent = true;
                }
            }
            Some(Err(error)) => {
                if sent {
                    tracing::error!(
                        "snapshot of {:?} at {} ended early: {}",
                        plan.tick,
                        plan.block_height,
                        error.to_string().escape_debug()
                    );
                }
                let _ = sender.send(Err(error.into())).await;
                return;
            }
            None => break,
        }
    }
    if plan.format == Format::Json {
        out.push_str("]}}");
    }
    let _ = sender.send(Ok(Bytes::from(out))).await;
}
