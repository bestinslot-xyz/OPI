//! BRC2.0 token balances, read from the in-process engine's logs.
//!
//! The rules are those of bestinslot-xyz/brc20-prog-balance-tracker. A
//! `BRC20Created(bytes ticker, address contract_address)` log from the BRC20
//! controller registers the token contract under the ticker its `name()`
//! returns. An ERC-20 `Transfer(from, to, value)` log from a registered
//! contract moves `value` between the two wallets' balances: from the zero
//! address it is a mint, to the zero address a burn, and a zero value changes
//! nothing. Wallets are lowercase EVM addresses and amounts are the token's
//! raw integer units.
//!
//! `brc20_prog_tickers` holds the registered contracts,
//! `brc20_prog_current_balances` each wallet's balance of each ticker and the
//! height of its last change, and `brc20_prog_historical_balances` the balance
//! at each height where it changed. Each block's rows are written in the
//! transaction that stores the block's hashes, and a reorg deletes the rows
//! above its height and restores each affected current balance from its
//! latest historical row. Both keep the BRC2.0 holder counts of
//! `ticker_holders` current.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
};

use alloy_primitives::{Address, B256};
use alloy_sol_types::{SolCall, SolEvent, sol};
use brc20_prog::{
    Brc20Prog, Brc20ProgApiServer,
    types::{EthCall, GetLogsFilter, LogED, RawBytes},
};
use sqlx::{PgConnection, Pool, Postgres, Row};

sol! {
    event BRC20Created(bytes indexed ticker, address indexed contract_address);
    event Transfer(address indexed from, address indexed to, uint256 value);
    function name() external view returns (string memory);
}

/// The BRC20 controller, at the same address on every network.
pub const CONTROLLER_ADDRESS: &str = "0xc54dd4581af2dbf18e4d90840226756e9d2b3cdb";

/// The zero address, the source of a mint and the target of a burn.
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// The tables and indexes, also in db_init.sql.
pub const TABLES_SQL: &str = "CREATE TABLE IF NOT EXISTS public.brc20_prog_tickers (
	contract_address text NOT NULL,
	ticker text NOT NULL,
	ticker_hash text NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_prog_tickers_pk PRIMARY KEY (contract_address)
);
CREATE INDEX IF NOT EXISTS brc20_prog_tickers_ticker_idx ON public.brc20_prog_tickers USING btree (ticker);
CREATE INDEX IF NOT EXISTS brc20_prog_tickers_block_height_idx ON public.brc20_prog_tickers USING btree (block_height);

CREATE TABLE IF NOT EXISTS public.brc20_prog_current_balances (
	wallet text NOT NULL,
	ticker text NOT NULL,
	amount numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_prog_current_balances_pk PRIMARY KEY (wallet, ticker)
);
CREATE INDEX IF NOT EXISTS brc20_prog_current_balances_ticker_idx ON public.brc20_prog_current_balances USING btree (ticker);
CREATE INDEX IF NOT EXISTS brc20_prog_current_balances_block_height_idx ON public.brc20_prog_current_balances USING btree (block_height);

CREATE TABLE IF NOT EXISTS public.brc20_prog_historical_balances (
	wallet text NOT NULL,
	ticker text NOT NULL,
	amount numeric(40) NOT NULL,
	block_height int4 NOT NULL,
	CONSTRAINT brc20_prog_historical_balances_pk PRIMARY KEY (wallet, ticker, block_height)
);
CREATE INDEX IF NOT EXISTS brc20_prog_historical_balances_ticker_idx ON public.brc20_prog_historical_balances USING btree (ticker);
CREATE INDEX IF NOT EXISTS brc20_prog_historical_balances_block_height_idx ON public.brc20_prog_historical_balances USING btree (block_height);
";

/// The tables a block writes.
pub const TABLES: [&str; 3] = [
    "brc20_prog_tickers",
    "brc20_prog_current_balances",
    "brc20_prog_historical_balances",
];

/// Present while the tables wait to be filled from the engine's logs; its one
/// row holds the highest height filled so far, NULL before the first batch.
pub const FILL_TABLE: &str = "brc20_prog_balances_fill";

/// Blocks per fill transaction.
pub const FILL_BLOCKS: i32 = 1_000;

/// An engine log, as the rules read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgLog {
    pub block_height: i32,
    /// The emitting contract, lowercase.
    pub address: String,
    pub topics: Vec<B256>,
    pub data: Vec<u8>,
}

impl ProgLog {
    pub fn from_log(log: &LogED) -> Self {
        let block_height: u64 = log.block_number.into();
        ProgLog {
            block_height: block_height as i32,
            address: lowercase_address(&log.address.address),
            topics: log.topics.iter().map(|topic| topic.bytes).collect(),
            data: log.data.bytes.to_vec(),
        }
    }
}

/// The amounts of a `Transfer` log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferLog {
    pub from: String,
    pub to: String,
    pub amount: u128,
}

/// A log the rules act on, before its ticker name is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifiedLog {
    Created {
        ticker_hash: String,
        contract_address: String,
    },
    /// A `Transfer` log from a contract other than the controller. Its
    /// decoding error matters only when the contract is registered.
    Transfer {
        contract_address: String,
        transfer: Result<TransferLog, String>,
    },
}

/// A log the rules act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalanceEvent {
    Created {
        ticker: String,
        ticker_hash: String,
        contract_address: String,
    },
    Transfer {
        contract_address: String,
        transfer: Result<TransferLog, String>,
    },
}

/// A registered token contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickerRow {
    pub ticker: String,
    pub ticker_hash: String,
    pub contract_address: String,
}

/// A wallet's balance of a ticker at the end of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceRow {
    pub wallet: String,
    pub ticker: String,
    pub amount: u128,
}

/// What a block changes: the contracts it registers, and the end-of-block
/// balance of every (wallet, ticker) it touches, in order of first touch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlockChanges {
    pub new_tickers: Vec<TickerRow>,
    pub balances: Vec<BalanceRow>,
}

fn lowercase_address(address: &Address) -> String {
    format!("0x{}", hex::encode(address.as_slice()))
}

/// The address in the low 20 bytes of an indexed topic, lowercase.
fn address_from_topic(topic: &B256) -> String {
    format!("0x{}", hex::encode(&topic[12..32]))
}

/// The amount in the first 32-byte word of a log's data. A value above 128
/// bits is an error.
fn amount_from_data(data: &[u8]) -> Result<u128, String> {
    let word = data
        .get(..32)
        .ok_or_else(|| format!("Transfer data is {} bytes, expected 32", data.len()))?;
    let (high, low) = word.split_at(16);
    if high.iter().any(|byte| *byte != 0) {
        return Err(format!(
            "Transfer value 0x{} exceeds 128 bits",
            hex::encode(word)
        ));
    }
    let low: [u8; 16] = low.try_into().expect("16 bytes");
    Ok(u128::from_be_bytes(low))
}

/// Sorts out the logs the rules act on: `BRC20Created` from the controller and
/// `Transfer` from any other contract. Every other log, and a log without
/// topics, is `None`.
pub fn classify(log: &ProgLog) -> Result<Option<ClassifiedLog>, String> {
    let Some(topic0) = log.topics.first() else {
        return Ok(None);
    };
    if log.address == CONTROLLER_ADDRESS {
        if *topic0 != BRC20Created::SIGNATURE_HASH {
            return Ok(None);
        }
        let [_, ticker_hash, contract, ..] = log.topics.as_slice() else {
            return Err(format!(
                "BRC20Created log with {} topics, expected 3",
                log.topics.len()
            ));
        };
        return Ok(Some(ClassifiedLog::Created {
            ticker_hash: ticker_hash.to_string(),
            contract_address: address_from_topic(contract),
        }));
    }
    if *topic0 != Transfer::SIGNATURE_HASH {
        return Ok(None);
    }
    let transfer = match log.topics.as_slice() {
        [_, from, to, ..] => amount_from_data(&log.data).map(|amount| TransferLog {
            from: address_from_topic(from),
            to: address_from_topic(to),
            amount,
        }),
        topics => Err(format!(
            "Transfer log with {} topics, expected 3",
            topics.len()
        )),
    };
    Ok(Some(ClassifiedLog::Transfer {
        contract_address: log.address.clone(),
        transfer,
    }))
}

