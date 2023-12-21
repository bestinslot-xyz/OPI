# pip install python-dotenv
# pip install psycopg2-binary

import os, sys, requests
from dotenv import load_dotenv
import traceback, time, codecs, json
import psycopg2
import hashlib

## global variables
ticks = {}
in_commit = False
block_events_str = ""
EVENT_SEPARATOR = "|"
CAN_BE_FIXED_INDEXER_VERSIONS = [ "bis-brc20-open-source v0.1.0" ]
INDEXER_VERSION = "bis-brc20-open-source v0.1.1"
DB_VERSION = 1

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
report_name = os.getenv("REPORT_NAME") or "opi_brc20_indexer"

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
    except: return False
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
    except: return False
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

def is_used_or_invalid(inscription_id):
  global event_types
  cur.execute('''select coalesce(sum(case when event_type = %s then 1 else 0 end), 0) as inscr_cnt,
                        coalesce(sum(case when event_type = %s then 1 else 0 end), 0) as transfer_cnt
                        from brc20_events where inscription_id = %s;''', (event_types["transfer-inscribe"], event_types["transfer-transfer"], inscription_id,))
  row = cur.fetchall()[0]
  return (row[0] != 1) or (row[1] != 0)

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
    res += fix_numstr_decimals(event["max_supply"], decimals_int) + ";"
    res += event["decimals"] + ";"
    res += fix_numstr_decimals(event["limit_per_mint"], decimals_int)
    return res
  elif event_type == "mint-inscribe":
    decimals_int = ticks[event["tick"]][2]
    res = "mint-inscribe;"
    res += inscription_id + ";"
    res += event["minted_pkScript"] + ";"
    res += event["tick"] + ";"
    res += fix_numstr_decimals(event["amount"], decimals_int)
    return res
  elif event_type == "transfer-inscribe":
    decimals_int = ticks[event["tick"]][2]
    res = "transfer-inscribe;"
    res += inscription_id + ";"
    res += event["source_pkScript"] + ";"
    res += event["tick"] + ";"
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

def reset_caches():
  global balance_cache, transfer_inscribe_event_cache
  balance_cache = {}
  transfer_inscribe_event_cache = {}

def deploy_inscribe(block_height, inscription_id, deployer_pkScript, deployer_wallet, tick, max_supply, decimals, limit_per_mint):
  global ticks, in_commit, block_events_str, event_types
  cur.execute("BEGIN;")
  in_commit = True

  event = {
    "deployer_pkScript": deployer_pkScript,
    "deployer_wallet": deployer_wallet,
    "tick": tick,
    "max_supply": str(max_supply),
    "decimals": str(decimals),
    "limit_per_mint": str(limit_per_mint)
  }
  block_events_str += get_event_str(event, "deploy-inscribe", inscription_id) + EVENT_SEPARATOR
  cur.execute('''insert into brc20_events (event_type, block_height, inscription_id, event)
    values (%s, %s, %s, %s);''', (event_types["deploy-inscribe"], block_height, inscription_id, json.dumps(event)))
  
  cur.execute('''insert into brc20_tickers (tick, max_supply, decimals, limit_per_mint, remaining_supply, block_height)
    values (%s, %s, %s, %s, %s, %s);''', (tick, max_supply, decimals, limit_per_mint, max_supply, block_height))
  
  cur.execute("COMMIT;")
  in_commit = False
  ticks[tick] = [max_supply, limit_per_mint, decimals]

