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
  max: process.env.DB_MAX_CONNECTIONS || 10, // maximum number of clients!!
  ssl: process.env.DB_SSL == 'true' ? true : false
})

var use_extra_tables = process.env.USE_EXTRA_TABLES == 'true' ? true : false

const api_port = parseInt(process.env.API_PORT || "8000")
const api_host = process.env.API_HOST || '127.0.0.1'

var app = express();
app.set('trust proxy', parseInt(process.env.API_TRUSTED_PROXY_CNT || "0"))

var corsOptions = {
  origin: '*',
  optionsSuccessStatus: 200 // some legacy browsers (IE11, various SmartTVs) choke on 204
}
app.use([cors(corsOptions)])

app.get('/v1/pow20/ip', (request, response) => response.send(request.ip))

async function query_db(query, params = []) {
  return await db_pool.query(query, params)
}

async function get_block_height_of_db() {
  try {
    let res = await query_db('SELECT max(block_height) as max_block_height FROM pow20_block_hashes;')
    console.log(res)
    return res.rows[0].max_block_height
  } catch (err) {
    console.log(err)
    return -1
  }
}

app.get('/v1/pow20/block_height', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let block_height = await get_block_height_of_db()
    response.send(block_height + '')
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

// get a given ticker balance of a given pkscript at the start of a given block height
app.get('/v1/pow20/balance_on_block', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let block_height = request.query.block_height
    let pkscript = request.query.pkscript
    let tick = request.query.ticker.toLowerCase()

    let current_block_height = await get_block_height_of_db()
    if (block_height > current_block_height + 1) {
      response.status(400).send({ error: 'block not indexed yet', result: null })
      return
    }

    let query =  `select overall_balance, available_balance
                  from pow20_historic_balances
                  where block_height < $1
                    and pkscript = $2
                    and tick = $3
                  order by id desc
                  limit 1;`
    let res = await query_db(query, [block_height, pkscript, tick])
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no balance found', result: null })
      return
    }
    response.send({ error: null, result: res.rows[0] })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});

// get all pow20 activity of a given block height
app.get('/v1/pow20/activity_on_block', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let block_height = request.query.block_height

    let current_block_height = await get_block_height_of_db()
    if (block_height > current_block_height) {
      response.status(400).send({ error: 'block not indexed yet', result: null })
      return
    }

    let res1 = await query_db('select event_type_name, event_type_id from pow20_event_types;')
    let event_type_id_to_name = {}
    res1.rows.forEach((row) => {
      event_type_id_to_name[row.event_type_id] = row.event_type_name
    })

    let query =  `select event, event_type, inscription_id
                  from pow20_events
                  where block_height = $1
                  order by id asc;`
    let res = await query_db(query, [block_height])
    let result = []
    for (const row of res.rows) {
      let event = row.event
      let event_type = event_type_id_to_name[row.event_type]
      let inscription_id = row.inscription_id
      event.event_type = event_type
      event.inscription_id = inscription_id
      result.push(event)
    }
    response.send({ error: null, result: result })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});


app.get('/v1/pow20/get_current_balance_of_wallet', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let address = request.query.address || ''
    let pkscript = request.query.pkscript || ''
    let tick = request.query.ticker.toLowerCase()

    let current_block_height = await get_block_height_of_db()
    let balance = null
    if (!use_extra_tables) {
      let query = ` select overall_balance, available_balance
                    from pow20_historic_balances
                    where pkscript = $1
                      and tick = $2
                    order by id desc
                    limit 1;`
      let params = [pkscript, tick]
      if (address != '') {
        query = query.replace('pkscript', 'wallet')
        params = [address, tick]
      }

      let res = await query_db(query, params)
      if (res.rows.length == 0) {
        response.status(400).send({ error: 'no balance found', result: null })
        return
      }
      balance = res.rows[0]
    } else {
      let query = ` select overall_balance, available_balance
                    from pow20_current_balances
                    where pkscript = $1
                      and tick = $2
                    limit 1;`
      let params = [pkscript, tick]
      if (address != '') {
        query = query.replace('pkscript', 'wallet')
        params = [address, tick]
      }

      let res = await query_db(query, params)
      if (res.rows.length == 0) {
        response.status(400).send({ error: 'no balance found', result: null })
        return
      }
      balance = res.rows[0]
    }

    balance.block_height = current_block_height
    response.send({ error: null, result: balance })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});

app.get('/v1/pow20/get_valid_tx_notes_of_wallet', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    if (!use_extra_tables) {
      response.status(400).send({ error: 'not supported', result: null })
      return
    }

    let address = request.query.address || ''
    let pkscript = request.query.pkscript || ''

    let current_block_height = await get_block_height_of_db()
    let query = ` select tick, inscription_id, amount, block_height as genesis_height
                  from pow20_unused_tx_inscrs
                  where current_holder_pkscript = $1
                  order by tick asc;`
    let params = [pkscript]
    if (address != '') {
      query = query.replace('pkscript', 'wallet')
      params = [address]
    }

    let res = await query_db(query, params)
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no unused tx found', result: null })
      return
    }
    let result = {
      unused_txes: res.rows,
      block_height: current_block_height
    }

    response.send({ error: null, result: result })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});

