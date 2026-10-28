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
//! The tip, the ticker and the rows are read in one `REPEATABLE READ READ
//! ONLY` transaction, so they come from one database snapshot. Rows stream
//! from Postgres to the client through a bounded channel, in chunks of
//! about [`CHUNK_BYTES`], so a snapshot holds a few chunks in memory
//! whatever its size, and the body ends with an explicit [`Item::Done`].
//! Opening the transaction and reading the tip and ticker past the time
//! limit answers 503 `snapshot timed out`. A failure before the first chunk
//! answers 500. A failure after it, the
//! snapshot's time limit (API_SNAPSHOT_TIMEOUT_MS) passing, or a stream that
//! stops without `Done` ends the body with an error, so the client sees an
//! aborted transfer; the cause is logged. A snapshot holds a pool
//! connection while it streams; one that ends without committing closes the
//! connection, which ends its Postgres transaction. At most [`cap`]
//! snapshots run at once; another answers 503 until one ends.

use {
    super::tickers::{AMOUNT_DECIMALS, BRC20_PROG_MODULE_PKSCRIPT, HolderRow, ticker},
    crate::{
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
    sqlx::{PgPool, Postgres, pool::PoolConnection},
    std::{fmt::Write, sync::Arc, time::Duration},
    tokio::{
        sync::{OwnedSemaphorePermit, Semaphore, mpsc},
        time::{Instant, timeout_at},
    },
};

#[cfg(test)]
mod tests;

/// The most snapshots one app streams at once, whatever the pool size.
pub const MAX_RUNNING: usize = 4;

/// The snapshots that may run at once on a pool of `max_connections`: at
/// most [`MAX_RUNNING`] and at most half the pool, so other routes keep
/// connections, and at least one.
pub fn cap(max_connections: u32) -> usize {
    MAX_RUNNING.min(max_connections as usize / 2).max(1)
}

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

/// The route's state: the database, the permits of running snapshots and
/// how long one may stream.
#[derive(Clone)]
pub struct Snapshots {
    db: Db,
    running: Arc<Semaphore>,
    timeout: Duration,
}

impl Snapshots {
    /// The state for `db`, whose pool size (DB_MAX_CONNECTIONS) sets the
    /// [`cap`]. A test double without a pool gets [`MAX_RUNNING`].
    pub fn new(db: Db, timeout: Duration) -> Snapshots {
        let permits = match db.pool() {
            Ok(pool) => cap(pool.options().get_max_connections()),
            Err(_) => MAX_RUNNING,
        };
        Snapshots {
            db,
            running: Arc::new(Semaphore::new(permits)),
            timeout,
        }
    }
}

/// What the streaming task sends the body.
enum Item {
    Chunk(Bytes),
    /// The last item of a complete answer.
    Done,
    /// A failure that ends the answer early.
    Failed(anyhow::Error),
}

/// A pool connection inside a `REPEATABLE READ READ ONLY` transaction. One
/// dropped before [`ReadSnapshot::end`] succeeds is closed rather than
/// returned to the pool, which ends the transaction and any query still
/// running on it.
struct ReadSnapshot {
    conn: PoolConnection<Postgres>,
    open: bool,
}

impl ReadSnapshot {
    async fn begin(pool: &PgPool) -> Result<ReadSnapshot, sqlx::Error> {
        let mut snapshot = ReadSnapshot {
            conn: pool.acquire().await?,
            open: true,
        };
        sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *snapshot.conn)
            .await?;
        Ok(snapshot)
    }

    /// Ends the transaction; the connection goes back to the pool.
    async fn end(mut self) -> Result<(), sqlx::Error> {
        sqlx::query("COMMIT").execute(&mut *self.conn).await?;
        self.open = false;
        Ok(())
    }
}

