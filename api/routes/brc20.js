const crypto = require('crypto');

const EVENT_SEPARATOR = "|";

// Registers the /v1/brc20 routes on `app`, reading from `db_pool`.
function register(app, db_pool) {
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

      let query = `select overall_balance, available_balance
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

      let query = `select id, event, event_type, inscription_id
                    from brc20_events
                    where block_height = $1
                    union select id, event, event_type, inscription_id
                    from brc20_light_events
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

  // get all bitcoin rpc results for a given block height
  app.get('/v1/brc20/bitcoin_rpc_results_on_block', async (request, response) => {
    try {
      console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
      let block_height = request.query.block_height

      let current_block_height = await get_block_height_of_db()
      if (block_height > current_block_height) {
        response.status(400).send({ error: 'block not indexed yet', result: null })
        return
      }

      let query = `select
                    method, request, response
                    from brc20_bitcoin_rpc_result_cache
                    where block_height = $1
                    order by id asc;`
      let res = await query_db(query, [block_height])
      let result = []
      for (const row of res.rows) {
        let method = row.method
        let request = row.request
        let response = row.response
        result.push({ method, request, response })
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
      let balance = res.rows[0]

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

      let query = `select cumulative_event_hash, block_event_hash, cumulative_trace_hash, block_trace_hash
                    from brc20_cumulative_event_hashes
                    where block_height = $1;`
      let res = await query_db(query, [block_height])
      let cumulative_event_hash = res.rows[0].cumulative_event_hash
      let block_event_hash = res.rows[0].block_event_hash
      let cumulative_trace_hash = res.rows[0].cumulative_trace_hash
      let block_trace_hash = res.rows[0].block_trace_hash

      let res2 = await query_db('select indexer_version from brc20_indexer_version;')
      let indexer_version = res2.rows[0].indexer_version

      response.send({
        error: null, result: {
          cumulative_event_hash: cumulative_event_hash,
          block_event_hash: block_event_hash,
          cumulative_trace_hash: cumulative_trace_hash,
          block_trace_hash: block_trace_hash,
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
      let hash_hex = hash.digest('hex');

      let res2 = await query_db('select indexer_version from brc20_indexer_version;')
      let indexer_version = res2.rows[0].indexer_version

      response.send({
        error: null, result: {
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
      if (!inscription_id) {
        response.status(400).send({ error: 'inscription_id is required', result: null })
        return
      }

      let query = `select id, event, event_type, inscription_id
                    from brc20_events
                    where inscription_id = $1
                    union select id, event, event_type, inscription_id
                    from brc20_light_events
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

  // Explorer routes for the BRC2.0 blockscout frontend. They answer in blockscout's
  // response shapes, which the frontend consumes directly. The EVM -> BTC address
  // mapping comes from the evm_addresses table (evm_address = last 20 bytes of
  // keccak256(pkscript), stored lowercase), which an evm_addresses updater maintains.
  const EXPLORER_PAGE_SIZE = 50

  async function get_btc_address_of_evm_address(evm_address) {
    let res = await query_db('select btc_address from evm_addresses where evm_address = lower($1);', [evm_address])
    return res.rows.length > 0 ? res.rows[0].btc_address : null
  }

  app.get('/v1/brc20/explorer/btc_address/:evm_address', async (request, response) => {
    try {
      console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
      let btc_address = await get_btc_address_of_evm_address(request.params.evm_address)
      response.send({ btc_address: btc_address })
    } catch (err) {
      console.log(err)
      response.status(500).send({ error: 'internal error', result: null })
    }
  });

  app.get('/v1/brc20/explorer/balances/:evm_address', async (request, response) => {
    try {
      console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
      let btc_address = await get_btc_address_of_evm_address(request.params.evm_address)
      if (btc_address == null) {
        response.send({ items: [] })
        return
      }
      let query = ` select bcb.tick, bcb.overall_balance, bt.decimals
                    from brc20_current_balances bcb
                    left join brc20_tickers bt on bcb.tick = bt.tick
                    where bcb.wallet = $1 and bcb.overall_balance > 0
                    order by bcb.overall_balance desc;`
      let res = await query_db(query, [btc_address])
      response.send({
        items: res.rows.map((row) => ({
          external_token_name: row.tick,
          external_value: row.overall_balance,
          external_decimals: row.decimals + '',
        }))
      })
    } catch (err) {
      console.log(err)
      response.status(500).send({ error: 'internal error', result: null })
    }
  });

  app.get('/v1/brc20/explorer/address_tabs_counters/:evm_address', async (request, response) => {
    try {
      console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
      let token_balances_count = 0
      let btc_address = await get_btc_address_of_evm_address(request.params.evm_address)
      if (btc_address != null) {
        let res = await query_db(`select count(*) as brc20_count
                                  from brc20_current_balances
                                  where wallet = $1 and overall_balance > 0;`, [btc_address])
        token_balances_count = parseInt(res.rows[0].brc20_count)
      }
      response.send({
        internal_transactions_count: 0,
        logs_count: 0,
        token_balances_count: token_balances_count,
        token_transfers_count: 0,
        transactions_count: 0,
        validations_count: 0,
        withdrawals_count: 0,
      })
    } catch (err) {
      console.log(err)
      response.status(500).send({ error: 'internal error', result: null })
    }
  });

  // Base BRC-20 tokens with at least one holder, most held first. Pages by offset:
  // next_page_params.items_count is the offset of the next page.
  app.get('/v1/brc20/explorer/tokens', async (request, response) => {
    try {
      console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
      let q = typeof request.query.q === 'string' ? request.query.q : ''
      let offset = parseInt(request.query.items_count || '0') || 0
      // sort/order are interpolated into SQL, so only whitelisted values get through
      let direction = String(request.query.order).toLowerCase() === 'asc' ? 'asc' : 'desc'
      let query = ` select bt.tick, bt.decimals, bt.max_supply - bt.remaining_supply as total_supply, count(*) as holder_count
                    from brc20_tickers bt
                    join brc20_current_balances bcb on bt.tick = bcb.tick
                    where bt.tick ilike $1 and bcb.overall_balance > 0
                    group by 1, 2, 3
                    order by holder_count ${direction}, bt.tick asc
                    limit $2 offset $3;`
      let res = await query_db(query, ['%' + q + '%', EXPLORER_PAGE_SIZE + 1, offset])
      let rows = res.rows.slice(0, EXPLORER_PAGE_SIZE)
      let next_page_params = null
      if (res.rows.length > EXPLORER_PAGE_SIZE) {
        let last = rows[rows.length - 1]
        next_page_params = {
          holder_count: last.holder_count,
          items_count: offset + EXPLORER_PAGE_SIZE,
          name: last.tick,
          market_cap: null,
        }
      }
      response.send({
        items: rows.map((row) => ({
          address: '',
          type: 'ERC-20',
          symbol: row.tick,
          name: row.tick,
          decimals: row.decimals + '',
          holders: row.holder_count + '',
          exchange_rate: null,
          total_supply: row.total_supply + '',
          icon_url: null,
          circulating_market_cap: null,
        })),
        next_page_params: next_page_params,
      })
    } catch (err) {
      console.log(err)
      response.status(500).send({ error: 'internal error', result: null })
    }
  });
}

module.exports = register
