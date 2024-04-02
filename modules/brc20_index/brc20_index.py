# pip install python-dotenv
# pip install psycopg2-binary

import os, sys, requests
from dotenv import load_dotenv
import traceback, time, codecs, json
import psycopg2
import hashlib

if not os.path.isfile('.env'):
  print(".env file not found, please run \"python3 reset_init.py\" first")
  sys.exit(1)

## global variables
ticks = {}
in_commit = False
block_events_str = ""
EVENT_SEPARATOR = "|"
INDEXER_VERSION = "opi-brc20-full-node v0.4.1"
RECOVERABLE_DB_VERSIONS = [ 4 ]
DB_VERSION = 5
EVENT_HASH_VERSION = 2

SELF_MINT_ENABLE_HEIGHT = 837090

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
report_to_indexer = (os.getenv("REPORT_TO_INDEXER") or "true") == "true"
report_url = os.getenv("REPORT_URL") or "https://api.opi.network/report_block"
report_retries = int(os.getenv("REPORT_RETRIES") or "10")
report_name = os.getenv("REPORT_NAME") or "opi_brc20_indexer"
create_extra_tables = (os.getenv("CREATE_EXTRA_TABLES") or "false") == "true"
network_type = os.getenv("NETWORK_TYPE") or "mainnet"

first_inscription_heights = {
  'mainnet': 767430,
  'testnet': 2413343,
  'signet': 112402,
  'regtest': 0,
}
first_inscription_height = first_inscription_heights[network_type]

first_brc20_heights = {
  'mainnet': 779832,
  'testnet': 2413343,
  'signet': 112402,
  'regtest': 0,
}
first_brc20_height = first_brc20_heights[network_type]

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

conn_metaprotocol = psycopg2.connect(
  host=db_metaprotocol_host,
  port=db_metaprotocol_port,
  database=db_metaprotocol_database,
  user=db_metaprotocol_user,
  password=db_metaprotocol_password)
conn_metaprotocol.autocommit = True
cur_metaprotocol = conn_metaprotocol.cursor()

## create tables if not exists
## does brc20_block_hashes table exist?
cur.execute('''SELECT EXISTS (SELECT 1 FROM pg_tables WHERE tablename = 'brc20_block_hashes') AS table_existence;''')
if cur.fetchone()[0] == False:
  print("Initialising database...")
  with open('db_init.sql', 'r') as f:
    sql = f.read()
    cur.execute(sql)
  conn.commit()

if create_extra_tables:
  cur.execute('''SELECT EXISTS (SELECT 1 FROM pg_tables WHERE tablename = 'brc20_extras_block_hashes') AS table_existence;''')
  if cur.fetchone()[0] == False:
    print("Initialising extra tables...")
    with open('db_init_extra.sql', 'r') as f:
      sql = f.read()
      cur.execute(sql)
    conn.commit()

cur_metaprotocol.execute('SELECT network_type from ord_network_type LIMIT 1;')
if cur_metaprotocol.rowcount == 0:
  print("ord_network_type not found, main db needs to be recreated from scratch or fixed with index.js, please run index.js in main_index")
  sys.exit(1)

network_type_db = cur_metaprotocol.fetchone()[0]
if network_type_db != network_type:
  print("network_type mismatch between main index and brc20 index")
  sys.exit(1)

cur_metaprotocol.execute('SELECT event_type, max_transfer_cnt from ord_transfer_counts;')
if cur_metaprotocol.rowcount == 0:
  print("ord_transfer_counts not found, please run index.js in main_index to fix db")
  sys.exit(1)

default_max_transfer_cnt = 0
tx_limits = cur_metaprotocol.fetchall()
for tx_limit in tx_limits:
  if tx_limit[0] == 'default':
    default_max_transfer_cnt = tx_limit[1]
    break

if default_max_transfer_cnt < 2:
  print("default max_transfer_cnt is less than 2, brc20_indexer requires at least 2, please recreate db from scratch and rerun ord with default tx limit set to 2 or more")
  sys.exit(1)

## helper functions

def utf8len(s):
  return len(s.encode('utf-8'))

def is_positive_number(s, do_strip=False):
  try:
    if do_strip:
      s = s.strip()
    try:
      if len(s) == 0: return False
      for ch in s:
        if ord(ch) > ord('9') or ord(ch) < ord('0'):
          return False
      return True
    except KeyboardInterrupt:
      raise KeyboardInterrupt
    except: return False
  except KeyboardInterrupt:
    raise KeyboardInterrupt
  except: return False ## has to be a string

def is_positive_number_with_dot(s, do_strip=False):
  try:
    if do_strip:
      s = s.strip()
    try:
      dotFound = False
      if len(s) == 0: return False
      if s[0] == '.': return False
      if s[-1] == '.': return False
      for ch in s:
        if ord(ch) > ord('9') or ord(ch) < ord('0'):
          if ch != '.': return False
          if dotFound: return False
          dotFound = True
      return True
    except KeyboardInterrupt:
      raise KeyboardInterrupt
    except: return False
  except KeyboardInterrupt:
    raise KeyboardInterrupt
  except: return False ## has to be a string

def get_number_extended_to_18_decimals(s, decimals, do_strip=False):
  if do_strip:
    s = s.strip()
  
  if '.' in s:
    normal_part = s.split('.')[0]
    if len(s.split('.')[1]) > decimals or len(s.split('.')[1]) == 0: ## more decimal digit than allowed or no decimal digit after dot
      return None
    decimals_part = s.split('.')[1][:decimals]
    decimals_part += '0' * (18 - len(decimals_part))
    return int(normal_part + decimals_part)
  else:
    return int(s) * 10 ** 18

def fix_numstr_decimals(num_str, decimals):
  if len(num_str) <= 18:
    num_str = '0' * (18 - len(num_str)) + num_str
    num_str = '0.' + num_str
    if decimals < 18:
      num_str = num_str[:-18+decimals]
  else:
    num_str = num_str[:-18] + '.' + num_str[-18:]
    if decimals < 18:
      num_str = num_str[:-18+decimals]
  if num_str[-1] == '.': num_str = num_str[:-1] ## remove trailing dot
  return num_str