def mint_inscribe(block_height, inscription_id, minted_pkScript, minted_wallet, tick, amount):
  global ticks, in_commit, block_events_str, event_types
  cur.execute("BEGIN;")
  in_commit = True

  event = {
    "minted_pkScript": minted_pkScript,
    "minted_wallet": minted_wallet,
    "tick": tick,
    "amount": str(amount)
  }
  block_events_str += get_event_str(event, "mint-inscribe", inscription_id) + EVENT_SEPARATOR
  cur.execute('''insert into brc20_events (event_type, block_height, inscription_id, event)
    values (%s, %s, %s, %s) returning id;''', (event_types["mint-inscribe"], block_height, inscription_id, json.dumps(event)))
  event_id = cur.fetchone()[0]
  cur.execute('''update brc20_tickers set remaining_supply = remaining_supply - %s where tick = %s;''', (amount, tick))

  last_balance = get_last_balance(minted_pkScript, tick)
  last_balance["overall_balance"] += amount
  last_balance["available_balance"] += amount
  cur.execute('''insert into brc20_historic_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id) 
                values (%s, %s, %s, %s, %s, %s, %s);''', 
                (minted_pkScript, minted_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))
  
  cur.execute("COMMIT;")
  in_commit = False
  ticks[tick][0] -= amount

def transfer_inscribe(block_height, inscription_id, source_pkScript, source_wallet, tick, amount):
  global in_commit, block_events_str, event_types
  cur.execute("BEGIN;")
  in_commit = True

  event = {
    "source_pkScript": source_pkScript,
    "source_wallet": source_wallet,
    "tick": tick,
    "amount": str(amount)
  }
  block_events_str += get_event_str(event, "transfer-inscribe", inscription_id) + EVENT_SEPARATOR
  cur.execute('''insert into brc20_events (event_type, block_height, inscription_id, event)
    values (%s, %s, %s, %s) returning id;''', (event_types["transfer-inscribe"], block_height, inscription_id, json.dumps(event)))
  event_id = cur.fetchone()[0]
  
  last_balance = get_last_balance(source_pkScript, tick)
  last_balance["available_balance"] -= amount
  cur.execute('''insert into brc20_historic_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id)
                values (%s, %s, %s, %s, %s, %s, %s);''', 
                (source_pkScript, source_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))
  
  cur.execute("COMMIT;")
  in_commit = False
  save_transfer_inscribe_event(inscription_id, event)

def transfer_transfer_normal(block_height, inscription_id, spent_pkScript, spent_wallet, tick, amount, using_tx_id):
  global in_commit, block_events_str, event_types
  cur.execute("BEGIN;")
  in_commit = True

  inscribe_event = get_transfer_inscribe_event(inscription_id)
  source_pkScript = inscribe_event["source_pkScript"]
  source_wallet = inscribe_event["source_wallet"]
  event = {
    "source_pkScript": source_pkScript,
    "source_wallet": source_wallet,
    "spent_pkScript": spent_pkScript,
    "spent_wallet": spent_wallet,
    "tick": tick,
    "amount": str(amount),
    "using_tx_id": str(using_tx_id)
  }
  block_events_str += get_event_str(event, "transfer-transfer", inscription_id) + EVENT_SEPARATOR
  cur.execute('''insert into brc20_events (event_type, block_height, inscription_id, event)
    values (%s, %s, %s, %s) returning id;''', (event_types["transfer-transfer"], block_height, inscription_id, json.dumps(event)))
  event_id = cur.fetchone()[0]
  
  last_balance = get_last_balance(source_pkScript, tick)
  last_balance["overall_balance"] -= amount
  cur.execute('''insert into brc20_historic_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id)
                values (%s, %s, %s, %s, %s, %s, %s);''', 
                (source_pkScript, source_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))
  
  if spent_pkScript != source_pkScript:
    last_balance = get_last_balance(spent_pkScript, tick)
  last_balance["overall_balance"] += amount
  last_balance["available_balance"] += amount
  cur.execute('''insert into brc20_historic_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id) 
                values (%s, %s, %s, %s, %s, %s, %s);''', 
                (spent_pkScript, spent_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, -1 * event_id)) ## negated to make a unique event_id
  
  cur.execute("COMMIT;")
  in_commit = False

