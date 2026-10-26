# opi-indexer

Rust workspace that links upstream `ord` 0.29.1 as a library. The `opi-indexer`
binary runs ord as a journal source and the BRC-20 module in one process.

## Prerequisites

- bitcoind with `-txindex`, not pruned. Finalize may fetch `getblock <hash> 3`,
  which needs undo data.
- Postgres for the brc20 module.
- brc20-prog, reachable at `BRC20_PROG_RPC_URL` (its admin listener). The brc20
  module always runs BRC2.0.

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
brc20 runs as a module (`OPERATION_MODE=full`): it indexes each block once the
journal finalizes it, and rolls back when its stored hashes differ from the
journal's. A brc20 error or panic restarts it, with a freshly built indexer and
database state, after a backoff of 5 seconds, doubling to 5 minutes; the source
keeps running. Log verbosity follows `RUST_LOG`.

## Validation runbook

Cut a deployment over to opi-indexer only after `validate` passes on signet and
on mainnet.

### `validate`

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

### Cut-over check

Run signet first, then mainnet. With `opi-indexer run` synced to the network's
best verified block, run `opi-indexer validate`. Expected: `OK <first>..<tip>`.

On a mismatch, the printed height is the first one where brc20 and the network
differ. Rerun with `--from <h> --to <h>` to confirm it is deterministic, then
compare that block's events with the network's. Fix the cause in the owning
component with a regression test, then rebuild brc20's hashes from that height
and run `validate` again.

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
   followed by `\uDE00` is 😀). A name or namespace that holds any other
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