/// Applies a block's events, in log order, to the registered contracts
/// `tickers` (contract address to ticker) and the balances `balances` (by
/// wallet and ticker; a missing pair is zero). A contract registered twice
/// keeps its first ticker. Errors on a malformed `Transfer` from a registered
/// contract and on a balance that would overflow or go negative.
pub fn apply(
    events: &[BalanceEvent],
    tickers: &HashMap<String, String>,
    balances: &HashMap<(String, String), u128>,
) -> Result<BlockChanges, String> {
    let mut tickers = tickers.clone();
    let mut balances = balances.clone();
    let mut changes = BlockChanges::default();
    let mut touched: Vec<(String, String)> = Vec::new();
    let mut touched_set: HashSet<(String, String)> = HashSet::new();
    let mut set = |key: (String, String), amount: u128, balances: &mut HashMap<_, _>| {
        if touched_set.insert(key.clone()) {
            touched.push(key.clone());
        }
        balances.insert(key, amount);
    };
    for event in events {
        match event {
            BalanceEvent::Created {
                ticker,
                ticker_hash,
                contract_address,
            } => {
                if tickers.contains_key(contract_address) {
                    continue;
                }
                tickers.insert(contract_address.clone(), ticker.clone());
                changes.new_tickers.push(TickerRow {
                    ticker: ticker.clone(),
                    ticker_hash: ticker_hash.clone(),
                    contract_address: contract_address.clone(),
                });
            }
            BalanceEvent::Transfer {
                contract_address,
                transfer,
            } => {
                let Some(ticker) = tickers.get(contract_address) else {
                    continue;
                };
                let transfer = transfer
                    .as_ref()
                    .map_err(|error| format!("{contract_address} ({ticker}): {error}"))?;
                if transfer.amount == 0 {
                    continue;
                }
                let balance = |wallet: &str, balances: &HashMap<(String, String), u128>| -> u128 {
                    balances
                        .get(&(wallet.to_string(), ticker.clone()))
                        .copied()
                        .unwrap_or(0)
                };
                if transfer.from != ZERO_ADDRESS {
                    let from = balance(&transfer.from, &balances)
                        .checked_sub(transfer.amount)
                        .ok_or_else(|| {
                            format!(
                                "{ticker}: {} has less than {}",
                                transfer.from, transfer.amount
                            )
                        })?;
                    set((transfer.from.clone(), ticker.clone()), from, &mut balances);
                }
                if transfer.from == ZERO_ADDRESS || transfer.to != ZERO_ADDRESS {
                    let to = balance(&transfer.to, &balances)
                        .checked_add(transfer.amount)
                        .ok_or_else(|| format!("{ticker}: {} overflows", transfer.to))?;
                    set((transfer.to.clone(), ticker.clone()), to, &mut balances);
                }
            }
        }
    }
    changes.balances = touched
        .into_iter()
        .map(|key| {
            let amount = balances[&key];
            BalanceRow {
                wallet: key.0,
                ticker: key.1,
                amount,
            }
        })
        .collect();
    Ok(changes)
}

/// The token name a contract's `name()` returns, read from the engine's
/// latest state. A BRC20 token's name is fixed at creation.
async fn token_name(prog: &Brc20Prog, contract_address: &str) -> Result<String, Box<dyn Error>> {
    let call = EthCall::new(
        Some(Address::ZERO.into()),
        Some(contract_address.try_into()?),
        RawBytes::new(format!("0x{}", hex::encode(nameCall {}.abi_encode()))),
    );
    let output = prog.eth_call(call, None).await?;
    let output = hex::decode(output.trim_start_matches("0x"))?;
    Ok(nameCall::abi_decode_returns(&output)?)
}

/// The events of the engine's blocks `from..=to`, per block in height order,
/// each block's in log order. Blocks without events are left out. The span
/// must fit the engine's `GET_LOGS_MAX_BLOCKS`.
pub async fn balance_events(
    prog: &Brc20Prog,
    from: i32,
    to: i32,
) -> Result<Vec<(i32, Vec<BalanceEvent>)>, Box<dyn Error>> {
    let mut logs = prog
        .eth_get_logs(GetLogsFilter {
            from_block: Some(format!("0x{from:x}")),
            to_block: Some(format!("0x{to:x}")),
            address: None,
            topics: None,
        })
        .await?;
    logs.sort_by_key(|log| {
        let block: u64 = log.block_number.into();
        let transaction: u64 = log.transaction_index.into();
        let index: u64 = log.log_index.into();
        (block, transaction, index)
    });
    let mut blocks: Vec<(i32, Vec<BalanceEvent>)> = Vec::new();
    for log in &logs {
        let log = ProgLog::from_log(log);
        let event = match classify(&log)? {
            None => continue,
            Some(ClassifiedLog::Created {
                ticker_hash,
                contract_address,
            }) => BalanceEvent::Created {
                ticker: token_name(prog, &contract_address).await?,
                ticker_hash,
                contract_address,
            },
            Some(ClassifiedLog::Transfer {
                contract_address,
                transfer,
            }) => BalanceEvent::Transfer {
                contract_address,
                transfer,
            },
        };
        match blocks.last_mut() {
            Some((height, events)) if *height == log.block_height => events.push(event),
            _ => blocks.push((log.block_height, vec![event])),
        }
    }
    Ok(blocks)
}

