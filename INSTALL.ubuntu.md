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

## Build opi-indexer:

opi-indexer runs the BRC-20, Bitmap, SNS and Runes indexers in one process, and `opi-indexer api` serves the HTTP API. See the [README](README.md#configuration) for its configuration.

```bash
sudo apt install build-essential libssl-dev pkg-config clang -y;
cargo build --release --locked -p opi-indexer;
```

## Initialise .env configuration and databases

Copy `.env_sample` to `.env` in the OPI folder and fill it in (`OPI_MODULES` selects which of `brc20`, `bitmap`, `sns`, `runes` and `inscriptions` run). Create each module's database first (for example `sudo -u postgres createdb opi_brc20`); its schema is created on first start.

`opi-indexer run` and `opi-indexer api` read this one `.env` (the API settings are listed in the README's [API configuration](README.md#api-configuration)).

Each enabled module needs its database name (`DB_DATABASE`, `BITMAP_DB_DATABASE`, `SNS_DB_DATABASE`, `RUNES_DB_DATABASE`, `INSCRIPTIONS_DB_DATABASE`); the start fails when it is unset. One `REPORT_NAME` names the node for all modules.

# Run

Postgres will auto run on system start. \
Bitcoind needs to be run with `-txindex=1` flag before running main indexer.

**Main Meta-Protocol Indexer (BRC-20, Bitmap, SNS and Runes)**

> [!NOTE]
> With the `brc20` module, the BRC2.0 engine (brc20-prog) runs inside `opi-indexer`; there is no separate server to install. Set `BRC20_PROG_DB_PATH` in `.env` to a writable directory. `opi-indexer run` serves the engine's public JSON-RPC on `BRC20_PROG_RPC_SERVER_URL` (default `127.0.0.1:18545`).
>
> The engine gets its Bitcoin data in process, through the indexer: from the Bitcoin RPC cache when `BITCOIN_RPC_CACHE_ENABLED=true`, and in full mode from bitcoind at `BITCOIN_RPC_URL` with `BITCOIN_RPC_USER` and `BITCOIN_RPC_PASSWD`, or `BITCOIN_RPC_COOKIE` (re-read on every request). Full mode requires one of the two credentials. Light mode needs no bitcoind and no Bitcoin credentials, and requires `BITCOIN_RPC_CACHE_ENABLED=true`. The network follows `NETWORK_TYPE`. For keys left over from older setups, see the BRC2.0 engine section of the README.
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
./target/release/opi-indexer api;
```

Give the API a read-only database role (see the README's [API](README.md#api) section).

# Update

- Stop the indexer and the api (preferably starting from main indexer but actually the order shouldn't matter)
- Update the repo (`git pull`)
- Recompile opi-indexer (`cargo build --release --locked -p opi-indexer;`)
- Re-run the indexer and the api
