# pip install python-dotenv
# pip install psycopg2-binary
# pip install stdiomask

import os, psycopg2, pathlib, stdiomask
from dotenv import load_dotenv

init_env = True

# does .env file exist?
if os.path.isfile('.env'):
  res = input("Do you want to re-initialise .env file? (y/n) ")
  if res != 'y':
    init_env = False

if init_env:
  DB_USER="postgres"
  DB_HOST="localhost"
  DB_PORT="5432"
  DB_DATABASE="postgres"
  DB_PASSWD=""
  DB_SSL="true"
  DB_MAX_CONNECTIONS="50"
  BITCOIN_CHAIN_FOLDER="~/.bitcoin/"
  BITCOIN_RPC_URL=""
  COOKIE_FILE=""
  BITCOIN_RPC_USER=""
  BITCOIN_RPC_PASSWD=""
  ORD_BINARY="./ord"
  ORD_FOLDER="../../ord/target/release/"
  ORD_DATADIR="."
  NETWORK_TYPE="mainnet"
  print("Initialising .env file")
  print("leave blank to use default values")
  res = input("Main Postgres DB username (Default: postgres): ")
  if res != '':
    DB_USER = res
  res = input("Main Postgres DB host (Default: localhost) leave default if postgres is installed on the same machine: ")
  if res != '':
    DB_HOST = res
  res = input("Main Postgres DB port (Default: 5432): ")
  if res != '':
    DB_PORT = res
  res = input("Main Postgres DB name (Default: postgres) leave default if no new dbs are created: ")
  if res != '':
    DB_DATABASE = res
  res = stdiomask.getpass("Main Postgres DB password: ")
  DB_PASSWD = res
  res = input("Main Postgres DB use SSL (Default: true) may need to be set to false on Windows machines: ")
  if res != '':
    DB_SSL = res
  res = input("Main Postgres DB max connections (Default: 50): ")
  if res != '':
    DB_MAX_CONNECTIONS = res
  res = input("Bitcoin datadir (Default: ~/.bitcoin/) use forward-slashes(/) even on Windows: ")
  if res != '':
    BITCOIN_CHAIN_FOLDER = res
  res = input("Bitcoin RPC URL (Default: (empty)) leave default to use default localhost bitcoin-rpc: ")
  if res != '':
    BITCOIN_RPC_URL = res
  res = input("Bitcoin RPC cookie file (Default: (empty)) leave default to use .cookie file in bitcoin datadir: ")
  if res != '':
    COOKIE_FILE = res
  res = input("Bitcoin RPC username (Default: (empty)) leave default to use .cookie file: ")
  if res != '':
    BITCOIN_RPC_USER = res
  res = stdiomask.getpass("Bitcoin RPC password (Default: (empty)) leave default to use .cookie file: ")
  if res != '':
    BITCOIN_RPC_PASSWD = res
  res = input("Ord binary command (Default: ./ord) change to ord.exe on Windows (without ./): ")
  if res != '':
    ORD_BINARY = res
  res = input("Path to ord folder (Default: ../../ord/target/release/) leave default if repository folder structure hasn't been changed: ")
  if res != '':
    ORD_FOLDER = res
  res = input("Ord datadir (relative to ord folder) (Default: .) leave default if repository folder structure hasn't been changed: ")
  if res != '':
    ORD_DATADIR = res
  res = input("Network type (Default: mainnet) options: mainnet, testnet, signet, regtest: ")
  if res != '':
    NETWORK_TYPE = res
  f = open(".env", "w")
  f.write("DB_USER=\"" + DB_USER + "\"\n")
  f.write("DB_HOST=\"" + DB_HOST + "\"\n")
  f.write("DB_PORT=\"" + DB_PORT + "\"\n")
  f.write("DB_DATABASE=\"" + DB_DATABASE + "\"\n")
  f.write("DB_PASSWD=\"" + DB_PASSWD + "\"\n")
  f.write("DB_SSL=\"" + DB_SSL + "\"\n")
  f.write("DB_MAX_CONNECTIONS=" + DB_MAX_CONNECTIONS + "\n")
  f.write("BITCOIN_CHAIN_FOLDER=\"" + BITCOIN_CHAIN_FOLDER + "\"\n")
  f.write("BITCOIN_RPC_URL=\"" + BITCOIN_RPC_URL + "\"\n")
  f.write("COOKIE_FILE=\"" + COOKIE_FILE + "\"\n")
  f.write("BITCOIN_RPC_USER=\"" + BITCOIN_RPC_USER + "\"\n")
  f.write("BITCOIN_RPC_PASSWD=\"" + BITCOIN_RPC_PASSWD + "\"\n")
  f.write("ORD_BINARY=\"" + ORD_BINARY + "\"\n")
  f.write("ORD_FOLDER=\"" + ORD_FOLDER + "\"\n")
  f.write("ORD_DATADIR=\"" + ORD_DATADIR + "\"\n")
  f.write("NETWORK_TYPE=\"" + NETWORK_TYPE + "\"\n")
  f.close()