/// Writes block `block_height`'s `events`: reads the registered contracts and
/// balances they name, applies the rules and stores the new contracts and
/// balances.
pub async fn write_block(
    conn: &mut PgConnection,
    block_height: i32,
    events: &[BalanceEvent],
) -> Result<(), Box<dyn Error>> {
    if events.is_empty() {
        return Ok(());
    }
    let contracts: Vec<&str> = events
        .iter()
        .map(|event| match event {
            BalanceEvent::Created {
                contract_address, ..
            }
            | BalanceEvent::Transfer {
                contract_address, ..
            } => contract_address.as_str(),
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let tickers: HashMap<String, String> = sqlx::query(
        "SELECT contract_address, ticker FROM brc20_prog_tickers WHERE contract_address = ANY($1)",
    )
    .bind(&contracts)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|row| (row.get(0), row.get(1)))
    .collect();

    // Every (wallet, ticker) a transfer of a registered contract can touch.
    let mut known = tickers.clone();
    let mut wallets = Vec::new();
    let mut wallet_tickers = Vec::new();
    for event in events {
        match event {
            BalanceEvent::Created {
                ticker,
                contract_address,
                ..
            } => {
                known
                    .entry(contract_address.clone())
                    .or_insert_with(|| ticker.clone());
            }
            BalanceEvent::Transfer {
                contract_address,
                transfer: Ok(transfer),
            } => {
                if let Some(ticker) = known.get(contract_address) {
                    for wallet in [&transfer.from, &transfer.to] {
                        wallets.push(wallet.clone());
                        wallet_tickers.push(ticker.clone());
                    }
                }
            }
            BalanceEvent::Transfer { .. } => {}
        }
    }
    let mut balances = HashMap::new();
    if !wallets.is_empty() {
        for row in sqlx::query(
            "SELECT b.wallet, b.ticker, b.amount::text
             FROM brc20_prog_current_balances b
             JOIN UNNEST($1::text[], $2::text[]) AS k(wallet, ticker)
               ON b.wallet = k.wallet AND b.ticker = k.ticker",
        )
        .bind(&wallets)
        .bind(&wallet_tickers)
        .fetch_all(&mut *conn)
        .await?
        {
            let amount: String = row.get(2);
            balances.insert((row.get(0), row.get(1)), amount.parse::<u128>()?);
        }
    }

    let changes = apply(events, &tickers, &balances)
        .map_err(|error| format!("BRC2.0 balances at block {block_height}: {error}"))?;

    if !changes.new_tickers.is_empty() {
        let (mut names, mut hashes, mut addresses) = (Vec::new(), Vec::new(), Vec::new());
        for row in &changes.new_tickers {
            names.push(row.ticker.as_str());
            hashes.push(row.ticker_hash.as_str());
            addresses.push(row.contract_address.as_str());
        }
        sqlx::query(
            "INSERT INTO brc20_prog_tickers (ticker, ticker_hash, contract_address, block_height)
             SELECT t.ticker, t.ticker_hash, t.contract_address, $4
             FROM UNNEST($1::text[], $2::text[], $3::text[]) AS t(ticker, ticker_hash, contract_address)
             ON CONFLICT (contract_address) DO NOTHING",
        )
        .bind(&names)
        .bind(&hashes)
        .bind(&addresses)
        .bind(block_height)
        .execute(&mut *conn)
        .await?;
    }

    if !changes.balances.is_empty() {
        let (mut wallets, mut tickers, mut amounts) = (Vec::new(), Vec::new(), Vec::new());
        for row in &changes.balances {
            wallets.push(row.wallet.as_str());
            tickers.push(row.ticker.as_str());
            amounts.push(row.amount.to_string());
        }
        sqlx::query(
            "INSERT INTO brc20_prog_current_balances (wallet, ticker, amount, block_height)
             SELECT b.wallet, b.ticker, b.amount::numeric, $4
             FROM UNNEST($1::text[], $2::text[], $3::text[]) AS b(wallet, ticker, amount)
             ON CONFLICT (wallet, ticker)
             DO UPDATE SET amount = excluded.amount, block_height = excluded.block_height",
        )
        .bind(&wallets)
        .bind(&tickers)
        .bind(&amounts)
        .bind(block_height)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "INSERT INTO brc20_prog_historical_balances (wallet, ticker, amount, block_height)
             SELECT b.wallet, b.ticker, b.amount::numeric, $4
             FROM UNNEST($1::text[], $2::text[], $3::text[]) AS b(wallet, ticker, amount)",
        )
        .bind(&wallets)
        .bind(&tickers)
        .bind(&amounts)
        .bind(block_height)
        .execute(&mut *conn)
        .await?;
        super::ticker_holders::recount_prog(conn, tickers).await?;
    }
    Ok(())
}

/// Deletes the rows above `block_height`, restores each deleted current
/// balance from its latest remaining historical row and recounts the BRC2.0
/// holders of the tickers involved. A pair without a historical row has no
/// current balance.
pub async fn reorg(conn: &mut PgConnection, block_height: i32) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM brc20_prog_tickers WHERE block_height > $1")
        .bind(block_height)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM brc20_prog_historical_balances WHERE block_height > $1")
        .bind(block_height)
        .execute(&mut *conn)
        .await?;
    let deleted = sqlx::query(
        "DELETE FROM brc20_prog_current_balances WHERE block_height > $1 RETURNING wallet, ticker",
    )
    .bind(block_height)
    .fetch_all(&mut *conn)
    .await?;
    if deleted.is_empty() {
        return Ok(());
    }
    let (wallets, tickers): (Vec<String>, Vec<String>) =
        deleted.iter().map(|row| (row.get(0), row.get(1))).unzip();
    sqlx::query(
        "INSERT INTO brc20_prog_current_balances (wallet, ticker, amount, block_height)
         SELECT DISTINCT ON (h.wallet, h.ticker) h.wallet, h.ticker, h.amount, h.block_height
         FROM brc20_prog_historical_balances h
         JOIN UNNEST($1::text[], $2::text[]) AS d(wallet, ticker)
           ON h.wallet = d.wallet AND h.ticker = d.ticker
         ORDER BY h.wallet, h.ticker, h.block_height DESC",
    )
    .bind(&wallets)
    .bind(&tickers)
    .execute(&mut *conn)
    .await?;
    super::ticker_holders::recount_prog(conn, tickers.iter().map(String::as_str)).await?;
    Ok(())
}

/// Creates the tables when any is missing, on a database indexed before they
/// existed, together with the fill table that marks them for filling. Returns
/// whether it created them.
pub async fn create_if_missing(pool: &Pool<Postgres>) -> Result<bool, sqlx::Error> {
    let missing: bool = sqlx::query(
        "SELECT to_regclass('brc20_prog_tickers') IS NULL
             OR to_regclass('brc20_prog_current_balances') IS NULL
             OR to_regclass('brc20_prog_historical_balances') IS NULL",
    )
    .fetch_one(pool)
    .await?
    .get(0);
    if !missing {
        return Ok(false);
    }
    let mut tx = pool.begin().await?;
    for table in TABLES.iter().chain([&FILL_TABLE]) {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS {table}")))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::raw_sql(TABLES_SQL).execute(&mut *tx).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {FILL_TABLE} (filled_height int4);
         INSERT INTO {FILL_TABLE} (filled_height) VALUES (NULL);"
    )))
    .execute(&mut *tx)
    .await?;
    // The new tables are empty and wait for their fill, which counts the
    // BRC2.0 holders again as it writes, so an existing holder count table
    // zeroes its BRC2.0 counts in the same transaction.
    let holders_table: bool =
        sqlx::query("SELECT to_regclass('public.brc20_ticker_holders') IS NOT NULL")
            .fetch_one(&mut *tx)
            .await?
            .get(0);
    if holders_table {
        sqlx::query!("UPDATE brc20_ticker_holders SET prog_holders = 0 WHERE prog_holders <> 0")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// The fill's progress: `None` when no fill is pending, else the highest
/// height filled so far (`None` before the first batch).
pub async fn fill_progress(pool: &Pool<Postgres>) -> Result<Option<Option<i32>>, sqlx::Error> {
    let pending: bool = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT to_regclass('{FILL_TABLE}') IS NOT NULL"
    )))
    .fetch_one(pool)
    .await?
    .get(0);
    if !pending {
        return Ok(None);
    }
    let filled: Option<i32> = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT max(filled_height) FROM {FILL_TABLE}"
    )))
    .fetch_one(pool)
    .await?
    .get(0);
    Ok(Some(filled))
}

