require('dotenv').config();
var express = require('express');
const { Pool } = require('pg')
var cors = require('cors')
const crypto = require('crypto');

// for self-signed cert of postgres
process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";

const EVENT_SEPARATOR = "|";

var db_pool = new Pool({
  user: process.env.DB_USER || 'postgres',
  host: process.env.DB_HOST || 'localhost',
  database: process.env.DB_DATABASE || 'postgres',
  password: process.env.DB_PASSWD,
  port: parseInt(process.env.DB_PORT || "5432"),
  max: 100, // maximum number of clients!!
  ssl: process.env.DB_SSL == 'true' ? true : false
})
const api_port = parseInt(process.env.API_PORT || "8000")
const api_host = process.env.API_HOST || '127.0.0.1'

var app = express();
app.set('trust proxy', parseInt(process.env.API_TRUSTED_PROXY_CNT || "0"))

var corsOptions = {
  origin: '*',
  optionsSuccessStatus: 200 // some legacy browsers (IE11, various SmartTVs) choke on 204
}
app.use([cors(corsOptions)])

app.get('/v1/brc20/ip', (request, response) => response.send(request.ip))

async function get_block_height_of_db() {
  let res = await db_pool.query('SELECT max(block_height) as max_block_height FROM brc20_block_hashes;')
  return res.rows[0].max_block_height
}

app.get('/v1/brc20/block_height', (request, response) => response.send(get_block_height_of_db()))

// get a given ticker balance of a given pkscript at the start of a given block height
app.get('/v1/brc20/balance_on_block', async (request, response) => {
  let block_height = params.block_height
  let pkscript = params.pkscript
  let tick = params.ticker.toLowerCase()

  let current_block_height = await get_block_height_of_db()
  if (block_height > current_block_height + 1) {
    response.status(400).send({ error: 'block not indexed yet', result: null })
    return
  }

  let query =  `select overall_balance, available_balance
                from brc20_historic_balances
                where block_height < $1
                  and pkscript = $2
                  and tick = $3
                order by id desc
                limit 1;`
  let res = await db_pool.query(query, [block_height, pkscript, tick])
  if (res.rows.length == 0) {
    response.status(400).send({ error: 'no balance found', result: null })
    return
  }
  response.send({ error: null, result: res.rows[0] })
});

// get all brc20 activity of a given block height
app.get('/v1/brc20/activity_on_block', async (request, response) => {
  let block_height = params.block_height

  let current_block_height = await get_block_height_of_db()
  if (block_height > current_block_height) {
    response.status(400).send({ error: 'block not indexed yet', result: null })
    return
  }

  let res1 = await db_pool.query('select event_type_name, event_type_id from brc20_event_types;')
  let event_type_id_to_name = {}
  res1.rows.forEach((row) => {
    event_type_id_to_name[row.event_type_id] = row.event_type_name
  })

  let query =  `select event, event_type, inscription_id
                from brc20_events
                where block_height = $1
                order by id asc;`
  let res = await db_pool.query(query, [block_height])
  let result = []
  for (const row of res.rows) {
    let event = row.event
    let event_type = event_type_id_to_name[row.event_type]
    let inscription_id = row.inscription_id
    event.event_type = event_type
    event.inscription_id = inscription_id
    result.push(event_obj)
  }
  response.send({ error: null, result: result })
});


app.get('/v1/brc20/get_current_balance_of_wallet', async (request, response) => {
  let address = params.address || ''
  let pkscript = params.pkscript || ''
  let tick = params.ticker.toLowerCase()

  let current_block_height = await get_block_height_of_db()
  let query = ` select overall_balance, available_balance
                from brc20_historic_balances
                where pkscript = $1
                  and tick = $2
                order by id desc
                limit 1;`
  let params = [pkscript, tick]
  if (address != '') {
    query = query.replace('pkscript', 'wallet')
    params = [address, tick]
  }

  let res = await db_pool.query(query, params)
  if (res.rows.length == 0) {
    response.status(400).send({ error: 'no balance found', result: null })
    return
  }
  let balance = res.rows[0]
  balance.block_height = current_block_height
  response.send({ error: null, result: balance })
});

app.get('/v1/brc20/get_hash_of_all_activity', async (request, response) => {
  let block_height = params.block_height

  let current_block_height = await get_block_height_of_db()
  if (block_height > current_block_height) {
    response.status(400).send({ error: 'block not indexed yet', result: null })
    return
  }

  let query =  `select cumulative_event_hash, block_event_hash
                from brc20_cumulative_event_hashes
                where block_height = $1;`
  let res = await db_pool.query(query, [block_height])

  let res2 = await db_pool.query('select indexer_version from brc20_indexer_version;')
  let indexer_version = res2.rows[0].indexer_version

  response.send({ error: null, result: {
      cumulative_event_hash: res.rows[0].cumulative_event_hash,
      block_event_hash: res.rows[0].block_event_hash,
      indexer_version: indexer_version,
      block_height: block_height
    } 
  })
});

// NOTE: this may take a few minutes to run
app.get('/v1/brc20/get_hash_of_all_current_balances', async (request, response) => {
  let current_block_height = await get_block_height_of_db()
  let query = ` with tempp as (
                  select max(id) as id
                  from brc20_historic_balances
                  where block_height <= $1
                  group by pkscript, tick
                )
                select bhb.pkscript, bhb.tick, bhb.overall_balance, bhb.available_balance
                from tempp t
                left join brc20_historic_balances bhb on bhb.id = t.id
                order by bhb.pkscript asc, bhb.tick asc;`
  let params = [current_block_height]

  let res = await db_pool.query(query, params)
  let whole_str = ''
  res.rows.forEach((row) => {
    whole_str += row.pkscript + ';' + row.tick + ';' + row.overall_balance + ';' + row.available_balance + EVENT_SEPARATOR
  })
  whole_str = whole_str.slice(0, -1)
  // get sha256 hash hex of the whole string
  const hash = crypto.createHash('sha256');
  hash.update(whole_str);
  let hash_hex = hash.digest('hex');

  let res2 = await db_pool.query('select indexer_version from brc20_indexer_version;')
  let indexer_version = res2.rows[0].indexer_version

  response.send({ error: null, result: {
      current_balances_hash: hash_hex,
      indexer_version: indexer_version,
      block_height: current_block_height
    }
  })
});

app.listen(api_port, api_host);