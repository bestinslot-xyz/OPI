# Detailed Installation Guide for OPI on Ubuntu 22.04

## Installing & Running bitcoind

```bash
sudo apt update
sudo apt install snapd
snap install bitcoin-core

## if you want to use a mounted media as chain folder:
snap connect bitcoin-core:removable-media

## create a folder for bitcoin chain
mkdir -p /mnt/HC_Volume/bitcoin_chain
## run bitcoind using the new folder
bitcoin-core.daemon -txindex=1 -datadir="/mnt/HC_Volume/bitcoin_chain" -rest
```

> [!WARNING]
> If running on Signet, add `-signet` to `bitcoin-core.daemon` command, if you're planning on running BRC2.0 Programmable Module, also enable RPC authentication by adding `-rpcuser=<USER>` and `-rpcpassword=<PASSWORD>`.

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
git clone https://github.com/bestinslot-xyz/OPI.git && cd OPI
```

> [!WARNING]
> If running on Signet with BRC2.0, clone the experimental signet branch by using:
> ```
> git clone -b experimental-signet-brc20-prog https://github.com/bestinslot-xyz/OPI.git && cd OPI
> ```

All next shell script groups assumes that you are in OPI folder cloned by above command.

## Installing node modules
```bash
cd modules/brc20_api; npm install;
cd ../bitmap_api; npm install;
cd ../pow20_api; npm install;
cd ../sns_api; npm install;
```

## Installing python libraries

**Create a virtual environment and install python libraries**
```bash
cd modules;
python3 -m venv .env;
source .env/bin/activate;
pip3 install -r requirements.txt;
```

## Build ord:

```bash
sudo apt install build-essential;
cd ord; cargo build --release;
```

## Initialise .env configuration and databases

Run `reset_init.py` in each module folder (preferrably start from main_index) to initialise .env file, databases and set other necessary files.

# Run

Postgres will auto run on system start. \
Bitcoind needs to be run with `-txindex=1` flag before running main indexer. \
**Here, we run the ord binary directly. Ord also serves an RPC server.**

**Main Meta-Protocol Indexer**
```bash
cd ord/target/release;
ord --data-dir . index run;
```

> [!NOTE]
> For ord to reach the bitcoin rpc server correctly, pass `--bitcoin-rpc-url`, `--bitcoin-rpc-username` and `--bitcoin-rpc-password` parameters before `index run`. To run on signet, add `--signet` as well.

**BRC-20 Indexer**

> [!WARNING]
> If running BRC2.0, set up and run brc20_prog server using the instructions at [bestinslot-xyz/brc20-programmable-module#usage](https://github.com/bestinslot-xyz/brc20-programmable-module#usage) before running `brc20_index.py`.
> 
> BRC2.0 requires setting up the `BITCOIN_RPC_USER` and `BITCOIN_RPC_PASSWORD`, and on Signet, `BITCOIN_RPC_NETWORK=signet`.
> 
> BRC2.0 needs to be running before starting the brc20 indexer. When running `reset_init.py`, enable BRC2.0 by setting the variable to true when asked.

```bash
cd modules/brc20_index_rust;
cargo build --release;
cd target/release;
./brc20-index;
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