/// Fills the tables from the engine's logs of blocks `first_height..=to_height`
/// when a fill is pending, and returns whether one was. Each transaction
/// writes `fill_blocks` blocks and records the last of them in the fill
/// table, so an interrupted fill resumes after the last committed block. Log
/// reads span at most `max_log_blocks` blocks. The finished fill drops the
/// fill table. The process metrics show the fill running, with its last
/// committed block and `to_height`.
pub async fn fill(
    pool: &Pool<Postgres>,
    prog: &Brc20Prog,
    first_height: i32,
    to_height: i32,
    max_log_blocks: u64,
    fill_blocks: i32,
) -> Result<bool, Box<dyn Error>> {
    let Some(filled) = fill_progress(pool).await? else {
        return Ok(false);
    };
    let metrics = opi_core::metrics::global().fill("brc20", "prog_balances");
    metrics.target(i64::from(to_height));
    let mut next = filled.map_or(first_height, |filled| (filled + 1).max(first_height));
    metrics.progress(i64::from(next) - 1);
    let log_span = max_log_blocks.clamp(1, i32::MAX as u64) as i32;
    let fill_blocks = fill_blocks.max(1);
    tracing::info!("Filling BRC2.0 balances from block {next} to {to_height}");
    while next <= to_height {
        let end = next.saturating_add(fill_blocks - 1).min(to_height);
        let mut tx = pool.begin().await?;
        let mut from = next;
        while from <= end {
            let to = from.saturating_add(log_span - 1).min(end);
            let blocks = balance_events(prog, from, to).await?;
            for (height, events) in blocks {
                write_block(&mut tx, height, &events).await?;
            }
            from = to + 1;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {FILL_TABLE} SET filled_height = $1"
        )))
        .bind(end)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        metrics.progress(i64::from(end));
        if end % 10_000 < fill_blocks {
            tracing::info!("BRC2.0 balances: filled to block {end}");
        }
        next = end + 1;
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {FILL_TABLE}")))
        .execute(pool)
        .await?;
    tracing::info!("Filled BRC2.0 balances");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{U256, keccak256};

    use super::*;

    const TOKEN: &str = "0x1111111111111111111111111111111111111111";
    const OTHER_TOKEN: &str = "0x2222222222222222222222222222222222222222";
    const ALICE: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// An address as an indexed topic: 12 zero bytes, then the address.
    fn address_topic(address: &str) -> B256 {
        let address: Address = address.parse().unwrap();
        B256::left_padding_from(address.as_slice())
    }

    fn word(amount: U256) -> Vec<u8> {
        amount.to_be_bytes::<32>().to_vec()
    }

    fn created_log(ticker: &str, contract: &str) -> ProgLog {
        ProgLog {
            block_height: 5,
            address: CONTROLLER_ADDRESS.into(),
            topics: vec![
                BRC20Created::SIGNATURE_HASH,
                keccak256(ticker.as_bytes()),
                address_topic(contract),
            ],
            data: Vec::new(),
        }
    }

    fn transfer_log(contract: &str, from: &str, to: &str, amount: U256) -> ProgLog {
        ProgLog {
            block_height: 5,
            address: contract.into(),
            topics: vec![
                Transfer::SIGNATURE_HASH,
                address_topic(from),
                address_topic(to),
            ],
            data: word(amount),
        }
    }

    fn created(ticker: &str, contract: &str) -> BalanceEvent {
        BalanceEvent::Created {
            ticker: ticker.into(),
            ticker_hash: keccak256(ticker.as_bytes()).to_string(),
            contract_address: contract.into(),
        }
    }

    fn transfer(contract: &str, from: &str, to: &str, amount: u128) -> BalanceEvent {
        BalanceEvent::Transfer {
            contract_address: contract.into(),
            transfer: Ok(TransferLog {
                from: from.into(),
                to: to.into(),
                amount,
            }),
        }
    }

    fn balance(wallet: &str, ticker: &str, amount: u128) -> BalanceRow {
        BalanceRow {
            wallet: wallet.into(),
            ticker: ticker.into(),
            amount,
        }
    }

    fn registered() -> HashMap<String, String> {
        HashMap::from([(TOKEN.to_string(), "ordi".to_string())])
    }

    fn held(rows: &[(&str, &str, u128)]) -> HashMap<(String, String), u128> {
        rows.iter()
            .map(|(wallet, ticker, amount)| ((wallet.to_string(), ticker.to_string()), *amount))
            .collect()
    }

    #[test]
    fn topics_are_the_event_signature_hashes() {
        assert_eq!(
            BRC20Created::SIGNATURE_HASH,
            keccak256("BRC20Created(bytes,address)")
        );
        assert_eq!(
            Transfer::SIGNATURE_HASH.to_string(),
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        );
        assert_eq!(hex::encode(nameCall {}.abi_encode()), "06fdde03");
    }

    #[test]
    fn controller_created_log_names_the_ticker_hash_and_contract() {
        let log = created_log("ordi", "0xAbCdEf0123456789aBcDeF0123456789AbCdEf01");
        assert_eq!(
            classify(&log),
            Ok(Some(ClassifiedLog::Created {
                ticker_hash: format!("0x{}", hex::encode(keccak256(b"ordi"))),
                contract_address: "0xabcdef0123456789abcdef0123456789abcdef01".into(),
            }))
        );
    }

    #[test]
    fn transfer_log_reads_its_parties_and_value() {
        assert_eq!(
            classify(&transfer_log(TOKEN, ALICE, BOB, U256::from(u128::MAX))),
            Ok(Some(ClassifiedLog::Transfer {
                contract_address: TOKEN.into(),
                transfer: Ok(TransferLog {
                    from: ALICE.into(),
                    to: BOB.into(),
                    amount: u128::MAX,
                }),
            }))
        );
    }

    #[test]
    fn a_transfer_value_above_128_bits_is_an_error() {
        for amount in [
            (U256::from(7u8) << 200) + U256::from(1_000u32),
            U256::from(u128::MAX) + U256::from(1u8),
        ] {
            let Ok(Some(ClassifiedLog::Transfer { transfer, .. })) =
                classify(&transfer_log(TOKEN, ALICE, BOB, amount))
            else {
                panic!("a Transfer log");
            };
            assert!(transfer.is_err(), "{amount}");
        }
    }

    #[test]
    fn other_logs_are_skipped() {
        // A Transfer from the controller (its own event has another
        // signature, and the rules read only BRC20Created there).
        let mut from_controller = transfer_log(TOKEN, ALICE, BOB, U256::from(1u8));
        from_controller.address = CONTROLLER_ADDRESS.into();
        assert_eq!(classify(&from_controller), Ok(None));
        // BRC20Created from another contract.
        let mut elsewhere = created_log("ordi", TOKEN);
        elsewhere.address = TOKEN.into();
        assert_eq!(classify(&elsewhere), Ok(None));
        // Another event, and an anonymous log.
        let mut approval = transfer_log(TOKEN, ALICE, BOB, U256::from(1u8));
        approval.topics[0] = keccak256("Approval(address,address,uint256)");
        assert_eq!(classify(&approval), Ok(None));
        let mut anonymous = approval;
        anonymous.topics.clear();
        assert_eq!(classify(&anonymous), Ok(None));
    }

    #[test]
    fn malformed_transfer_keeps_its_error_for_the_rules() {
        // ERC-721 Transfer: the same signature, the id indexed, no data.
        let mut nft = transfer_log(TOKEN, ALICE, BOB, U256::from(1u8));
        nft.topics.push(B256::with_last_byte(9));
        nft.data.clear();
        let Ok(Some(ClassifiedLog::Transfer { transfer, .. })) = classify(&nft) else {
            panic!("a Transfer log");
        };
        assert!(transfer.is_err());

        let mut short = transfer_log(TOKEN, ALICE, BOB, U256::from(1u8));
        short.topics.truncate(2);
        let Ok(Some(ClassifiedLog::Transfer { transfer, .. })) = classify(&short) else {
            panic!("a Transfer log");
        };
        assert!(transfer.is_err());

        let mut created = created_log("ordi", TOKEN);
        created.topics.truncate(2);
        assert!(classify(&created).is_err());
    }

    #[test]
    fn mint_burn_and_transfer_move_balances() {
        let events = [
            transfer(TOKEN, ZERO_ADDRESS, ALICE, 1_000),
            transfer(TOKEN, ALICE, BOB, 300),
            transfer(TOKEN, BOB, ZERO_ADDRESS, 100),
        ];
        assert_eq!(
            apply(&events, &registered(), &held(&[(BOB, "ordi", 5)])),
            Ok(BlockChanges {
                new_tickers: Vec::new(),
                balances: vec![balance(ALICE, "ordi", 700), balance(BOB, "ordi", 205)],
            })
        );
    }

    #[test]
    fn a_contract_created_in_the_block_counts_from_its_log_on() {
        let events = [
            transfer(OTHER_TOKEN, ZERO_ADDRESS, ALICE, 9),
            created("pepe", OTHER_TOKEN),
            transfer(OTHER_TOKEN, ZERO_ADDRESS, ALICE, 50),
        ];
        assert_eq!(
            apply(&events, &registered(), &HashMap::new()),
            Ok(BlockChanges {
                new_tickers: vec![TickerRow {
                    ticker: "pepe".into(),
                    ticker_hash: keccak256(b"pepe").to_string(),
                    contract_address: OTHER_TOKEN.into(),
                }],
                balances: vec![balance(ALICE, "pepe", 50)],
            })
        );
    }

    #[test]
    fn a_contract_registered_again_keeps_its_first_ticker() {
        let events = [
            created("dupe", TOKEN),
            transfer(TOKEN, ZERO_ADDRESS, ALICE, 1),
        ];
        assert_eq!(
            apply(&events, &registered(), &HashMap::new()),
            Ok(BlockChanges {
                new_tickers: Vec::new(),
                balances: vec![balance(ALICE, "ordi", 1)],
            })
        );
    }

    #[test]
    fn zero_values_and_unregistered_contracts_change_nothing() {
        let malformed = BalanceEvent::Transfer {
            contract_address: OTHER_TOKEN.into(),
            transfer: Err("no data".into()),
        };
        let events = [
            transfer(TOKEN, ALICE, BOB, 0),
            transfer(OTHER_TOKEN, ZERO_ADDRESS, ALICE, 5),
            malformed,
        ];
        assert_eq!(
            apply(&events, &registered(), &HashMap::new()),
            Ok(BlockChanges::default())
        );
    }

    #[test]
    fn a_self_transfer_leaves_the_balance() {
        let events = [transfer(TOKEN, ALICE, ALICE, 40)];
        assert_eq!(
            apply(&events, &registered(), &held(&[(ALICE, "ordi", 100)])),
            Ok(BlockChanges {
                new_tickers: Vec::new(),
                balances: vec![balance(ALICE, "ordi", 100)],
            })
        );
    }

    #[test]
    fn impossible_balances_and_malformed_registered_transfers_are_errors() {
        let overdraw = [transfer(TOKEN, ALICE, BOB, 2)];
        assert!(apply(&overdraw, &registered(), &held(&[(ALICE, "ordi", 1)])).is_err());
        let overburn = [transfer(TOKEN, ALICE, ZERO_ADDRESS, 2)];
        assert!(apply(&overburn, &registered(), &HashMap::new()).is_err());
        let overflow = [transfer(TOKEN, ZERO_ADDRESS, ALICE, 1)];
        assert!(
            apply(
                &overflow,
                &registered(),
                &held(&[(ALICE, "ordi", u128::MAX)])
            )
            .is_err()
        );
        let malformed = [BalanceEvent::Transfer {
            contract_address: TOKEN.into(),
            transfer: Err("no data".into()),
        }];
        assert!(apply(&malformed, &registered(), &HashMap::new()).is_err());
    }

    #[test]
    fn db_init_creates_the_tables() {
        let init = crate::database::brc20_database::SqlFiles::get_sql_file("db_init.sql").unwrap();
        assert!(init.contains(TABLES_SQL));
        let reset =
            crate::database::brc20_database::SqlFiles::get_sql_file("db_reset.sql").unwrap();
        for table in TABLES.iter().chain([&FILL_TABLE]) {
            assert!(
                reset.contains(&format!("drop table if exists {table};")),
                "{table}"
            );
        }
    }
}

