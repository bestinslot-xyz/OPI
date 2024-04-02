# OPI - Open Protocol Indexer - Experimental Runes Module

## Runes Indexer / API

**Runes Indexer** follows the latest version of ord from [ordinals/ord](https://github.com/ordinals/ord). Runes Indexer saves all events such as input to tx, transfer, mint, initial-allocation and burn. It also indexes all rune entries and outpoint to balance entries.

In addition to indexing all events, it also calculates a block hash and cumulative hash of all events for easier db comparison. Here's the pseudocode for hash calculation:
```python
## Calculation starts at first_rune_height from ordinals/ord

## event_type values:
## input
## new_rune_allocation
## mint
## output
## burn

EVENT_SEPARATOR = '|'
for event in block_events:
  ## if event_type is mint, burn or new_rune_allocation, outpoint and pkscript is empty
  block_str += '<event_type>;<outpoint>;<pkscript>;<rune_id>;<amount>' + EVENT_SEPARATOR

if block_str.last is EVENT_SEPARATOR: block_str.remove_last()
block_hash = sha256_hex(block_str)
## for first block last_cumulative_hash is empty
cumulative_hash = sha256_hex(last_cumulative_hash + block_hash)
```

There is an optional block event hash reporting system pointed at https://api.opi.network/report_block. If you want to exclude your node from this, just change `REPORT_TO_INDEXER` variable in `runes_index/.env`.
Also change `REPORT_NAME` to differentiate your node from others.

**Runes API** exposes activity on block (block events) `/v1/runes/activity_on_block`, balance of a wallet at the start of a given height `/v1/runes/balance_on_block`, current balance of a wallet `/v1/runes/get_current_balance_of_wallet`, unspent rune outpoints of a wallet `/v1/runes/get_unspent_rune_outpoints_of_wallet`, holders of a rune `/v1/runes/holders`, block hash and cumulative hash at a given block `/v1/runes/get_hash_of_all_activity`.

# Setup

OPI uses PostgreSQL as DB. Before running the indexer, setup a PostgreSQL DB (all modules can write into different databases as well as use a single database).

**Build ord-runes:**
```bash
cd modules/runes_index/ord-runes; cargo build --release;
```

**Install node modules**
```bash
cd modules/runes_index; npm install;
cd ../runes_api; npm install;
```
*Optional:*
Remove the following from `modules/runes_index/node_modules/bitcoinjs-lib/src/payments/p2tr.js`
```js
if (pubkey && pubkey.length) {
  if (!(0, ecc_lib_1.getEccLib)().isXOnlyPoint(pubkey))
    throw new TypeError('Invalid pubkey for p2tr');
}
```
Otherwise, it cannot decode some addresses such as `512057cd4cfa03f27f7b18c2fe45fe2c2e0f7b5ccb034af4dec098977c28562be7a2`

**Install python libraries**
```bash
python3 -m pip install python-dotenv;
python3 -m pip install psycopg2-binary;
```

**Setup .env files and DBs**

Run `reset_init.py` in runes_index and runes_api to initialise .env file, databases and set other necessary files.

# Run

**Runes Indexer**
```bash
cd modules/runes_index;
node index_runes.js;
```

**Runes API**
```bash
cd modules/runes_api;
node api.js;
```

# Update

- Stop all indexers and apis
- Update the repo (`git pull`)
- Recompile ord (`cd modules/runes_index/ord-runes; cargo build --release;`)
- Re-run all indexers and apis
