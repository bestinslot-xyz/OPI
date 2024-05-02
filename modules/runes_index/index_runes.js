// to run: node --max-old-space-size=8192 .\index_runes.js

require('dotenv').config();

const { Pool } = require('pg')
var fs = require('fs');
var bitcoin = require('bitcoinjs-lib');
var ecc = require('tiny-secp256k1');
const process = require('process');
const { execSync } = require("child_process");
const readline = require('readline');

bitcoin.initEccLib(ecc)

// for self-signed cert of postgres
process.env["NODE_TLS_REJECT_UNAUTHORIZED"] = 0;

const promise_limit = 50000

var db_pool = new Pool({
  user: process.env.DB_USER || 'postgres',
  host: process.env.DB_HOST || 'localhost',
  database: process.env.DB_DATABASE || 'postgres',
  password: process.env.DB_PASSWD,
  port: parseInt(process.env.DB_PORT || "5432"),
  max: process.env.DB_MAX_CONNECTIONS || 50, // maximum number of clients!!
  ssl: process.env.DB_SSL == 'true' ? true : false
})

var chain_folder = process.env.BITCOIN_CHAIN_FOLDER || "~/.bitcoin/"
var bitcoin_rpc_user = process.env.BITCOIN_RPC_USER || ""
var bitcoin_rpc_password = process.env.BITCOIN_RPC_PASSWD || ""
var bitcoin_rpc_url = process.env.BITCOIN_RPC_URL || ""

var ord_binary = process.env.ORD_BINARY || "ord"
var ord_folder = process.env.ORD_FOLDER || "ord-runes/target/release/"
if (ord_folder.length == 0) {
  console.error("ord_folder not set in .env, please run python3 reset_init.py")
  process.exit(1)
}
if (ord_folder[ord_folder.length - 1] != '/') ord_folder += '/'
var ord_datadir = process.env.ORD_DATADIR || "."
var cookie_file = process.env.COOKIE_FILE || ""

var report_to_indexer = (process.env.REPORT_TO_INDEXER || "true") == "true"
var report_url = process.env.REPORT_URL || "https://api.opi.network/report_block"
var report_retries = parseInt(process.env.REPORT_RETRIES || "10")
var report_name = process.env.REPORT_NAME || "opi_runes_indexer"

const network_type = process.env.NETWORK_TYPE || "mainnet"

if (network_type == 'regtest') {
  report_to_indexer = false
  console.log("Network type is regtest, reporting to indexer is disabled.")
}

var network = null
var network_folder = ""
if (network_type == "mainnet") {
  network = bitcoin.networks.bitcoin
  network_folder = ""
} else if (network_type == "testnet") {
  network = bitcoin.networks.testnet
  network_folder = "testnet3/"
} else if (network_type == "signet") {
  network = bitcoin.networks.testnet // signet is not supported by bitcoinjs-lib but wallet_addr calculation is the same as testnet
  network_folder = "signet/"
} else if (network_type == "regtest") {
  network = bitcoin.networks.regtest
  network_folder = "regtest/"
} else {
  console.error("Unknown network type: " + network_type)
  process.exit(1)
}
const first_rune_heights = {
  'mainnet': 840000,
  'testnet': 2520000,
  'signet': 173831,
  'regtest': 0,
}
const first_rune_height = first_rune_heights[network_type]
const fast_index_below = first_rune_height + 1000

const DB_VERSION = 6
// eslint-disable-next-line no-unused-vars
const INDEXER_VERSION = 'OPI-runes-alpha V0.4.2'
const ORD_VERSION = 'opi-runes-ord 0.18.1-2'

console.log(INDEXER_VERSION)

function delay(sec) {
  return new Promise(resolve => setTimeout(resolve, sec * 1000));
}

function save_error_log(log) {
  console.error(log)
  fs.appendFileSync("log_file_error.txt", log + "\n")
}

