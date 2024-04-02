# OPI - Open Protocol Indexer

Open Protocol Indexer, OPI, is the **best-in-slot open-source indexing client** for **meta-protocols** on Bitcoin.
OPI uses a fork of **ord 0.14.0** with minimal changes to maintain compatibility with base layer rules. Also, OPI is built with **modularity** in mind. The main indexer indexes all text/json inscriptions and modules can extend it with different meta-protocols.
All modules in OPI have been built with **reorg protection**.

Currently OPI has modules for **BRC-20**, **Bitmap** and **SNS**, we'll add new modules over time. Pull Requests are welcomed for other meta-protocols.

## Main Meta-Protocol Indexer

**Meta-Protocol indexer** sits in the core of OPI. It indexes **all json/text inscriptions** and their **first 2 transfers**.
Transfer limit can be changed via `INDEX_TX_LIMIT` variable in ord fork. This limit has been added since there are some UTXO's with a lot of inscription content and their movement floods transfers tables. Also, base indexing of most protocols only needs the first two transfers. BRC-20 becomes invalid after 2 hops, bitmap and SNS validity is calculated at inscription time.

## BRC-20 Indexer / API

**BRC-20 Indexer** is the first module of OPI. It follows the official protocol rules hosted [here](https://layer1.gitbook.io/layer1-foundation/protocols/brc-20/indexing). BRC-20 Indexer saves all historical balance changes and all BRC-20 events.

In addition to indexing all events, it also calculates a block hash and cumulative hash of all events for easier db comparison. Here's the pseudocode for hash calculation:
```python
## Calculation starts at block 767430 which is the first inscription block

EVENT_SEPARATOR = '|'
## max_supply, limit_per_mint, amount decimal count is the same as ticker's decimals (no trailing dot if decimals is 0)
## ticker_lowercase = lower(ticker)
## ticker_original is the ticker on inscription
for event in block_events:
  if event is 'deploy-inscribe':
    block_str += 'deploy-inscribe;<inscr_id>;<deployer_pkscript>;<ticker_lowercase>;<ticker_original>;<max_supply>;<decimals>;<limit_per_mint>;<is_self_mint("true" or "false")>' + EVENT_SEPARATOR
  if event is 'mint-inscribe':
    block_str += 'mint-inscribe;<inscr_id>;<minter_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>;<parent_id("" if null)>' + EVENT_SEPARATOR
  if event is 'transfer-inscribe':
    block_str += 'transfer-inscribe;<inscr_id>;<source_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>' + EVENT_SEPARATOR
  if event is 'transfer-transfer':
    ## if sent as fee, sent_pkscript is empty
    block_str += 'transfer-transfer;<inscr_id>;<source_pkscript>;<sent_pkscript>;<ticker_lowercase>;<ticker_original>;<amount>' + EVENT_SEPARATOR

if block_str.last is EVENT_SEPARATOR: block_str.remove_last()
block_hash = sha256_hex(block_str)
## for first block last_cumulative_hash is empty
cumulative_hash = sha256_hex(last_cumulative_hash + block_hash)
```

There is an optional block event hash reporting system pointed at https://api.opi.network/report_block. If you want to exclude your node from this, just change `REPORT_TO_INDEXER` variable in `brc20_index/.env`.
Also change `REPORT_NAME` to differentiate your node from others.

**BRC-20 API** exposes activity on block (block events), balance of a wallet at the start of a given height, current balance of a wallet, block hash and cumulative hash at a given block and hash of all current balances.

## Bitmap Indexer / API

**Bitmap Indexer** is the second module of OPI. It follows the official protocol rules hosted [here](https://gitbook.bitmap.land/ruleset/district-ruleset). Bitmap Indexer saves all bitmap-number inscription-id pairs.

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

**Bitmap API** exposes block hash and cumulative hash at a given block, hash of all bitmaps and inscription_id of a given bitmap.

## SNS Indexer / API

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

**SNS API** exposes block hash and cumulative hash at a given block, hash of all registered names, id number and domain of a given name, id number and name tuples of a domain, and all registered namespaces endpoints.

# Setup

For detailed installation guides:
- Ubuntu: [installation guide](INSTALL.ubuntu.md)
- Windows: [installation guide](INSTALL.windows.md)

OPI uses PostgreSQL as DB. Before running the indexer, setup a PostgreSQL DB (all modules can write into different databases as well as use a single database).

**Build ord:**
```bash
cd ord; cargo build --release;
```

**Install node modules**
```bash
cd modules/main_index; npm install;
cd ../brc20_api; npm install;
cd ../bitmap_api; npm install;
```
*Optional:*
Remove the following from `modules/main_index/node_modules/bitcoinjs-lib/src/payments/p2tr.js`
```js
if (pubkey && pubkey.length) {
  if (!(0, ecc_lib_1.getEccLib)().isXOnlyPoint(pubkey))
    throw new TypeError('Invalid pubkey for p2tr');
}
```
Otherwise, it cannot decode some addresses such as `512057cd4cfa03f27f7b18c2fe45fe2c2e0f7b5ccb034af4dec098977c28562be7a2`

**Install python libraries**
```bash
pip3 install python-dotenv;
pip3 install psycopg2-binary;
python3 -m pip install json5 stdiomask;
```

**Setup .env files and DBs**

Run `reset_init.py` in each module folder (preferrably start from main_index) to initialise .env file, databases and set other necessary files.

# (Optional) Restore from an online backup for faster initial sync

1) Install dependencies: (pbzip2 is optional but greatly impoves decompress speed)

```bash
sudo apt update
sudo apt install postgresql-client-common
sudo apt install postgresql-client-14
sudo apt install pbzip2

python3 -m pip install boto3
python3 -m pip install tqdm
```

2) Run `restore.py`

```bash
cd modules/;
python3 restore.py;
```

# Run

**Main Meta-Protocol Indexer**
```bash
cd modules/main_index;
node index.js;
```

**BRC-20 Indexer**
```bash
cd modules/brc20_index;
python3 brc20_index.py;
```

**BRC-20 API**
```bash
cd modules/brc20_api;
node api.js;
```

**Bitmap Indexer**
```bash
cd modules/bitmap_index;
python3 bitmap_index.py;
```

**Bitmap API**
```bash
cd modules/bitmap_api;
node api.js;
```

**SNS Indexer**
```bash
cd modules/sns_index;
python3 sns_index.py;
```

**SNS API**
```bash
cd modules/sns_api;
node api.js;
```

# Update

- Stop all indexers and apis (preferably starting from main indexer but actually the order shouldn't matter)
- Update the repo (`git pull`)
- Recompile ord (`cd ord; cargo build --release;`)
- Re-run all indexers and apis
- If rebuild is needed, you can run `restore.py` for faster initial sync