impl Drop for ReadSnapshot {
    fn drop(&mut self) {
        if self.open {
            self.conn.close_on_drop();
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
    let deadline = Instant::now() + snapshots.timeout;
    let timed_out = || Ok(Reply::error(503, "snapshot timed out").into_response());
    let pool = snapshots.db.pool()?;
    // A future dropped at the deadline drops its ReadSnapshot, which closes
    // the connection.
    let Ok(read) = timeout_at(deadline, ReadSnapshot::begin(pool)).await else {
        return timed_out();
    };
    let mut read = read?;
    let Ok(checked) = timeout_at(deadline, check(&mut read, request)).await else {
        return timed_out();
    };
    let plan = match checked? {
        Ok(plan) => plan,
        Err(refusal) => {
            // a failed COMMIT closes the connection
            let _ = timeout_at(deadline, read.end()).await;
            return Ok(refusal.into_response());
        }
    };
    let headers = headers(&plan);

    let (sender, mut receiver) = mpsc::channel(CHANNEL_CHUNKS);
    tokio::spawn(stream_rows(read, plan, sender, permit, deadline));
    // The answer waits for the first chunk, so a query that fails before
    // any row answers 500.
    let first = match receiver.recv().await {
        Some(Item::Chunk(first)) => first,
        Some(Item::Failed(error)) => return Err(error.into()),
        Some(Item::Done) | None => {
            return Err(anyhow::anyhow!("the snapshot ended before its first chunk").into());
        }
    };
    let rest = futures::stream::unfold(Some(receiver), |receiver| async move {
        let mut receiver = receiver?;
        let item = match receiver.recv().await {
            Some(Item::Chunk(chunk)) => return Some((Ok(chunk), Some(receiver))),
            Some(Item::Done) => return None,
            Some(Item::Failed(error)) => error,
            None => anyhow::anyhow!("snapshot ended early"),
        };
        Some((Err(item), None))
    });
    let body = Body::from_stream(futures::stream::once(async { Ok(first) }).chain(rest));
    let mut response = body.into_response();
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().extend(headers);
    Ok(response)
}

/// The snapshot `request` names, read in `read`'s transaction: its height
/// (the tip when it names none) and the ticker's precision, or the 400 or
/// 404 answer that refuses it.
async fn check(
    read: &mut ReadSnapshot,
    request: SnapshotRequest,
) -> Result<Result<Plan, Reply>, ApiError> {
    let tip: Option<i32> = sqlx::query_scalar!(
        "SELECT max(block_height) as max_block_height FROM brc20_block_hashes;"
    )
    .fetch_one(&mut *read.conn)
    .await?;
    let block_height = match (request.block_height, tip) {
        (Some(height), Some(tip)) if height <= tip => height,
        (None, Some(tip)) => tip,
        _ => {
            return Ok(Err(Reply::error(
                400,
                "block_height is above the indexed tip",
            )));
        }
    };
    let ticker = sqlx::query!(
        "select decimals, block_height from brc20_tickers where tick = $1;",
        request.tick
    )
    .fetch_optional(&mut *read.conn)
    .await?;
    let Some(ticker) = ticker else {
        return Ok(Err(Reply::error(404, "ticker not found")));
    };
    if block_height < ticker.block_height {
        return Ok(Err(Reply::error(
            400,
            "block_height is below the ticker's deploy height",
        )));
    }
    Ok(Ok(Plan {
        tick: request.tick,
        block_height,
        ticker_decimals: ticker.decimals,
        format: request.format,
    }))
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

/// Why a snapshot stopped before its end.
enum Stop {
    /// The client is gone: the body, and so the receiver, was dropped.
    Gone,
    /// The deadline passed.
    TimedOut,
}

/// Sends `item` unless the client is gone or the deadline passes first.
async fn send(sender: &mpsc::Sender<Item>, deadline: Instant, item: Item) -> Result<(), Stop> {
    match timeout_at(deadline, sender.send(item)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(Stop::Gone),
        Err(_) => Err(Stop::TimedOut),
    }
}

/// Runs the snapshot query in `read`'s transaction and sends its rows to
/// `sender` in chunks, then [`Item::Done`]. It holds `permit` and the
/// connection until it returns: at the end, when the client is gone, on a
/// failure, or at `deadline`. The connection is closed or returned before
/// the slot frees.
async fn stream_rows(
    read: ReadSnapshot,
    plan: Plan,
    sender: mpsc::Sender<Item>,
    permit: OwnedSemaphorePermit,
    deadline: Instant,
) {
    let mut permit = Some(permit);
    // `write_rows` owns `read`, so its connection is gone when it returns.
    let result = write_rows(read, &plan, &sender, &mut permit, deadline).await;
    drop(permit);
    let stopped = match result {
        Ok(()) => return,
        Err(stop) => stop,
    };
    match stopped {
        Stop::Gone => {
            tracing::debug!(
                "snapshot of {:?} at {}: the client went away",
                plan.tick,
                plan.block_height
            );
        }
        Stop::TimedOut => {
            tracing::warn!(
                "snapshot of {:?} at {} stopped at its time limit",
                plan.tick,
                plan.block_height
            );
        }
    }
}

async fn write_rows(
    mut read: ReadSnapshot,
    plan: &Plan,
    sender: &mpsc::Sender<Item>,
    permit: &mut Option<OwnedSemaphorePermit>,
    deadline: Instant,
) -> Result<(), Stop> {
    let mut out = String::with_capacity(CHUNK_BYTES * 2);
    match plan.format {
        Format::Json => out.push_str(&json_head(plan)),
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
    .fetch(&mut *read.conn);
    let mut sent = false;
    let mut first_row = true;
    loop {
        let next = timeout_at(deadline, rows.next())
            .await
            .map_err(|_| Stop::TimedOut)?;
        match next {
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
                    send(sender, deadline, Item::Chunk(chunk)).await?;
                    sent = true;
                }
            }
            Some(Err(error)) => {
                // Before the first chunk the handler answers 500 and logs
                // the error itself.
                if sent {
                    tracing::error!(
                        "snapshot of {:?} at {} ended early: {}",
                        plan.tick,
                        plan.block_height,
                        error.to_string().escape_debug()
                    );
                }
                let _ = send(sender, deadline, Item::Failed(error.into())).await;
                return Ok(());
            }
            None => break,
        }
    }
    drop(rows);
    if let Err(error) = read.end().await {
        tracing::error!(
            "snapshot of {:?} at {}: commit: {}",
            plan.tick,
            plan.block_height,
            error.to_string().escape_debug()
        );
        let _ = send(sender, deadline, Item::Failed(error.into())).await;
        return Ok(());
    }
    if plan.format == Format::Json {
        out.push_str("]}}");
    }
    send(sender, deadline, Item::Chunk(Bytes::from(out))).await?;
    // The slot frees before the client can read the end of the answer; the
    // connection went back to the pool at the commit.
    drop(permit.take());
    send(sender, deadline, Item::Done).await
}