var current_outpoint_to_pkscript_wallet_map = {}
async function main_index() {
  await check_db()

  let first = true;
  // eslint-disable-next-line no-constant-condition
  while (true) {
    current_outpoint_to_pkscript_wallet_map = {}

    if (first) first = false
    else await delay(2)

    let start_tm = +(new Date())

    if (!fs.existsSync(ord_folder + network_folder + "runes_output.txt")) {
      console.error("runes_output.txt not found, creating")
      fs.writeFileSync(ord_folder + network_folder + "runes_output.txt", '')
    }
    if (!fs.existsSync(ord_folder + network_folder + "runes_output_blocks.txt")) {
      console.error("runes_output_blocks.txt not found, creating")
      fs.writeFileSync(ord_folder + network_folder + "runes_output_blocks.txt", '')
    }

    let ord_last_block_height_q = await db_pool.query(`SELECT coalesce(max(block_height), -1) as max_height from runes_block_hashes;`)
    let ord_last_block_height = ord_last_block_height_q.rows[0].max_height
    if (ord_last_block_height < fast_index_below) { // first inscription
      ord_last_block_height = fast_index_below
    }

    let ord_index_st_tm = +(new Date())
    let ord_end_block_height = ord_last_block_height + 500

    let cookie_arg = cookie_file ? ` --cookie-file=${cookie_file} ` : ""

    let current_directory = process.cwd()
    process.chdir(ord_folder);
    let ord_version_cmd = ord_binary + " --version"
    let rpc_argument = ""
    if (bitcoin_rpc_url != "") {
      rpc_argument = " --rpc-url " + bitcoin_rpc_url
    }

    if (bitcoin_rpc_user != "") {
      rpc_argument += " --bitcoin-rpc-user " + bitcoin_rpc_user + " --bitcoin-rpc-pass " + bitcoin_rpc_password
    }
    let network_argument = ""
    if (network_type == 'signet') {
      network_argument = " --signet"
    } else if (network_type == 'regtest') {
      network_argument = " --regtest"
    } else if (network_type == 'testnet') {
      network_argument = " --testnet"
    }
    let ord_index_cmd = ord_binary + " --no-index-inscriptions --index-runes" + network_argument + " --bitcoin-data-dir " + chain_folder + " --data-dir " + ord_datadir + cookie_arg + " --height-limit " + (ord_end_block_height) + " " + rpc_argument + " index run"
    try {
      let version_string = execSync(ord_version_cmd).toString()
      console.log("ord version: " + version_string)
      if (!version_string.includes(ORD_VERSION)) {
        console.error("ord-runes version mismatch, please recompile ord-runes via 'cargo build --release' in ord-runes folder.")
        process.exit(1)
      }
      execSync(ord_index_cmd, {stdio: 'inherit'})
    }
    catch (err) {
      console.error("ERROR ON ORD!!!")
      console.error(err)
      process.chdir(current_directory);

      continue
    }
    process.chdir(current_directory);
    let ord_index_tm = +(new Date()) - ord_index_st_tm
    
    const fileStream = fs.createReadStream(ord_folder + network_folder + "runes_output.txt", { encoding: 'UTF-8' });
    const rl = readline.createInterface({
      input: fileStream,
      crlfDelay: Infinity
    });
    let lines = []
    for await (const line of rl) {
      lines.push(line)
    }
    let lines_index = fs.readFileSync(ord_folder + network_folder + "runes_output_blocks.txt", "utf8").split('\n')
    if (lines_index.length == 1) {
      console.log("Nothing new, waiting!!")
      continue
    }

    let current_height_q = await db_pool.query(`SELECT coalesce(max(block_height), -1) as max_height from runes_block_hashes;`)
    let current_height = current_height_q.rows[0].max_height

    console.log("Checking for possible reorg")
    for (const l of lines_index) {
      if (l.trim() == "") continue
      let parts = l.split(';')
      if (parts[2].trim() == "new_block") {
        let block_height = parseInt(parts[1].trim())
        if (block_height > current_height) continue
        console.warn("Block repeating, possible reorg!!")
        let blockhash = parts[3].trim()
        let blockhash_db_q = await db_pool.query("select block_hash from runes_block_hashes where block_height = $1;", [block_height])
        if (blockhash_db_q.rows[0].block_hash != blockhash) {
          let reorg_st_tm = +(new Date())
          console.error("Reorg detected at block_height " + block_height)
          await handle_reorg(block_height)
          console.log("Reverted to block_height " + (block_height - 1))
          let reorg_tm = +(new Date()) - reorg_st_tm
          reorg_tm = Math.round(reorg_tm)
          
          await db_pool.query(`INSERT into runes_indexer_reorg_stats
              (reorg_tm, old_block_height, new_block_height)
              values ($1, $2, $3);`, 
              [reorg_tm, current_height, block_height - 1])
          current_height = Math.min(current_height, block_height - 1)
        }
      }
    }

    // some sanity checks and checks for possible early exit of ord
    let last_start_idx = null
    let last_start_block = null
    let last_end_idx = null
    let block_start_idxes = {}
    let next_expected_start = true
    let lenlines = lines.length
    let ioffset = 0
    for (let i = 0; i < lenlines; i++) {
      let l = lines[i + ioffset]
      if (l.trim() == "") { continue }
      
      let parts = l.split(';')
      if (parts[0] != "cmd") { continue }
      if (parts[2] == "block_start") {
        if (last_start_idx == null && i != 0) {
          console.error("Faulty block_start position: " + l)
          process.exit(1)
        }
        let block_height = parseInt(parts[1])
        if ((last_start_block != null) && (block_height <= last_start_block)) {
          // repeating block_start, remove early entries
          console.error("start with less or equal block_height in latter: " + l)
          lines.splice(block_start_idxes[block_height] + ioffset, i + ioffset)
          ioffset -= i - block_start_idxes[block_height]
          let temp_i = 0
          while ((block_height + temp_i) in block_start_idxes) {
            delete block_start_idxes[block_height + temp_i]
            temp_i += 1
          }
        }
        else if (!next_expected_start) {
          console.error("two start but bigger block_height in latter: " + l)
          process.exit(1)
        }
        else if (i != ioffset && i - 1 != last_end_idx) {
          console.error("block_start not right after block_end: " + l)
          process.exit(1)
        }
        last_start_idx = i
        last_start_block = block_height
        next_expected_start = false
        block_start_idxes[block_height] = i
      }
      else if (parts[2] == "block_end") {
        if (next_expected_start) {
          console.error("NOT expected block_end: " + l)
          process.exit(1)
        }
        let block_height = parseInt(parts[1])
        if (block_height != last_start_block) {
          console.error("block_end block_height != block_start block_height: " + l)
          process.exit(1)
        }
        last_end_idx = i
        next_expected_start = true
      }
      else {
        continue
      }
    }
    if (!next_expected_start) {
      console.error("logs didn't end with block_end - did ord crash?")
      let all_tm = +(new Date()) - start_tm
      ord_index_tm = Math.round(ord_index_tm)
      all_tm = Math.round(all_tm)

      await db_pool.query(`INSERT into runes_indexer_work_stats
          (ord_index_tm, all_tm)
          values ($1, $2);`, 
          [ord_index_tm, all_tm])
      continue
    }

    let ord_sql_st_tm = +(new Date())

    let sql_query_id_to_entry_insert = `INSERT into runes_id_to_entry (rune_id, rune_block, burned, divisibility, etching
                                                                      , terms_amount, terms_cap, terms_height_l, terms_height_h
                                                                      , terms_offset_l, terms_offset_h
                                                                      , mints, "number", premine, rune_name, spacers, symbol
                                                                      , "timestamp", turbo, genesis_height, last_updated_block_height) values ($1, $2, $3, $4, $5
                                                                      , $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21);`
    let sql_query_outpoint_to_balances_insert = `INSERT into runes_outpoint_to_balances (outpoint, pkscript, wallet_addr, rune_ids, balances, block_height) values ($1, $2, $3, $4, $5, $6);`
    let sql_query_id_to_entry_changes_insert = `INSERT into runes_id_to_entry_changes (rune_id, burned, mints, block_height) values ($1, $2, $3, $4);`
    let sql_query_id_to_entry_update = `UPDATE runes_id_to_entry
                                        SET burned = $1, mints = $2, last_updated_block_height = $3
                                        WHERE rune_id = $4 AND last_updated_block_height < $5;`
    let sql_query_outpoint_to_balances_remove = `UPDATE runes_outpoint_to_balances SET spent = true, spent_block_height = $1 WHERE outpoint = $2;`
    let sql_query_runes_events_insert = `INSERT into runes_events (id, event_type, txid, outpoint, pkscript, wallet_addr, rune_id, amount, block_height) values ($1, $2, $3, $4, $5, $6, $7, $8, $9);`
    // first run inserts, then updates
    // runes_id_to_entry updates must either be ordered or ran once by combining
    // NOTE: runes_id_to_entry_changes must have only one entry per rune_id per block_height

    let current_runes_events_id_q = await db_pool.query(`SELECT coalesce(max(id), -1) as maxid from runes_events;`)
    let current_runes_events_id = parseInt(current_runes_events_id_q.rows[0].maxid) + 1
    
    let ord_sql_query_count = 0
    let new_runes_count = 0
    let updated_runes_count = 0
    let new_balances_count = 0
    let removed_balances_count = 0
    let added_entry_history_count = 0
    let added_event_count = 0

    let max_height = -1
    for (const l of lines_index) {
      if (l.trim() == '') { continue } 
      let parts = l.split(';')

      if (parts[0] != "cmd") { continue } 
      if (parts[2] != "new_block") { continue }
      if (parseInt(parts[1]) > max_height) max_height = parseInt(parts[1])
    }

    console.log("db_height: " + current_height + " -> " + max_height)
    let main_min_block_height = current_height + 1
    let main_max_block_height = max_height

    let running_promises = []
    let delayed_queries = []
    let idx = 0
    for (const l of lines) {
      if (l.trim() == '') { continue }
      idx += 1
      if (idx % 10000 == 0) console.log(idx + " / " + lines.length)

      let parts = l.split(';')
      if (parts[0] != "cmd") { continue }

      if (running_promises.length > promise_limit) {
        await Promise.all(running_promises)
        running_promises = []
      }

      let block_height = parseInt(parts[1])
      if (block_height <= current_height) continue
      if (parts[2] == "block_start") continue
      else if (parts[2] == "block_end") continue
      else if (parts[2] == "outpoint_to_balances_insert") {
        if (block_height > current_height) {
          let pkscript = parts[3]
          let wallet_addr = wallet_from_pkscript(pkscript, network)
          let outpoint = parts[4]
          current_outpoint_to_pkscript_wallet_map[outpoint] = [pkscript, wallet_addr]

          let balances_str = parts[5]
          let rune_ids = []
          let balances = []
          for (let pair of balances_str.split(',')) {
            if (pair.trim() == '') continue
            let parts2 = pair.split('-')
            rune_ids.push(parts2[0])
            balances.push(parts2[1])
          }
          running_promises.push(execute_on_db(sql_query_outpoint_to_balances_insert, [outpoint, pkscript, wallet_addr, rune_ids, balances, block_height]))
          new_balances_count += 1
          ord_sql_query_count += 1
        }
      }
      else if (parts[2] == "id_to_entry_insert") {
        if (block_height > current_height) {
          let rune_id = parts[3]
          let rune_block = parts[4]
          let burned = parts[5]
          let divisibility = parts[6]
          let etching = parts[7]
          let terms_str = parts[8]
          let terms_amount = null
          let terms_cap = null
          let terms_height_l = null
          let terms_height_h = null
          let terms_offset_l = null
          let terms_offset_h = null
          if (terms_str != '') {
            let terms = terms_str.split('-')
            terms_amount = terms[0]
            if (terms_amount == 'null') terms_amount = null
            terms_cap = terms[1]
            if (terms_cap == 'null') terms_cap = null
            terms_height_l = terms[2]
            if (terms_height_l == 'null') terms_height_l = null
            terms_height_h = terms[3]
            if (terms_height_h == 'null') terms_height_h = null
            terms_offset_l = terms[4]
            if (terms_offset_l == 'null') terms_offset_l = null
            terms_offset_h = terms[5]
            if (terms_offset_h == 'null') terms_offset_h = null
          }
          let mints = parts[9]
          let number = parts[10]
          let premine = parts[11]
          let rune_name = parts[12]
          let spacers = parts[13]
          let symbol = parts[14]
          if (symbol == 'null') symbol = null
          let timestamp = parseInt(parts[15])
          timestamp = new Date(timestamp * 1000)
          let turbo = parts[16] == 'true'

          running_promises.push(execute_on_db(sql_query_id_to_entry_insert, [rune_id, rune_block, burned, divisibility, etching
            , terms_amount, terms_cap, terms_height_l, terms_height_h, terms_offset_l, terms_offset_h
            , mints, number, premine, rune_name, spacers, symbol, timestamp, turbo, block_height, block_height]))
          new_runes_count += 1
          ord_sql_query_count += 1
        }
      }
      else if (parts[2] == "outpoint_to_balances_remove") {
        if (block_height > current_height) {
          let outpoint = parts[3]
          delayed_queries.push([0, sql_query_outpoint_to_balances_remove, [block_height, outpoint]])
        }
      }
      else if (parts[2] == "id_to_entry_update") {
        if (block_height > current_height) {
          let rune_id = parts[3]
          let burned = parts[4]
          let mints = parts[5]
          for (let i = 0; i < delayed_queries.length; i++) {
            if (delayed_queries[i][0] == 1 && delayed_queries[i][2][4] == rune_id) {
              delayed_queries.splice(i, 1)
              i -= 1
            }
          }
          delayed_queries.push([1, sql_query_id_to_entry_update, [burned, mints,  block_height, rune_id, block_height]])

          running_promises.push(execute_on_db(sql_query_id_to_entry_changes_insert, [rune_id, burned, mints, block_height]))
          added_entry_history_count += 1
          ord_sql_query_count += 1
        }
      }
      else if (parts[2] == "tx_events_input") {
        if (block_height > current_height) {
          let txid = parts[3]
          let outpoint = parts[4]
          let rune_id = parts[5]
          let amount = parts[6]

          running_promises.push(get_info_and_insert_event(sql_query_runes_events_insert, txid, outpoint, rune_id, amount, block_height, current_runes_events_id))
          current_runes_events_id += 1
          added_event_count += 1
          ord_sql_query_count += 2
        }
      }
      else if (parts[2] == "tx_events_new_rune_allocation") {
        if (block_height > current_height) {
          let txid = parts[3]
          let rune_id = parts[4]
          let amount = parts[5]

          running_promises.push(execute_on_db(sql_query_runes_events_insert, [current_runes_events_id, 1, txid, null, null, null, rune_id, amount, block_height]))
          current_runes_events_id += 1
          added_event_count += 1
          ord_sql_query_count += 1
        }
      }
      else if (parts[2] == "tx_events_mint") {
        if (block_height > current_height) {
          let txid = parts[3]
          let rune_id = parts[4]
          let amount = parts[5]

          running_promises.push(execute_on_db(sql_query_runes_events_insert, [current_runes_events_id, 2, txid, null, null, null, rune_id, amount, block_height]))
          current_runes_events_id += 1
          added_event_count += 1
          ord_sql_query_count += 1
        }
      }
      else if (parts[2] == "tx_events_output") {
        if (block_height > current_height) {
          let txid = parts[3]
          let outpoint = parts[4]
          let rune_id = parts[5]
          let amount = parts[6]
          let pkscript = parts[7]
          let wallet_addr = wallet_from_pkscript(pkscript, network)
          current_outpoint_to_pkscript_wallet_map[outpoint] = [pkscript, wallet_addr]

          running_promises.push(execute_on_db(sql_query_runes_events_insert, [current_runes_events_id, 3, txid, outpoint, pkscript, wallet_addr, rune_id, amount, block_height]))
          current_runes_events_id += 1
          added_event_count += 1
          ord_sql_query_count += 1
        }
      }
      else if (parts[2] == "tx_events_burn") {
        if (block_height > current_height) {
          let txid = parts[3]
          let rune_id = parts[4]
          let amount = parts[5]

          running_promises.push(execute_on_db(sql_query_runes_events_insert, [current_runes_events_id, 4, txid, null, null, null, rune_id, amount, block_height]))
          current_runes_events_id += 1
          added_event_count += 1
          ord_sql_query_count += 1
        }
      }
    }
    await Promise.all(running_promises)
    running_promises = []
    
    for (const query of delayed_queries) {
      idx += 1
      if (idx % 10000 == 0) console.log(idx + " / " + lines.length)

      if (running_promises.length > promise_limit) {
        await Promise.all(running_promises)
        running_promises = []
      }

      running_promises.push(execute_on_db(query[1], query[2]))
      if (query[0] == 0) removed_balances_count += 1
      else if (query[0] == 1) updated_runes_count += 1
      ord_sql_query_count += 1
    }
    
    await Promise.all(running_promises)
    running_promises = []

    let to_be_inserted_hashes = {}
    for (const l of lines_index) {
      if (l.trim() == '') { continue } 
      let parts = l.split(';')

      if (parts[0] != "cmd") { continue } 
      if (parts[2] != "new_block") { continue }

      let block_height = parseInt(parts[1])
      if (block_height < first_rune_height) { continue }
      let blockhash = parts[3].trim()
      let blocktime = parseInt(parts[4])
      blocktime = new Date(blocktime * 1000)
      to_be_inserted_hashes[block_height] = [blockhash, blocktime]
    }

    await update_cumulative_block_hashes(max_height, to_be_inserted_hashes)

    for (const k of Object.keys(to_be_inserted_hashes)) {
      let block_height = parseInt(k)
      let blockhash = to_be_inserted_hashes[k][0]
      let blocktime = to_be_inserted_hashes[k][1]
      await db_pool.query(`INSERT into runes_block_hashes (block_height, block_hash, block_time) values ($1, $2, $3) ON CONFLICT (block_height) DO NOTHING;`, [block_height, blockhash, blocktime])
    }
    
    let ord_sql_tm = +(new Date()) - ord_sql_st_tm

    console.log("Updating Log Files")
    let update_log_st_tm = +(new Date())
    fs.writeFileSync(ord_folder + network_folder + "runes_output.txt", '')
    fs.writeFileSync(ord_folder + network_folder + "runes_output_blocks.txt", '')
    let update_log_tm = +(new Date()) - update_log_st_tm

    ord_index_tm = Math.round(ord_index_tm)
    ord_sql_tm = Math.round(ord_sql_tm)
    update_log_tm = Math.round(update_log_tm)
    
    let all_tm = +(new Date()) - start_tm
    all_tm = Math.round(all_tm)

    await db_pool.query(`INSERT into runes_indexer_work_stats
      (main_min_block_height, main_max_block_height, ord_sql_query_count, new_runes_count, updated_runes_count, 
        new_balances_count, removed_balances_count, added_entry_history_count, added_event_count, ord_index_tm, ord_sql_tm, update_log_tm, all_tm)
      values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13);`, 
        [main_min_block_height, main_max_block_height, ord_sql_query_count, new_runes_count, updated_runes_count,
          new_balances_count, removed_balances_count, added_entry_history_count, added_event_count, ord_index_tm, ord_sql_tm, update_log_tm, all_tm])
    
    console.log("ALL DONE")
  }
}

