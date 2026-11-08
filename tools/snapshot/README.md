# Snapshots

A snapshot is a node's state packed at one block: the `opi-data` volume (ord
index, journal, BRC2.0 engine) and the `postgres-data` volume of
[`docker/compose.yml`](../../docker/compose.yml). Restoring one starts a node
near the chain tip instead of indexing from the start.
<https://snap.opi.network> lists the published snapshots for mainnet and
signet, each with the brc20 cumulative event and trace hashes of its block.

| Kind | Journal | What the restored node runs |
| --- | --- | --- |
| pruned (`<chain>/<height>/`) | the last 2,000 blocks | the snapshot's modules (`modules` in `manifest.json`) from its height onward |
| full (`<chain>/<height>-full/`) | every height | the same, and any module from scratch: runes, inscriptions, or a brc20 rebuild that recomputes every hash |

## Restore

Needs Docker (Compose v2.17 or later), `curl`, `zstd`, a synced Bitcoin Core
with `-txindex`, and disk for the extracted data (about 250 GB on mainnet) plus
the download.

```bash
git clone https://github.com/bestinslot-xyz/OPI.git && cd OPI
cp .env_sample .env
```

In `.env`, set `OPI_CHAIN` and `NETWORK_TYPE`, the bitcoind RPC settings,
`DB_PASSWD`, `OPI_MODULES="brc20,bitmap,sns"`, the bitmap and sns database
settings (`BITMAP_DB_DATABASE="opi_bitmap"`, `SNS_DB_DATABASE="opi_sns"`, with
the same user and password as `DB_USER`), `EVM_RECORD_TRACES="true"` and, for a
pruned snapshot, `OPI_PRUNE="2000"`. Then:

```bash
docker compose -f docker/compose.yml build
tools/snapshot/restore.sh                   # newest pruned snapshot for OPI_CHAIN
docker compose -f docker/compose.yml up -d
```

`OPI_JOURNAL=full tools/snapshot/restore.sh` takes the newest full-journal
snapshot, and `tools/snapshot/restore.sh mainnet/975052/` a given one.

The restore checks every file against `SHA256SUMS`, extracts into the two
volumes, and sets the database passwords from `.env`. Check the node against the
network once it is at the tip:

```bash
H=$(curl -s localhost:8000/v1/brc20/block_height)
curl -s "localhost:8000/v1/brc20/get_hash_of_all_activity?block_height=$H"
curl -s "https://api.opi.network/lc/get_best_hashes_for_block/$H?event_hash_version=3"
```

`cumulative_event_hash` and `cumulative_trace_hash` equal
`best_cumulative_hash` and `best_cumulative_trace_hash`.

## Make one

On a node at the tip, run with `docker/compose.yml`:

```bash
tools/snapshot/snapshot.sh            # writes ./snapshots/<chain>-<height>[-full]/
```

The indexer, the API and postgres stop for the pack. The archives carry the
placeholder database password `opi`, never the node's own, and the node gets its
password back when the stack restarts. The postgres archive is the whole
cluster, so the script refuses to pack one that holds databases outside the
module databases of `.env`, or roles other than `DB_USER` with a password.

## Layout

```
<chain>-<height>[-full]/
  manifest.json      chain, modules, height, brc20 cumulative hashes, journal kind, OPI commit, file sizes and sha256
  SHA256SUMS
  opi-data.tar.zst   opi/{ord,journal,brc20-prog}
  postgres.tar.zst   postgres/pgdata/ (PostgreSQL 16)
```
