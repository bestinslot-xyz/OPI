const crypto = require('crypto');

const EVENT_SEPARATOR = "|";

// Registers the /v1/sns routes on `app`, reading from `db_pool`.
function register(app, db_pool) {
  app.get('/v1/sns/ip', (request, response) => response.send(request.ip))

  async function get_block_height_of_db() {
    let res = await db_pool.query('SELECT max(block_height) as max_block_height FROM sns_block_hashes;')
    return res.rows[0].max_block_height
  }

  app.get('/v1/sns/block_height', async (request, response) => {
    try {
      console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
      let block_height = await get_block_height_of_db()
      response.send(block_height + '')
    } catch (err) {
      console.log(err)
      response.status(500).send({ error: 'internal error', result: null })
    }
  })

  app.get('/v1/sns/get_hash_of_all_activity', async (request, response) => {
    let block_height = request.params.block_height

    let current_block_height = await get_block_height_of_db()
    if (block_height > current_block_height) {
      response.status(400).send({ error: 'block not indexed yet', result: null })
      return
    }

    let query =  `select cumulative_event_hash, block_event_hash
                  from sns_names_cumulative_event_hashes
                  where block_height = $1;`
    let res = await db_pool.query(query, [block_height])

    let res2 = await db_pool.query('select indexer_version from sns_names_indexer_version;')
    let indexer_version = res2.rows[0].indexer_version

    response.send({ error: null, result: {
        cumulative_event_hash: res.rows[0].cumulative_event_hash,
        block_event_hash: res.rows[0].block_event_hash,
        indexer_version: indexer_version,
        block_height: block_height
      }
    })
  });

  app.get('/v1/sns/get_hash_of_all_registered_names', async (request, response) => {
    let current_block_height = await get_block_height_of_db()
    let query = ` select "name", domain, inscription_id, inscription_number
                  from sns_names
                  order by inscription_number asc;`
    let params = []

    let res = await db_pool.query(query, params)
    let whole_str = ''
    res.rows.forEach((row) => {
      whole_str += row.name + ';' + row.domain + ';' + row.inscription_id + ';' + row.inscription_number + EVENT_SEPARATOR
    })
    whole_str = whole_str.slice(0, -1)
    // get sha256 hash hex of the whole string
    const hash = crypto.createHash('sha256');
    hash.update(whole_str);
    let hash_hex = hash.digest('hex');

    let res2 = await db_pool.query('select indexer_version from sns_names_indexer_version;')
    let indexer_version = res2.rows[0].indexer_version

    response.send({ error: null, result: {
        current_sns_names_hash: hash_hex,
        indexer_version: indexer_version,
        block_height: current_block_height
      }
    })
  });

  app.get('/v1/sns/get_info_of_sns', async (request, response) => {
    let name = request.query.name

    let query = ` select inscription_id, inscription_number, domain
                  from sns_names
                  where "name" = $1;`
    let params = [name]

    let res = await db_pool.query(query, params)
    if (res.rows.length == 0) {
      response.status(400).send({ error: 'sns name not found', result: null })
      return
    }
    let inscription_id = res.rows[0].inscription_id
    let inscription_number = res.rows[0].inscription_number
    let domain = res.rows[0].domain

    response.send({ error: null, result: {
        inscription_id: inscription_id,
        inscription_number: inscription_number,
        domain: domain,
        sns_name: name
      }
    })
  });

  app.get('/v1/sns/get_inscriptions_of_domain', async (request, response) => {
    let domain = request.query.domain

    let query = ` select inscription_id, inscription_number, "name" as sns_name
                  from sns_names
                  where domain = $1;`
    let params = [domain]

    let res = await db_pool.query(query, params)

    response.send({ error: null, result: res.rows })
  });

  app.get('/v1/sns/get_registered_namespaces', async (request, response) => {
    let query = ` select inscription_id, inscription_number, "namespace"
                  from sns_namespaces;`

    let res = await db_pool.query(query)

    response.send({ error: null, result: res.rows })
  });
}

module.exports = register
