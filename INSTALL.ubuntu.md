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

All next shell script groups assumes that you are in OPI folder cloned by above command.

## Installing node modules
```bash
cd api; npm install;
cd ..;
```

## Installing python libraries

**Create a virtual environment and install python libraries**
```bash
sudo apt install python3.12-venv -y;
python3 -m venv .venv;
source .venv/bin/activate;
pip3 install -r requirements.txt;
```

## Build opi-indexer:

opi-indexer runs the BRC-20, Bitmap, SNS and Runes indexers in one process. See the [README](README.md#configuration) for its configuration.

```bash
sudo apt install build-essential libssl-dev pkg-config clang -y;
cargo build --release --locked -p opi-indexer;
```

## Initialise .env configuration and databases

Copy `.env_sample` to `.env` in the OPI folder and fill it in (`OPI_MODULES` selects which of `brc20`, `bitmap`, `sns` and `runes` run). The BRC-20, Bitmap, SNS and Runes databases are created from the schema on first start.

`opi-indexer` and the API read this one `.env`. `python3 api/reset_init.py` asks for the API settings and writes them into it. `crates/opi-brc20/reset_init.py` is an optional helper that writes brc20's settings into the root `.env` and initialises the brc20 database from it; run it from `crates/opi-brc20`.

Each enabled module needs its database name (`DB_DATABASE`, `BITMAP_DB_DATABASE`, `SNS_DB_DATABASE`, `RUNES_DB_DATABASE`); the start fails when it is unset. One `REPORT_NAME` names the node for all modules.

# Run

Postgres will auto run on system start. \
Bitcoind needs to be run with `-txindex=1` flag before running main indexer.

**Main Meta-Protocol Indexer (BRC-20, Bitmap, SNS and Runes)**

> [!NOTE]
> With the `brc20` module, the BRC2.0 engine (brc20-prog) runs inside `opi-indexer`; there is no separate server to install. Set `BRC20_PROG_DB_PATH` in `.env` to a writable directory. `opi-indexer run` serves the engine's public JSON-RPC on `BRC20_PROG_RPC_SERVER_URL` (default `127.0.0.1:18545`).
>
> The engine reaches bitcoind through the Bitcoin RPC proxy when `BITCOIN_RPC_PROXY_SERVER_ENABLED=true` (the code default is `false`; `.env_sample` sets `true`). The proxy authenticates to bitcoind with `BITCOIN_RPC_USER` and `BITCOIN_RPC_PASSWD`, or with `BITCOIN_RPC_COOKIE`, which it re-reads on every request. With the proxy off the engine uses `BITCOIN_RPC_URL` with `BITCOIN_RPC_USER` and `BITCOIN_RPC_PASSWD`, which are then required. Light mode needs the proxy on. The network follows `NETWORK_TYPE`.
>
> The public JSON-RPC takes no credentials; keep it on loopback or firewall it. A panic in brc20 or the engine stops the process, so run `opi-indexer` under systemd (or another supervisor) that restarts it.
>
> A node that ran the standalone brc20-prog server stops it and points `BRC20_PROG_DB_PATH` at its database.

```bash
./target/release/opi-indexer run;
```

**API**

This is an optional API and doesn't need to be run. One process serves every module in `OPI_MODULES` on `API_PORT` (default 8000).

```bash
cd api;
node server.js;
```

# Update

- Stop the indexer and the api (preferably starting from main indexer but actually the order shouldn't matter)
- Update the repo (`git pull`)
- Recompile opi-indexer (`cargo build --release --locked -p opi-indexer;`)
- Re-run the indexer and the api
