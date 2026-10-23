# opi-indexer

Rust workspace that links upstream `ord` 0.23.2 as a library. The `opi-indexer`
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
A brc20 error restarts brc20 after a backoff of 5 seconds, doubling to 5
minutes; the source keeps running. Log verbosity follows `RUST_LOG`.

## Validation runbook

Status: none of the runs below has been performed yet. The `validate`
subcommand is implemented and unit-tested; the signet and mainnet gates are
still to do, and cutover must wait for them.

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

`ord` is a git dependency (`https://github.com/ordinals/ord`, tag `0.23.2`), the
same source the `mockcore` dev-dependency comes from. With the crates.io `ord`
alongside the git `mockcore`, Cargo builds two copies of `ord` and `ordinals`, so
their types do not unify. A single git source gives one copy of each.

`Cargo.lock` is seeded from ord 0.23.2's own lockfile: ord requires `bip322 =
0.0.9`, which crates.io has yanked, and Cargo only keeps a yanked version that is
already pinned in the lockfile. Keep that pin when updating the lockfile.

The brc20 path dependency needs `axum-server` 0.8 and `bitcoin` 0.32.8 alongside
ord's `axum-server` 0.7: the lockfile holds `axum-server` 0.7.3 and `bitcoin`
0.32.8 for them to build together. `bitcoin` 0.32.102 breaks ord's
`satscard.rs`, so `cargo update` must not move it past 0.32.8.