def transfer_transfer_spend_to_fee(block_height, inscription_id, tick, amount, using_tx_id):
  global in_commit, block_events_str, event_types
  cur.execute("BEGIN;")
  in_commit = True

  inscribe_event = get_transfer_inscribe_event(inscription_id)
  source_pkScript = inscribe_event["source_pkScript"]
  source_wallet = inscribe_event["source_wallet"]
  event = {
    "source_pkScript": source_pkScript,
    "source_wallet": source_wallet,
    "spent_pkScript": None,
    "spent_wallet": None,
    "tick": tick,
    "amount": str(amount),
    "using_tx_id": str(using_tx_id)
  }
  block_events_str += get_event_str(event, "transfer-transfer", inscription_id) + EVENT_SEPARATOR
  cur.execute('''insert into brc20_events (event_type, block_height, inscription_id, event)
    values (%s, %s, %s, %s) returning id;''', (event_types["transfer-transfer"], block_height, inscription_id, json.dumps(event)))
  event_id = cur.fetchone()[0]
  
  last_balance = get_last_balance(source_pkScript, tick)
  last_balance["available_balance"] += amount
  cur.execute('''insert into brc20_historic_balances (pkscript, wallet, tick, overall_balance, available_balance, block_height, event_id) 
                values (%s, %s, %s, %s, %s, %s, %s);''', 
                (source_pkScript, source_wallet, tick, last_balance["overall_balance"], last_balance["available_balance"], block_height, event_id))
  
  cur.execute("COMMIT;")
  in_commit = False


def update_event_hashes(block_height):
  global block_events_str
  if block_events_str[-1] == EVENT_SEPARATOR: block_events_str = block_events_str[:-1] ## remove last separator
  block_event_hash = get_sha256_hash(block_events_str)
  cumulative_event_hash = None
  cur.execute('''select cumulative_event_hash from brc20_cumulative_event_hashes where block_height = %s;''', (block_height - 1,))
  if cur.rowcount == 0:
    cumulative_event_hash = block_event_hash
  else:
    cumulative_event_hash = get_sha256_hash(cur.fetchone()[0] + block_event_hash)
  cur.execute('''INSERT INTO brc20_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) VALUES (%s, %s, %s);''', (block_height, block_event_hash, cumulative_event_hash))
    