app.get('/v1/pow20/get_valid_tx_notes_of_ticker', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    if (!use_extra_tables) {
      response.status(400).send({ error: 'not supported', result: null })
      return
    }

    let tick = request.query.ticker.toLowerCase() || ''

    let current_block_height = await get_block_height_of_db()
    let query = ` select current_holder_pkscript, current_holder_wallet, inscription_id, amount, block_height as genesis_height
                  from pow20_unused_tx_inscrs
                  where tick = $1
                  order by current_holder_pkscript asc;`
    let params = [tick]

    let res = await query_db(query, params)
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no unused tx found', result: null })
      return
    }
    let result = {
      unused_txes: res.rows,
      block_height: current_block_height
    }

    response.send({ error: null, result: result })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});

app.get('/v1/pow20/holders', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    if (!use_extra_tables) {
      response.status(400).send({ error: 'not supported', result: null })
      return
    }

    let tick = request.query.ticker.toLowerCase() || ''

    let current_block_height = await get_block_height_of_db()
    let query = ` select pkscript, wallet, overall_balance, available_balance
                  from pow20_current_balances
                  where tick = $1
                  order by overall_balance asc;`
    let params = [tick]

    let res = await query_db(query, params)
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no unused tx found', result: null })
      return
    }
    let rows = res.rows
    // order rows using parseInt(overall_balance) desc
    rows.sort((a, b) => parseInt(b.overall_balance) - parseInt(a.overall_balance))
    // remove rows with parseInt(overall_balance) == 0
    rows = rows.filter((row) => parseInt(row.overall_balance) != 0)
    let result = {
      unused_txes: rows,
      block_height: current_block_height
    }

    response.send({ error: null, result: result })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});



app.get('/v1/pow20/get_hash_of_all_activity', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let block_height = request.query.block_height
  
    let current_block_height = await get_block_height_of_db()
    if (block_height > current_block_height) {
      response.status(400).send({ error: 'block not indexed yet', result: null })
      return
    }

    let query =  `select cumulative_event_hash, block_event_hash
                  from pow20_cumulative_event_hashes
                  where block_height = $1;`
    let res = await query_db(query, [block_height])
    let cumulative_event_hash = res.rows[0].cumulative_event_hash
    let block_event_hash = res.rows[0].block_event_hash
  
    let res2 = await query_db('select indexer_version from pow20_indexer_version;')
    let indexer_version = res2.rows[0].indexer_version
  
    response.send({ error: null, result: {
        cumulative_event_hash: cumulative_event_hash,
        block_event_hash: block_event_hash,
        indexer_version: indexer_version,
        block_height: block_height
      } 
    })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});

// NOTE: this may take a few minutes to run
app.get('/v1/pow20/get_hash_of_all_current_balances', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let current_block_height = await get_block_height_of_db()
    let hash_hex = null
    if (!use_extra_tables) {
      let query = ` with tempp as (
                      select max(id) as id
                      from pow20_historic_balances
                      where block_height <= $1
                      group by pkscript, tick
                    )
                    select bhb.pkscript, bhb.tick, bhb.overall_balance, bhb.available_balance
                    from tempp t
                    left join pow20_historic_balances bhb on bhb.id = t.id
                    order by bhb.pkscript asc, bhb.tick asc;`
      let params = [current_block_height]

      let res = await query_db(query, params)
      res.rows.sort((a, b) => {
        if (a.pkscript < b.pkscript) {
          return -1
        } else if (a.pkscript > b.pkscript) {
          return 1
        } else {
          if (a.tick < b.tick) {
            return -1
          } else if (a.tick > b.tick) {
            return 1
          } else {
            return 0
          }
        }
      })
      let whole_str = ''
      res.rows.forEach((row) => {
        if (parseInt(row.overall_balance) != 0) {
          whole_str += row.pkscript + ';' + row.tick + ';' + row.overall_balance + ';' + row.available_balance + EVENT_SEPARATOR
        }
      })
      whole_str = whole_str.slice(0, -1)
      // get sha256 hash hex of the whole string
      const hash = crypto.createHash('sha256');
      hash.update(whole_str);
      hash_hex = hash.digest('hex');
    } else {
      let query = ` select pkscript, tick, overall_balance, available_balance
                    from pow20_current_balances
                    order by pkscript asc, tick asc;`
      let params = []

      let res = await query_db(query, params)
      res.rows.sort((a, b) => {
        if (a.pkscript < b.pkscript) {
          return -1
        } else if (a.pkscript > b.pkscript) {
          return 1
        } else {
          if (a.tick < b.tick) {
            return -1
          } else if (a.tick > b.tick) {
            return 1
          } else {
            return 0
          }
        }
      })
      let whole_str = ''
      res.rows.forEach((row) => {
        if (parseInt(row.overall_balance) != 0) {
          whole_str += row.pkscript + ';' + row.tick + ';' + row.overall_balance + ';' + row.available_balance + EVENT_SEPARATOR
        }
      })
      whole_str = whole_str.slice(0, -1)
      // get sha256 hash hex of the whole string
      const hash = crypto.createHash('sha256');
      hash.update(whole_str);
      hash_hex = hash.digest('hex');
    }

    let res2 = await query_db('select indexer_version from pow20_indexer_version;')
    let indexer_version = res2.rows[0].indexer_version

    response.send({ error: null, result: {
        current_balances_hash: hash_hex,
        indexer_version: indexer_version,
        block_height: current_block_height
      }
    })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
});

app.listen(api_port, api_host);
console.log(`Running on http://${api_host}:${api_port}`);