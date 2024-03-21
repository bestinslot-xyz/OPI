# pip install python-dotenv
# pip install psycopg2-binary

import os, sys, requests
from dotenv import load_dotenv
import traceback, time, codecs, json
import psycopg2
import hashlib

## psycopg2 doesn't get decimal size from postgres and defaults to 28 which is not enough for brc-20 so we use long which is infinite for integers
DEC2LONG = psycopg2.extensions.new_type(
    psycopg2.extensions.DECIMAL.values,
    'DEC2LONG',
    lambda value, curs: int(value) if value is not None else None)
psycopg2.extensions.register_type(DEC2LONG)

## load env variables
load_dotenv()
db_user = os.getenv("DB_USER") or "postgres"
db_host = os.getenv("DB_HOST") or "localhost"
db_port = int(os.getenv("DB_PORT") or "5432")
db_database = os.getenv("DB_DATABASE") or "postgres"
db_password = os.getenv("DB_PASSWD")
db_metaprotocol_user = os.getenv("DB_METAPROTOCOL_USER") or "postgres"
db_metaprotocol_host = os.getenv("DB_METAPROTOCOL_HOST") or "localhost"
db_metaprotocol_port = int(os.getenv("DB_METAPROTOCOL_PORT") or "5432")
db_metaprotocol_database = os.getenv("DB_METAPROTOCOL_DATABASE") or "postgres"
db_metaprotocol_password = os.getenv("DB_METAPROTOCOL_PASSWD")
first_inscription_height = int(os.getenv("FIRST_INSCRIPTION_HEIGHT") or "767430")
report_to_indexer = (os.getenv("REPORT_TO_INDEXER") or "true") == "true"
report_url = os.getenv("REPORT_URL") or "https://api.opi.network/report_block"
report_retries = int(os.getenv("REPORT_RETRIES") or "10")
report_name = os.getenv("REPORT_NAME") or "opi_pow20_indexer"

if first_inscription_height != 767430:
  print("first_inscription_height must be 767430, please check if you are using the correct .env file")
  sys.exit(1)

## connect to db
cur = None
cur_metaprotocol = None
try:
  conn = psycopg2.connect(
    host=db_host,
    port=db_port,
    database=db_database,
    user=db_user,
    password=db_password)
  conn.autocommit = True
  cur = conn.cursor()
except:
  print("Error connecting to pow20 database, please check .env file")
  traceback.print_exc()
  sys.exit(1)

try:
  conn_metaprotocol = psycopg2.connect(
    host=db_metaprotocol_host,
    port=db_metaprotocol_port,
    database=db_metaprotocol_database,
    user=db_metaprotocol_user,
    password=db_metaprotocol_password)
  conn_metaprotocol.autocommit = True
  cur_metaprotocol = conn_metaprotocol.cursor()
except:
  print("Error connecting to metaprotocol database, please check .env file")
  traceback.print_exc()
  sys.exit(1)

## get block height info
pow20_min_height = 0
pow20_max_height = 0
try:
  cur.execute("SELECT min(block_height), max(block_height) FROM pow20_block_hashes;")
  row = cur.fetchone()
  if row:
    pow20_min_height = row[0]
    pow20_max_height = row[1]
except:
  print("Error getting pow20 block height info")
  traceback.print_exc()
  sys.exit(1)

## get block height info of main db
main_min_height = 0
main_max_height = 0
try:
  cur_metaprotocol.execute("SELECT min(block_height), max(block_height) FROM block_hashes;")
  row = cur_metaprotocol.fetchone()
  if row:
    main_min_height = row[0]
    main_max_height = row[1]
except:
  print("Error getting main block height info")
  traceback.print_exc()
  sys.exit(1)

if main_min_height > 767430:
  print("main_min_height is greater than 767430, please check if you are using the correct .env file and rerun the main & pow20 indexer from start (run reset.py first)")
  sys.exit(1)

if pow20_min_height != 767430:
  print("pow20_min_height is not equal to 767430, please check if you are using the correct .env file and rerun the pow20 indexer from start (run reset.py first)")
  sys.exit(1)

def check_block_hashes(height):
  print("Checking block " + str(height))
  cur.execute('''select bceh.block_event_hash, bceh.cumulative_event_hash, bbh.block_hash 
    from pow20_cumulative_event_hashes bceh 
    left join pow20_block_hashes bbh on bbh.block_height = bceh.block_height
    where bceh.block_height = %s;''', (height,))
  if cur.rowcount == 0:
    print("Block not found on DB!!")
    return False
  row = cur.fetchone()
  block_event_hash, cumulative_event_hash, block_hash = row
  url = 'https://opi.network/api/get_best_hashes_for_block/' + str(height)
  r = requests.get(url)
  js = json.loads(r.text)
  opi_best_block_hash = js['data']['best_block_hash']
  opi_best_cumulative_hash = js['data']['best_cumulative_hash']
  if opi_best_block_hash == block_hash and opi_best_cumulative_hash == cumulative_event_hash:
    print("same")
    return True
  if opi_best_block_hash != None or opi_best_cumulative_hash != None:
    print("different")
    return False
  print("not found on OPI API")
  return True
  

current_min_height = 767430
current_max_height = pow20_max_height
if check_block_hashes(current_max_height):
  print("pow20 block hashes are correct")
  sys.exit(0)

if not check_block_hashes(current_min_height):
  print("pow20 block hashes are incorrect from the start, please check if you are using the correct .env file and rerun the pow20 indexer from start (run reset.py first)")
  sys.exit(1)

while True:
  if current_max_height - current_min_height <= 1:
    print("pow20 block hashes are incorrect starting at block (" + str(current_max_height) + "), please check if you are using the correct .env file and rerun the pow20 indexer from start (run reset.py first)")
    sys.exit(1)
  mid_block = (current_min_height + current_max_height) // 2
  if check_block_hashes(mid_block):
    current_min_height = mid_block
  else:
    current_max_height = mid_block