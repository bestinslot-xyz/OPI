# opi-indexer

Rust workspace that links upstream `ord` 0.29.1 as a library. The `opi-indexer`
binary runs ord as a journal source and the BRC-20 module in one process.

## Prerequisites

- bitcoind with `-txindex`, not pruned. Finalize may fetch `getblock <hash> 3`,
  which needs undo data.
- Postgres for the brc20 module.
- brc20-prog when BRC2.0 is enabled (`BRC20_PROG_ENABLED`).

## Configuration

Copy `.env_sample` to `.env` (loaded from the working directory) or export the
variables.

| Variable | Meaning |
| --- | --- |
| `OPI_CHAIN` | `mainnet`, `signet`, `testnet4` or `regtest` |
| `OPI_ORD_DATA_DIR` | directory of ord's index database |
| `OPI_JOURNAL_DIR` | directory of the journal |
| `BITCOIN_RPC_URL` | bitcoind RPC URL |
| `BITCOIN_RPC_USER`, `BITCOIN_RPC_PASSWD` | bitcoind RPC credentials |
| `BITCOIN_RPC_COOKIE` | bitcoind cookie file, instead of user and password |
| `OPI_CHUNK` | blocks per ord update, default `1000` |
| `OPI_MODULES` | comma-separated modules, default `brc20` |

brc20's own variables (`DB_*`, `NETWORK_TYPE`, `BRC20_PROG_*`, `REPORT_*`) are
listed in `modules/brc20_index/.env_sample`. `NETWORK_TYPE` must match
`OPI_CHAIN`.

## Build

```
cargo build --release --locked -p opi-indexer
```

## Run

```
target/release/opi-indexer run
```

`run` is the default subcommand. SIGINT and SIGTERM close the source gate, stop
brc20, join the source and exit 0. A source error is logged and exits non-zero.
brc20 starts once the journal has its first finalized block. A brc20 error
restarts brc20, with a freshly built indexer and database state, after a
backoff of 5 seconds, doubling to 5 minutes; the source keeps running. Log verbosity follows `RUST_LOG`.

## Validation runbook

Cut a deployment over to opi-indexer only after it passes the signet and
mainnet gates below.

### `validate`

```
opi-indexer validate --db-reader-url <url> --from <height> --to <height>
```

`validate` reads `OPI_CHAIN`, `OPI_JOURNAL_DIR` and the other variables from the
same environment as `run`, opens the journal itself and compares, for every
height in `from..=to`:

- BRC-20: `JournalBrc20Source::get_transfers` against `getBlockBRC20Txes`.
- Bitmap and SNS: journal creations filtered by `is_valid_bitmap` /
  `is_valid_sns` (the filters copied from `db_reader`) against
  `getBlockBitmapInscrs` / `getBlockSNSInscrs`. Bitmap is sorted by
  inscription number.
- Headers: the journal's block hash and time against `getBlockHashAndTs`.

Equality is on canonical JSON. The one accepted difference is `new_satpoint` of
an entry with `sent_as_fee` whose `db_reader` satpoint sits at outpoint
`0000000000000000000000000000000000000000000000000000000000000000:4294967295`;
the journal carries upstream ord's lost-sats position there.

It prints `OK <from>..<to>` and exits 0 when every height matches. On the first
divergence it prints the height, the method, and both values (journal and
`db_reader`) pretty-printed, and exits 1. redb locks the journal file to one
process, so `validate` exits 1 with `stop opi-indexer before validating` while
`opi-indexer` or another `validate` holds it. Use `RUST_LOG=info` for progress
every 1000 heights.

### Gate 11.1: journal against `db_reader`

Run signet first, then mainnet.

1. Sync the old fork's index (with its `db_reader` serving RPC) and
   `opi-indexer` against the same bitcoind. Both stacks must be at the same
   chain; the common tip is the lower of the two tips.
2. Stop `opi-indexer` (SIGTERM; wait for exit 0).
3. Run `opi-indexer validate --db-reader-url http://<db_reader>:<port> --from
   <first> --to <common tip>`.
   - signet: `<first>` is the first inscription height on signet.
   - mainnet: `<first>` is `767430`.
   Expected: `OK <first>..<tip>`.
4. Restart `opi-indexer`.

On a divergence, bisect: the failing height is in the output. Rerun with
`--from <h> --to <h>` to confirm it is deterministic, then compare the printed
journal and `db_reader` values field by field. The method name says which side
of the pipeline to read: `getBlockBRC20Txes` points at the journal's transfer
rows and `JournalBrc20Source`, the Bitmap/SNS methods at the creation filter
and inscription info, `getBlockHashAndTs` at the header table. If the cause is
not obvious, binary-search with `--from`/`--to` over earlier ranges to confirm
nothing before the reported height differs, and inspect the journal row for the
height with the opi-source journal reader. Fix the cause in the owning
component with a regression test, then rerun from the divergent height. Every
run requires `opi-indexer` stopped.

### Gate 11.3: BRC-20 event hashes (mainnet)

With brc20 on `opi-indexer` synced to the tip, and a second brc20 node fed by
the fork's `db_reader` synced over the same range:

1. Compare `brc20_cumulative_event_hashes` (`block_height`,
   `block_event_hash`, `cumulative_event_hash`) at every height between the two
   databases, with SQL over both (for example `psql` against each database,
   dumping `SELECT block_height, block_event_hash, cumulative_event_hash FROM
   brc20_cumulative_event_hashes ORDER BY block_height` and diffing the
   outputs, or a `dblink` / `postgres_fdw` join that selects rows where the
   hashes differ). Expected: identical at every height.
2. Compare the same hashes with the ones the OPI network reports for those
   heights. Expected: identical at every height.

The first height where they differ is the starting point for a bisect with
gate 11.1.

### Signet smoke run

Run `opi-indexer` on signet with a fresh brc20 database for at least 1,000
blocks past the first BRC-20 height. Expected: brc20's current block height
advances and no errors are logged.

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
likewise needs a fresh ord data dir and journal.

## Dependency notes

Dependencies track their latest releases, except:

- `brc20-prog` (in `modules/brc20_index`) stays within
  `BRC20_PROG_VERSION_REQUIREMENT` (`~0.16.0`), which must match the brc20-prog
  server the node runs.
- `redb` stays at `=3.1.3`, ord's locked version (see above), although redb 4
  is released.
- `bitcoin` follows ord's lock (0.32.102) rather than a 0.33 pre-release.

`reqwest` 0.13 uses rustls (aws-lc-rs with the platform certificate verifier)
as its default TLS backend, in both workspaces; ord pulls the same reqwest with
default features. brc20-index's `sqlx` also uses rustls (`tls-rustls`), so
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
defaults to false. `ord_settings` passes `--index-runes` and
`--commit-interval 4294967295` and never sets `--integration-test`. `ORD_*`
environment variables and an `ord.yaml` in the data dir are merged by
`Settings::load`, so `spawn_source` checks the merged settings
(`check_ord_settings` in `opi-source/src/ord_settings.rs`) and refuses to start
when `integration_test` is on or the commit interval differs.

**Inscription rules.** The journal reproduces the fork's numbering and transfer
rules, which are built on ord 0.23.2. Between 0.23.2 and 0.29.1 ord changes no
curse, charm, numbering, pointer or fee rule. Envelope parsing recognizes tag
19 (`property_encoding`, odd, so it never curses), and inscription creation
adds gallery and collection bookkeeping that emits no events.