def get_event_str(event, event_type, inscription_id):
  global ticks
  if event_type == "deploy-inscribe":
    decimals_int = int(event["decimals"])
    res = "deploy-inscribe;"
    res += inscription_id + ";"
    res += event["deployer_pkScript"] + ";"
    res += event["tick"] + ";"
    res += event["original_tick"] + ";"
    res += fix_numstr_decimals(event["max_supply"], decimals_int) + ";"
    res += event["decimals"] + ";"
    res += fix_numstr_decimals(event["limit_per_mint"], decimals_int) + ";"
    res += event["is_self_mint"]
    return res
  elif event_type == "mint-inscribe":
    decimals_int = ticks[event["tick"]][2]
    res = "mint-inscribe;"
    res += inscription_id + ";"
    res += event["minted_pkScript"] + ";"
    res += event["tick"] + ";"
    res += event["original_tick"] + ";"
    res += fix_numstr_decimals(event["amount"], decimals_int) + ";"
    res += event["parent_id"]
    return res
  elif event_type == "transfer-inscribe":
    decimals_int = ticks[event["tick"]][2]
    res = "transfer-inscribe;"
    res += inscription_id + ";"
    res += event["source_pkScript"] + ";"
    res += event["tick"] + ";"
    res += event["original_tick"] + ";"
    res += fix_numstr_decimals(event["amount"], decimals_int)
    return res
  elif event_type == "transfer-transfer":
    decimals_int = ticks[event["tick"]][2]
    res = "transfer-transfer;"
    res += inscription_id + ";"
    res += event["source_pkScript"] + ";"
    if event["spent_pkScript"] is not None:
      res += event["spent_pkScript"] + ";"
    else:
      res += ";"
    res += event["tick"] + ";"
    res += event["original_tick"] + ";"
    res += fix_numstr_decimals(event["amount"], decimals_int)
    return res
  else:
    print("EVENT TYPE ERROR!!")
    exit(1)

def get_sha256_hash(s):
  return hashlib.sha256(s.encode('utf-8')).hexdigest()





## caches
transfer_inscribe_event_cache = {} ## single use cache for transfer inscribe events
def get_transfer_inscribe_event(inscription_id):
  global transfer_inscribe_event_cache, event_types
  if inscription_id in transfer_inscribe_event_cache:
    event = transfer_inscribe_event_cache[inscription_id]
    del transfer_inscribe_event_cache[inscription_id]
    return event
  cur.execute('''select event from brc20_events where event_type = %s and inscription_id = %s;''', (event_types["transfer-inscribe"], inscription_id,))
  return cur.fetchall()[0][0]

def save_transfer_inscribe_event(inscription_id, event):
  transfer_inscribe_event_cache[inscription_id] = event

balance_cache = {}
def get_last_balance(pkscript, tick):
  global balance_cache
  cache_key = pkscript + tick
  if cache_key in balance_cache:
    return balance_cache[cache_key]
  cur.execute('''select overall_balance, available_balance from brc20_historic_balances where pkscript = %s and tick = %s order by block_height desc, id desc limit 1;''', (pkscript, tick))
  row = cur.fetchone()
  balance_obj = None
  if row is None:
    balance_obj = {
      "overall_balance": 0,
      "available_balance": 0
    }
  else:
    balance_obj = {
      "overall_balance": row[0],
      "available_balance": row[1]
    }
  balance_cache[cache_key] = balance_obj
  return balance_obj

def check_available_balance(pkScript, tick, amount):
  last_balance = get_last_balance(pkScript, tick)
  available_balance = last_balance["available_balance"]
  if available_balance < amount: return False
  return True


transfer_validity_cache = {}
def is_used_or_invalid(inscription_id):
  global event_types, transfer_validity_cache
  if inscription_id in transfer_validity_cache:
    return transfer_validity_cache[inscription_id] != 1
  cur.execute('''select coalesce(sum(case when event_type = %s then 1 else 0 end), 0) as inscr_cnt,
                        coalesce(sum(case when event_type = %s then 1 else 0 end), 0) as transfer_cnt
                        from brc20_events where inscription_id = %s;''', (event_types["transfer-inscribe"], event_types["transfer-transfer"], inscription_id,))
  row = cur.fetchall()[0]
  if row[0] != 1:
    transfer_validity_cache[inscription_id] = 0 ## invalid transfer (no inscribe event)
    return True
  elif row[1] != 0:
    transfer_validity_cache[inscription_id] = -1 ## used
    return True
  else:
    transfer_validity_cache[inscription_id] = 1 ## valid
    return False

def set_transfer_as_used(inscription_id):
  global transfer_validity_cache
  transfer_validity_cache[inscription_id] = -1

def set_transfer_as_valid(inscription_id):
  global transfer_validity_cache
  transfer_validity_cache[inscription_id] = 1

def reset_caches():
  global balance_cache, transfer_inscribe_event_cache, ticks, transfer_validity_cache
  balance_cache = {}
  transfer_inscribe_event_cache = {}
  transfer_validity_cache = {}
  sttm = time.time()
  cur.execute('''select tick, remaining_supply, limit_per_mint, decimals, is_self_mint, deploy_inscription_id from brc20_tickers;''')
  ticks_ = cur.fetchall()
  ticks = {}
  for t in ticks_:
    ticks[t[0]] = [t[1], t[2], t[3], t[4], t[5]]
  print("Ticks refreshed in " + str(time.time() - sttm) + " seconds")

block_start_max_event_id = None
brc20_events_insert_sql = '''insert into brc20_events (id, event_type, block_height, inscription_id, event) values '''
brc20_events_insert_cache = []
brc20_tickers_insert_sql = '''insert into brc20_tickers (tick, original_tick, max_supply, decimals, limit_per_mint, remaining_supply, block_height, is_self_mint, deploy_inscription_id) values '''
brc20_tickers_insert_cache = []
brc20_tickers_remaining_supply_update_sql = '''update brc20_tickers set remaining_supply = remaining_supply - %s where tick = %s;'''
brc20_tickers_remaining_supply_update_cache = {}
brc20_tickers_burned_supply_update_sql = '''update brc20_tickers set burned_supply = burned_supply + %s where tick = %s;'''
brc20_tickers_burned_supply_update_cache = {}
brc20_historic_balances_insert_sql = '''insert into brc20_historic_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id) values '''
brc20_historic_balances_insert_cache = []

