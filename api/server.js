const { loadConfig, loadEnv } = require('./config')
const { createApp, createPools } = require('./app')

// for self-signed cert of postgres
process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";

loadEnv()

let config
try {
  config = loadConfig(process.env)
} catch (err) {
  console.error(`opi-api: ${err.message}`)
  process.exit(1)
}

const app = createApp(config, createPools(config))
app.listen(config.api.port, config.api.host)

console.log(`opi-api (${config.modules.join(', ')}) listening on ${config.api.host}:${config.api.port}`)