async function try_to_report_with_retries(to_send) {
  for (let i = 0; i < report_retries; i++) {
    try {
      let r = await fetch(report_url, {
        method: "POST",
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(to_send),
      })
      if (r.status == 200) {
        console.log("Reported hashes to metaprotocol indexer indexer.")
        return
      } else {
        console.log("Error while reporting hashes to metaprotocol indexer indexer, status code: " + r.status)
        console.log(await r.text())
      }
    } catch (err) {
      console.log("Error while reporting hashes to metaprotocol indexer indexer, retrying...")
    }
    await delay(1)
  }
  console.log("Error while reporting hashes to metaprotocol indexer indexer, giving up.")
}

async function update_cumulative_block_hashes(until_height, to_be_inserted_hashes) {
  console.log("Updating cumulative block hashes")

  let last_hash_height_q = await db_pool.query(`SELECT coalesce(max(block_height), -1) as max_height from runes_cumulative_event_hashes;`)
  let last_hash_height = last_hash_height_q.rows[0].max_height
  if (last_hash_height < 0) { last_hash_height = first_rune_height - 1 }

  const event_type_rev = {
    0: "input",
    1: "new_rune_allocation",
    2: "mint",
    3: "output",
    4: "burn",
  }

  console.log("last_hash_height: " + last_hash_height + " -> " + until_height)
  for (let height = last_hash_height + 1; height <= until_height; height++) {
    let events_q = await db_pool.query(`SELECT event_type, outpoint, pkscript, rune_id, amount, block_height from runes_events where block_height = $1 order by id asc;`, [height])

    let block_events_string = ""
    for (const row of events_q.rows) {
      block_events_string += event_type_rev[row.event_type] + ";" + (row.outpoint || "") + ";" + (row.pkscript || "") + ";" + row.rune_id + ";" + row.amount + "|"
    }
    block_events_string = block_events_string.slice(0, -1) // remove last separator
    let block_event_hash = bitcoin.crypto.sha256(Buffer.from(block_events_string, 'utf8')).toString('hex')
    let cumulative_event_hash = null
    let cumulative_event_hash_q = await db_pool.query(`SELECT cumulative_event_hash from runes_cumulative_event_hashes where block_height = $1;`, [height - 1])
    if (cumulative_event_hash_q.rows.length > 0) {
      let temp_concat = cumulative_event_hash_q.rows[0].cumulative_event_hash + block_event_hash
      cumulative_event_hash = bitcoin.crypto.sha256(Buffer.from(temp_concat, 'utf8')).toString('hex')
    } else {
      cumulative_event_hash = block_event_hash
    }
    await db_pool.query(`INSERT into runes_cumulative_event_hashes (block_height, block_event_hash, cumulative_event_hash) values ($1, $2, $3);`, [height, block_event_hash, cumulative_event_hash])
  }

  if (report_to_indexer) {
    for (let height = Math.max(last_hash_height + 1, until_height - 9); height <= until_height; height++) {
      let event_hash_q = await db_pool.query(`select block_event_hash, cumulative_event_hash from runes_cumulative_event_hashes where block_height = $1;`, [height])
      let block_event_hash = event_hash_q.rows[0].block_event_hash
      let cumulative_event_hash = event_hash_q.rows[0].cumulative_event_hash
      let block_hash = to_be_inserted_hashes[height][0]
      if (!block_hash) {
        let block_hash_q = await db_pool.query(`select block_hash from runes_block_hashes where block_height = $1;`, [height])
        block_hash = block_hash_q.rows[0].block_hash
      }
      
      let to_send = {
        "name": report_name,
        "type": "runes",
        "node_type": "full_node",
        "network_type": network_type,
        "version": INDEXER_VERSION,
        "db_version": DB_VERSION,
        "block_height": height,
        "block_hash": block_hash,
        "block_event_hash": block_event_hash,
        "cumulative_event_hash": cumulative_event_hash
      }
      console.log("Sending hashes to metaprotocol indexer indexer...")
      await try_to_report_with_retries(to_send)
    }
  } else {
    console.log("Reporting to metaprotocol indexer is disabled")
  }
}

