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

To be filled in.

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
