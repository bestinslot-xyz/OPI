# pip install -r ../../requirements.txt
#
# Writes the brc20 settings into the repository root .env, the file opi-indexer
# reads, and initialises the brc20 database from it. Other settings already in
# that file are kept. Run it from crates/opi-brc20.

import os, psycopg2, stdiomask
from dotenv import dotenv_values, set_key

ENV_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', '.env')

init_env = True

# does .env file exist?
if os.path.isfile(ENV_PATH):
  res = input("Do you want to re-initialise the brc20 settings in the root .env file? (y/n) ")
  if res != 'y':
    init_env = False

if init_env:
  DB_USER="postgres"
  DB_HOST="localhost"
  DB_PORT="5432"
  DB_DATABASE=""
  DB_PASSWD=""
  DB_SSL="false"
  OPERATION_MODE="full"
  NETWORK_TYPE="mainnet"
  REPORT_TO_INDEXER="true"
  REPORT_URL="https://api.opi.network/report_block"
  REPORT_RETRIES="10"
  REPORT_NAME="opi_brc20_index"
  BRC20_PROG_RPC_URL="http://localhost:18545"
  BRC20_PROG_RPC_USER="user"
  BRC20_PROG_RPC_PASSWORD="password"

  print("Initialising .env file")
  print("leave blank to use default values")
  use_other_env = False
  other_env_exists = bool(dotenv_values(dotenv_path=ENV_PATH).get("DB_DATABASE")) if os.path.isfile(ENV_PATH) else False
  if other_env_exists:
    res = input("The repository root .env already has brc20 DB settings, do you want to use them? (y/n) ")
    if res == 'y':
      use_other_env = True
  if use_other_env:
    env = dotenv_values(dotenv_path=ENV_PATH)
    DB_USER = env.get("DB_USER") or "postgres"
    DB_HOST = env.get("DB_HOST") or "localhost"
    DB_PORT = env.get("DB_PORT") or "5432"
    DB_DATABASE = env.get("DB_DATABASE")
    DB_PASSWD = env.get("DB_PASSWD") or ""
    DB_SSL = env.get("DB_SSL") or "false"
  else:
    res = input("BRC20 Postgres DB username (Default: postgres): ")
    if res != '':
      DB_USER = res
    res = input("BRC20 Postgres DB host (Default: localhost) leave default if postgres is installed on the same machine: ")
    if res != '':
      DB_HOST = res
    res = input("BRC20 Postgres DB port (Default: 5432): ")
    if res != '':
      DB_PORT = res
    while True:
      res = input("BRC20 Postgres DB name (required, brc20's own database): ")
      if res != '':
        DB_DATABASE = res
        break
      else:
        print('DB name cannot be empty')
    res = stdiomask.getpass("BRC20 Postgres DB password: ")
    DB_PASSWD = res
    res = input("BRC20 Postgres DB SSL (Default: false) options: true, false: ")
    if res != '':
      DB_SSL = res
  res = input("Operation mode (Default: full) options: full, light: ")
  if res != '':
    OPERATION_MODE = res
  res = input("Network type (Default: mainnet) options: mainnet, testnet, testnet4, signet, regtest: ")
  if res != '':
    NETWORK_TYPE = res
  res = input("Report to main indexer (Default: true): ")
  if res != '':
    REPORT_TO_INDEXER = res
  if REPORT_TO_INDEXER == 'true':
    res = input("Report URL (Default: https://api.opi.network/report_block): ")
    if res != '':
      REPORT_URL = res
    res = input("Report retries (Default: 10): ")
    if res != '':
      REPORT_RETRIES = res
    while True:
      res = input("Report name: ")
      if res != '':
        REPORT_NAME = res
        break
      else:
        print('Report name cannot be empty')
  print('If brc20_prog sets BRC20_PROG_ADMIN_RPC_SERVER_URL, use the admin listener URL here (e.g. http://localhost:18546).')
  res = input("BRC20 programmable module RPC URL (Default: http://localhost:18545): ")
  if res != '':
    BRC20_PROG_RPC_URL = res
  res = input("BRC20 programmable module RPC username (Default: user): ")
  if res != '':
    BRC20_PROG_RPC_USER = res
  res = stdiomask.getpass("BRC20 programmable module RPC password (Default: password): ")
  if res != '':
    BRC20_PROG_RPC_PASSWORD = res

  if not os.path.isfile(ENV_PATH):
    open(ENV_PATH, 'a').close()
  settings = {
    "DB_USER": DB_USER,
    "DB_HOST": DB_HOST,
    "DB_PORT": DB_PORT,
    "DB_DATABASE": DB_DATABASE,
    "DB_PASSWD": DB_PASSWD,
    "DB_SSL": DB_SSL,
    "OPERATION_MODE": OPERATION_MODE,
    "NETWORK_TYPE": NETWORK_TYPE,
    "REPORT_TO_INDEXER": REPORT_TO_INDEXER,
    "REPORT_URL": REPORT_URL,
    "REPORT_RETRIES": REPORT_RETRIES,
    "REPORT_NAME": REPORT_NAME,
    "BRC20_PROG_RPC_URL": BRC20_PROG_RPC_URL,
    "BRC20_PROG_RPC_USER": BRC20_PROG_RPC_USER,
    "BRC20_PROG_RPC_PASSWORD": BRC20_PROG_RPC_PASSWORD,
  }
  for key, value in settings.items():
    set_key(ENV_PATH, key, value, quote_mode='always')

res = input("Are you sure you want to initialise/reset the brc20 database? (y/n) ")
if res != 'y':
  print('aborting')
  exit(1)

env = dotenv_values(dotenv_path=ENV_PATH) if os.path.isfile(ENV_PATH) else {}
db_user = env.get("DB_USER") or "postgres"
db_host = env.get("DB_HOST") or "localhost"
db_port = int(env.get("DB_PORT") or "5432")
db_database = env.get("DB_DATABASE")
if not db_database:
  print('DB_DATABASE is not set in ' + os.path.normpath(ENV_PATH))
  exit(1)
db_password = env.get("DB_PASSWD")

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
  cur.execute('select count(*) from brc20_block_hashes;')
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
sqls = open('src/database/sql/db_reset.sql', 'r').read().split(';')
for sql in sqls:
  if sql.strip() != '':
    cur.execute(sql)

sqls = open('src/database/sql/db_init.sql', 'r').read().split(';')
for sql in sqls:
  if sql.strip() != '':
    cur.execute(sql)

## close db
cur.close()
conn.close()

print('done')

def sanitise(value):
    """Sanitise a string for safe output."""
    return value.replace("/", "%2F").replace(":", "%3A").replace("@", "%40")

## print DATABASE_URL
print("Run the following command to set DATABASE_URL in your environment before compiling:")
print(f"export DATABASE_URL=postgresql://{sanitise(db_user)}:{sanitise(db_password)}@{sanitise(db_host)}:{db_port}/{sanitise(db_database)}")