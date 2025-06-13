# pip install python-dotenv
# pip install psycopg2-binary

import os, sys
from dotenv import load_dotenv
import traceback, time, codecs, json
import psycopg2
import hashlib
import requests

## global variables
in_commit = False
block_events_str = ""
EVENT_SEPARATOR = "|"
INDEXER_VERSION = "opi-bitmap-full-node v0.3.0"
DB_VERSION = 3

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
network_type = os.getenv("NETWORK_TYPE") or "mainnet"

first_inscription_heights = {
  'mainnet': 767430,
  'testnet': 2413343,
  'testnet4': 0,
  'signet': 112402,
  'regtest': 0,
}
first_inscription_height = first_inscription_heights[network_type]

report_to_indexer = (os.getenv("REPORT_TO_INDEXER") or "true") == "true"
report_url = os.getenv("REPORT_URL") or "https://api.opi.network/report_block"
report_retries = int(os.getenv("REPORT_RETRIES") or "10")
report_name = os.getenv("REPORT_NAME") or "opi_bitmap_indexer"

if network_type == 'regtest':
  report_to_indexer = False
  print("Network type is regtest, reporting to indexer is disabled.")

## connect to db
conn = psycopg2.connect(
  host=db_host,
  port=db_port,
  database=db_database,
  user=db_user,
  password=db_password)
conn.autocommit = True
cur = conn.cursor()


def get_block_hash_by_height(block_height):
  url = "http://localhost:11030/"
  data = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "getBlockHashAndTs",
    "params": [block_height]
  }
  r = requests.post(url, json=data)
  if r.status_code != 200:
    print("Error while getting block hash by height: " + str(r.status_code))
    raise Exception("Error while getting block hash by height: " + str(r.status_code))
  js = r.json()
  if 'error' in js:
    print("Error while getting block hash by height: " + str(js['error']))
    raise Exception("Error while getting block hash by height: " + str(js['error']))
  if 'result' not in js:
    print("Error while getting block hash by height: no result in response")
    raise Exception("Error while getting block hash by height: no result in response")
  if 'block_hash' not in js['result']:
    print("Error while getting block hash by height: no block_hash in result")
    raise Exception("Error while getting block hash by height: no block_hash in result")
  return js['result']['block_hash']

def get_ord_block_height():
  url = "http://localhost:11030/"
  data = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "getLatestBlockHeight"
  }
  r = requests.post(url, json=data)
  if r.status_code != 200:
    print("Error while getting ord block height: " + str(r.status_code))
    raise Exception("Error while getting ord block height: " + str(r.status_code))
  js = r.json()
  if 'error' in js:
    print("Error while getting ord block height: " + str(js['error']))
    raise Exception("Error while getting ord block height: " + str(js['error']))
  if 'result' not in js:
    print("Error while getting ord block height: no result in response")
    raise Exception("Error while getting ord block height: no result in response")
  return js['result']

def get_bitmap_inscrs(block_height):
  url = "http://localhost:11030/"
  data = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "getBlockBitmapInscrs",
    "params": [block_height]
  }
  r = requests.post(url, json=data)
  if r.status_code != 200:
    print("Error while getting ord bitmap inscrs: " + str(r.status_code))
    raise Exception("Error while getting ord bitmap inscrs: " + str(r.status_code))
  js = r.json()
  if 'error' in js:
    print("Error while getting ord bitmap inscrs: " + str(js['error']))
    raise Exception("Error while getting ord bitmap inscrs: " + str(js['error']))
  if 'result' not in js:
    print("Error while getting ord bitmap inscrs: no result in response")
    raise Exception("Error while getting ord bitmap inscrs: no result in response")
  res = []
  for inscr in js['result']:
    res.append((inscr['inscription_id'], inscr['content_hex'], inscr['inscription_number']))
  return res

## helper functions
def get_bitmap_number(content_hex):
  content = None
  try:
    content = codecs.decode(content_hex, "hex").decode('utf-8')
  except:
    pass
  if content is None: return None
  if not content.endswith('.bitmap'): return None
  content = content[:-len('.bitmap')]
  if len(content) == 0: return None
  for ch in content:
    if ord(ch) > ord('9') or ord(ch) < ord('0'):
      return None
  if ord(content[0]) == ord('0') and len(content) != 1: return None
  return int(content)

def get_event_str(bitmap_number, inscription_id):
  res = "inscribe;"
  res += inscription_id + ";"
  res += bitmap_number
  return res

def get_sha256_hash(s):
  return hashlib.sha256(s.encode('utf-8')).hexdigest()

