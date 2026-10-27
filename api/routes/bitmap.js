const crypto = require('crypto');

const EVENT_SEPARATOR = "|";

// Registers the /v1/bitmap routes on `app`, reading from `db_pool`.
function register(app, db_pool) {
  app.get('/v1/bitmap/ip', (request, response) => response.send(request.ip))

  async function get_block_height_of_db() {
    let res = await db_pool.query('SELECT max(block_height) as max_block_height FROM bitmap_block_hashes;')
    return res.rows[0].max_block_height
  }

  app.get('/v1/bitmap/block_height', async (request, response) => {
    try {
      console.log(`${request.protocol}://${request.get('host')}${request.originalUrl}`)
      let block_height = await get_block_height_of_db()
      response.send(block_height + '')
    } catch (err) {
      console.log(err)
      response.status(500).send({ error: 'internal error', result: null })
    }
  })

  app.get('/v1/bitmap/get_hash_of_all_activity', async (request, response) => {
    let block_height = request.query.block_height

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
}

module.exports = register