async function get_info_and_insert_event(query, txid, outpoint, rune_id, amount, block_height, current_runes_events_id) {
  let pkscript = null
  let wallet_addr = null
  if (current_outpoint_to_pkscript_wallet_map[outpoint]) {
    pkscript = current_outpoint_to_pkscript_wallet_map[outpoint][0]
    wallet_addr = current_outpoint_to_pkscript_wallet_map[outpoint][1]
  } else {
    let outpoint_info_q = await db_pool.query(`SELECT pkscript, wallet_addr from runes_outpoint_to_balances where outpoint = $1;`, [outpoint])
    pkscript = outpoint_info_q.rows[0].pkscript
    wallet_addr = outpoint_info_q.rows[0].wallet_addr
    current_outpoint_to_pkscript_wallet_map[outpoint] = [pkscript, wallet_addr]
  }

  if (pkscript == null) {
    console.error("pkscript not found for outpoint: " + outpoint)
    process.exit(1)
  }

  let params = [
    current_runes_events_id, 0, txid, outpoint, pkscript, wallet_addr, rune_id, amount, block_height
  ]

  try {
    await db_pool.query(query, params)
  } catch (err) {
    console.error("ERROR ON DB!!!")
    console.error(err)
    console.error(query)
    console.error(params)
    process.exit(1)
  }
}

