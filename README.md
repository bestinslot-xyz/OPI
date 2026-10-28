# OPI - Open Protocol Indexer

Open Protocol Indexer, OPI, is the **best-in-slot open-source indexing client** for **meta-protocols** on Bitcoin.
OPI links upstream **ord 0.29.1** as a library and is built with **modularity** in mind.
All modules in OPI have been built with **reorg protection**.

OPI has modules for **BRC-20**, **Bitmap**, **SNS** and **Runes**, which run together in the `opi-indexer` binary and are served by one API, `opi-indexer api`. Pull Requests are welcomed for other meta-protocols.

## Repository layout

One root Cargo workspace holds every crate, the indexer and the API included.

| Path | Contents |
| --- | --- |
| `crates/opi-indexer` | the `opi-indexer` binary: `run`, `api`, `validate` and the `brc20` subcommands |
| `crates/opi-source` | ord as a journal source: gate proxy, stager and the journal of finalized blocks |
| `crates/opi-runtime` | drives every module over the journal, one supervised task per module |
| `crates/opi-core` | the types shared by the source, the runtime and the modules |
| `crates/opi-brc20` | the BRC-20 and BRC2.0 module, its database schema |
| `crates/opi-bitmap` | the Bitmap module |
| `crates/opi-sns` | the SNS module |
| `crates/opi-runes` | the Runes module |
| `crates/opi-api` | the HTTP API for every enabled module, served by `opi-indexer api` |
| `tools/parity` | Python reference implementations and comparison tests for the modules, and the API comparison script |
| `.env_sample` | every setting of `opi-indexer` and the API |

## Main Meta-Protocol Indexer / opi-indexer

**opi-indexer** sits in the core of OPI. It runs ord as a journal source that records **all json/text inscriptions** and their **first 2 transfers**, and feeds the journal to the BRC-20, Bitmap and SNS modules, and ord's rune events to the Runes module, in one process. Each module keeps its own Postgres database. BRC-20 becomes invalid after 2 hops, bitmap and SNS validity is calculated at inscription time.

### Modules

`OPI_MODULES` lists the modules to run: any subset of `brc20,bitmap,sns,runes`.
Each module reads the journal and keeps its own Postgres database. The API serves the same list.