/// Postgres-backed tests, each on a throwaway database. They skip unless
/// `OPI_TEST_PG=1`.
#[cfg(test)]
mod db_tests {
    use alloy_primitives::{Bytes, U256};
    use bitcoin::Network;
    use brc20_prog::types::{AddressED, B256ED};
    use sqlx::{Connection, postgres::PgConnectOptions};

    use super::*;
    use crate::{
        config::Brc20IndexerConfig,
        database::{
            Brc20Database, evm_addresses::evm_address, get_brc20_database, replace_brc20_database,
        },
        indexer::{Brc20Indexer, brc20_prog_client::test_engine::TestEngine},
    };

    sol! {
        function transfer(address to, uint256 value) external returns (bool);
        function balanceOf(bytes ticker, address account) external view returns (uint256);
    }

    const ALICE: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const TOKEN: &str = "0x1111111111111111111111111111111111111111";
    const OTHER_TOKEN: &str = "0x2222222222222222222222222222222222222222";

    /// Runs `body` against a throwaway database, dropped even when `body`
    /// panics. `body` runs on a local task, since an indexer's start is not
    /// `Send`.
    async fn with_database<F, Fut>(tag: &str, body: F)
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = ()> + 'static,
    {
        if std::env::var("OPI_TEST_PG").as_deref() != Ok("1") {
            eprintln!("OPI_TEST_PG is not 1; skipping");
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("opi_brc20_prog_{tag}_{}_{nanos}", std::process::id());
        let admin = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(5432)
            .username("alper")
            .database("postgres");
        let mut root = sqlx::PgConnection::connect_with(&admin).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut root)
            .await
            .unwrap();
        let body = body(name.clone());
        let result = tokio::task::LocalSet::new()
            .run_until(async { tokio::task::spawn_local(body).await })
            .await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {name} WITH (FORCE)"
        )))
        .execute(&mut root)
        .await
        .unwrap();
        if let Err(error) = result {
            std::panic::resume_unwind(error.into_panic());
        }
    }

    async fn database(name: &str, light_client_mode: bool) -> Brc20Database {
        let config = Brc20IndexerConfig {
            db_host: "127.0.0.1".into(),
            db_port: "5432".into(),
            db_user: "alper".into(),
            db_password: String::new(),
            db_database: name.into(),
            db_ssl: false,
            network_type: Network::Signet,
            light_client_mode,
            ..Default::default()
        };
        let mut database = Brc20Database::new(&config);
        database.init().await.unwrap();
        database
    }

    /// Every row of the three tables, as text, in a fixed order.
    async fn snapshot(database: &Brc20Database) -> Vec<String> {
        let mut rows = Vec::new();
        for query in [
            "SELECT 'ticker ' || contract_address || ' ' || ticker || ' ' || ticker_hash || ' ' || block_height
             FROM brc20_prog_tickers ORDER BY 1",
            "SELECT 'current ' || wallet || ' ' || ticker || ' ' || amount || ' ' || block_height
             FROM brc20_prog_current_balances ORDER BY 1",
            "SELECT 'historical ' || block_height || ' ' || wallet || ' ' || ticker || ' ' || amount
             FROM brc20_prog_historical_balances ORDER BY 1",
        ] {
            for row in sqlx::query(query)
                .fetch_all(&database.client)
                .await
                .unwrap()
            {
                rows.push(row.get::<String, _>(0));
            }
        }
        rows
    }

    fn created(ticker: &str, contract: &str) -> BalanceEvent {
        BalanceEvent::Created {
            ticker: ticker.into(),
            ticker_hash: format!("hash-{ticker}"),
            contract_address: contract.into(),
        }
    }

    fn transfer(contract: &str, from: &str, to: &str, amount: u128) -> BalanceEvent {
        BalanceEvent::Transfer {
            contract_address: contract.into(),
            transfer: Ok(TransferLog {
                from: from.into(),
                to: to.into(),
                amount,
            }),
        }
    }

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[tokio::test]
    async fn blocks_write_balances_and_a_reorg_restores_them() {
        for light in [false, true] {
            with_database(
                if light { "blocks_light" } else { "blocks" },
                move |name| async move {
                    let mut database = database(&name, light).await;
                    let blocks = [
                        vec![
                            created("ordi", TOKEN),
                            transfer(TOKEN, ZERO_ADDRESS, ALICE, 1_000),
                            transfer(TOKEN, ALICE, BOB, 100),
                        ],
                        vec![],
                        vec![
                            transfer(TOKEN, BOB, ALICE, 40),
                            created("pepe", OTHER_TOKEN),
                            transfer(OTHER_TOKEN, ZERO_ADDRESS, BOB, 7),
                        ],
                        vec![transfer(TOKEN, ALICE, ZERO_ADDRESS, 940)],
                    ];
                    for (offset, events) in blocks.iter().enumerate() {
                        let height = 10 + offset as i32;
                        database
                            .set_block_hashes(height, &format!("h{height}"), "", None, events)
                            .await
                            .unwrap();
                    }
                    assert_eq!(
                        snapshot(&database).await,
                        lines(&[
                            "ticker 0x1111111111111111111111111111111111111111 ordi hash-ordi 10",
                            "ticker 0x2222222222222222222222222222222222222222 pepe hash-pepe 12",
                            "current 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ordi 0 13",
                            "current 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ordi 60 12",
                            "current 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb pepe 7 12",
                            "historical 10 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ordi 900",
                            "historical 10 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ordi 100",
                            "historical 12 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ordi 940",
                            "historical 12 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ordi 60",
                            "historical 12 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb pepe 7",
                            "historical 13 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ordi 0",
                        ])
                    );

                    database.reorg(11).await.unwrap();
                    assert_eq!(
                        snapshot(&database).await,
                        lines(&[
                            "ticker 0x1111111111111111111111111111111111111111 ordi hash-ordi 10",
                            "current 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ordi 900 10",
                            "current 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ordi 100 10",
                            "historical 10 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ordi 900",
                            "historical 10 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ordi 100",
                        ])
                    );

                    // The re-indexed block reads the restored balances.
                    database
                        .set_block_hashes(12, "h12b", "", None, &[transfer(TOKEN, ALICE, BOB, 1)])
                        .await
                        .unwrap();
                    assert!(
                        snapshot(&database).await.contains(
                            &"current 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ordi 101 12"
                                .to_string()
                        )
                    );

                    database.reorg(9).await.unwrap();
                    assert_eq!(snapshot(&database).await, Vec::<String>::new());
                },
            )
            .await;
        }
    }

    #[tokio::test]
    async fn an_impossible_balance_fails_the_block_and_writes_nothing() {
        with_database("impossible", |name| async move {
            let mut database = database(&name, false).await;
            let events = [created("ordi", TOKEN), transfer(TOKEN, ALICE, BOB, 1)];
            assert!(
                database
                    .set_block_hashes(10, "h10", "", None, &events)
                    .await
                    .is_err()
            );
            assert_eq!(snapshot(&database).await, Vec::<String>::new());
            let hashes: i64 = sqlx::query("SELECT count(*) FROM brc20_block_hashes")
                .fetch_one(&database.client)
                .await
                .unwrap()
                .get(0);
            assert_eq!(hashes, 0);
        })
        .await;
    }

    #[tokio::test]
    async fn a_database_without_the_tables_gets_them_marked_for_filling() {
        with_database("missing", |name| async move {
            let fresh = database(&name, false).await;
            assert_eq!(fill_progress(&fresh.client).await.unwrap(), None);

            sqlx::raw_sql("DROP TABLE brc20_prog_current_balances")
                .execute(&fresh.client)
                .await
                .unwrap();
            let restarted = database(&name, false).await;
            assert_eq!(fill_progress(&restarted.client).await.unwrap(), Some(None));
            assert_eq!(snapshot(&restarted).await, Vec::<String>::new());

            // A second start keeps the pending fill.
            let again = database(&name, false).await;
            assert_eq!(fill_progress(&again.client).await.unwrap(), Some(None));
        })
        .await;
    }

    #[tokio::test]
    async fn reset_drops_the_tables_and_a_fresh_init_recreates_them_empty() {
        with_database("reset", |name| async move {
            let mut first = database(&name, false).await;
            first
                .set_block_hashes(
                    10,
                    "h10",
                    "",
                    None,
                    &[
                        created("ordi", TOKEN),
                        transfer(TOKEN, ZERO_ADDRESS, ALICE, 5),
                    ],
                )
                .await
                .unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "CREATE TABLE {FILL_TABLE} (filled_height int4)"
            )))
            .execute(&first.client)
            .await
            .unwrap();
            first.reset().await.unwrap();
            let fresh = database(&name, false).await;
            assert_eq!(snapshot(&fresh).await, Vec::<String>::new());
            assert_eq!(fill_progress(&fresh.client).await.unwrap(), None);
        })
        .await;
    }

    const ALICE_PKSCRIPT: &str = "0014751e76e8199196d454941c45d1b3a323f1433bd6";
    const BOB_PKSCRIPT: &str =
        "5120a60869f0dbcf1dc659c9cecbaf8050135ea9e8cdc487053f1dc6880949dc684c";
    const CAROL_PKSCRIPT: &str = "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac";

    fn block_hash(height: u8) -> B256ED {
        [height; 32].into()
    }

    /// Three engine blocks: block 1 creates `ordi` with deposits to Alice and
    /// Bob and a withdrawal from Alice; block 2 holds Alice's transfer to Bob
    /// and her transfer to herself through the token contract; block 3
    /// creates `test` with a deposit to Carol.
    async fn mine_three_blocks(prog: &Brc20Prog) {
        prog.brc20_initialise([0u8; 32].into(), 0, 0).await.unwrap();
        let time = 1_700_000_000;
        prog.brc20_deposit(
            ALICE_PKSCRIPT.into(),
            "ordi".into(),
            1_000u128.into(),
            time,
            block_hash(1),
            0,
            "d1i0".into(),
        )
        .await
        .unwrap();
        prog.brc20_deposit(
            BOB_PKSCRIPT.into(),
            "ordi".into(),
            5u128.into(),
            time,
            block_hash(1),
            1,
            "d2i0".into(),
        )
        .await
        .unwrap();
        prog.brc20_withdraw(
            ALICE_PKSCRIPT.into(),
            "ordi".into(),
            100u128.into(),
            time,
            block_hash(1),
            2,
            "w1i0".into(),
        )
        .await
        .unwrap();
        prog.brc20_finalise_block(time, block_hash(1), 3)
            .await
            .unwrap();

        let Some(BalanceEvent::Created {
            contract_address, ..
        }) = balance_events(prog, 1, 1).await.unwrap()[0]
            .1
            .first()
            .cloned()
        else {
            panic!("block 1 creates ordi first");
        };
        let token: AddressED = contract_address.as_str().try_into().unwrap();
        let bob: Address = evm_address(&hex::decode(BOB_PKSCRIPT).unwrap())
            .parse()
            .unwrap();
        let alice: Address = evm_address(&hex::decode(ALICE_PKSCRIPT).unwrap())
            .parse()
            .unwrap();
        for (tx_idx, (to, value)) in [(bob, 50u64), (alice, 10)].into_iter().enumerate() {
            let data = transferCall {
                to,
                value: U256::from(value),
            }
            .abi_encode();
            let receipt = prog
                .brc20_call(
                    ALICE_PKSCRIPT.into(),
                    Some(token),
                    None,
                    Some(RawBytes::new(format!("0x{}", hex::encode(data)))),
                    None,
                    time + 1,
                    block_hash(2),
                    tx_idx as u64,
                    format!("c{tx_idx}i0"),
                    1_000,
                    [0u8; 32].into(),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(receipt.logs.len(), 1, "the transfer emits one log");
        }
        prog.brc20_finalise_block(time + 1, block_hash(2), 2)
            .await
            .unwrap();

        prog.brc20_deposit(
            CAROL_PKSCRIPT.into(),
            "test".into(),
            7u128.into(),
            time + 2,
            block_hash(3),
            0,
            "d3i0".into(),
        )
        .await
        .unwrap();
        prog.brc20_finalise_block(time + 2, block_hash(3), 1)
            .await
            .unwrap();
    }

    /// The controller's `balanceOf(ticker, wallet)` at the engine's tip.
    async fn balance_of(prog: &Brc20Prog, ticker: &str, wallet: &str) -> u128 {
        let data = balanceOfCall {
            ticker: Bytes::from(ticker.as_bytes().to_vec()),
            account: wallet.parse().unwrap(),
        }
        .abi_encode();
        let call = EthCall::new(
            Some(Address::ZERO.into()),
            Some(CONTROLLER_ADDRESS.try_into().unwrap()),
            RawBytes::new(format!("0x{}", hex::encode(data))),
        );
        let output = prog.eth_call(call, None).await.unwrap();
        let value = balanceOfCall::abi_decode_returns(
            &hex::decode(output.trim_start_matches("0x")).unwrap(),
        )
        .unwrap();
        value.to::<u128>()
    }

    async fn drop_tables(database: &Brc20Database) {
        for table in TABLES {
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {table}")))
                .execute(&database.client)
                .await
                .unwrap();
        }
    }

    /// The engine's logs give the balances its controller reports, through
    /// the per-block writes and through the fill at start, which resumes
    /// after an interruption.
    #[tokio::test]
    async fn engine_logs_index_and_fill_the_balances_balance_of_reports() {
        let engine = TestEngine::open();
        let prog = engine.prog.clone();
        with_database("engine", move |name| async move {
            mine_three_blocks(&prog).await;
            let alice = evm_address(&hex::decode(ALICE_PKSCRIPT).unwrap());
            let bob = evm_address(&hex::decode(BOB_PKSCRIPT).unwrap());
            let carol = evm_address(&hex::decode(CAROL_PKSCRIPT).unwrap());

            let mut per_block = database(&name, false).await;
            for height in 1..=3 {
                let events: Vec<BalanceEvent> = balance_events(&prog, height, height)
                    .await
                    .unwrap()
                    .into_iter()
                    .flat_map(|(_, events)| events)
                    .collect();
                per_block
                    .set_block_hashes(height, &format!("h{height}"), "", None, &events)
                    .await
                    .unwrap();
            }
            let indexed = snapshot(&per_block).await;

            let current: Vec<(String, String, String)> = sqlx::query(
                "SELECT wallet, ticker, amount::text FROM brc20_prog_current_balances ORDER BY 1, 2",
            )
            .fetch_all(&per_block.client)
            .await
            .unwrap()
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect();
            let mut expected = vec![
                (alice.clone(), "ordi".to_string(), "850".to_string()),
                (bob.clone(), "ordi".to_string(), "55".to_string()),
                (carol.clone(), "test".to_string(), "7".to_string()),
            ];
            expected.sort();
            assert_eq!(current, expected);
            for (wallet, ticker, amount) in &current {
                assert_eq!(
                    balance_of(&prog, ticker, wallet).await.to_string(),
                    *amount,
                    "{wallet} {ticker}"
                );
            }
            let tickers: Vec<String> =
                sqlx::query_scalar("SELECT ticker_hash FROM brc20_prog_tickers ORDER BY block_height")
                    .fetch_all(&per_block.client)
                    .await
                    .unwrap();
            assert_eq!(
                tickers,
                vec![
                    alloy_primitives::keccak256(b"ordi").to_string(),
                    alloy_primitives::keccak256(b"test").to_string(),
                ]
            );

            // A database indexed before the tables existed fills them at
            // start, reading logs two blocks at a time.
            drop_tables(&per_block).await;
            let restarted = database(&name, false).await;
            assert!(fill(&restarted.client, &prog, 0, 3, 2, 2).await.unwrap());
            assert_eq!(snapshot(&restarted).await, indexed);
            assert_eq!(fill_progress(&restarted.client).await.unwrap(), None);
            assert!(!fill(&restarted.client, &prog, 0, 3, 2, 2).await.unwrap());

            // A fill interrupted after block 1 resumes at block 2.
            drop_tables(&restarted).await;
            let interrupted = database(&name, false).await;
            let mut tx = interrupted.client.begin().await.unwrap();
            for (height, events) in balance_events(&prog, 1, 1).await.unwrap() {
                write_block(&mut tx, height, &events).await.unwrap();
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE {FILL_TABLE} SET filled_height = 1"
            )))
            .execute(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            assert!(fill(&interrupted.client, &prog, 0, 3, 6, FILL_BLOCKS).await.unwrap());
            assert_eq!(snapshot(&interrupted).await, indexed);
        })
        .await;
    }

    sol! {
        function getTxDetails(bytes32 txid);
    }

    /// In light mode a public call that needs a transaction the cache does
    /// not hold fails, as a revert, and the engine keeps answering: the
    /// lookup answers "not found" instead of an error the engine would retry
    /// and then panic on.
    #[tokio::test]
    async fn a_light_public_call_needing_an_uncached_transaction_fails_and_the_engine_keeps_running()
     {
        with_database("light_uncached", |name| async move {
            database(&name, true).await;
            let config = Brc20IndexerConfig {
                db_host: "127.0.0.1".into(),
                db_port: "5432".into(),
                db_user: "alper".into(),
                db_password: String::new(),
                db_database: name.clone(),
                db_ssl: false,
                bitcoin_rpc_cache_enabled: true,
                ..crate::indexer::brc20_prog_client::test_engine::regtest_config(
                    String::new(),
                    true,
                )
            };
            let engine = TestEngine::open_with(&config);
            let prog = engine.prog.clone();
            prog.brc20_initialise([0u8; 32].into(), 0, 0).await.unwrap();
            let data = getTxDetailsCall { txid: B256::ZERO }.abi_encode();
            let call = EthCall::new(
                Some(Address::ZERO.into()),
                Some(
                    "0x00000000000000000000000000000000000000fd"
                        .try_into()
                        .unwrap(),
                ),
                RawBytes::new(format!("0x{}", hex::encode(data))),
            );
            let answer = prog.eth_call(call, None).await;
            assert!(answer.is_err(), "{answer:?}");
            assert_eq!(engine_height(&prog).await, 0);
            drop(engine);
        })
        .await;
    }

    /// The BRC2.0 activation height the start-up tests run with.
    const PHASE_ONE: i32 = 2;

    /// A full-mode indexer on database `name` and `engine`, with BRC-20 from
    /// block 1 and BRC2.0 from `PHASE_ONE`.
    fn indexer(name: &str, engine: &TestEngine) -> Brc20Indexer {
        let config = Brc20IndexerConfig {
            db_host: "127.0.0.1".into(),
            db_port: "5432".into(),
            db_user: "alper".into(),
            db_password: String::new(),
            db_database: name.into(),
            db_ssl: false,
            network_type: Network::Signet,
            light_client_mode: false,
            first_inscription_height: 1,
            first_brc20_height: 1,
            first_brc20_prog_phase_one_height: PHASE_ONE,
            ..Default::default()
        };
        Brc20Indexer::new(
            config,
            engine.prog.clone(),
            engine.bitcoin_rpc_buffer.clone(),
            None,
        )
    }

    /// The engine's block hash at `height`, as brc20 stores it.
    async fn engine_hash(prog: &Brc20Prog, height: i32) -> String {
        prog.eth_get_block_by_number(height.to_string(), Some(false))
            .await
            .unwrap()
            .hash
            .bytes
            .to_string()
            .trim_start_matches("0x")
            .to_string()
    }

    async fn engine_height(prog: &Brc20Prog) -> i64 {
        i64::from_str_radix(
            prog.eth_block_number()
                .await
                .unwrap()
                .trim_start_matches("0x"),
            16,
        )
        .unwrap()
    }

    /// Indexes blocks 1 to 3 from the engine's three blocks, then two more
    /// blocks the engine never saw, as brc20 ahead of a lagging engine data
    /// dir. Returns the BRC2.0 tables as they stood at block 3.
    async fn brc20_ahead_of_the_engine(
        database: &mut Brc20Database,
        prog: &Brc20Prog,
    ) -> Vec<String> {
        let mut token = None;
        for height in 1..=3 {
            let events: Vec<BalanceEvent> = balance_events(prog, height, height)
                .await
                .unwrap()
                .into_iter()
                .flat_map(|(_, events)| events)
                .collect();
            for event in &events {
                if let BalanceEvent::Created {
                    contract_address, ..
                } = event
                {
                    token.get_or_insert(contract_address.clone());
                }
            }
            let hash = engine_hash(prog, height).await;
            database
                .set_block_hashes(height, &hash, "", None, &events)
                .await
                .unwrap();
        }
        let at_three = snapshot(database).await;
        let alice = evm_address(&hex::decode(ALICE_PKSCRIPT).unwrap());
        let bob = evm_address(&hex::decode(BOB_PKSCRIPT).unwrap());
        database
            .set_block_hashes(
                4,
                "h4",
                "",
                None,
                &[transfer(&token.unwrap(), &alice, &bob, 1)],
            )
            .await
            .unwrap();
        database
            .set_block_hashes(5, "h5", "", None, &[])
            .await
            .unwrap();
        assert_ne!(snapshot(database).await, at_three);
        at_three
    }

    /// An engine below brc20 at start rolls brc20 back to the engine's
    /// height through the `brc20 reorg` path; the BRC2.0 tables return to
    /// that block and the per-block check that precedes indexing passes.
    #[tokio::test]
    async fn a_lagging_engine_rolls_brc20_back_to_its_height() {
        let _serialised = crate::database::BRC20_DATABASE_TEST_LOCK.lock().await;
        let engine = TestEngine::open();
        let prog = engine.prog.clone();
        with_database("lagging", move |name| async move {
            mine_three_blocks(&prog).await;
            prog.brc20_commit_to_database().await.unwrap();
            let mut database = database(&name, false).await;
            let at_three = brc20_ahead_of_the_engine(&mut database, &prog).await;
            replace_brc20_database(database).await;

            let mut indexer = indexer(&name, &engine);
            indexer.init().await.unwrap();
            {
                let database = get_brc20_database();
                let database = database.lock().await;
                assert_eq!(database.get_current_block_height().await.unwrap(), 3);
                assert_eq!(snapshot(&database).await, at_three);
            }
            assert_eq!(engine_height(&prog).await, 3);
            indexer.check_brc20_prog_height().await.unwrap();
            drop(engine);
        })
        .await;
    }

    /// A fresh engine data dir rolls brc20 back to the block before BRC2.0's
    /// activation, which the engine then mines up to.
    #[tokio::test]
    async fn a_fresh_engine_rolls_brc20_back_to_before_activation() {
        let _serialised = crate::database::BRC20_DATABASE_TEST_LOCK.lock().await;
        let engine = TestEngine::open();
        let prog = engine.prog.clone();
        with_database("fresh_engine", move |name| async move {
            let mut database = database(&name, false).await;
            database
                .set_block_hashes(
                    1,
                    "h1",
                    "",
                    None,
                    &[
                        created("ordi", TOKEN),
                        transfer(TOKEN, ZERO_ADDRESS, ALICE, 1_000),
                    ],
                )
                .await
                .unwrap();
            let at_one = snapshot(&database).await;
            database
                .set_block_hashes(2, "h2", "", None, &[transfer(TOKEN, ALICE, BOB, 100)])
                .await
                .unwrap();
            database
                .set_block_hashes(
                    3,
                    "h3",
                    "",
                    None,
                    &[
                        created("pepe", OTHER_TOKEN),
                        transfer(OTHER_TOKEN, ZERO_ADDRESS, BOB, 7),
                    ],
                )
                .await
                .unwrap();
            replace_brc20_database(database).await;
            assert_eq!(engine_height(&prog).await, 0);

            let mut indexer = indexer(&name, &engine);
            indexer.init().await.unwrap();
            {
                let database = get_brc20_database();
                let database = database.lock().await;
                assert_eq!(
                    database.get_current_block_height().await.unwrap(),
                    PHASE_ONE - 1
                );
                assert_eq!(snapshot(&database).await, at_one);
            }
            assert_eq!(engine_height(&prog).await, i64::from(PHASE_ONE - 1));
            indexer.check_brc20_prog_height().await.unwrap();
            drop(engine);
        })
        .await;
    }

    /// Between blocks an engine below the block before activation is an
    /// error naming both heights, and brc20 stays where it is.
    #[tokio::test]
    async fn between_blocks_an_engine_below_activation_is_an_error() {
        let _serialised = crate::database::BRC20_DATABASE_TEST_LOCK.lock().await;
        let engine = TestEngine::open();
        with_database("per_block", move |name| async move {
            let mut database = database(&name, false).await;
            for height in 1..=3 {
                database
                    .set_block_hashes(height, &format!("h{height}"), "", None, &[])
                    .await
                    .unwrap();
            }
            replace_brc20_database(database).await;
            let mut indexer = indexer(&name, &engine);
            let error = indexer
                .reorg_to_last_synced_block_height(crate::indexer::Reconcile::PerBlock)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("at height 0"), "{error}");
            assert!(error.contains("brc20 is at 3"), "{error}");
            assert_eq!(
                get_brc20_database()
                    .lock()
                    .await
                    .get_current_block_height()
                    .await
                    .unwrap(),
                3
            );
            drop(engine);
        })
        .await;
    }

    /// The indexer's start repairs an incomplete current-balances table once
    /// and records the marker.
    #[tokio::test]
    async fn the_start_repairs_an_incomplete_current_balances_table() {
        let _serialised = crate::database::BRC20_DATABASE_TEST_LOCK.lock().await;
        let engine = TestEngine::open();
        with_database("extras_start", move |name| async move {
            let mut database = database(&name, false).await;
            sqlx::raw_sql(
                "INSERT INTO brc20_historic_balances
                     (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id)
                 VALUES ('a1', 'wa1', 'ordi', 5, 5, 1, 1), ('b2', 'wb2', 'ordi', 3, 2, 1, 2);",
            )
            .execute(&database.client)
            .await
            .unwrap();
            database.index_extra_tables(1).await.unwrap();
            database.set_block_hashes(1, "h1", "", None, &[]).await.unwrap();
            sqlx::raw_sql(
                "DELETE FROM brc20_current_balances WHERE pkscript = 'b2';
                 DROP TABLE brc20_extras_verified;",
            )
            .execute(&database.client)
            .await
            .unwrap();
            assert_eq!(
                crate::database::extras_verified::compare(&database.client)
                    .await
                    .unwrap(),
                (1, 0)
            );
            replace_brc20_database(database).await;

            indexer(&name, &engine).init().await.unwrap();
            let database = get_brc20_database();
            let database = database.lock().await;
            assert_eq!(
                crate::database::extras_verified::compare(&database.client)
                    .await
                    .unwrap(),
                (0, 0)
            );
            assert!(
                crate::database::extras_verified::is_verified(&database.client)
                    .await
                    .unwrap()
            );
            drop(engine);
        })
        .await;
    }
}
