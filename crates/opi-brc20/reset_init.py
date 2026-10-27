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
  # current values of the shared root .env are the prompt defaults
  existing = dotenv_values(dotenv_path=ENV_PATH) if os.path.isfile(ENV_PATH) else {}
  DB_USER = existing.get("DB_USER") or "postgres"
  DB_HOST = existing.get("DB_HOST") or "localhost"
  DB_PORT = existing.get("DB_PORT") or "5432"
  DB_DATABASE = existing.get("DB_DATABASE") or ""
  DB_PASSWD = existing.get("DB_PASSWD") or ""
  DB_SSL = existing.get("DB_SSL") or "false"
  OPERATION_MODE = existing.get("OPERATION_MODE") or "full"
  NETWORK_TYPE = existing.get("NETWORK_TYPE") or "mainnet"
  REPORT_TO_INDEXER = existing.get("REPORT_TO_INDEXER") or "true"
  REPORT_URL = existing.get("REPORT_URL") or "https://api.opi.network/report_block"
  REPORT_RETRIES = existing.get("REPORT_RETRIES") or "10"
  REPORT_NAME = existing.get("REPORT_NAME") or "opi_brc20_index"
  BRC20_PROG_DB_PATH = existing.get("BRC20_PROG_DB_PATH") or ""
  BRC20_PROG_RPC_SERVER_URL = existing.get("BRC20_PROG_RPC_SERVER_URL") or "127.0.0.1:18545"

  print("Initialising .env file")
  print("leave blank to use default values")
  use_other_env = False
  if existing.get("DB_DATABASE"):
    res = input("The repository root .env already has brc20 DB settings, do you want to use them? (y/n) ")
    if res == 'y':
      use_other_env = True
  if use_other_env:
    pass
  else:
    res = input("BRC20 Postgres DB username (Default: " + DB_USER + "): ")
    if res != '':
      DB_USER = res
    res = input("BRC20 Postgres DB host (Default: " + DB_HOST + ") leave default if postgres is installed on the same machine: ")
    if res != '':
      DB_HOST = res
    res = input("BRC20 Postgres DB port (Default: " + DB_PORT + "): ")
    if res != '':
      DB_PORT = res
    while True:
      res = input("BRC20 Postgres DB name (required, brc20's own database" + (", Default: " + DB_DATABASE if DB_DATABASE else "") + "): ")
      if res != '':
        DB_DATABASE = res
        break
      elif DB_DATABASE != '':
        break
      else:
        print('DB name cannot be empty')
    res = stdiomask.getpass("BRC20 Postgres DB password (blank keeps the current one): ")
    if res != '':
      DB_PASSWD = res
    res = input("BRC20 Postgres DB SSL (Default: " + DB_SSL + ") options: true, false: ")
    if res != '':
      DB_SSL = res
  res = input("Operation mode (Default: " + OPERATION_MODE + ") options: full, light: ")
  if res != '':
    OPERATION_MODE = res
  res = input("Network type (Default: " + NETWORK_TYPE + ") options: mainnet, testnet, testnet4, signet, regtest: ")
  if res != '':
    NETWORK_TYPE = res
  res = input("Report to main indexer (Default: " + REPORT_TO_INDEXER + "): ")
  if res != '':
    REPORT_TO_INDEXER = res
  if REPORT_TO_INDEXER == 'true':
    res = input("Report URL (Default: " + REPORT_URL + "): ")
    if res != '':
      REPORT_URL = res
    res = input("Report retries (Default: " + REPORT_RETRIES + "): ")
    if res != '':
      REPORT_RETRIES = res
    res = input("Report name (Default: " + REPORT_NAME + "): ")
    if res != '':
      REPORT_NAME = res
  while True:
    res = input("BRC20 programmable module database directory (required" + (", Default: " + BRC20_PROG_DB_PATH if BRC20_PROG_DB_PATH else "") + "): ")
    if res != '':
      BRC20_PROG_DB_PATH = res
      break
    elif BRC20_PROG_DB_PATH != '':
      break
    else:
      print('Database directory cannot be empty')
  res = input("BRC20 programmable module public RPC address (Default: " + BRC20_PROG_RPC_SERVER_URL + "): ")
  if res != '':
    BRC20_PROG_RPC_SERVER_URL = res

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
    "BRC20_PROG_DB_PATH": BRC20_PROG_DB_PATH,
    "BRC20_PROG_RPC_SERVER_URL": BRC20_PROG_RPC_SERVER_URL,
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