| Module | Indexes | Database variables |
| --- | --- | --- |
| `brc20` | BRC-20 and BRC2.0 | `DB_*` |
| `bitmap` | Bitmap | `BITMAP_DB_*` |
| `sns` | Sats Names, by the written spec (see [SNS indexing rules](#sns-indexing-rules)) | `SNS_DB_*` |
| `runes` | Runes, from ord's rune events | `RUNES_DB_*` |

The `*_DB_*` variables are `USER`, `HOST`, `PORT`, `DATABASE`, `PASSWD` and
`SSL`, for example `BITMAP_DB_DATABASE`. The runes module reports `event_hash_version` 2 because it orders a
transaction's input and burn events canonically.
`NETWORK_TYPE` and `REPORT_TO_INDEXER`, `REPORT_URL`, `REPORT_NAME`,
`REPORT_RETRIES` are shared by all modules.

#### Running without runes

Rune indexing follows `OPI_MODULES`: ord runs with `--index-runes` and the
journal keeps rune records only when `runes` is in the list. A box that serves
BRC-20, Bitmap and SNS sets, for example:

```bash
OPI_MODULES=brc20,bitmap,sns
```

ord then builds no rune tables and does no rune work per block, and the
journal has no rune tables, so both ord's index and the journal need less disk.

The choice is fixed per ord data dir and journal: ord records it in its index
when it creates the index, and the journal records it in its `runes` meta row.
A start whose `OPI_MODULES` disagrees with either is refused with a message
naming the dirs to rebuild. The journal is checked first, for example:

```text
opening the journal of ord's data dir /data/ord: journal at /data/journal/journal.redb was built with rune indexing, but runes is disabled; start with runes enabled again, or delete the journal and ord's data dir and index again
```

To switch, either put `OPI_MODULES` back as it was, or delete `OPI_ORD_DATA_DIR`
and `OPI_JOURNAL_DIR` and index from the start with the new list. Adding runes
to an index built without them always takes a rebuild.

Every ord data dir and journal built by earlier versions has rune indexing. To
keep one, add `runes` to `OPI_MODULES` (this also starts the runes module and
needs `RUNES_DB_DATABASE`), or delete both dirs to run without runes.

### Prerequisites

- bitcoind with `-txindex`, not pruned. Finalize may fetch `getblock <hash> 3`,
  which needs undo data.
- One Postgres database per enabled module.
- A writable directory for the brc20-prog engine (`BRC20_PROG_DB_PATH`) when
  `brc20` is enabled. The engine runs inside the indexer process, and the
  brc20 module always runs BRC2.0.

### Configuration

Copy `.env_sample` to `.env` in the repository root (loaded from the working
directory or an ancestor, so run `opi-indexer` from inside the repository) or export the variables.

| Variable | Meaning |
| --- | --- |
| `OPI_CHAIN` | `mainnet`, `signet`, `testnet4` or `regtest` |
| `OPI_ORD_DATA_DIR` | directory of ord's index database |
| `OPI_JOURNAL_DIR` | directory of the journal |
| `BITCOIN_RPC_URL` | bitcoind RPC URL |
| `BITCOIN_RPC_USER`, `BITCOIN_RPC_PASSWD` | bitcoind RPC credentials |
| `BITCOIN_RPC_COOKIE` | bitcoind cookie file, instead of user and password |
| `OPI_CHUNK` | blocks per ord update, default `1000` |
| `OPI_MODULES` | comma-separated modules (`brc20`, `bitmap`, `sns`, `runes`), default `brc20`; `runes` also switches on ord's rune indexing (see [Running without runes](#running-without-runes)) |
| `DB_*`, `BITMAP_DB_*`, `SNS_DB_*`, `RUNES_DB_*` | the module databases (see Modules); `DB_DATABASE`, `BITMAP_DB_DATABASE`, `SNS_DB_DATABASE` and `RUNES_DB_DATABASE` have no default and are required when the module is enabled |
| `NETWORK_TYPE` | network every module indexes; must match `OPI_CHAIN` |
| `REPORT_*` | block reporting, shared by all modules; one `REPORT_NAME` names the node for all of them, and each report's payload carries its module type |
| `RUST_LOG` | log filter, default `info` |

`opi-indexer` reads one `.env`, from its working directory or an ancestor. brc20's own
variables (`DB_*`, `OPERATION_MODE`, `BRC20_PROG_*`, `BITCOIN_RPC_CACHE_ENABLED`, ...)
live in that same file; `.env_sample` lists them. Set `DB_DATABASE` to brc20's
own database, not `postgres`. brc20 runs in full mode (`OPERATION_MODE=full`).
All enabled modules' settings are read and checked before the source starts, so
a missing or invalid one fails the start.

`BITCOIN_RPC_URL` and the bitcoind credentials are shared by the source and
brc20's engine lookups; set them once.

### Build

```bash
cargo build --release --locked -p opi-indexer
```

### Run

```bash
target/release/opi-indexer run
```

`run` is the default subcommand. SIGINT and SIGTERM close the source gate, stop
the modules, join the source and exit 0. A source error is logged and exits
non-zero. Each module indexes a block once the journal finalizes it, and rolls
back when its stored hashes differ from the journal's. A module error or panic
restarts that module, with a freshly built instance and database state, after a
backoff of 5 seconds, doubling to 5 minutes; the other modules and the source
keep running. A panic in brc20 stops the process instead (see BRC2.0 engine).
On shutdown `run` waits up to 150 seconds for brc20 to finish the block in
flight. Log verbosity follows `RUST_LOG`. Logs go to stderr for every
command.

#### BRC2.0 engine

When `brc20` is enabled, the brc20-prog engine (`Brc20Prog`, brc20-prog 0.16)
runs inside the `opi-indexer` process. There is no separate brc20-prog server.

| Variable | Meaning |
| --- | --- |
| `BRC20_PROG_DB_PATH` | directory of the engine's database; required |
| `BRC20_PROG_RPC_SERVER_URL` | address of the public JSON-RPC server, default `127.0.0.1:18545` |
| `BRC20_PROG_RPC_SERVER_ENABLED` | the public JSON-RPC server runs when this is unset or exactly `true`; any other value turns it off |
| `EVM_RECORD_TRACES` | the engine records transaction traces when this is unset or exactly `true`; any other value turns it off. The block trace hash that OPI nodes compare is built from these traces, so with traces off every block's trace hash is the hash of the empty string and differs from the network's. Traces also serve `debug_traceTransaction`. The engine fixes the setting in its database when it creates it, so changing it takes a fresh `BRC20_PROG_DB_PATH` and a brc20 reindex |

`run` serves brc20-prog's public JSON-RPC on `BRC20_PROG_RPC_SERVER_URL`.
Blockscout, the swap sequencer and other clients connect there. It rejects the
indexer methods (`brc20_mine` and the like) and takes no credentials, so bind
it to loopback or firewall it. brc20-prog's `BRC20_PROG_RPC_SERVER_ENABLE_AUTH`,
`BRC20_PROG_RPC_SERVER_USER` and `BRC20_PROG_RPC_SERVER_PASSWORD` do not apply
to it; `BRC20_PROG_RPC_SERVER_ENABLE_AUTH=true` without a user and password
makes the engine fail to open. The engine reads its remaining settings from
brc20-prog's own variables (`EVM_CALL_GAS_LIMIT`, request limits; see
brc20-prog's configuration docs).

The engine gets its Bitcoin data in process, through the indexer. A lookup reads the Bitcoin RPC cache first
when `BITCOIN_RPC_CACHE_ENABLED=true` (an in-memory buffer, then the Postgres
table; the code default is `false`, `.env_sample` sets `true`). A "not yet"
answer (an unknown transaction, or a transaction not yet in a block) is reused
only within the block that looked it up: in full mode a later block asks
bitcoind again, while light mode replays the rows full nodes published. A
stored `getrawtransaction` answer is likewise served in full mode only when
the block it names is indexed; otherwise bitcoind is asked again. In
full mode a miss goes to bitcoind at `BITCOIN_RPC_URL`, authenticated with
`BITCOIN_RPC_USER` and `BITCOIN_RPC_PASSWD`, or with `BITCOIN_RPC_COOKIE`, which
is re-read on every request. Full mode requires one of the two credentials; the
config check fails otherwise. Light mode needs no bitcoind and no Bitcoin
credentials: it answers from the cache alone, so it requires
`BITCOIN_RPC_CACHE_ENABLED=true` (the config check fails otherwise), and the
network comes from `NETWORK_TYPE`.

The cache stores successful answers and `getrawtransaction` "not found" (-5)
answers. Only the indexer's own lookups write it; public `eth_call`s read it. A
lookup gives up after 20 seconds (a request to bitcoind after 15, a wait for a
Postgres connection after 5), and the engine retries a failed lookup 5 times
before it panics, which stops the process (see below).

The network and chain id follow `NETWORK_TYPE`. brc20-prog's own
`BITCOIN_RPC_PASSWORD` and `BITCOIN_RPC_NETWORK` have no effect;
`BITCOIN_RPC_URL` and `BITCOIN_RPC_USER` are opi-indexer's settings, as
described above. An `.env` that still sets `BITCOIN_RPC_PROXY_SERVER_ENABLED` or
`BITCOIN_RPC_PROXY_SERVER_ADDR` has them ignored; remove them.

A panic on the brc20 thread or in the engine's public JSON-RPC server stops the
whole process, since the engine may be left mid-write. Run `opi-indexer` under a
supervisor such as systemd that restarts it.

To move a node that ran the standalone brc20-prog server, stop that server and
point `BRC20_PROG_DB_PATH` at its database; the on-disk format is the same.

### Subcommands

```
opi-indexer [run | api | validate | brc20 <COMMAND>]
```

`api` serves the HTTP API (see [API](#api)); it needs neither the journal nor ord.

`opi-indexer brc20` holds the BRC-20 light client and maintenance commands. It
reads only brc20's environment, so it runs on light-client hosts.
`event-str` runs alongside `run`. Every other command opens brc20-prog's
database, which `run` holds, so stop `run` first; they fail with "brc20-prog's
database at <path> is in use; stop `opi-indexer run` first" otherwise. In full
mode these commands read Bitcoin data like `run` does, so the `.env` carries the
same bitcoind credentials (`BITCOIN_RPC_USER` and `BITCOIN_RPC_PASSWD`, or
`BITCOIN_RPC_COOKIE`).

| Command | Does |
| --- | --- |
| `brc20 light` | runs the BRC-20 light client until stopped; needs `OPERATION_MODE=light` |
| `brc20 reset [--yes]` | deletes all BRC-20 data and starts fresh |
| `brc20 reorg [--yes] <HEIGHT>` | rolls the BRC-20 state back to a block height |
| `brc20 recalculate-hashes <FROM>` | recomputes the cumulative event hashes from a height to the latest indexed block |
| `brc20 reindex-extras [--yes]` | rebuilds the extra tables from the indexed data; may take a long time |
| `brc20 event-str <HEIGHT>` | prints the block event string at a height |
| `brc20 trace-str <HEIGHT>` | prints the block trace string at a height |

The destructive commands (`reset`, `reorg`, `reindex-extras`) ask for a y/N confirmation; `--yes`
skips it. `opi-indexer <command> --help` prints each command's usage.

### Validation runbook

Cut a deployment over to opi-indexer only after `validate` passes on signet and
on mainnet.

#### `validate`

```
opi-indexer validate [--from <height>] [--to <height>]
```

A BRC-20 node is correct when its cumulative event hash (and, from BRC2.0 on,
its cumulative trace hash) equals the OPI network's at the same height. Each
block's hash folds into the next, so a match at height N vouches for every
height up to N.

`validate` reads brc20's hashes from its Postgres database and the network's
from the OPI API, with the same `DB_*` and `NETWORK_TYPE` variables, API URL
and event hash version as `run`. It never opens the journal, so it runs
while `opi-indexer run` is running.

- `--from` defaults to brc20's first BRC-20 height for the network.
- `--to` defaults to the lower of brc20's latest height and the network's best
  verified block.

It compares the hashes at `--to`. When the event hashes match, and the trace
hashes match or the network has none, it prints `OK <from>..<to>` and exits 0.
Otherwise it bisects `<from>..<to>` for the first height whose event or trace
hash differs, prints that height with brc20's and the network's values for both
hashes, and exits 1. A height missing on either side, an unreachable API or
database, or an empty range prints the cause and exits 2.

#### Cut-over check

Run signet first, then mainnet. With `opi-indexer run` synced to the network's
best verified block, run `opi-indexer validate`. Expected: `OK <first>..<tip>`.

On a mismatch, the printed height is the first one where brc20 and the network
differ. Rerun with `--from <h> --to <h>` to confirm it is deterministic, then
compare that block's events with the network's. Fix the cause in the owning
component with a regression test, then rebuild brc20's hashes from that height
(`opi-indexer brc20 recalculate-hashes <h>`) and run `validate` again.

#### Signet smoke run

Run `opi-indexer` on signet with a fresh brc20 database for at least 1,000
blocks past the first BRC-20 height. Expected: brc20's current block height
advances and no errors are logged.

## BRC-20 Indexer

**BRC-20 Indexer** is the first module of OPI. It follows the official protocol rules hosted [here](https://layer1.gitbook.io/layer1-foundation/protocols/brc-20/indexing). BRC-20 Indexer saves all historical balance changes and all BRC-20 events.

In addition to indexing all events, it also calculates a block hash and cumulative hash of all events for easier db comparison.

It also calculates a hash of all BRC-20 programmable module traces in the current block, and a cumulative hash of the traces.

Here's the pseudocode for hash calculation:

```python
## Calculation starts at block 767430 which is the first inscription block

EVENT_SEPARATOR = '|'
## max_supply, limit_per_mint, amount decimal count is the same as ticker's decimals (no trailing dot if decimals is 0)
## ticker_lowercase = lower(ticker)
## ticker_original is the ticker on inscription
## PRAGUE_ACTIVATION_HEIGHT marks support for PRAGUE EVM on BRC2.0 and it's 923369 on Mainnet and 275000 on Signet
for event in block_events:
  if event is 'predeploy-inscribe':
    block_str += 'predeploy-inscribe;<inscr_id>;<predeployer_pkscript>;<hash>;<block_height>' + EVENT_SEPARATOR
  if event is 'deploy-inscribe':
    block_str += 'deploy-inscribe;<inscr_id>;<deployer_pkscript>;<ticker_lowercase>;<ticker_original>;<max_supply>;<decimals>;<limit_per_mint>;<is_self_mint("true" or "false")>' + EVENT_SEPARATOR
  if event is 'mint-inscribe':
    block_str += 'mint-inscribe;<inscr_id>;<minter_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>;<parent_id("" if null)>' + EVENT_SEPARATOR
  if event is 'transfer-inscribe':
    block_str += 'transfer-inscribe;<inscr_id>;<source_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>' + EVENT_SEPARATOR
  if event is 'transfer-transfer':
    ## if sent as fee, sent_pkscript is empty
    block_str += 'transfer-transfer;<inscr_id>;<source_pkscript>;<sent_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>' + EVENT_SEPARATOR
  if event is 'brc20prog-deploy-inscribe':
    block_str += 'brc20prog-deploy-inscribe;<inscr_id>;<source_pkscript>;<data>;<base64_data>' + EVENT_SEPARATOR
  if event is 'brc20prog-deploy-transfer':
    if block_height >= PRAGUE_ACTIVATION_HEIGHT:
      block_str += 'brc20prog-deploy-transfer;<inscr_id>;<source_pkscript>;<spent_pkscript>;<data>;<base64_data>;<byte_len>;<op_return_tx_id>' + EVENT_SEPARATOR
    else:
      block_str += 'brc20prog-deploy-transfer;<inscr_id>;<source_pkscript>;<spent_pkscript>;<data>;<base64_data>;<byte_len>' + EVENT_SEPARATOR
  if event is 'brc20prog-call-inscribe':
    block_str += '<inscr_id>;<source_pkscript>;<contract_address>;<contract_inscription_id>;<data>;<base64_data>' + EVENT_SEPARATOR
  if event is 'brc20prog-call-transfer':
    if block_height >= PRAGUE_ACTIVATION_HEIGHT:
      block_str += '<inscr_id>;<source_pkscript>;<spent_pkscript>;<contract_address>;<contract_inscription_id>;<data>;<base64_data>;<byte_len>;<op_return_tx_id>' + EVENT_SEPARATOR
    else:
      block_str += '<inscr_id>;<source_pkscript>;<spent_pkscript>;<contract_address>;<contract_inscription_id>;<data>;<base64_data>' + EVENT_SEPARATOR
  if event is 'brc20prog-transact-inscribe':
    block_str += '<inscr_id>;<source_pkscript>;<data>;<base64_data>' + EVENT_SEPARATOR
  if event is 'brc20prog-transact-transfer':
    if block_height >= PRAGUE_ACTIVATION_HEIGHT:
      block_str += '<inscr_id>;<source_pkscript>;<spent_pkscript>;<data>;<base64_data>;<byte_len>;<op_return_tx_id>' + EVENT_SEPARATOR
    else:
      block_str += '<inscr_id>;<source_pkscript>;<spent_pkscript>;<data>;<base64_data>;<byte_len>' + EVENT_SEPARATOR
  if event is 'brc20prog-withdraw-inscribe':
    block_str += '<inscr_id>;<source_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>' + EVENT_SEPARATOR
  if event is 'brc20prog-withdraw-transfer':
    block_str += '<inscr_id>;<source_pkscript>;<spent_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>' + EVENT_SEPARATOR

if block_str.last is EVENT_SEPARATOR: block_str.remove_last()
block_hash = sha256_hex(block_str)
## for first block last_cumulative_hash is empty
cumulative_hash = sha256_hex(last_cumulative_hash + block_hash)
```

To calculate the trace hashes in a stable way, the JSON string representation of an EVM trace uses the suggested schema at RFC 8785, which has implementations in both Rust and Python:

```python
  ### Calculation starts at block 912690, which is the first BRC2.0 block
  traces_str = ""
  for tx in block_txes:
    trace_str = rfc8785.dumps(brc20_prog_client.debug_traceTransaction(tx).result)
    traces_str += trace_str + EVENT_SEPARATOR
  if traces_str.last is EVENT_SEPARATOR: traces_str.remove_last()
  traces_hash = sha256_hex(traces_str)
  cumulative_traces_hash = sha256_hex(last_cumulative_traces_hash + traces_hash)
```

There is an optional block event hash reporting system pointed at https://api.opi.network/report_block. If you want to exclude your node from this, just change the `REPORT_TO_INDEXER` variable in `.env`.
Also change `REPORT_NAME` to differentiate your node from others.

## Bitmap Indexer

**Bitmap Indexer** is the second module of OPI. It follows the official protocol rules hosted [here](https://gitbook.bitmap.land/bitmap-theory-whitepaper/theory). Bitmap Indexer saves all bitmap-number inscription-id pairs.

In addition to indexing all pairs, it also calculates a block hash and cumulative hash of all events for easier db comparison. Here's the pseudocode for hash calculation:

```python
## Calculation starts at block 767430 which is the first inscription block

EVENT_SEPARATOR = '|'
for bitmap in new_bitmaps_in_block:
  block_str += 'inscribe;<inscr_id>;<bitmap_number>' + EVENT_SEPARATOR

if block_str.last is EVENT_SEPARATOR: block_str.remove_last()
block_hash = sha256_hex(block_str)
## for first block last_cumulative_hash is empty
cumulative_hash = sha256_hex(last_cumulative_hash + block_hash)
```

## SNS Indexer

**SNS Indexer** is the third module of OPI. It follows the official protocol rules hosted [here](https://docs.satsnames.org/sats-names/sns-spec/index-names). SNS Indexer saves all name, domain, inscription-id and namespace, inscription-id tuples.

In addition to indexing all tuples, it also calculates a block hash and cumulative hash of all events for easier db comparison. Here's the pseudocode for hash calculation:

```python
## Calculation starts at block 767430 which is the first inscription block

EVENT_SEPARATOR = '|'
for event in new_events_in_block:
  if event is 'name-registration':
    ## name is the full name, domain is the part afler dot
    block_str += 'register;<inscr_id>;<name>;<domain>' + EVENT_SEPARATOR
  elif event is 'namespace-registration':
    block_str += 'ns_register;<inscr_id>;<namespace>' + EVENT_SEPARATOR

if block_str.last is EVENT_SEPARATOR: block_str.remove_last()
block_hash = sha256_hex(block_str)
## for first block last_cumulative_hash is empty
cumulative_hash = sha256_hex(last_cumulative_hash + block_hash)
```

## Runes Indexer

The runes module (`crates/opi-runes`) turns ord's rune events into the runes
Postgres schema in `crates/opi-runes/src/db_init.sql`. It saves every event (input to a tx, transfer, mint,
initial allocation and burn), the rune entries and the outpoint to balance
entries, and calculates a block hash and a cumulative hash of all events. State
starts at ord's first rune height; event hashes start at height 840000 on
mainnet, 2520000 on testnet, 173831 on signet and 0 elsewhere.

**Event order.** A transaction's events are, in this order:

1. `input`: for each input in input order that spends a rune outpoint, one
   event per stored `(rune_id, amount)` in rune id order;
2. `new_rune_allocation`: when the transaction etches a rune through a
   runestone, with the premine as amount;
3. `mint`: each mint, in emission order;
4. `output`: each transfer, by output index and rune id;
5. `burn`: each burn, by rune id, zero amounts included.

Reports carry `event_hash_version` 2 for this order.

**Event hash.** Each event is the string
`<type>;<outpoint>;<pkscript>;<rune_id>;<amount>`, with `type` one of `input`,
`new_rune_allocation`, `mint`, `output`, `burn`. `outpoint` and `pkscript` are
set for `input` and `output` events and empty for `new_rune_allocation`,
`mint` and `burn`. The block string is the events joined with `|`,
`block_event_hash = sha256_hex(block_string)` (an empty block hashes `""`), and
`cumulative_hash = sha256_hex(previous_cumulative_hash + block_event_hash)`.
The first hashed block uses its `block_event_hash` as the cumulative hash.

**Versions.** Reports send `opi-runes-full-node v1.0.0` with `db_version` 6. A
database with another `db_version` is refused and must be indexed from scratch.
`RUNES_DB_DATABASE` names a new database: one that holds blocks written by
another indexer is refused.

## API

`opi-indexer api` serves every module in `OPI_MODULES` on one port, from its own
process. It reads the same `.env` as `run`.

- **BRC-20** exposes activity on block (block events), balance of a wallet at the start of a given height, current balance of a wallet, block hash and cumulative hash at a given block and hash of all current balances, and tickers, holders, wallet balances and event feeds that merge BRC-20 and BRC2.0 (see [Ticker, wallet and lookup routes](#ticker-wallet-and-lookup-routes)).
- **Bitmap** exposes block hash and cumulative hash at a given block, hash of all bitmaps and inscription_id of a given bitmap.
- **SNS** exposes block hash and cumulative hash at a given block, hash of all registered names, id number and domain of a given name, id number and name tuples of a domain, and all registered namespaces endpoints.
- **Runes** exposes activity on a block (`/v1/runes/activity_on_block`), the balance of a wallet at the start of a height (`/v1/runes/balance_on_block`), the current balance of a wallet (`/v1/runes/get_current_balance_of_wallet`), the unspent rune outpoints of a wallet (`/v1/runes/get_unspent_rune_outpoints_of_wallet`), the holders of a rune (`/v1/runes/holders`), and the block hash and cumulative hash at a height (`/v1/runes/get_hash_of_all_activity`).

Each module's routes live under `/v1/<module>/`. A module outside `OPI_MODULES`
answers `404 {"error":"<module> is not enabled on this node"}`. `/v1/status`
lists the enabled modules; a module entry is `{"block_height":N}`, or
`{"error":"unavailable"}` when its database query fails (the cause is logged).
A failing handler answers
`500 {"error":"internal error","result":null}`.

The server speaks HTTP/1 only and closes a connection that has not sent its
request headers within 60 seconds. On SIGINT or SIGTERM it stops accepting
connections, lets requests in flight finish for at most 10 seconds and exits.
Responses carry no `X-Powered-By` and no `ETag`.

The API needs Postgres 12 or newer: every connection starts with the startup
option `plan_cache_mode=force_custom_plan`, which keeps the row order of
queries without a full `ORDER BY`. A pooler that drops startup options (some
PgBouncer setups) loses it; the API logs a warning at startup when a database
runs another mode. `<MODULE>_SSL=true` encrypts the database connection
without verifying the server certificate. An idle keep-alive connection closes
at the same 60 second header-read timeout.

The API only reads from the module databases, so give it a read-only database
role (see `OPI_ENV_FILE` below for pointing it at its own env file).

The brc20 explorer routes read `evm_addresses`, which the brc20 module creates
and keeps current: each block's events add their addresses, and a database
without the table gets it filled from the indexed events on the next start
(an existing table is adopted as it is). When `opi-indexer` takes over a
database whose `evm_addresses` the Python sidecar
`brc20_update_evm_addresses_custom.py` maintains, stop the sidecar; no wait is
needed. The next start adds the rows for the blocks above the sidecar's
progress table `brc20_block_hashes_evm_addresses` and drops that table. The
sidecar's DDL makes `postgres` the table owner, so the indexer's database role
needs INSERT and DELETE on `evm_addresses` and USAGE on
`evm_addresses_id_seq`, or ownership
(`ALTER TABLE evm_addresses OWNER TO <role>`), and the right to drop the
progress table (its owner or a superuser).

The unused-transaction routes read the tables `brc20_unused_tx_inscrs` and
`brc20_extras_block_hashes`, which neither the indexer nor
`brc20 reindex-extras` creates; they answer the 500 body until those tables
exist in the brc20 database.

### Ticker, wallet and lookup routes

These routes answer `{"error":null,"result":...}`. A missing or invalid
parameter answers `400 {"error":"<reason>","result":null}`, and an unknown
ticker or inscription answers `404` with `"ticker not found"`,
`"bitmap not found"` or `"sns name not found"` as the error. Lists take
`limit` (default `100`, at most `500`). A list result echoes the request's
resolved filters (`sort` and `order`, `ticker`, `address`, `pkscript`,
`event_type`, `prefix`, `namespace`), as the route applied them: a lowercased
ticker, an EVM address answered as its mapped Bitcoin address, the lowercase
SNS prefix and namespace. Parameters are bounded: `offset` is at most
`1000000000`, `before_id` at most `999999999999999999`, the bitmap `after` at
most `2147483647`, `address` and `inscription_id` at most 128 bytes,
`pkscript` at most 20000 hex digits, `ticker`, `search` and `type` at most 256
bytes, and the SNS `prefix`, `namespace` and `after` at most 2048 bytes. A
text parameter holds no NUL byte.

| Route | Parameters | Lists |
| --- | --- | --- |
| `/v1/brc20/ticker` | `ticker` | one ticker |
| `/v1/brc20/tickers` | `sort`, `order`, `search`, `offset`, `limit` | tickers |
| `/v1/brc20/ticker/holders` | `ticker`, `offset`, `limit` | a ticker's holders |
| `/v1/brc20/wallet/balances` | `address`, `offset`, `limit` | a wallet's tickers |
| `/v1/brc20/ticker/events` | `ticker`, `type`, `before_id`, `limit` | a ticker's events, newest first |
| `/v1/brc20/wallet/events` | `address` or `pkscript`, `type`, `before_id`, `limit` | a wallet's events, newest first |
| `/v1/bitmap/by_inscription` | `inscription_id` | the bitmap an inscription registered |
| `/v1/bitmap/search` | `prefix`, `after`, `limit` | bitmaps by number prefix |
| `/v1/sns/by_inscription` | `inscription_id` | the name an inscription registered |
| `/v1/sns/search` | `prefix`, `namespace`, `after`, `limit` | names by prefix |

**Amounts.** Amounts are raw integers as strings. Every BRC-20 and BRC2.0
amount carries 18 fractional digits (1 token is `1000000000000000000`), so
`decimals` is always `18`: divide by `10^decimals` for tokens.
`ticker_decimals` is the ticker's deploy precision, the most fractional digits
an amount of it may have; it is null for a BRC2.0 balance of a ticker brc20
does not list and for an event without a ticker. Tickers match in lowercase,
as brc20 stores them, so `ticker=ORDI` finds `ordi`. Each answer of the brc20
routes carries the database's `block_height`.

**BRC2.0 balances.** The brc20 module keeps every wallet's BRC2.0 token balance
in its own database (`brc20_prog_tickers`, `brc20_prog_current_balances` and
`brc20_prog_historical_balances`), read from the engine's logs in the same
transaction as each block. A `BRC20Created` log of the BRC20 controller
registers a token contract under its `name()`, and the contract's ERC-20
`Transfer` logs move balances between lowercase EVM addresses. The historical
table holds one row per wallet, ticker and block, the balance at the end of
that block. A database without these tables gets them filled from the engine's
logs, from the BRC2.0 activation height up, on the next start before indexing
resumes (a few seconds on signet). The fill needs the engine and brc20 at the
same height and refuses to start otherwise.

**Tickers.** `/v1/brc20/ticker` answers the ticker's `tick`, `original_tick`,
`decimals`, `ticker_decimals`, `max_supply`, `minted_supply`,
`remaining_supply`, `burned_supply`, `limit_per_mint`, `is_self_mint`,
`deploy_inscription_id`, `deploy_block_height`, `holders`, `prog_holders` and
`prog_contract_address` (the BRC2.0 token contract, null before the ticker's
first deposit). `holders` counts the pkScripts with a positive BRC-20
balance, and `prog_holders` the EVM addresses with a positive BRC2.0 balance;
a Bitcoin address holding both counts in both. Neither counts the BRC2.0
module's pkScript `6a09425243323050524f47` (`OP_RETURN "BRC20PROG"`), which
holds the BRC-20 side of every deposit: that balance is listed per EVM
address on the BRC2.0 side.
`/v1/brc20/tickers` lists the same objects without `prog_contract_address`.
`sort` is `holders` (default), `prog_holders`, `max_supply`, `mint_progress`
(minted over max supply) or `deploy_height`; `order` is `desc` (default) or
`asc`; ties order by ticker. `search` keeps the tickers that start with it.

**Holders and wallet balances.** A holder is a Bitcoin address: its BRC-20
balance and the BRC2.0 balance of its EVM address (from `evm_addresses`) form
one row, `{"address","pkscript","evm_address","balance","available_balance",
"prog_balance","total_balance"}`, where `total_balance` is `balance` plus
`prog_balance`. A BRC-20 balance whose pkScript has no address is listed by
its `pkscript`, and an EVM address without a Bitcoin address (a contract, for
example) by its `evm_address` with a null `address`. `/v1/brc20/ticker/holders`
orders by `total_balance`, largest first, then by holder. The module pkScript
is left out of holders and wallet balances.
`/v1/brc20/wallet/balances` takes a Bitcoin address, whose EVM address comes
from `evm_addresses`, or an EVM address (`0x` and 40 hex digits, any case),
whose Bitcoin address comes from the same table when it has one. It answers
`address`, `evm_address` and `balances`, one row per ticker with a positive
balance on either side, by ticker: `{"tick","decimals","ticker_decimals",
"balance","available_balance","prog_balance","total_balance"}`. Both lists
page with `offset` (default `0`) and `limit`, and report `offset`, `limit`
and `has_more`. A wallet the database does not know answers `200` with an
empty list, here and on `/v1/brc20/wallet/events`.

**Event feeds.** An event is `{"id","block_height","block_time",
"inscription_id","event_type","ticker_decimals","event"}`: `block_time` is the
block's time in seconds and is null when the block's time is unknown (a block
indexed before block times were stored that the journal did not hold at a
start, or any such block in light mode), `event_type` is the name from
`brc20_event_types` and `event` is the stored event object. `type` keeps one
event type by that name. A ticker's events are those whose `tick` is the
ticker. A wallet's events are those where it is any party (a `*_pkScript` or
`*_wallet` field: `deployer`, `minted`, `source`, `spent`, `predeployer`);
`address` is a Bitcoin address or an EVM address that answers as its mapped
Bitcoin address (an unmapped one has no events), and `pkscript` is hex.
BRC2.0 deposits are `transfer-transfer` events spent to the module pkScript,
and withdrawals are `brc20prog-withdraw-*` events. The feeds page by key:
`before_id` is the previous page's `next_before_id`, and a page holds the
events with a lower id, so pages neither repeat nor skip events as new ones
arrive. `has_more` tells whether older events exist; `next_before_id` is null
on the last page.

**Bitmap and SNS lookups.** A bitmap is `{"inscription_id",
"inscription_number","bitmap_number","block_height"}` and a name is
`{"inscription_id","inscription_number","name","namespace","block_height"}`,
where `block_height` is the registering block and `namespace` is the part of
the name after its dot. `/v1/bitmap/search` lists the bitmaps whose decimal
number starts with `prefix` (1 to 10 digits), ascending by number.
`/v1/sns/search` lists the names that start with `prefix`, bytewise ascending,
optionally within one `namespace`; the prefix and namespace are lowercased as
SNS stores names (the result's `prefix` and `namespace` are those lowercase
forms), and `%`, `_` and `\` match themselves. Both page by key:
`after` is the previous page's `next_after`, `has_more` tells whether more
follow and `next_after` is null on the last page.

### API configuration

`opi-indexer api` reads the `.env` of its working directory (or an ancestor), or
the file `OPI_ENV_FILE` names (set it as a process environment variable, since
it is read before the file is loaded); variables already in the environment
win, and a missing file is ignored.

| Variable | Meaning |
| --- | --- |
| `OPI_MODULES` | the modules to serve, as for `opi-indexer`, default `brc20` |
| `OPI_ENV_FILE` | alternative `.env` path; only works as a process environment variable |
| `DB_*`, `BITMAP_DB_*`, `SNS_DB_*`, `RUNES_DB_*` | the module databases; the database name is required for each enabled module |
| `DB_MAX_CONNECTIONS` | maximum connections per module database pool, default `10` |
| `API_HOST` | listen address, default `127.0.0.1` |
| `API_PORT` | listen port, default `8000` |
| `API_TRUSTED_PROXY_CNT` | number of trusted proxies (such as nginx) in front of the API, default `0` |
| `RATE_LIMIT_ENABLE` | `true` turns the rate limiter on, default `false` |
| `RATE_LIMIT_WINDOW_MS` | rate limit window in milliseconds, default `900000` |
| `RATE_LIMIT_MAX` | requests per client per window, default `100` |

The `DB_*`, `BITMAP_DB_*`, `SNS_DB_*` and `RUNES_DB_*` variables are the same
ones `opi-indexer run` reads (`USER` and `HOST` default to `postgres` and
`localhost`, `PORT` to `5432`, `SSL` is on only when exactly `true`).

Run `opi-indexer api` in place of the Node API or the four per-module API
processes (ports 8000-8003) and point reverse proxies at `API_PORT`. Route
paths are unchanged.

## Setup

For detailed installation guides:
- Ubuntu: [installation guide](INSTALL.ubuntu.md)

Modules use PostgreSQL as DB. Before running the indexer, setup a PostgreSQL DB (all modules can write into different databases as well as use a single database).

**Build opi-indexer:**
```bash
cargo build --release --locked -p opi-indexer
```

**Setup the .env file and DBs**

Copy `.env_sample` to `.env` in the repository root and fill it in, as described in [Configuration](#configuration) and [API configuration](#api-configuration). The BRC-20, Bitmap, SNS and Runes databases are created from the schema on first start, and `OPI_MODULES` selects the modules to run. `opi-indexer brc20 reset` deletes all BRC-20 data and starts fresh.

# Run

**Main Meta-Protocol Indexer / BRC-20, Bitmap, SNS and Runes Indexers**

With the `brc20` module, set `BRC20_PROG_DB_PATH`; the brc20-prog engine runs inside `opi-indexer` and needs no separate server.

```bash
target/release/opi-indexer run
```

**API**
```bash
target/release/opi-indexer api
```

# Tests

```bash
SQLX_OFFLINE=true cargo test --workspace
```

The `opi-api` crate's database tests run when `OPI_API_TEST_DATABASE_URL`
(brc20) and `OPI_API_TEST_BITMAP_URL`, `OPI_API_TEST_SNS_URL` and
`OPI_API_TEST_RUNES_URL` name indexed databases, and are skipped otherwise.

## Developer note: sqlx offline data

`opi-api` and `opi-brc20` check their SQL at compile time with sqlx macros. The
committed `.sqlx` directory of each crate lets `SQLX_OFFLINE=true cargo build`
work without a database. After changing a query, regenerate that crate's data
with `cargo sqlx prepare` (sqlx-cli 0.9) against a database that holds the
schema, then verify it with `cargo sqlx prepare --check`. The `DATABASE_URL`
needs an explicit user.

For `opi-api`, build a throwaway database from the four schemas plus the two
tables the API reads that no `db_init.sql` creates (`brc20_unused_tx_inscrs`
and `brc20_extras_block_hashes`), whose DDL goes in a local file
`api_extra_tables.sql`:

```sql
CREATE TABLE public.brc20_unused_tx_inscrs (
	id bigserial NOT NULL, inscription_id text NOT NULL, tick varchar(4) NOT NULL,
	amount numeric(40) NOT NULL, current_holder_pkscript text NOT NULL,
	current_holder_wallet text NULL, event_id int8 NOT NULL, block_height int4 NOT NULL,
	CONSTRAINT brc20_unused_tx_inscrs_pk PRIMARY KEY (id));
CREATE TABLE public.brc20_extras_block_hashes (
	id bigserial NOT NULL, block_height int4 NOT NULL, block_hash text NOT NULL,
	CONSTRAINT brc20_extras_block_hashes_pk PRIMARY KEY (id));
```

```bash
createdb -h /tmp opi_api_sqlx_prepare
for f in crates/opi-brc20/src/database/sql/db_init.sql crates/opi-bitmap/src/db_init.sql \
         crates/opi-sns/src/db_init.sql crates/opi-runes/src/db_init.sql api_extra_tables.sql; do
  psql -h /tmp -d opi_api_sqlx_prepare -X -q -v ON_ERROR_STOP=1 -f "$f"
done
cd crates/opi-api
export DATABASE_URL='postgres://<user>@localhost/opi_api_sqlx_prepare?host=/tmp'
cargo sqlx prepare
cargo sqlx prepare --check
cd ../.. && dropdb -h /tmp opi_api_sqlx_prepare
```

For `opi-brc20`, create a database from `crates/opi-brc20/src/database/sql/db_init.sql` the same way and run `cargo sqlx prepare` and `cargo sqlx prepare --check` in `crates/opi-brc20`.

The parity tests in `tools/parity/tests` compare the modules with the Python reference implementations. They need a local Postgres:

```bash
OPI_TEST_PG=1 uv run --with 'psycopg[binary]' --with pytest --with json5 python -m pytest tools/parity/tests
```

`tools/parity/api_compare.py` replays every API route, bad-parameter cases and request quirks against two running APIs by base URL, for example a release of the API and a new build, and diffs status, headers and body (any difference gives exit code 1). It needs only the Python standard library. Run it on databases nobody is writing to:

```bash
python3 tools/parity/api_compare.py --pair js-rust \
  --js http://127.0.0.1:18200 --rust http://127.0.0.1:18201
```

`--js` names the reference and `--rust` the candidate. `--rate-limit` replays the rate-limit sequence on two freshly started servers; the script's docstring lists the remaining options.

`tools/parity/brc20_hash_compare.py --a <postgres url> --b <postgres url>` compares the event hash, cumulative event hash, trace hash and cumulative trace hash of two brc20 databases at every common height (exit 0 equal, 1 difference, 2 usage or database error).

# Update

- Stop all indexers and apis (preferably starting from main indexer but actually the order shouldn't matter)
- Update the repo (`git pull`)
- Recompile opi-indexer (`cargo build --release --locked -p opi-indexer`)
- Re-run the indexer and the api

## ord dependency

`ord` (`=0.29.1`) and `ordinals` (`=0.0.17`) come from crates.io. The tests'
mock bitcoind, `mockcore`, is not published, so it is a git dev-dependency on
the matching ord tag (`https://github.com/ordinals/ord`, tag `0.29.1`). mockcore
depends on ord by path inside that repository; the workspace's
`[patch."https://github.com/ordinals/ord"]` resolves that ord to the crates.io
release, so the tree holds one `ord`. An ord bump moves the crates.io versions,
the mockcore tag and the patch together.

`redb` is pinned to `=3.1.3`, the version ord 0.29.1's own lockfile holds, so
the journal and ord's index use the redb release ord is tested with. `bitcoin`
resolves to 0.32.102, also ord's locked version. `cargo tree -d` shows one copy
of each of `ord`, `ordinals`, `bitcoin` and `redb`.

An ord index has a schema version: ord 0.29.1 (schema 34) refuses an index
written by an older ord, and redb 3 refuses a file written in redb 2's format.
An ord data dir or journal written by ord 0.23.2 (schema 30) or by redb 2 does
not open; delete both and index again. A new ord release with a new schema
likewise needs a fresh ord data dir and journal. A journal that has finalized
blocks but no rune tables does not open; delete it and the ord data dir and
index again.

## Dependency notes

Dependencies track their latest releases, except:

- `brc20-prog` (in `crates/opi-brc20`) stays within
  `BRC20_PROG_VERSION_REQUIREMENT` (`~0.16.0`); `opi-indexer` links
  brc20-prog as a library (`crates/opi-indexer` pins the same release).
- `redb` stays at `=3.1.3`, ord's locked version (see above), although redb 4
  is released.
- `bitcoin` follows ord's lock (0.32.102) rather than a 0.33 pre-release.

`reqwest` 0.13 uses rustls (aws-lc-rs with the platform certificate verifier)
as its default TLS backend; ord pulls the same reqwest with
default features. opi-brc20's `sqlx` also uses rustls (`tls-rustls`), so
neither binary links native-tls.

## Upstream assumptions

The crash-safety design rests on four properties of ord. Each ord bump re-checks
them against the new release's source; ord 0.29.1 holds all four.

**A1: every commit follows a `getblockchaininfo` barrier.** In
`Updater::update_index` (`src/index/updater.rs`), each block is followed by:

```rust
uncommitted += 1;

if uncommitted == self.index.settings.commit_interval()
  || (!self.index.settings.integration_test()
    && Reorg::is_savepoint_required(self.index, self.height)?)
{
  self.commit(wtx, utxo_cache)?;
```

`--commit-interval 4294967295` makes the first term false, so
`Reorg::is_savepoint_required` runs, and it calls
`index.client.get_blockchain_info()` (`src/index/reorg.rs`) on the updater
thread before any commit decision. It returns early without the call only when
the index durability is `Durability::None`. The only other commit is `if
uncommitted > 0 { self.commit(wtx, utxo_cache)?; }` after the loop, which
follows the last block's barrier. The gate proxy (`opi-source/src/proxy.rs`)
holds that call until the stager (`opi-source/src/stager.rs`) has synced the
raw log.

**A2: `getblockhash` error -8 ends the block stream.** The fetcher thread's
`get_block_with_retries` maps `client.get_block_hash(height).into_option()`
through `BitcoinCoreRpcResultExt::into_option` (`src/index.rs`), which turns
`RpcError { code: -8, .. }` into `Ok(None)`, and `Ok(None) => break` ends the
stream. The proxy answers -8 above the chunk end.

**A3: event shapes and order.** `src/index/event.rs` defines the `Event` enum
that `RawEvent`'s exhaustive `From<Event>` (`opi-source/src/raw_event.rs`)
mirrors, so a changed variant fails to compile. Finalize
(`opi-source/src/journal/finalize.rs`) relies on emission order within a block
(transactions in block order, coinbase last; fee-bound inscriptions emitted at
coinbase time) and on `InscriptionCreated.parent_inscription_ids` holding the
parents ord accepted. In 0.29.1 that list skips a purported parent that has no
inscription entry yet; 0.23.2 panics on such a parent, so no block indexed by
the fork carries one.

**A4: durable, non-test settings.** ord opens its index with
`Durability::Immediate` outside its own unit tests, and `integration_test`
defaults to false. `ord_settings` passes `--commit-interval 4294967295`,
passes `--index-runes` when runes is enabled, and never sets
`--integration-test`. `ORD_*` environment variables and an `ord.yaml` in the
data dir are merged by `Settings::load`, so `spawn_source` checks the merged
settings (`check_ord_settings` in `opi-source/src/ord_settings.rs`) and refuses
to start when `integration_test` is on, the commit interval differs, or rune
indexing differs from the runes switch. ord stores its rune setting in the
index's statistics when it creates the index and reads it back on every open,
ignoring the flag, so `check_ord_index` compares `Index::has_rune_index` with
the switch after the open.

**Inscription rules.** The journal reproduces the fork's numbering and transfer
rules, which are built on ord 0.23.2. Between 0.23.2 and 0.29.1 ord changes no
curse, charm, numbering, pointer or fee rule. Envelope parsing recognizes tag
19 (`property_encoding`, odd, so it never curses), and inscription creation
adds gallery and collection bookkeeping that emits no events.

## SNS indexing rules

The sns module (`crates/opi-sns`) indexes Sats Names by the written spec,
[index-names](https://docs.satsnames.org/sats-names/sns-spec/index-names.md) and
[mint-names](https://docs.satsnames.org/sats-names/sns-spec/mint-names). Where
the spec leaves a choice open, the rule below states the one opi-sns makes
(marked *decision*).

1. **Inscriptions.** Creations only, in ascending inscription number. An
   inscription counts when it is blessed (number >= 0, *decision*) and its
   content type is `text/plain` or `application/json`, alone or followed by `;`
   parameters (*decision*). The content type is matched byte for byte and is
   case-sensitive (*decision*).
2. **UTF-8.** Content that is not valid UTF-8 registers nothing.
3. **Names.** The content is parsed as JSON5.
   - If it does not parse, the whole text is the candidate name.
   - If it parses, the value must be an object with `p == "sns"`,
     `op == "reg"` and a string `name`, which is the candidate. Any other value
     registers nothing, so `1.5`, `"x.sats"` and `true` register nothing
     (*decision*: no raw-text fallback for parsed values).
   - The candidate is lowercased and reduced to its first whitespace-separated
     token: leading whitespace is trimmed, then the text is cut at the next
     whitespace, so `" alice.sats"` registers `alice.sats` (*decision*: the
     spec's trim step only does anything if the cut skips leading whitespace).
   - An escaped line break or other whitespace inside a JSON `name`
     (`"\nalice.sats"`) is whitespace, and is trimmed or cuts like any other
     (*decision*). A raw line break inside a string fails JSON5 parsing (rule
     7).
   - It must contain exactly one `.`, with text on both sides (*decision*:
     both parts non-empty). The part after the `.` is the namespace, stored as
     `domain`.
4. **Namespaces.** Only a JSON5 object with `p == "sns"`, `op == "ns"` and a
   string `ns`. It is lowercased and must be non-empty, without `.` and without
   whitespace (*decision*). When one inscription yields both a name and a
   namespace, the name wins.
5. **Whitespace** is the Unicode `White_Space` property (*decision*: the spec
   says "whitespace or newline").
6. **Unicode version.** Lowercasing (the full mapping, with final sigma) and
   `White_Space` come from committed Unicode 17.0.0 tables
   (`crates/opi-sns/src/unicode_tables.rs`), so results do not depend on the
   Rust toolchain (*decision*). `tools/parity/unicode_tables.py` regenerates
   them with Python 3.15.
7. **JSON5 grammar.** JSON5 1.0 as Python's json5 0.12.0 implements it
   (*decision*), with these details:
   - nesting deeper than 128 arrays or objects is a parse failure; the depth
     counts every value in the text, including one a repeated key later
     replaces;
   - raw line breaks inside strings are a parse failure;
   - trailing commas are allowed;
   - a repeated key keeps its first position and its last value;
   - a number that does not convert is a parse failure: a sign before hex, a
     fraction or exponent without digits, or an integer of more than 4300
     digits.

   `crates/opi-sns/src/json5.rs` lists the full grammar.
8. **Surrogates.** JSON5 strings are ES5 strings, so a `\u` high surrogate
   directly followed by a `\u` low surrogate is one character (`\uD83D`
   followed by `\uDE00` is 😀); a `\` line continuation between the two
   halves does not break the pair. A name or namespace that holds any other
   surrogate (unpaired, reversed, or followed by a non-surrogate) is skipped
   (*decision*).
9. **Storage limits.** A name or namespace that holds `\0`, or is longer than
   2048 UTF-8 bytes after lowercasing, is skipped (*decision*: Postgres `text`
   cannot store `\0`).
10. **First is first.** The first inscription of a name or namespace
    registers it. Later ones insert nothing and emit no event. Events are
    `register;<inscription_id>;<name>;<namespace>` and
    `ns_register;<inscription_id>;<namespace>`, joined with `|`. Each block
    hashes them with sha256 and chains the hash onto the previous block's
    cumulative hash.
11. **Versions.** Reports send `opi-sns-names-full-node v3.0.0` with
    `db_version` 5. A database with another `db_version` is refused and must
    be indexed from scratch.

`tools/parity/sns_spec_vectors.py` implements these rules independently in
Python, and `crates/opi-sns/tests/spec_vectors.json` holds its results. The
module's tests require the Rust rules to match every vector.
