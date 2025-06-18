# pip install python-dotenv
# pip install psycopg2-binary
# pip install stdiomask

import os, psycopg2, stdiomask
from dotenv import load_dotenv, dotenv_values

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
  NETWORK_TYPE="mainnet"
  REPORT_TO_INDEXER="true"
  REPORT_URL="https://api.opi.network/report_block"
  REPORT_RETRIES="10"
  REPORT_NAME="opi_bitmap_index"
  print("Initialising .env file")
  print("leave blank to use default values")
  use_other_env = False
  other_env_exists = os.path.isfile('../bitmap_api/.env')
  if other_env_exists:
    res = input(".env on bitmap_api already exists, do you want to use values from there? (y/n) ")
    if res == 'y':
      use_other_env = True
  if use_other_env:
    env = dotenv_values(dotenv_path='../bitmap_api/.env')
    DB_USER = env.get("DB_USER") or "postgres"
    DB_HOST = env.get("DB_HOST") or "localhost"
    DB_PORT = env.get("DB_PORT") or "5432"
    DB_DATABASE = env.get("DB_DATABASE") or "postgres"
    DB_PASSWD = env.get("DB_PASSWD")
  else:
    res = input("Bitmap Postgres DB username (Default: postgres): ")
    if res != '':
      DB_USER = res
    res = input("Bitmap Postgres DB host (Default: localhost) leave default if postgres is installed on the same machine: ")
    if res != '':
      DB_HOST = res
    res = input("Bitmap Postgres DB port (Default: 5432): ")
    if res != '':
      DB_PORT = res
    res = input("Bitmap Postgres DB name (Default: postgres) leave default if no new dbs are created: ")
    if res != '':
      DB_DATABASE = res
    res = stdiomask.getpass("Bitmap Postgres DB password: ")
    DB_PASSWD = res
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
  f = open('.env', 'w')
  f.write('DB_USER="' + DB_USER + '"\n')
  f.write('DB_HOST="' + DB_HOST + '"\n')
  f.write('DB_PORT="' + DB_PORT + '"\n')
  f.write('DB_DATABASE="' + DB_DATABASE + '"\n')
  f.write('DB_PASSWD="' + DB_PASSWD + '"\n')
  f.write('NETWORK_TYPE="' + NETWORK_TYPE + '"\n')
  f.write('REPORT_TO_INDEXER="' + REPORT_TO_INDEXER + '"\n')
  f.write('REPORT_URL="' + REPORT_URL + '"\n')
  f.write('REPORT_RETRIES="' + REPORT_RETRIES + '"\n')
  f.write('REPORT_NAME="' + REPORT_NAME + '"\n')
  f.close()

res = input("Are you sure you want to initialise/reset the bitmaps database? (y/n) ")
if res != 'y':
  print('aborting')
  exit(1)

load_dotenv()
db_user = os.getenv("DB_USER") or "postgres"
db_host = os.getenv("DB_HOST") or "localhost"
db_port = int(os.getenv("DB_PORT") or "5432")
db_database = os.getenv("DB_DATABASE") or "postgres"
db_password = os.getenv("DB_PASSWD")

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
  cur.execute('select count(*) from bitmap_block_hashes;')
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

## close db
cur.close()
conn.close()

print('done')