def update_event_hashes(block_height):
  global block_events_str
  if len(block_events_str) > 0 and block_events_str[-1] == EVENT_SEPARATOR: block_events_str = block_events_str[:-1] ## remove last separator
  block_event_hash = get_sha256_hash(block_events_str)
  cumulative_event_hash = None
  cur.execute('''select cumulative_event_hash from bitmap_cumulative_event_hashes where block_height = %s;''', (block_height - 1,))
  if cur.rowcount == 0:
    cumulative_event_hash = block_event_hash
  else:
    cumulative_event_hash = get_sha256_hash(cur.fetchone()[0] + block_event_hash)
  cur.execute('''INSERT INTO bitmap_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) VALUES (%s, %s, %s);''', (block_height, block_event_hash, cumulative_event_hash))
    




def index_block(block_height, current_block_hash):
  global block_events_str
  print("Indexing block " + str(block_height))
  block_events_str = ""
  
  ## get text/plain inscrs from ord
  # cur_metaprotocol.execute('''SELECT oc.inscription_id, oc.text_content
  #                             FROM ord_content oc
  #                             LEFT JOIN ord_number_to_id onti on oc.inscription_id = onti.inscription_id
  #                             WHERE oc.block_height = %s AND oc.text_content is not null AND
  #                                   oc.content_type LIKE '746578742f706c61696e%%' AND
  #                                   onti.inscription_number >= 0
  #                             ORDER BY onti.inscription_number asc;''', (block_height,))
  inscrs = get_bitmap_inscrs(block_height)
  if len(inscrs) == 0:
    print("No new inscrs found for block " + str(block_height))
    update_event_hashes(block_height)
    cur.execute('''INSERT INTO bitmap_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, current_block_hash))
    return
  print("New inscrs count: ", len(inscrs))
  
  idx = 0
  for inscr in inscrs:
    idx += 1
    if idx % 1000 == 0:
      print(idx, '/', len(inscrs))
    inscr_id, content_hex, inscr_num = inscr
    bitmap_number = get_bitmap_number(content_hex)
    if bitmap_number is None: continue
    if bitmap_number > block_height: 
      print("bitmap_number > block_height: " + str(bitmap_number) + " > " + str(block_height))
      continue
    cur.execute('''INSERT INTO bitmaps (inscription_id, inscription_number, bitmap_number, block_height) VALUES (%s, %s, %s, %s) ON CONFLICT (bitmap_number) 
                    DO NOTHING RETURNING id;''', 
                (inscr_id, inscr_num, bitmap_number, block_height))
    if cur.rowcount == 0: 
      print("bitmap_number already exists: " + str(bitmap_number))
      continue
    bitmap_id = cur.fetchone()[0]
    print("bitmap_number: " + str(bitmap_number) + " id: " + str(bitmap_id))
    block_events_str += get_event_str(str(bitmap_number), str(inscr_id)) + EVENT_SEPARATOR
  
  update_event_hashes(block_height)
  # end of block
  cur.execute('''INSERT INTO bitmap_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, current_block_hash))
  conn.commit()
  print("ALL DONE")



def check_for_reorg():
  cur.execute('select block_height, block_hash from bitmap_block_hashes order by block_height desc limit 1;')
  if cur.rowcount == 0: return None ## nothing indexed yet
  last_block = cur.fetchone()

  last_block_ord = get_block_hash_by_height(last_block[0])
  if last_block_ord == last_block[1]: return None ## last block hashes are the same, no reorg

  print("REORG DETECTED!!")
  cur.execute('select block_height, block_hash from bitmap_block_hashes order by block_height desc limit 10;')
  hashes = cur.fetchall() ## get last 10 hashes
  for h in hashes:
    block = get_block_hash_by_height(h[0])
    if block == h[1]: ## found reorg height by a matching hash
      print("REORG HEIGHT FOUND: " + str(h[0]))
      return h[0]
  
  ## bigger than 10 block reorg is not supported by ord
  print("CRITICAL ERROR!!")
  sys.exit(1)

def reorg_fix(reorg_height):
  cur.execute('begin;')
  cur.execute('delete from bitmaps where block_height > %s;', (reorg_height,)) ## delete new bitmaps
  cur.execute("SELECT setval('bitmaps_id_seq', max(id)) from bitmaps;") ## reset id sequence
  cur.execute('delete from bitmap_cumulative_event_hashes where block_height > %s;', (reorg_height,)) ## delete new bitmaps
  cur.execute("SELECT setval('bitmap_cumulative_event_hashes_id_seq', max(id)) from bitmap_cumulative_event_hashes;") ## reset id sequence
  cur.execute('delete from bitmap_block_hashes where block_height > %s;', (reorg_height,)) ## delete new block hashes
  cur.execute("SELECT setval('bitmap_block_hashes_id_seq', max(id)) from bitmap_block_hashes;") ## reset id sequence
  cur.execute('commit;')

