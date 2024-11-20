require('dotenv').config();
var express = require('express');
const { Pool } = require('pg')
var cors = require('cors')
const crypto = require('crypto');
const rateLimit = require('express-rate-limit');
const { pkscript_from_address, address_from_pkscript } = require("./utils");

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

app.get('/v1/brc20/ip', (request, response) => response.send(request.ip))

async function query_db(query, params = []) {
  return await db_pool.query(query, params)
}

app.get('/v1/brc20/db_version', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let res = await query_db('SELECT db_version FROM brc20_indexer_version;')
    response.send(res.rows[0].db_version + '')
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

app.get('/v1/brc20/event_hash_version', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let res = await query_db('SELECT event_hash_version FROM brc20_indexer_version;')
    response.send(res.rows[0].event_hash_version + '')
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

async function get_block_height_of_db() {
  try {
    let res = await query_db('SELECT max(block_height) as max_block_height FROM brc20_block_hashes;')
    return res.rows[0].max_block_height
  } catch (err) {
    console.log(err)
    return -1
  }
}

async function get_extras_block_height_of_db() {
  try {
    let res = await query_db('SELECT max(block_height) as max_block_height FROM brc20_extras_block_hashes;')
    return res.rows[0].max_block_height
  } catch (err) {
    console.log(err)
    return -1
  }
}

app.get('/v1/brc20/extras_block_height', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let block_height = await get_extras_block_height_of_db()
    response.send(block_height + '')
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

app.get('/v1/brc20/block_height', async (request, response) => {
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
app.get('/v1/brc20/balance_on_block', async (request, response) => {
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
                  from brc20_historic_balances
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

// get all brc20 activity of a given block height
app.get('/v1/brc20/activity_on_block', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let block_height = request.query.block_height

    let current_block_height = await get_block_height_of_db()
    if (block_height > current_block_height) {
      response.status(400).send({ error: 'block not indexed yet', result: null })
      return
    }

    let res1 = await query_db('select event_type_name, event_type_id from brc20_event_types;')
    let event_type_id_to_name = {}
    res1.rows.forEach((row) => {
      event_type_id_to_name[row.event_type_id] = row.event_type_name
    })

    let query =  `select event, event_type, inscription_id
                  from brc20_events
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


app.get('/v1/brc20/get_current_balance_of_wallet', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let address = request.query.address || ''
    let pkscript = request.query.pkscript || ''
    let tick = request.query.ticker.toLowerCase()

    let current_block_height = await get_block_height_of_db()
    let balance = null
    if (!use_extra_tables) {
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

      let res = await query_db(query, params)
      if (res.rows.length == 0) {
        response.status(400).send({ error: 'no balance found', result: null })
        return
      }
      balance = res.rows[0]
    } else {
      let query = ` select overall_balance, available_balance
                    from brc20_current_balances
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

app.get('/v1/brc20/get_valid_tx_notes_of_wallet', async (request, response) => {
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
                  from brc20_unused_tx_inscrs
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

app.get('/v1/brc20/get_valid_tx_notes_of_ticker', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    if (!use_extra_tables) {
      response.status(400).send({ error: 'not supported', result: null })
      return
    }

    let tick = request.query.ticker.toLowerCase() || ''

    let current_block_height = await get_block_height_of_db()
    let query = ` select current_holder_pkscript, current_holder_wallet, inscription_id, amount, block_height as genesis_height
                  from brc20_unused_tx_inscrs
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

app.get('/v1/brc20/holders', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    if (!use_extra_tables) {
      response.status(400).send({ error: 'not supported', result: null })
      return
    }

    let tick = request.query.ticker.toLowerCase() || ''

    let current_block_height = await get_block_height_of_db()
    let query = ` select pkscript, wallet, overall_balance, available_balance
                  from brc20_current_balances
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



app.get('/v1/brc20/get_hash_of_all_activity', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let block_height = request.query.block_height

    let current_block_height = await get_block_height_of_db()
    if (block_height > current_block_height) {
      response.status(400).send({ error: 'block not indexed yet', result: null })
      return
    }

    let query =  `select cumulative_event_hash, block_event_hash
                  from brc20_cumulative_event_hashes
                  where block_height = $1;`
    let res = await query_db(query, [block_height])
    let cumulative_event_hash = res.rows[0].cumulative_event_hash
    let block_event_hash = res.rows[0].block_event_hash

    let res2 = await query_db('select indexer_version from brc20_indexer_version;')
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
app.get('/v1/brc20/get_hash_of_all_current_balances', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
    let current_block_height = await get_block_height_of_db()
    let hash_hex = null
    if (!use_extra_tables) {
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
                    from brc20_current_balances
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

    let res2 = await query_db('select indexer_version from brc20_indexer_version;')
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

// get all events with a specific inscription id
app.get('/v1/brc20/event', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)

    let res1 = await query_db('select event_type_name, event_type_id from brc20_event_types;')
    let event_type_id_to_name = {}
    res1.rows.forEach((row) => {
      event_type_id_to_name[row.event_type_id] = row.event_type_name
    })

    let inscription_id = request.query.inscription_id;
    if(!inscription_id) {
      response.status(400).send({ error: 'inscription_id is required', result: null })
      return
    }

    let query =  `select event, event_type, inscription_id block_height
                  from brc20_events
                  where inscription_id = $1
                  order by id asc;`
    let res = await query_db(query, [inscription_id])
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

app.get('/v1/brc20_swap/get_current_balance_of_wallet', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)

    let { address, pkscript, ticker, module_id } = request.query
    if (!pkscript) {
      pkscript = pkscript_from_address(address)
    }
    let tick = ticker.toLowerCase()
    let current_block_height = await get_block_height_of_db()
    let balance = null
    let query = ` select swap_balance, available_balance, approveable_balance,
                  cond_approveable_balance, withdrawable_balance
                  from brc20_swap_user_balance where pkscript = decode($1, 'hex') and tick = $2 and module_id = $3
                  order by id
                  limit 1;`
    let params = [pkscript, tick, module_id]

    let res = await query_db(query, params)
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no balance found', result: null })
      return
    }
    balance = res.rows[0]

    balance.block_height = current_block_height
    response.send({ error: null, result: balance })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

let history_type_id_to_name;
app.get('/v1/brc20_swap/history', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`);
    let { module_id, start_height, end_height, cursor, size } = request.query;
    start_height = parseInt(start_height);
    end_height = parseInt(end_height);
    cursor = parseInt(cursor) || 0;
    size = parseInt(size) || 10;

    if (!history_type_id_to_name) {
      history_type_id_to_name = {}
      let res0 = await query_db('select history_type_name, history_type_id from brc20_history_types;');

      res0.rows.forEach((row) => {
        history_type_id_to_name[row.history_type_id] = row.history_type_name;
      });
    }

    let query = `
              select id, block_height, module_id, history_type, valid, encode(txid, 'hex') as txid,
              idx, vout, output_value, output_offset, encode(pkscript_from, 'hex') as pkscript_from,
              encode(pkscript_to, 'hex') as pkscript_to, fee, txidx, block_time, inscription_number,
              inscription_id, encode(inscription_content, 'hex') as inscription_content, encode(extra_data, 'hex') as extra_data
              from brc20_swap_history
              where module_id = $1 and block_height between $2 and $3
              order by id
              limit $4 offset $5;
    `;
    let res1 = await query_db(query, [module_id, start_height, end_height, size, cursor]);
    if (res1.rows.length == 0) {
      response.status(400).send({ error: 'no history found', result: null });
      return;
    }
    res1.rows.forEach((row) => {
      row.history_type = history_type_id_to_name[row.history_type];
      row.address_from = row.pkscript_from ? address_from_pkscript(row.pkscript_from) : "";
      row.address_to = row.pkscript_to ? address_from_pkscript(row.pkscript_to) : "";

    });

    let query_count = `
                      select count(*)
                      from brc20_swap_history
                      where module_id = $1 and block_height between $2 and $3
                      `;
    let res2 = await query_db(query_count, [module_id, start_height, end_height]);
    let result = {
      total: parseInt(res2.rows[0].count),
      list: res1.rows
    }

    response.send({ error: null, result});
  } catch (err) {
    console.log(err);
    response.status(500).send({ error: 'internal error', result: null });
  }
});

app.get('/v1/brc20/tick_holders', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)

    let { tick, cursor, size } = request.query;
    cursor = parseInt(cursor) || 0;
    size = parseInt(size) || 10;

    let query = `
                with max_height_data as (
                    select tick, pkscript, max(block_height) as max_height
                    from public.brc20_user_balance
                    where tick = $1
                    group by tick, pkscript
                ),
                filtered_data as (
                    select
                        encode(b.pkscript, 'hex') as pkscript,
                        b.available_balance,
                        b.transferable_balance
                    from public.brc20_user_balance b
                    join max_height_data m on b.tick = m.tick and b.pkscript = m.pkscript and b.block_height = m.max_height
                    where b.available_balance + b.transferable_balance > 0
                )
                select
                    *,
                    count(*) over() as total_count
                from filtered_data
                limit $2
                offset $3;
              `;

    let params = [tick, size, cursor];

    let res = await query_db(query, params)
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no tick info found', result: null })
      return
    }
    let total = parseInt(res.rows[0].total_count);
    res.rows.forEach((row) => {
      row.address = address_from_pkscript(row.pkscript);
      delete row.total_count
    })
    let ret = {
      total,
      list: res.rows
    }
    response.send({ error: null, result: ret })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

app.get('/v1/brc20/status', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)

    let { cursor, size } = request.query;
    cursor = parseInt(cursor) || 0;
    size = parseInt(size) || 10;

    let query = `
                  with latest_data as (
                      select *
                      from public.brc20_user_balance
                      where (pkscript, tick, block_height) in (
                          select pkscript, tick, max(block_height)
                          from public.brc20_user_balance
                          group by pkscript, tick
                      )
                  ), holders_data as (
                      select tick, count(*) as holders
                      from latest_data
                      where available_balance + transferable_balance > 0
                      group by tick
                  ), total_count as (
                      select count(*) as total
                      from (
                          select tick, max(block_height)
                          from public.brc20_ticker_info
                          group by tick
                      ) as filtered_ticker_info
                  ), max_height as (
                      select max(block_height) as max_block_height
                      from public.brc20_ticker_info
                  ), latest_ticker_info as (
                      select *
                      from public.brc20_ticker_info
                      where (tick, block_height) in (
                          select tick, max(block_height)
                          from public.brc20_ticker_info
                          group by tick
                      )
                  )
                  select t.block_height, t.tick, t.max_supply, t.decimals, t.limit_per_mint, t.remaining_supply, encode(t.pkscript_deployer, 'hex') as pkscript_deployer, COALESCE(h.holders, 0) as holders, tc.total, mh.max_block_height
                  from latest_ticker_info t
                  left join holders_data h on t.tick = h.tick
                  cross join total_count tc
                  cross join max_height mh
                  limit $1 offset $2;
              `;

    let params = [size, cursor]

    let res = await query_db(query, params)
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no tick info found', result: null })
      return
    }
    let total = res.rows[0].total;
    let height = res.rows[0].max_block_height;
    let list = res.rows.map((row) => {
      delete row.total
      delete row.max_block_height;
      row.deployer = address_from_pkscript(row.pkscript_deployer);
      return row;
    })
    let ret = {
      total,
      height,
      list
    }
    response.send({ error: null, result: ret })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

app.get('/v1/brc20/get_current_balance_of_wallet_2', async (request, response) => {
  try {
    console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)

    let { pkscript, tick, address } = request.query;
    if (!pkscript) {
      pkscript = pkscript_from_address(address);
    }
    let query = `
                select available_balance, transferable_balance
                from public.brc20_user_balance
                where pkscript = decode($1, 'hex') and tick = $2 and block_height = (
                    select max(block_height)
                    from public.brc20_user_balance
                    where pkscript = decode($1, 'hex') and tick = $2
                );
                `;
    let res = await query_db(query, [pkscript, tick]);

    if (res.rows.length == 0) {
      response.status(400).send({ error: 'no balance found', result: null })
      return
    }
    response.send({ error: null, result: res.rows[0] })
  } catch (err) {
    console.log(err)
    response.status(500).send({ error: 'internal error', result: null })
  }
})

app.listen(api_port, api_host);