def index_block(block_height, current_block_hash):
  global ticks, block_events_str
  print("Indexing block " + str(block_height))
  block_events_str = ""
  
  cur_metaprotocol.execute('''SELECT ot.id, ot.inscription_id, ot.old_satpoint, ot.new_pkscript, ot.new_wallet, ot.sent_as_fee, oc."content", oc.content_type
                              FROM ord_transfers ot
                              LEFT JOIN ord_content oc ON ot.inscription_id = oc.inscription_id
                              WHERE ot.block_height = %s 
                                 AND oc."content" is not null AND oc."content"->>'p'='brc-20'
                              ORDER BY ot.id asc;''', (block_height,))
  transfers = cur_metaprotocol.fetchall()
  if len(transfers) == 0:
    print("No transfers found for block " + str(block_height))
    update_event_hashes(block_height)
    cur.execute('''INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, current_block_hash))
    return
  print("Transfer count: ", len(transfers))

  ## refresh ticks
  sttm = time.time()
  cur.execute('''select tick, remaining_supply, limit_per_mint, decimals from brc20_tickers;''')
  ticks_ = cur.fetchall()
  ticks = {}
  for t in ticks_:
    ticks[t[0]] = [t[1], t[2], t[3]]
  print("Ticks refreshed in " + str(time.time() - sttm) + " seconds")
  
  idx = 0
  for transfer in transfers:
    idx += 1
    if idx % 100 == 0:
      print(idx, '/', len(transfers))
    
    tx_id, inscr_id, old_satpoint, new_pkScript, new_addr, sent_as_fee, js, content_type = transfer
    
    if sent_as_fee and old_satpoint == '': continue ## inscribed as fee

    if content_type is None: continue ## invalid inscription
    try: content_type = codecs.decode(content_type, "hex").decode('utf-8')
    except: pass
    content_type = content_type.split(';')[0]
    if content_type != 'application/json' and content_type != 'text/plain': continue ## invalid inscription

    if "tick" not in js: continue ## invalid inscription
    if "op" not in js: continue ## invalid inscription
    tick = js["tick"]
    try: tick = tick.lower()
    except: continue ## invalid tick
    if utf8len(tick) != 4: continue ## invalid tick
    
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
        if max_supply > (2**64-1) * (10**18) or max_supply <= 0: continue ## invalid max supply
      limit_per_mint = max_supply
      if "lim" in js:
        if not is_positive_number_with_dot(js["lim"]): continue ## invalid limit per mint
        else:
          limit_per_mint = get_number_extended_to_18_decimals(js["lim"], decimals)
          if limit_per_mint is None: continue ## invalid limit per mint
          if limit_per_mint > (2**64-1) * (10**18) or limit_per_mint <= 0: continue ## invalid limit per mint
      deploy_inscribe(block_height, inscr_id, new_pkScript, new_addr, tick, max_supply, decimals, limit_per_mint)
    
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
      mint_inscribe(block_height, inscr_id, new_pkScript, new_addr, tick, amount)
    
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
        transfer_inscribe(block_height, inscr_id, new_pkScript, new_addr, tick, amount)
      else:
        if is_used_or_invalid(inscr_id): continue ## already used or invalid
        if sent_as_fee: transfer_transfer_spend_to_fee(block_height, inscr_id, tick, amount, tx_id)
        else: transfer_transfer_normal(block_height, inscr_id, new_pkScript, new_addr, tick, amount, tx_id)
  
  update_event_hashes(block_height)
  # end of block
  cur.execute('''INSERT INTO brc20_block_hashes (block_height, block_hash) VALUES (%s, %s);''', (block_height, current_block_hash))
  conn.commit()
  print("ALL DONE")



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

cur.execute('select event_type_name, event_type_id from brc20_event_types;')
event_types = {}
for row in cur.fetchall():
  event_types[row[0]] = row[1]

event_types_rev = {}
for key in event_types:
  event_types_rev[event_types[key]] = key


def reindex_cumulative_hashes():
  global event_types_rev, ticks
  cur.execute('''delete from brc20_cumulative_event_hashes;''')
  cur.execute('''select min(block_height), max(block_height) from brc20_block_hashes;''')
  row = cur.fetchone()
  min_block = row[0]
  max_block = row[1]

  sttm = time.time()
  cur.execute('''select tick, remaining_supply, limit_per_mint, decimals from brc20_tickers;''')
  ticks_ = cur.fetchall()
  ticks = {}
  for t in ticks_:
    ticks[t[0]] = [t[1], t[2], t[3]]
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

cur.execute('select indexer_version from brc20_indexer_version;')
if cur.rowcount == 0:
  cur.execute('insert into brc20_indexer_version (indexer_version, db_version) values (%s, %s);', (INDEXER_VERSION, DB_VERSION,))
else:
  db_indexer_version = cur.fetchone()[0]
  if db_indexer_version != INDEXER_VERSION:
    print("Indexer version mismatch!!")
    if db_indexer_version not in CAN_BE_FIXED_INDEXER_VERSIONS:
      print("This version (" + db_indexer_version + ") cannot be fixed, please reset tables and reindex.")
      exit(1)
    else:
      print("This version (" + db_indexer_version + ") can be fixed, fixing in 5 secs...")
      time.sleep(5)
      reindex_cumulative_hashes()
      cur.execute('alter table brc20_indexer_version add column if not exists db_version int4;') ## next versions will use DB_VERSION for DB check
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
    "version": INDEXER_VERSION,
    "db_version": DB_VERSION,
    "block_height": block_height,
    "block_hash": block_hash,
    "block_event_hash": block_event_hash,
    "cumulative_event_hash": cumulative_event_hash
  }
  print("Sending hashes to metaprotocol indexer indexer...")
  try_to_report_with_retries(to_send)

check_if_there_is_residue_from_last_run()
while True:
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
    report_hashes(current_block)
  except:
    traceback.print_exc()
    if in_commit: ## rollback commit if any
      print("rolling back")
      cur.execute('''ROLLBACK;''')
      in_commit = False
    time.sleep(10)
