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
sudo apt-mark hold postgresql postgresql-14 postgresql-client-14 postgresql-client-common postgresql-common postgresql-contrib
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

## Cloning the repository

```bash
git clone https://github.com/bestinslot-xyz/OPI.git
```

All next shell script groups assumes that you are in OPI folder cloned by above command.

## Installing node modules
```bash
cd modules/main_index; npm install;
cd ../brc20_api; npm install;
cd ../bitmap_api; npm install;
cd ../pow20_api; npm install;
cd ../sns_api; npm install;
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

1) If you don't have pip installed, start by installing pip. [guide](https://pip.pypa.io/en/stable/installation/).

```bash
wget https://bootstrap.pypa.io/get-pip.py
python3 get-pip.py
rm get-pip.py
```

or

```sh
sudo apt install python3-pip
```

2) Install dependencies

```bash
python3 -m pip install python-dotenv;
python3 -m pip install psycopg2-binary;
python3 -m pip install json5;
python3 -m pip install stdiomask;
python3 -m pip install requests;
```

## Build ord:

```bash
sudo apt install build-essential;
cd ord; cargo build --release;
```

**Do not run ord binary directly. Main indexer will run ord periodically**

## Initialise .env configuration and databases

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

Postgres will auto run on system start. \
Bitcoind needs to be run with `-txindex=1` flag before running main indexer. \
**Do not run ord binary directly. Main indexer will run ord periodically**

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

This is an optional API and doesn't need to be run.

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

This is an optional API and doesn't need to be run.

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

This is an optional API and doesn't need to be run.

```bash
cd modules/sns_api;
node api.js;
```

**POW20 Indexer**
```bash
cd modules/pow20_index;
python3 pow20_index.py;
```

**POW20 API**

This is an optional API and doesn't need to be run.

```bash
cd modules/pow20_api;
node api.js;
```

# Update

- Stop all indexers and apis (preferably starting from main indexer but actually the order shouldn't matter)
- Update the repo (`git pull`)
- Recompile ord (`cd ord; cargo build --release;`)
- Re-run all indexers and apis
- If rebuild is needed, you can run `restore.py` for faster initial sync