res = input("Are you sure you want to initialise/reset the main database? (y/n) ")
if res != 'y':
  print('aborting')
  exit(1)

load_dotenv()
db_user = os.getenv("DB_USER") or "postgres"
db_host = os.getenv("DB_HOST") or "localhost"
db_port = int(os.getenv("DB_PORT") or "5432")
db_database = os.getenv("DB_DATABASE") or "postgres"
db_password = os.getenv("DB_PASSWD")

network_type = os.getenv("NETWORK_TYPE") or "mainnet"

## connect to db
conn = psycopg2.connect(
  host=db_host,
  port=db_port,
  database=db_database,
  user=db_user,
  password=db_password)
conn.autocommit = True
cur = conn.cursor()

db_exists = False
try:
  cur.execute('select count(*) from block_hashes;')
  hash_cnt = cur.fetchone()[0]
  if hash_cnt > 0:
    db_exists = True
except:
  pass

if db_exists:
  res = input("It seems like you have entries on DB, are you sure to reset databases? This WILL RESET indexing progress. (y/n) ")
  if res != 'y':
    print('aborting')
    exit(1)

## reset db
sqls = open('db_reset.sql', 'r').read().split(';')
for sql in sqls:
  if sql.strip() != '':
    cur.execute(sql)

sqls = open('db_init.sql', 'r').read().split(';')
for sql in sqls:
  if sql.strip() != '':
    cur.execute(sql)

cur.execute('INSERT INTO ord_network_type (network_type) VALUES (%s);', (network_type,))

## close db
cur.close()
conn.close()

ord_folder = os.getenv("ORD_FOLDER") or "../../ord/target/release/"
ord_datadir = os.getenv("ORD_DATADIR") or "."

ord_folder = pathlib.Path(ord_folder).absolute()
ord_datadir = pathlib.Path(ord_folder, ord_datadir).absolute()

network_path = ""
if network_type == "mainnet":
  network_path = ""
elif network_type == "testnet":
  network_path = "testnet3"
elif network_type == "signet":
  network_path = "signet"
elif network_type == "regtest":
  network_path = "regtest"

ord_index_redb_path = pathlib.Path(ord_datadir, network_path, "index.redb").absolute()
ord_index_redb_path.unlink(missing_ok=True)

if not pathlib.Path(ord_folder, network_path).exists():
  pathlib.Path(ord_folder, network_path).mkdir(parents=True)

ord_log_file_path = pathlib.Path(ord_folder, network_path, "log_file.txt").absolute()
ord_log_file_path.write_text("")

ord_log_file_index_path = pathlib.Path(ord_folder, network_path, "log_file_index.txt").absolute()
ord_log_file_index_path.write_text("")

log_file_error_path = pathlib.Path("log_file_error.txt").absolute()
log_file_error_path.write_text("")

print('done')
