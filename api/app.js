const express = require('express')
const cors = require('cors')
const rateLimit = require('express-rate-limit')
const { Pool } = require('pg')
const { MODULES } = require('./config')

const ROUTES = {
  brc20: require('./routes/brc20'),
  bitmap: require('./routes/bitmap'),
  sns: require('./routes/sns'),
  runes: require('./routes/runes'),
}

// Each module's table holding one row per indexed block.
const BLOCK_HASHES_TABLE = {
  brc20: 'brc20_block_hashes',
  bitmap: 'bitmap_block_hashes',
  sns: 'sns_block_hashes',
  runes: 'runes_block_hashes',
}

// Opens a pool for each enabled module.
function createPools(config) {
  const pools = {}
  for (const module_name of config.modules) {
    const pool = new Pool(config.dbs[module_name])
    // an idle client dropped by postgres must not crash the process
    pool.on('error', (err) => console.log(`${module_name} pool: ${err.message}`))
    pools[module_name] = pool
  }
  return pools
}

// Builds the express app for the enabled modules. `pools` maps each enabled
// module to an object with a pg-style `query(sql, params)`.
function createApp(config, pools) {
  const app = express()
  app.set('trust proxy', config.api.trustedProxies)

  const corsOptions = {
    origin: '*',
    optionsSuccessStatus: 200 // some legacy browsers (IE11, various SmartTVs) choke on 204
  }
  app.use([cors(corsOptions)])

  if (config.rateLimit.enabled) {
    const limiter = rateLimit({
      windowMs: config.rateLimit.windowMs,
      max: config.rateLimit.max,
      standardHeaders: true,
      legacyHeaders: false,
    })
    // Apply the limiter to all requests.
    app.use(limiter)
  }

  app.get('/v1/status', async (request, response) => {
    const modules = {}
    for (const module_name of config.modules) {
      try {
        const res = await pools[module_name].query(
          `SELECT max(block_height) as max_block_height FROM ${BLOCK_HASHES_TABLE[module_name]};`
        )
        modules[module_name] = { block_height: res.rows[0].max_block_height }
      } catch (err) {
        modules[module_name] = { error: err.message }
      }
    }
    response.send({ error: null, result: { modules: modules } })
  })

  for (const module_name of MODULES) {
    if (config.modules.includes(module_name)) {
      ROUTES[module_name](app, pools[module_name])
    } else {
      app.use(`/v1/${module_name}`, (request, response) => {
        response.status(404).send({ error: `${module_name} is not enabled on this node` })
      })
    }
  }

  return app
}

module.exports = { createApp, createPools }
