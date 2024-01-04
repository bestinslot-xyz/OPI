# Detailed Installation Guide for OPI on Ubuntu 22.04

## Installing & Running bitcoind

```bash
sudo apt update
sudo apt install snapd
snap install bitcoin-core

## if you want to use a mounted media as chain folder:
snap connect bitcoin-core:removable-media

## create a folder for bitcoin chain
mkdir /mnt/HC_Volume/bitcoin_chain
## run bitcoind using the new folder
bitcoin-core.daemon -txindex=1 -datadir="/mnt/HC_Volume/bitcoin_chain" -rest
```

## Installing PostgreSQL

1) First install and run postgresql binaries.

```bash
sudo apt update
sudo apt install postgresql postgresql-contrib
sudo systemctl start postgresql.service
```

2) *(Optional)*, I'll usually mark postgres on hold since apt will try to auto update postgres which will restart its process and close all active connections.

```bash
apt-mark hold postgresql postgresql-14 postgresql-client-14 postgresql-client-common postgresql-common postgresql-contrib
```

3) Set a password for postgresql user.

```bash
sudo -u postgres psql
```
```SQL
ALTER USER postgres WITH PASSWORD '********';
\q
```

4) *(Optional)*, if you want to connect to DB instance remotely (if postgres is not installed on your local PC) you need to configure pg_hba.conf file.

```bash
nano /etc/postgresql/14/main/pg_hba.conf
```
```
## add the following line to the end of the file, change ip_address_of_your_pc with real IP
hostssl all             all             <ip_address_of_your_pc>/32       scram-sha-256
```

To reload the new configuration:

```bash
sudo -u postgres psql
```
```SQL
SELECT pg_reload_conf();
\q
```

5) *(Optional)*, some configuration changes:

```bash
nano /etc/postgresql/14/main/postgresql.conf
```
```
listen_addresses = '*'
max_connections = 2000
```
```bash
sudo systemctl restart postgresql
```


## Installing NodeJS

These steps are following the guide at [here](https://github.com/nodesource/distributions).

```bash
sudo apt-get update
sudo apt-get install -y ca-certificates curl gnupg
sudo mkdir -p /etc/apt/keyrings
curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | sudo gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg

NODE_MAJOR=20
echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_$NODE_MAJOR.x nodistro main" | sudo tee /etc/apt/sources.list.d/nodesource.list

sudo apt-get update
sudo apt-get install nodejs -y
```

## Installing Cargo & Rust

These steps are following the guide at [here](https://doc.rust-lang.org/cargo/getting-started/installation.html).

```bash
curl https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
```

To update cargo & rust:

```bash
rustup update stable
```

## Installing node modules
```bash
cd modules/main_index; npm install;
cd ../brc20_api; npm install;
cd ../bitmap_api; npm install;
```
*(Optional):*
Remove the following from `modules/main_index/node_modules/bitcoinjs-lib/src/payments/p2tr.js`
```js
if (pubkey && pubkey.length) {
  if (!(0, ecc_lib_1.getEccLib)().isXOnlyPoint(pubkey))
    throw new TypeError('Invalid pubkey for p2tr');
}
```
Otherwise, it cannot decode some addresses such as `512057cd4cfa03f27f7b18c2fe45fe2c2e0f7b5ccb034af4dec098977c28562be7a2`

## Installing python libraries

If you don't have pip installed, start by installing pip. [guide](https://pip.pypa.io/en/stable/installation/).

```bash
wget https://bootstrap.pypa.io/get-pip.py
python3 get-pip.py
rm get-pip.py
```

```bash
python3 -m pip install python-dotenv;
python3 -m pip install psycopg2-binary;
```

## Build ord:

```bash
sudo apt install build-essential;
cd ord; cargo build --release;
```

**Do not run ord binary directly. Main indexer will run ord periodically**

## Setup .env files

Copy `.env_sample` in main_index, brc20_index, brc20_api, bitmap_index and bitmap_api as `.env` and fill necessary information.

- Do not change `FIRST_INSCRIPTION_HEIGHT` if you want to report hashes, since cumulative hash calculation will start from this height and it'll be faulty if you change this variable.
- All scripts can use the same database. In sample env files, we used different `DB_DATABASE` but using postgres on all of them will also work correctly.
- `BITCOIN_CHAIN_FOLDER` is the datadir folder that is set when starting bitcoind.
- `ORD_BINARY` `ORD_FOLDER` and `ORD_DATADIR` can stay the same if you do not change the folder structure after `git clone`.

## Initialise databases

After setting .env files, you can run `reset_init.py` in each indexer folder to initialise databases and set other necessary files.

# Run

Postgres will auto run on system start. \
Bitcoind needs to be run with `-txindex=1` flag before running main indexer. \
**Do not run ord binary directly. Main indexer will run ord periodically**

**Main Meta-Protocol Indexer**
```bash
cd modules/main_index; node index.js;
```

**BRC-20 Indexer**
```bash
cd modules/brc20_index; python3 brc20_index.py;
```

**BRC-20 API**

This is an optional API and doesn't need to be run.

```bash
cd modules/brc20_api; node api.js;
```

**Bitmap Indexer**
```bash
cd modules/bitmap_index; python3 bitmap_index.py;
```

**Bitmap API**

This is an optional API and doesn't need to be run.

```bash
cd modules/bitmap_api; node api.js;
```

# Update

- Stop all indexers and apis (preferably starting from main indexer but actually the order shouldn't matter)
- Update the repo (`git pull`)
- Recompile ord (`cd ord; cargo build --release;`)
- Re-run all indexers and apis