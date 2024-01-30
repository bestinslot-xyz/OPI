require('dotenv').config();
var express = require('express');
const { Pool } = require('pg')
var cors = require('cors')
const crypto = require('crypto');
const rateLimit = require('express-rate-limit');

// for self-signed cert of postgres
process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";

const EVENT_SEPARATOR = "|";

var db_pool = new Pool({
  user: process.env.DB_USER || 'postgres',
  host: process.env.DB_HOST || 'localhost',
  database: process.env.DB_DATABASE || 'postgres',
  password: process.env.DB_PASSWD,
  port: parseInt(process.env.DB_PORT || "5432"),
  max: process.env.DB_MAX_CONNECTIONS || 10, // maximum number of clients!!
  ssl: process.env.DB_SSL == 'true' ? true : false
})
const api_port = parseInt(process.env.API_PORT || "8001")
const api_host = process.env.API_HOST || '127.0.0.1'

const rate_limit_enabled = process.env.RATE_LIMIT_ENABLE || 'false'
const rate_limit_window_ms = process.env.RATE_LIMIT_WINDOW_MS || 15 * 60 * 1000
const rate_limit_max = process.env.RATE_LIMIT_MAX || 100

var app = express();
app.set('trust proxy', parseInt(process.env.API_TRUSTED_PROXY_CNT || "0"))

var corsOptions = {
  origin: '*',
  optionsSuccessStatus: 200 // some legacy browsers (IE11, various SmartTVs) choke on 204
}
app.use([cors(corsOptions)])

if (rate_limit_enabled === 'true') {
  const limiter = rateLimit({
    windowMs: rate_limit_window_ms,
    max: rate_limit_max,
    standardHeaders: true,
    legacyHeaders: false,
  })
  // Apply the delay middleware to all requests.
  app.use(limiter);
}

app.get('/v1/bitmap/ip', (request, response) => response.send(request.ip))

async function get_block_height_of_db() {
  let res = await db_pool.query('SELECT max(block_height) as max_block_height FROM bitmap_block_hashes;')
  return res.rows[0].max_block_height
}

app.get('/v1/bitmap/block_height', (request, response) => response.send(get_block_height_of_db()))

app.get('/v1/bitmap/get_hash_of_all_activity', async (request, response) => {
  let block_height = request.params.block_height

  let current_block_height = await get_block_height_of_db()
  if (block_height > current_block_height) {
    response.status(400).send({ error: 'block not indexed yet', result: null })
    return
  }

  let query =  `select cumulative_event_hash, block_event_hash
                from bitmap_cumulative_event_hashes
                where block_height = $1;`
  let res = await db_pool.query(query, [block_height])

  let res2 = await db_pool.query('select indexer_version from bitmap_indexer_version;')
  let indexer_version = res2.rows[0].indexer_version

  response.send({ error: null, result: {
      cumulative_event_hash: res.rows[0].cumulative_event_hash,
      block_event_hash: res.rows[0].block_event_hash,
      indexer_version: indexer_version,
      block_height: block_height
    } 
  })
});

app.get('/v1/bitmap/get_hash_of_all_bitmaps', async (request, response) => {
  let current_block_height = await get_block_height_of_db()
  let query = ` select bitmap_number, inscription_id
                from bitmaps
                order by bitmap_number asc;`
  let params = [current_block_height]

  let res = await db_pool.query(query, params)
  let whole_str = ''
  res.rows.forEach((row) => {
    whole_str += row.bitmap_number + ';' + row.inscription_id + EVENT_SEPARATOR
  })
  whole_str = whole_str.slice(0, -1)
  // get sha256 hash hex of the whole string
  const hash = crypto.createHash('sha256');
  hash.update(whole_str);
  let hash_hex = hash.digest('hex');

  let res2 = await db_pool.query('select indexer_version from bitmap_indexer_version;')
  let indexer_version = res2.rows[0].indexer_version

  response.send({ error: null, result: {
      current_bitmaps_hash: hash_hex,
      indexer_version: indexer_version,
      block_height: current_block_height
    }
  })
});

app.get('/v1/bitmap/get_inscription_id_of_bitmap', async (request, response) => {
  let bitmap_number = request.query.bitmap_number

  let query = ` select inscription_id
                from bitmaps
                where bitmap_number = $1;`
  let params = [bitmap_number]

  let res = await db_pool.query(query, params)
  if (res.rows.length == 0) {
    response.status(400).send({ error: 'bitmap not found', result: null })
    return
  }
  let inscription_id = res.rows[0].inscription_id

  response.send({ error: null, result: {
      inscription_id: inscription_id,
      bitmap_number: bitmap_number
    }
  })
});

app.listen(api_port, api_host);