async function execute_on_db(query, params) {
  try {
    await db_pool.query(query, params)
  } catch (err) {
    console.error("ERROR ON DB!!!")
    console.error(err)
    console.error(query)
    console.error(params)
    process.exit(1)
  }
}

/*
NOTE: removed following from node_modules/bitcoinjs-lib/src/payments/p2tr.js
//if (pubkey && pubkey.length) {
//  if (!(0, ecc_lib_1.getEccLib)().isXOnlyPoint(pubkey))
//    throw new TypeError('Invalid pubkey for p2tr');
//}
o.w. it cannot decode 512057cd4cfa03f27f7b18c2fe45fe2c2e0f7b5ccb034af4dec098977c28562be7a2
*/
function wallet_from_pkscript(pkscript, network) {
  try {
    let address = bitcoin.payments.p2tr({ output: Buffer.from(pkscript, 'hex'), network: network })
    return address.address
  } catch { /* try others */ }
  try {
    let address = bitcoin.payments.p2wsh({ output: Buffer.from(pkscript, 'hex'), network: network })
    return address.address
  } catch { /* try others */ }
  try {
    let address = bitcoin.payments.p2wpkh({ output: Buffer.from(pkscript, 'hex'), network: network })
    return address.address
  } catch { /* try others */ }
  try {
    let address = bitcoin.payments.p2sh({ output: Buffer.from(pkscript, 'hex'), network: network })
    return address.address
  } catch { /* try others */ }
  try {
    let address = bitcoin.payments.p2pkh({ output: Buffer.from(pkscript, 'hex'), network: network })
    return address.address
  } catch { /* end */ }

  return null
}