def deploy_inscribe(block_height, inscription_id, deployer_pkScript, deployer_wallet, tick, original_tick, max_supply, decimals, limit_per_mint, is_self_mint):
  global ticks, in_commit, block_events_str, event_types

  event = {
    "deployer_pkScript": deployer_pkScript,
    "deployer_wallet": deployer_wallet,
    "tick": tick,
    "original_tick": original_tick,
    "max_supply": str(max_supply),
    "decimals": str(decimals),
    "limit_per_mint": str(limit_per_mint),
    "is_self_mint": str(is_self_mint)
  }
  block_events_str += get_event_str(event, "deploy-inscribe", inscription_id) + EVENT_SEPARATOR
  event_id = block_start_max_event_id + len(brc20_events_insert_cache) + 1
  brc20_events_insert_cache.append((event_id, event_types["deploy-inscribe"], block_height, inscription_id, json.dumps(event)))
  
  brc20_tickers_insert_cache.append((tick, original_tick, max_supply, decimals, limit_per_mint, max_supply, block_height, is_self_mint == "true", inscription_id))
  
  ticks[tick] = [max_supply, limit_per_mint, decimals, is_self_mint == "true", inscription_id]

def mint_inscribe(block_height, inscription_id, minted_pkScript, minted_wallet, tick, original_tick, amount, parent_id):
  global ticks, in_commit, block_events_str, event_types

  event = {
    "minted_pkScript": minted_pkScript,
    "minted_wallet": minted_wallet,
    "tick": tick,
    "original_tick": original_tick,
    "amount": str(amount),
    "parent_id": parent_id
  }
  block_events_str += get_event_str(event, "mint-inscribe", inscription_id) + EVENT_SEPARATOR
  event_id = block_start_max_event_id + len(brc20_events_insert_cache) + 1
  brc20_events_insert_cache.append((event_id, event_types["mint-inscribe"], block_height, inscription_id, json.dumps(event)))
  brc20_tickers_remaining_supply_update_cache[tick] = brc20_tickers_remaining_supply_update_cache.get(tick, 0) + amount

  last_balance = get_last_balance(minted_pkScript, tick)
  last_balance["overall_balance"] += amount
  last_balance["available_balance"] += amount
  brc20_historic_balances_insert_cache.append((minted_pkScript, minted_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))
  
  ticks[tick][0] -= amount

def transfer_inscribe(block_height, inscription_id, source_pkScript, source_wallet, tick, original_tick, amount):
  global in_commit, block_events_str, event_types

  event = {
    "source_pkScript": source_pkScript,
    "source_wallet": source_wallet,
    "tick": tick,
    "original_tick": original_tick,
    "amount": str(amount)
  }
  block_events_str += get_event_str(event, "transfer-inscribe", inscription_id) + EVENT_SEPARATOR
  event_id = block_start_max_event_id + len(brc20_events_insert_cache) + 1
  brc20_events_insert_cache.append((event_id, event_types["transfer-inscribe"], block_height, inscription_id, json.dumps(event)))
  set_transfer_as_valid(inscription_id)
  
  last_balance = get_last_balance(source_pkScript, tick)
  last_balance["available_balance"] -= amount
  brc20_historic_balances_insert_cache.append((source_pkScript, source_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))
  
  save_transfer_inscribe_event(inscription_id, event)

def transfer_transfer_normal(block_height, inscription_id, spent_pkScript, spent_wallet, tick, original_tick, amount, using_tx_id):
  global in_commit, block_events_str, event_types

  inscribe_event = get_transfer_inscribe_event(inscription_id)
  source_pkScript = inscribe_event["source_pkScript"]
  source_wallet = inscribe_event["source_wallet"]
  event = {
    "source_pkScript": source_pkScript,
    "source_wallet": source_wallet,
    "spent_pkScript": spent_pkScript,
    "spent_wallet": spent_wallet,
    "tick": tick,
    "original_tick": original_tick,
    "amount": str(amount),
    "using_tx_id": str(using_tx_id)
  }
  block_events_str += get_event_str(event, "transfer-transfer", inscription_id) + EVENT_SEPARATOR
  event_id = block_start_max_event_id + len(brc20_events_insert_cache) + 1
  brc20_events_insert_cache.append((event_id, event_types["transfer-transfer"], block_height, inscription_id, json.dumps(event)))
  set_transfer_as_used(inscription_id)
  
  last_balance = get_last_balance(source_pkScript, tick)
  last_balance["overall_balance"] -= amount
  brc20_historic_balances_insert_cache.append((source_pkScript, source_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))
  
  if spent_pkScript != source_pkScript:
    last_balance = get_last_balance(spent_pkScript, tick)
  last_balance["overall_balance"] += amount
  last_balance["available_balance"] += amount
  brc20_historic_balances_insert_cache.append((spent_pkScript, spent_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, -1 * event_id)) ## negated to make a unique event_id
  
  if spent_pkScript == '6a':
    brc20_tickers_burned_supply_update_cache[tick] = brc20_tickers_burned_supply_update_cache.get(tick, 0) + amount

def transfer_transfer_spend_to_fee(block_height, inscription_id, tick, original_tick, amount, using_tx_id):
  global in_commit, block_events_str, event_types

  inscribe_event = get_transfer_inscribe_event(inscription_id)
  source_pkScript = inscribe_event["source_pkScript"]
  source_wallet = inscribe_event["source_wallet"]
  event = {
    "source_pkScript": source_pkScript,
    "source_wallet": source_wallet,
    "spent_pkScript": None,
    "spent_wallet": None,
    "tick": tick,
    "original_tick": original_tick,
    "amount": str(amount),
    "using_tx_id": str(using_tx_id)
  }
  block_events_str += get_event_str(event, "transfer-transfer", inscription_id) + EVENT_SEPARATOR
  event_id = block_start_max_event_id + len(brc20_events_insert_cache) + 1
  brc20_events_insert_cache.append((event_id, event_types["transfer-transfer"], block_height, inscription_id, json.dumps(event)))
  set_transfer_as_used(inscription_id)
  
  last_balance = get_last_balance(source_pkScript, tick)
  last_balance["available_balance"] += amount
  brc20_historic_balances_insert_cache.append((source_pkScript, source_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))


def update_event_hashes(block_height):
  global block_events_str
  if len(block_events_str) > 0 and block_events_str[-1] == EVENT_SEPARATOR: block_events_str = block_events_str[:-1] ## remove last separator
  block_event_hash = get_sha256_hash(block_events_str)
  cumulative_event_hash = None
  cur.execute('''select cumulative_event_hash from brc20_cumulative_event_hashes where block_height = %s;''', (block_height - 1,))
  if cur.rowcount == 0:
    cumulative_event_hash = block_event_hash
  else:
    cumulative_event_hash = get_sha256_hash(cur.fetchone()[0] + block_event_hash)
  cur.execute('''INSERT INTO brc20_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) VALUES (%s, %s, %s);''', (block_height, block_event_hash, cumulative_event_hash))