def check_if_there_is_residue_from_last_run():
  cur.execute('''select max(block_height) from bitmap_block_hashes;''')
  row = cur.fetchone()
  current_block = None
  if row[0] is None: current_block = first_inscription_height
  else: current_block = row[0] + 1
  residue_found = False
  cur.execute('''select coalesce(max(block_height), -1) from bitmaps;''')
  if cur.rowcount != 0 and cur.fetchone()[0] >= current_block:
    residue_found = True
    print("residue on bitmaps")
  if residue_found:
    print("There is residue from last run, rolling back to " + str(current_block - 1))
    reorg_fix(current_block - 1)
    print("Rolled back to " + str(current_block - 1))
    return

try:
  cur.execute('select db_version from bitmap_indexer_version;')
  if cur.rowcount == 0:
    print("Indexer version not found, db needs to be recreated from scratch, please run reset_init.py")
    exit(1)
  else:
    db_version = cur.fetchone()[0]
    if db_version != DB_VERSION:
      print("This version (" + str(db_version) + ") cannot be fixed, please run reset_init.py")
      exit(1)
except:
  print("Indexer version not found, db needs to be recreated from scratch, please run reset_init.py")
  exit(1)

def try_to_report_with_retries(to_send):
  global report_url, report_retries
  for _ in range(0, report_retries):
    try:
      r = requests.post(report_url, json=to_send)
      if r.status_code == 200:
        print("Reported hashes to metaprotocol indexer indexer.")
        return
      else:
        print("Error while reporting hashes to metaprotocol indexer indexer, status code: " + str(r.status_code))
    except:
      print("Error while reporting hashes to metaprotocol indexer indexer, retrying...")
    time.sleep(1)
  print("Error while reporting hashes to metaprotocol indexer indexer, giving up.")

def report_hashes(block_height):
  global report_to_indexer
  if not report_to_indexer:
    print("Reporting to metaprotocol indexer is disabled.")
    return
  cur.execute('''select block_event_hash, cumulative_event_hash from bitmap_cumulative_event_hashes where block_height = %s;''', (block_height,))
  row = cur.fetchone()
  block_event_hash = row[0]
  cumulative_event_hash = row[1]
  cur.execute('''select block_hash from bitmap_block_hashes where block_height = %s;''', (block_height,))
  block_hash = cur.fetchone()[0]
  to_send = {
    "name": report_name,
    "type": "bitmap",
    "node_type": "full_node",
    "network_type": network_type,
    "version": INDEXER_VERSION,
    "db_version": DB_VERSION,
    "block_height": block_height,
    "block_hash": block_hash,
    "block_event_hash": block_event_hash,
    "cumulative_event_hash": cumulative_event_hash
  }
  print("Sending hashes to metaprotocol indexer indexer...")
  try_to_report_with_retries(to_send)

last_report_height = 0
check_if_there_is_residue_from_last_run()
while True:
  check_if_there_is_residue_from_last_run()
  ## check if a new block is indexed
  max_block_of_metaprotocol_db = get_ord_block_height()
  cur.execute('''select max(block_height) from bitmap_block_hashes;''')
  row = cur.fetchone()
  current_block = None
  if row[0] is None: current_block = first_inscription_height
  else: current_block = row[0] + 1
  if current_block > max_block_of_metaprotocol_db:
    print("Waiting for new blocks...")
    time.sleep(5)
    continue
  
  print("Processing block %s" % current_block)
  current_block_hash = get_block_hash_by_height(current_block)
  reorg_height = check_for_reorg()
  if reorg_height is not None:
    print("Rolling back to ", reorg_height)
    reorg_fix(reorg_height)
    print("Rolled back to " + str(reorg_height))
    continue
  try:
    index_block(current_block, current_block_hash)
    if max_block_of_metaprotocol_db - current_block < 10 or current_block - last_report_height > 100: ## do not report if there are more than 10 blocks to index
      report_hashes(current_block)
      last_report_height = current_block
  except:
    traceback.print_exc()
    if in_commit: ## rollback commit if any
      print("rolling back")
      cur.execute('''ROLLBACK;''')
      in_commit = False
    time.sleep(10)