async function handle_reorg(block_height) {
  let last_correct_blockheight = block_height - 1

  await db_pool.query(`DELETE from runes_outpoint_to_balances where block_height > $1;`, [last_correct_blockheight])
  await db_pool.query(`UPDATE runes_outpoint_to_balances SET spent = false, spent_block_height = null WHERE spent_block_height > $1;`, [last_correct_blockheight])

  await db_pool.query(`DELETE from runes_id_to_entry where genesis_height > $1;`, [last_correct_blockheight])
  await db_pool.query(`DELETE from runes_id_to_entry_changes where block_height > $1;`, [last_correct_blockheight])
  let res = await db_pool.query(`SELECT rune_id from runes_id_to_entry where last_updated_block_height > $1;`, [last_correct_blockheight])
  for (const row of res.rows) {
    let res_inner = await db_pool.query(`SELECT burned, mints, block_height from runes_id_to_entry_changes WHERE rune_id = $1 AND block_height <= $2 ORDER BY block_height desc LIMIT 1;`, [row.rune_id, last_correct_blockheight])
    if (res_inner.rows.length == 0) {
      await db_pool.query(`UPDATE runes_id_to_entry SET burned = $1, mints = $2, last_updated_block_height = genesis_height WHERE rune_id = $3;`, [0, 0, row.rune_id])
    } else {
      let burned = res_inner.rows[0].burned
      let mints = res_inner.rows[0].mints
      let block_height = res_inner.rows[0].block_height
      await db_pool.query(`UPDATE runes_id_to_entry SET burned = $1, mints = $2, last_updated_block_height = $3 WHERE rune_id = $4;`, [burned, mints, block_height, row.rune_id])
    }
  }

  await db_pool.query(`DELETE from runes_cumulative_event_hashes where block_height > $1;`, [last_correct_blockheight])
  await db_pool.query(`DELETE from runes_block_hashes where block_height > $1;`, [last_correct_blockheight])

  await db_pool.query(`SELECT setval('runes_id_to_entry_id_seq', max(id)) from runes_id_to_entry;`)
  await db_pool.query(`SELECT setval('runes_outpoint_to_balances_id_seq', max(id)) from runes_outpoint_to_balances;`)
  await db_pool.query(`SELECT setval('runes_cumulative_event_hashes_id_seq', max(id)) from runes_cumulative_event_hashes;`)
  await db_pool.query(`SELECT setval('runes_block_hashes_id_seq', max(id)) from runes_block_hashes;`)
}