def index_block(block_height, current_block_hash):
  global ticks, block_events_str, block_start_max_event_id, brc20_events_insert_cache, brc20_tickers_insert_cache, brc20_tickers_remaining_supply_update_cache, brc20_tickers_burned_supply_update_cache, brc20_historic_balances_insert_cache, in_commit
  print("Indexing block " + str(block_height))
  block_events_str = ""

  if block_height < first_brc20_height:
    print("Block height is before first brc20 height, skipping")
    update_event_hashes(block_height)
    cur.execute('''INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, current_block_hash))
    return
  
  cur_metaprotocol.execute('''SELECT ot.id, ot.inscription_id, ot.old_satpoint, ot.new_pkscript, ot.new_wallet, ot.sent_as_fee, oc."content", oc.content_type, onti.parent_id
                              FROM ord_transfers ot
                              LEFT JOIN ord_content oc ON ot.inscription_id = oc.inscription_id
                              LEFT JOIN ord_number_to_id onti ON ot.inscription_id = onti.inscription_id
                              WHERE ot.block_height = %s 
                                 AND onti.cursed_for_brc20 = false
                                 AND oc."content" is not null AND oc."content"->>'p'='brc-20'
                              ORDER BY ot.id asc;''', (block_height,))
  transfers = cur_metaprotocol.fetchall()
  if len(transfers) == 0:
    print("No transfers found for block " + str(block_height))
    update_event_hashes(block_height)
    cur.execute('''INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, current_block_hash))
    return
  print("Transfer count: ", len(transfers))

  cur.execute('''select COALESCE(max(id), -1) from brc20_events;''')
  block_start_max_event_id = cur.fetchone()[0]
  brc20_events_insert_cache = []
  brc20_tickers_insert_cache = []
  brc20_tickers_remaining_supply_update_cache = {}
  brc20_tickers_burned_supply_update_cache = {}
  brc20_historic_balances_insert_cache = []
  
  idx = 0
  for transfer in transfers:
    idx += 1
    if idx % 100 == 0:
      print(idx, '/', len(transfers))
    
    tx_id, inscr_id, old_satpoint, new_pkScript, new_addr, sent_as_fee, js, content_type, parent_id = transfer
    if parent_id is None: parent_id = ""
    
    if sent_as_fee and old_satpoint == '': continue ## inscribed as fee

    if content_type is None: continue ## invalid inscription
    try: content_type = codecs.decode(content_type, "hex").decode('utf-8')
    except KeyboardInterrupt:
      raise KeyboardInterrupt
    except: pass
    content_type = content_type.split(';')[0]
    if content_type != 'application/json' and content_type != 'text/plain': continue ## invalid inscription

    if "tick" not in js: continue ## invalid inscription
    if "op" not in js: continue ## invalid inscription
    tick = js["tick"]
    original_tick = tick
    try: tick = tick.lower()
    except KeyboardInterrupt:
      raise KeyboardInterrupt
    except: continue ## invalid tick
    original_tick_len = utf8len(original_tick)
    if original_tick_len != 4 and original_tick_len != 5: continue ## invalid tick
    
    # handle deploy
    if js["op"] == 'deploy' and old_satpoint == '':
      if "max" not in js: continue ## invalid inscription
      if tick in ticks: continue ## already deployed
      decimals = 18
      if "dec" in js:
        if not is_positive_number(js["dec"]): continue ## invalid decimals
        else:
          decimals = int(js["dec"])
      if decimals > 18: continue ## invalid decimals
      max_supply = js["max"]
      if not is_positive_number_with_dot(max_supply): continue
      else:
        max_supply = get_number_extended_to_18_decimals(max_supply, decimals)
        if max_supply is None: continue ## invalid max supply
        if max_supply > (2**64-1) * (10**18) or max_supply < 0: continue ## invalid max supply
      limit_per_mint = max_supply
      if "lim" in js:
        if not is_positive_number_with_dot(js["lim"]): continue ## invalid limit per mint
        else:
          limit_per_mint = get_number_extended_to_18_decimals(js["lim"], decimals)
          if limit_per_mint is None: continue ## invalid limit per mint
          if limit_per_mint > (2**64-1) * (10**18) or limit_per_mint <= 0: continue ## invalid limit per mint
      is_self_mint = "false"
      if original_tick_len == 5: ## this is a self-mint token
        if block_height < SELF_MINT_ENABLE_HEIGHT: continue ## self-mint not enabled yet
        if "self_mint" not in js: continue ## invalid inscription
        if js["self_mint"] != "true": continue ## invalid inscription
        is_self_mint = "true"
        if max_supply == 0: 
          max_supply = (2**64-1) * (10**18) ## infinite(ish) mint
          if limit_per_mint == 0:
            limit_per_mint = (2**64-1) * (10**18)
      if max_supply == 0: continue ## invalid max supply
      deploy_inscribe(block_height, inscr_id, new_pkScript, new_addr, tick, original_tick, max_supply, decimals, limit_per_mint, is_self_mint)
    
    # handle mint
    if js["op"] == 'mint' and old_satpoint == '':
      if "amt" not in js: continue ## invalid inscription
      if tick not in ticks: continue ## not deployed
      amount = js["amt"]
      if not is_positive_number_with_dot(amount): continue ## invalid amount
      else:
        amount = get_number_extended_to_18_decimals(amount, ticks[tick][2])
        if amount is None: continue ## invalid amount
        if amount > (2**64-1) * (10**18) or amount <= 0: continue ## invalid amount
      if ticks[tick][0] <= 0: continue ## mint ended
      if ticks[tick][1] is not None and amount > ticks[tick][1]: continue ## mint too much
      if amount > ticks[tick][0]: ## mint remaining tokens
        amount = ticks[tick][0]
      if ticks[tick][3]: ## self-mint
        ## check parent token
        if ticks[tick][4] != parent_id: continue ## invalid parent token
      mint_inscribe(block_height, inscr_id, new_pkScript, new_addr, tick, original_tick, amount, parent_id)
    
    # handle transfer
    if js["op"] == 'transfer':
      if "amt" not in js: continue ## invalid inscription
      if tick not in ticks: continue ## not deployed
      amount = js["amt"]
      if not is_positive_number_with_dot(amount): continue ## invalid amount
      else:
        amount = get_number_extended_to_18_decimals(amount, ticks[tick][2])
        if amount is None: continue ## invalid amount
        if amount > (2**64-1) * (10**18) or amount <= 0: continue ## invalid amount
      ## check if available balance is enough
      if old_satpoint == '':
        if not check_available_balance(new_pkScript, tick, amount): continue ## not enough available balance
        transfer_inscribe(block_height, inscr_id, new_pkScript, new_addr, tick, original_tick, amount)
      else:
        if is_used_or_invalid(inscr_id): continue ## already used or invalid
        if sent_as_fee: transfer_transfer_spend_to_fee(block_height, inscr_id, tick, original_tick, amount, tx_id)
        else: transfer_transfer_normal(block_height, inscr_id, new_pkScript, new_addr, tick, original_tick, amount, tx_id)
  
  cur.execute("BEGIN;")
  in_commit = True
  print("inserting events...")
  execute_batch_insert(brc20_events_insert_sql, brc20_events_insert_cache, 1000)
  print("inserting tickers...")
  execute_batch_insert(brc20_tickers_insert_sql, brc20_tickers_insert_cache, 1000)
  print("updating tickers remaining_supply...")
  for tick in brc20_tickers_remaining_supply_update_cache:
    cur.execute(brc20_tickers_remaining_supply_update_sql, (brc20_tickers_remaining_supply_update_cache[tick], tick))
  print("updating tickers burned_supply...")
  for tick in brc20_tickers_burned_supply_update_cache:
    cur.execute(brc20_tickers_burned_supply_update_sql, (brc20_tickers_burned_supply_update_cache[tick], tick))
  print("inserting historic balances...")
  execute_batch_insert(brc20_historic_balances_insert_sql, brc20_historic_balances_insert_cache, 1000)
  update_event_hashes(block_height)
  # end of block
  cur.execute('''INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, current_block_hash))
  print("committing...")
  cur.execute("COMMIT;")
  in_commit = False
  conn.commit()
  print("ALL DONE")

def execute_batch_insert(sql_start, cache, batch_size):
  if len(cache) > 0:
    single_elem_cnt = len(cache[0])
    single_insert_sql_part = '(' + ','.join(['%s' for _ in range(single_elem_cnt)]) + ')'
    for i in range(0, len(cache), batch_size):
      elem_cnt = min(batch_size, len(cache) - i)
      sql = sql_start + ','.join([single_insert_sql_part for _ in range(elem_cnt)]) + ';'
      cur.execute(sql, [elem for sublist in cache[i:i+batch_size] for elem in sublist])

      

def check_for_reorg():
  cur.execute('select block_height, block_hash from brc20_block_hashes order by block_height desc limit 1;')
  if cur.rowcount == 0: return None ## nothing indexed yet
  last_block = cur.fetchone()

  cur_metaprotocol.execute('select block_height, block_hash from block_hashes where block_height = %s;', (last_block[0],))
  last_block_ord = cur_metaprotocol.fetchone()
  if last_block_ord[1] == last_block[1]: return None ## last block hashes are the same, no reorg

  print("REORG DETECTED!!")
  cur.execute('select block_height, block_hash from brc20_block_hashes order by block_height desc limit 10;')
  hashes = cur.fetchall() ## get last 10 hashes
  for h in hashes:
    cur_metaprotocol.execute('select block_height, block_hash from block_hashes where block_height = %s;', (h[0],))
    block = cur_metaprotocol.fetchone()
    if block[1] == h[1]: ## found reorg height by a matching hash
      print("REORG HEIGHT FOUND: " + str(h[0]))
      return h[0]
  
  ## bigger than 10 block reorg is not supported by ord
  print("CRITICAL ERROR!!")
  sys.exit(1)

def reorg_fix(reorg_height):
  global event_types
  cur.execute('begin;')
  cur.execute('delete from brc20_tickers where block_height > %s;', (reorg_height,)) ## delete new tickers
  ## fetch mint events for reverting remaining_supply in other tickers
  cur.execute('''select event from brc20_events where event_type = %s and block_height > %s;''', (event_types["mint-inscribe"], reorg_height,))
  rows = cur.fetchall()
  tick_changes = {}
  for row in rows:
    event = row[0]
    tick = event["tick"]
    amount = int(event["amount"])
    if tick not in tick_changes:
      tick_changes[tick] = 0
    tick_changes[tick] += amount
  for tick in tick_changes:
    cur.execute('''update brc20_tickers set remaining_supply = remaining_supply + %s where tick = %s;''', (tick_changes[tick], tick))
  cur.execute('delete from brc20_historic_balances where block_height > %s;', (reorg_height,)) ## delete new balances
  cur.execute('delete from brc20_events where block_height > %s;', (reorg_height,)) ## delete new events
  cur.execute('delete from brc20_cumulative_event_hashes where block_height > %s;', (reorg_height,)) ## delete new bitmaps
  cur.execute("SELECT setval('brc20_cumulative_event_hashes_id_seq', max(id)) from brc20_cumulative_event_hashes;") ## reset id sequence
  cur.execute("SELECT setval('brc20_tickers_id_seq', max(id)) from brc20_tickers;") ## reset id sequence
  cur.execute("SELECT setval('brc20_historic_balances_id_seq', max(id)) from brc20_historic_balances;") ## reset id sequence
  cur.execute("SELECT setval('brc20_events_id_seq', max(id)) from brc20_events;") ## reset id sequence
  cur.execute('delete from brc20_block_hashes where block_height > %s;', (reorg_height,)) ## delete new block hashes
  cur.execute("SELECT setval('brc20_block_hashes_id_seq', max(id)) from brc20_block_hashes;") ## reset id sequence
  cur.execute('commit;')
  reset_caches()

def check_if_there_is_residue_from_last_run():
  cur.execute('''select max(block_height) from brc20_block_hashes;''')
  row = cur.fetchone()
  current_block = None
  if row[0] is None: current_block = first_inscription_height
  else: current_block = row[0] + 1
  residue_found = False
  cur.execute('''select coalesce(max(block_height), -1) from brc20_events;''')
  if cur.rowcount != 0 and cur.fetchone()[0] >= current_block:
    residue_found = True
    print("residue on brc20_events")
  cur.execute('''select coalesce(max(block_height), -1) from brc20_historic_balances;''')
  if cur.rowcount != 0 and cur.fetchone()[0] >= current_block:
    residue_found = True
    print("residue on historic balances")
  cur.execute('''select coalesce(max(block_height), -1) from brc20_tickers;''')
  if cur.rowcount != 0 and cur.fetchone()[0] >= current_block:
    residue_found = True
    print("residue on tickers")
  cur.execute('''select coalesce(max(block_height), -1) from brc20_cumulative_event_hashes;''')
  if cur.rowcount != 0 and cur.fetchone()[0] >= current_block:
    residue_found = True
    print("residue on cumulative hashes")
  if residue_found:
    print("There is residue from last run, rolling back to " + str(current_block - 1))
    reorg_fix(current_block - 1)
    print("Rolled back to " + str(current_block - 1))
    return

def check_if_there_is_residue_on_extra_tables_from_last_run():
  cur.execute('''select max(block_height) from brc20_extras_block_hashes;''')
  row = cur.fetchone()
  current_block = None
  if row[0] is None: current_block = first_inscription_height
  else: current_block = row[0] + 1
  residue_found = False
  cur.execute('''select coalesce(max(block_height), -1) from brc20_unused_tx_inscrs;''')
  if cur.rowcount != 0 and cur.fetchone()[0] >= current_block:
    residue_found = True
    print("residue on brc20_unused_tx_inscrs")
  cur.execute('''select coalesce(max(block_height), -1) from brc20_current_balances;''')
  if cur.rowcount != 0 and cur.fetchone()[0] >= current_block:
    residue_found = True
    print("residue on brc20_current_balances")
  if residue_found:
    print("There is residue on extra tables from last run, rolling back to " + str(current_block - 1))
    reorg_on_extra_tables(current_block - 1)
    print("Rolled back to " + str(current_block - 1))
    return

cur.execute('select event_type_name, event_type_id from brc20_event_types;')
event_types = {}
for row in cur.fetchall():
  event_types[row[0]] = row[1]

event_types_rev = {}
for key in event_types:
  event_types_rev[event_types[key]] = key

sttm = time.time()
cur.execute('''select tick, remaining_supply, limit_per_mint, decimals, is_self_mint, deploy_inscription_id from brc20_tickers;''')
ticks_ = cur.fetchall()
ticks = {}
for t in ticks_:
  ticks[t[0]] = [t[1], t[2], t[3], t[4], t[5]]
print("Ticks refreshed in " + str(time.time() - sttm) + " seconds")

def reindex_cumulative_hashes():
  global event_types_rev, ticks
  cur.execute('''delete from brc20_cumulative_event_hashes;''')
  cur.execute('''select min(block_height), max(block_height) from brc20_block_hashes;''')
  row = cur.fetchone()
  min_block = row[0]
  max_block = row[1]

  sttm = time.time()
  cur.execute('''select tick, remaining_supply, limit_per_mint, decimals, is_self_mint, deploy_inscription_id from brc20_tickers;''')
  ticks_ = cur.fetchall()
  ticks = {}
  for t in ticks_:
    ticks[t[0]] = [t[1], t[2], t[3], t[4], t[5]]
  print("Ticks refreshed in " + str(time.time() - sttm) + " seconds")

  print("Reindexing cumulative hashes from " + str(min_block) + " to " + str(max_block))
  for block_height in range(min_block, max_block + 1):
    print("Reindexing block " + str(block_height))
    block_events_str = ""
    cur.execute('''select event, event_type, inscription_id from brc20_events where block_height = %s order by id asc;''', (block_height,))
    rows = cur.fetchall()
    for row in rows:
      event = row[0]
      event_type = event_types_rev[row[1]]
      inscription_id = row[2]
      block_events_str += get_event_str(event, event_type, inscription_id) + EVENT_SEPARATOR
    update_event_hashes(block_height)

def fix_db_from_version(version):
  if version == 4:
    print("Fixing db from version 4")
    ## change type of original_tick in brc20_tickers to text
    cur.execute('''alter table brc20_tickers alter column original_tick type text;''')
    reorg_fix(SELF_MINT_ENABLE_HEIGHT - 1)
  else:
    print("Unknown db version, cannot fix db.")
    exit(1)

cur.execute('select db_version from brc20_indexer_version;')
if cur.rowcount == 0:
  print("Indexer version not found, db needs to be recreated from scratch, please run reset_init.py")
  exit(1)
else:
  db_version = cur.fetchone()[0]
  if db_version != DB_VERSION:
    print("Indexer version mismatch!!")
    if db_version not in RECOVERABLE_DB_VERSIONS:
      print("This version (" + str(db_version) + ") cannot be fixed, please run reset_init.py")
      exit(1)
    else:
      print("This version (" + str(db_version) + ") can be fixed, fixing in 5 secs...")
      time.sleep(5)
      fix_db_from_version(db_version)
      cur.execute('update brc20_indexer_version set indexer_version = %s, db_version = %s;', (INDEXER_VERSION, DB_VERSION,))
      print("Fixed.")

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
    except KeyboardInterrupt:
      raise KeyboardInterrupt
    except:
      print("Error while reporting hashes to metaprotocol indexer indexer, retrying...")
    time.sleep(1)
  print("Error while reporting hashes to metaprotocol indexer indexer, giving up.")

def report_hashes(block_height):
  global report_to_indexer
  if not report_to_indexer:
    print("Reporting to metaprotocol indexer is disabled.")
    return
  cur.execute('''select block_event_hash, cumulative_event_hash from brc20_cumulative_event_hashes where block_height = %s;''', (block_height,))
  row = cur.fetchone()
  block_event_hash = row[0]
  cumulative_event_hash = row[1]
  cur.execute('''select block_hash from brc20_block_hashes where block_height = %s;''', (block_height,))
  block_hash = cur.fetchone()[0]
  to_send = {
    "name": report_name,
    "type": "brc20",
    "node_type": "full_node",
    "network_type": network_type,
    "version": INDEXER_VERSION,
    "db_version": DB_VERSION,
    "event_hash_version": EVENT_HASH_VERSION,
    "block_height": block_height,
    "block_hash": block_hash,
    "block_event_hash": block_event_hash,
    "cumulative_event_hash": cumulative_event_hash
  }
  print("Sending hashes to metaprotocol indexer indexer...")
  try_to_report_with_retries(to_send)

def reorg_on_extra_tables(reorg_height):
  cur.execute('begin;')
  cur.execute('delete from brc20_current_balances where block_height > %s RETURNING pkscript, tick;', (reorg_height,)) ## delete new balances
  rows = cur.fetchall()
  ## fetch balances of deleted rows for reverting balances
  for r in rows:
    pkscript = r[0]
    tick = r[1]
    cur.execute(''' select overall_balance, available_balance, wallet, block_height
                    from brc20_historic_balances 
                    where block_height <= %s and pkscript = %s and tick = %s
                    order by id desc
                    limit 1;''', (reorg_height, pkscript, tick))
    if cur.rowcount != 0:
      balance = cur.fetchone()
      cur.execute('''insert into brc20_current_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height)
                      values (%s, %s, %s, %s, %s, %s);''', (pkscript, balance[2], tick, balance[0], balance[1], balance[3]))
  
  cur.execute('truncate table brc20_unused_tx_inscrs restart identity;')
  cur.execute('''with tempp as (
                  select inscription_id, event, id, block_height
                  from brc20_events
                  where event_type = %s and block_height <= %s
                ), tempp2 as (
                  select inscription_id, event
                  from brc20_events
                  where event_type = %s and block_height <= %s
                )
                select t.event, t.id, t.block_height, t.inscription_id
                from tempp t
                left join tempp2 t2 on t.inscription_id = t2.inscription_id
                where t2.inscription_id is null;''', (event_types['transfer-inscribe'], reorg_height, event_types['transfer-transfer'], reorg_height))
  rows = cur.fetchall()
  for row in rows:
    new_event = row[0]
    event_id = row[1]
    block_height = row[2]
    inscription_id = row[3]
    cur.execute('''INSERT INTO brc20_unused_tx_inscrs (inscription_id, tick, amount, current_holder_pkscript, current_holder_wallet, event_id, block_height)
                    VALUES (%s, %s, %s, %s, %s, %s, %s)''', 
                    (inscription_id, new_event["tick"], int(new_event["amount"]), new_event["source_pkScript"], new_event["source_wallet"], event_id, block_height))

  cur.execute('delete from brc20_extras_block_hashes where block_height > %s;', (reorg_height,)) ## delete new block hashes
  cur.execute("SELECT setval('brc20_extras_block_hashes_id_seq', max(id)) from brc20_extras_block_hashes;") ## reset id sequence
  cur.execute('commit;')

def initial_index_of_extra_tables():
  cur.execute('begin;')
  print("resetting brc20_unused_tx_inscrs")
  cur.execute('truncate table brc20_unused_tx_inscrs restart identity;')
  print("selecting unused txes")
  cur.execute('''with tempp as (
                  select inscription_id, event, id, block_height
                  from brc20_events
                  where event_type = %s
                ), tempp2 as (
                  select inscription_id, event
                  from brc20_events
                  where event_type = %s
                )
                select t.event, t.id, t.block_height, t.inscription_id
                from tempp t
                left join tempp2 t2 on t.inscription_id = t2.inscription_id
                where t2.inscription_id is null;''', (event_types['transfer-inscribe'], event_types['transfer-transfer']))
  rows = cur.fetchall()
  print("inserting unused txes")
  idx = 0
  for row in rows:
    idx += 1
    if idx % 200 == 0: print(idx, '/', len(rows))
    new_event = row[0]
    event_id = row[1]
    block_height = row[2]
    inscription_id = row[3]
    cur.execute('''INSERT INTO brc20_unused_tx_inscrs (inscription_id, tick, amount, current_holder_pkscript, current_holder_wallet, event_id, block_height)
                    VALUES (%s, %s, %s, %s, %s, %s, %s)''', 
                    (inscription_id, new_event["tick"], int(new_event["amount"]), new_event["source_pkScript"], new_event["source_wallet"], event_id, block_height))
  
  print("resetting brc20_current_balances")
  cur.execute('truncate table brc20_current_balances restart identity;')
  print("selecting current balances")
  cur.execute('''with tempp as (
                    select max(id) as id
                    from brc20_historic_balances
                    group by pkscript, tick
                  )
                  select bhb.pkscript, bhb.tick, bhb.overall_balance, bhb.available_balance, bhb.wallet, bhb.block_height
                  from tempp t
                  left join brc20_historic_balances bhb on bhb.id = t.id
                  order by bhb.pkscript asc, bhb.tick asc;''')
  rows = cur.fetchall()
  print("inserting current balances")
  idx = 0
  for r in rows:
    idx += 1
    if idx % 200 == 0: print(idx, '/', len(rows))
    pkscript = r[0]
    tick = r[1]
    overall_balance = r[2]
    available_balance = r[3]
    wallet = r[4]
    block_height = r[5]
    cur.execute('''insert into brc20_current_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height)
                   values (%s, %s, %s, %s, %s, %s);''', (pkscript, wallet, tick, overall_balance, available_balance, block_height))
  
  print("resetting brc20_extras_block_hashes")
  cur.execute('truncate table brc20_extras_block_hashes restart identity;')
  print("inserting brc20_extras_block_hashes")
  cur.execute('''select block_height, block_hash from brc20_block_hashes order by block_height asc;''')
  rows = cur.fetchall()
  idx = 0
  for row in rows:
    idx += 1
    if idx % 200 == 0: print(idx, '/', len(rows))
    block_height = row[0]
    block_hash = row[1]
    cur.execute('''INSERT INTO brc20_extras_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, block_hash))

  cur.execute('commit;')

