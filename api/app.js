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

// Express 4 ignores the promise an async handler returns, so a rejection would
// go unhandled and end the process. This forwards it to the error middleware.
function guard(handler) {
  return (request, response, next) => Promise.resolve(handler(request, response, next)).catch(next)
}

// The registration surface the route files use: `get` with every handler guarded.
function guarded_routes(app) {
  return {
    get: (path, ...handlers) => app.get(path, ...handlers.map(guard)),
  }
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

  const routes = guarded_routes(app)

  routes.get('/v1/status', async (request, response) => {
    const modules = {}
    await Promise.all(
      config.modules.map(async (module_name) => {
        try {
          const res = await pools[module_name].query(
            `SELECT max(block_height) as max_block_height FROM ${BLOCK_HASHES_TABLE[module_name]};`
          )
          modules[module_name] = { block_height: res.rows[0].max_block_height }
        } catch (err) {
          console.error(`status query failed for ${module_name}: ${err.message}`)
          modules[module_name] = { error: 'unavailable' }
        }
      })
    )
    response.send({ error: null, result: { modules: modules } })
  })

  for (const module_name of MODULES) {
    if (config.modules.includes(module_name)) {
      ROUTES[module_name](routes, pools[module_name])
    } else {
      app.use(`/v1/${module_name}`, (request, response) => {
        response.status(404).send({ error: `${module_name} is not enabled on this node` })
      })
    }
  }

  // express identifies an error middleware by its four parameters
  // eslint-disable-next-line no-unused-vars
  app.use((err, request, response, next) => {
    console.log(err)
    if (response.headersSent) {
      return request.socket.destroy()
    }
    response.status(500).send({ error: 'internal error', result: null })
  })

  return app
}

module.exports = { createApp, createPools }