async function check_db() {
  console.log("checking db")

  try {
    let db_version_q = await db_pool.query(`SELECT db_version from runes_indexer_version;`)
    let db_version = db_version_q.rows[0].db_version
    if (db_version != DB_VERSION) {
      console.error("db_version mismatch, db needs to be recreated from scratch, please run reset_init.py")
      process.exit(1)
    }
  } catch (err) {
    console.error(err)
    console.error("db_version not found, db needs to be recreated from scratch, please run reset_init.py")
    process.exit(1)
  }

  let res_q = await db_pool.query(`SELECT * from runes_network_type LIMIT 1;`)
  if (res_q.rows.length == 0) {
    console.error("runes_network_type not found, db needs to be recreated from scratch, please run reset_init.py")
    process.exit(1)
  }
  let network_type_db = res_q.rows[0].network_type
  if (network_type_db != network_type) {
    console.error("network_type mismatch, db needs to be recreated from scratch, please run reset_init.py")
    process.exit(1)
  }

  let residue_found = false
  let current_height_q = await db_pool.query(`SELECT coalesce(max(block_height), -1) as max_height from runes_block_hashes;`)
  let current_height = current_height_q.rows[0].max_height
  console.log("current_height: " + current_height)

  let current_runes_id_to_entry_height_q = await db_pool.query(`SELECT coalesce(max(last_updated_block_height), -1) as max_height from runes_id_to_entry;`)
  let current_runes_id_to_entry_height = current_runes_id_to_entry_height_q.rows[0].max_height
  console.log("current_runes_id_to_entry_height: " + current_runes_id_to_entry_height)
  if (current_runes_id_to_entry_height > current_height) {
    console.error("current_runes_id_to_entry_height > current_height")
    residue_found = true
  }

  let current_runes_id_to_entry_changes_height_q = await db_pool.query(`SELECT coalesce(max(block_height), -1) as max_height from runes_id_to_entry_changes;`)
  let current_runes_id_to_entry_changes_height = current_runes_id_to_entry_changes_height_q.rows[0].max_height
  console.log("current_runes_id_to_entry_changes_height: " + current_runes_id_to_entry_changes_height)
  if (current_runes_id_to_entry_changes_height > current_height) {
    console.error("current_runes_id_to_entry_changes_height > current_height")
    residue_found = true
  }

  let current_runes_outpoint_to_balances_height_q = await db_pool.query(`SELECT coalesce(max(block_height), -1) as max_height from runes_outpoint_to_balances;`)
  let current_runes_outpoint_to_balances_height = current_runes_outpoint_to_balances_height_q.rows[0].max_height
  console.log("current_runes_outpoint_to_balances_height: " + current_runes_outpoint_to_balances_height)
  if (current_runes_outpoint_to_balances_height > current_height) {
    console.error("current_runes_outpoint_to_balances_height > current_height")
    residue_found = true
  }

  if (residue_found) {
    console.error("residue found, will be fixed by handle_reorg")
    handle_reorg(current_height + 1)
  }

  

  // initialise mainnet with seed rune
  if (network_type == "mainnet") {
    let seed_rune_id = "1:0"

    let seed_rune_q = await db_pool.query(`SELECT count(*) as count from runes_id_to_entry where rune_id = $1;`, [seed_rune_id])
    let seed_rune_count = seed_rune_q.rows[0].count
    if (seed_rune_count == 0) {
      console.log("seed rune not found, initialising db with seed rune")

      await db_pool.query(`INSERT into runes_id_to_entry (rune_id, rune_block, burned, divisibility, etching,
                                                          terms_amount, terms_cap, terms_height_l, terms_height_h,
                                                          terms_offset_l, terms_offset_h,
                                                          mints, "number", premine, rune_name, spacers, symbol,
                                                          "timestamp", turbo, genesis_height, last_updated_block_height) values ($1, $2, $3, $4, $5,
                                                          $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                                                          $19, $20, $21);`,
                          [seed_rune_id, 1, 0, 0, "0000000000000000000000000000000000000000000000000000000000000000",
                          1, "340282366920938463463374607431768211455", 840000, 1050000, null, null,
                          0, 0, 0, "UNCOMMONGOODS", 128, "⧉",
                          new Date(0), true, 1, 1])
    }
  }
}

main_index()