def index_extra_tables(block_height, block_hash):
  ebh_current_height = 0
  cur.execute('select max(block_height) as current_ebh_height from brc20_extras_block_hashes;')
  if cur.rowcount > 0:
    res = cur.fetchone()[0]
    if res is not None:
      ebh_current_height = res
  if ebh_current_height >= block_height:
    print("reorg detected on extra tables, rolling back to: " + str(block_height))
    reorg_on_extra_tables(block_height - 1)
  
  print("updating extra tables for block: " + str(block_height))

  cur.execute('''select pkscript, wallet, tick, overall_balance, available_balance 
                 from brc20_historic_balances 
                 where block_height = %s 
                 order by id asc;''', (block_height,))
  balance_changes = cur.fetchall()
  if len(balance_changes) == 0:
    print("No balance_changes found for block " + str(block_height))
  else:
    balance_changes_map = {}
    for balance_change in balance_changes:
      pkscript = balance_change[0]
      tick = balance_change[2]
      key = pkscript + '_' + tick
      balance_changes_map[key] = balance_change
    print("Balance_change count: ", len(balance_changes_map))
    idx = 0
    for key in balance_changes_map:
      new_balance = balance_changes_map[key]
      idx += 1
      if idx % 200 == 0: print(idx, '/', len(balance_changes_map))
      cur.execute('''INSERT INTO brc20_current_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height) VALUES (%s, %s, %s, %s, %s, %s)
                     ON CONFLICT (pkscript, tick) 
                     DO UPDATE SET overall_balance = EXCLUDED.overall_balance
                                , available_balance = EXCLUDED.available_balance
                                , block_height = EXCLUDED.block_height;''', new_balance + (block_height,))
    
  cur.execute('''select event, id, event_type, inscription_id 
                 from brc20_events where block_height = %s and (event_type = %s or event_type = %s) 
                 order by id asc;''', (block_height, event_types['transfer-inscribe'], event_types['transfer-transfer'],))
  events = cur.fetchall()
  if len(events) == 0:
    print("No events found for block " + str(block_height))
  else:
    print("Events count: ", len(events))
    idx = 0
    for row in events:
      new_event = row[0]
      event_id = row[1]
      new_event["event_type"] = event_types_rev[row[2]]
      new_event["inscription_id"] = row[3]
      idx += 1
      if idx % 200 == 0: print(idx, '/', len(events))
      if new_event["event_type"] == 'transfer-inscribe':
        cur.execute('''INSERT INTO brc20_unused_tx_inscrs (inscription_id, tick, amount, current_holder_pkscript, current_holder_wallet, event_id, block_height)
                        VALUES (%s, %s, %s, %s, %s, %s, %s) ON CONFLICT (inscription_id) DO NOTHING''', 
                        (new_event["inscription_id"], new_event["tick"], int(new_event["amount"]), new_event["source_pkScript"], new_event["source_wallet"], event_id, block_height))
      elif new_event["event_type"] == 'transfer-transfer':
        cur.execute('''DELETE FROM brc20_unused_tx_inscrs WHERE inscription_id = %s;''', (new_event["inscription_id"],))
      else:
        print("Unknown event type: " + new_event["event_type"])
        sys.exit(1)

  cur.execute('''INSERT INTO brc20_extras_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, block_hash))
  return True

def check_extra_tables():
  global first_inscription_height
  try:
    cur.execute('''
      select min(ebh.block_height) as ebh_tocheck_height
      from brc20_extras_block_hashes ebh
      left join brc20_block_hashes bh on bh.block_height = ebh.block_height
      where bh.block_hash != ebh.block_hash
    ''')
    ebh_tocheck_height = 0
    if cur.rowcount > 0:
      res = cur.fetchone()[0]
      if res is not None:
        ebh_tocheck_height = res
        print("hash diff found on block: " + str(ebh_tocheck_height))
    if ebh_tocheck_height == 0:
      cur.execute('select max(block_height) as current_ebh_height from brc20_extras_block_hashes;')
      if cur.rowcount > 0:
        res = cur.fetchone()[0]
        if res is not None:
          ebh_tocheck_height = res + 1
    if ebh_tocheck_height == 0:
      print("no extra table data found")
      ebh_tocheck_height = first_inscription_height
    cur.execute('''select max(block_height) from brc20_block_hashes;''')
    main_block_height = first_inscription_height
    if cur.rowcount > 0:
      res = cur.fetchone()[0]
      if res is not None:
        main_block_height = res
    if ebh_tocheck_height > main_block_height:
      print("no new extra table data found")
      return
    while ebh_tocheck_height <= main_block_height:
      if ebh_tocheck_height == first_inscription_height:
        print("initial indexing of extra tables, may take a few minutes")
        initial_index_of_extra_tables()
        return
      cur.execute('''select block_hash from brc20_block_hashes where block_height = %s;''', (ebh_tocheck_height,))
      block_hash = cur.fetchone()[0]
      if index_extra_tables(ebh_tocheck_height, block_hash):
        print("extra table data indexed for block: " + str(ebh_tocheck_height))
        ebh_tocheck_height += 1
      else:
        print("extra table data index failed for block: " + str(ebh_tocheck_height))
        return
  except KeyboardInterrupt:
    raise KeyboardInterrupt
  except:
    traceback.print_exc()
    return

check_if_there_is_residue_from_last_run()
if create_extra_tables:
  check_if_there_is_residue_on_extra_tables_from_last_run()
  print("checking extra tables")
  check_extra_tables()

last_report_height = 0
while True:
  check_if_there_is_residue_from_last_run()
  if create_extra_tables:
    check_if_there_is_residue_on_extra_tables_from_last_run()
  ## check if a new block is indexed
  cur_metaprotocol.execute('''SELECT coalesce(max(block_height), -1) as max_height from block_hashes;''')
  max_block_of_metaprotocol_db = cur_metaprotocol.fetchone()[0]
  cur.execute('''select max(block_height) from brc20_block_hashes;''')
  row = cur.fetchone()
  current_block = None
  if row[0] is None: current_block = first_inscription_height
  else: current_block = row[0] + 1
  if current_block > max_block_of_metaprotocol_db:
    print("Waiting for new blocks...")
    time.sleep(5)
    continue
  
  print("Processing block %s" % current_block)
  cur_metaprotocol.execute('select block_hash from block_hashes where block_height = %s;', (current_block,))
  current_block_hash = cur_metaprotocol.fetchone()[0]
  reorg_height = check_for_reorg()
  if reorg_height is not None:
    print("Rolling back to ", reorg_height)
    reorg_fix(reorg_height)
    print("Rolled back to " + str(reorg_height))
    continue
  try:
    index_block(current_block, current_block_hash)
    if create_extra_tables and max_block_of_metaprotocol_db - current_block < 10: ## only update extra tables at the end of sync
      print("checking extra tables")
      check_extra_tables()
    if max_block_of_metaprotocol_db - current_block < 10 or current_block - last_report_height > 100: ## do not report if there are more than 10 blocks to index
      report_hashes(current_block)
      last_report_height = current_block
  except KeyboardInterrupt:
    traceback.print_exc()
    if in_commit: ## rollback commit if any
      print("rolling back")
      cur.execute('''ROLLBACK;''')
      in_commit = False
    print("Exiting...")
    sys.exit(1)
  except:
    traceback.print_exc()
    if in_commit: ## rollback commit if any
      print("rolling back")
      cur.execute('''ROLLBACK;''')
      in_commit = False
    time.sleep(10)
