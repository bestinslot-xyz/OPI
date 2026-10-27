const path = require('path')

const MODULES = ['brc20', 'bitmap', 'sns', 'runes']

// brc20 keeps the unprefixed DB_* variables; the other modules use <MODULE>_DB_*.
const DB_PREFIX = { brc20: 'DB', bitmap: 'BITMAP_DB', sns: 'SNS_DB', runes: 'RUNES_DB' }

// Loads the .env opi-indexer reads (the repository root one), or the file
// OPI_ENV_FILE names. Variables already in the environment win.
function loadEnv(env = process.env) {
  const env_file = env.OPI_ENV_FILE || path.join(__dirname, '..', '.env')
  require('dotenv').config({ path: env_file })
}

// Parses OPI_MODULES with the rules of opi-indexer: comma-separated, entries
// trimmed, empty entries ignored, duplicates dropped. Unset means brc20.
function parseModules(value) {
  const modules = []
  const entries = (value === undefined ? 'brc20' : value)
    .split(',')
    .map((name) => name.trim())
    .filter((name) => name !== '')
  for (const name of entries) {
    if (!MODULES.includes(name)) {
      throw new Error(`unknown module \`${name}\` in OPI_MODULES (supported: ${MODULES.join(', ')})`)
    }
    if (!modules.includes(name)) {
      modules.push(name)
    }
  }
  if (modules.length === 0) {
    throw new Error(`OPI_MODULES names no module (supported: ${MODULES.join(', ')})`)
  }
  return modules
}

function parse_db_config(env, module_name) {
  const prefix = DB_PREFIX[module_name]
  const database = env[`${prefix}_DATABASE`]
  if (!database) {
    throw new Error(`${prefix}_DATABASE is not set (required because ${module_name} is enabled)`)
  }
  return {
    user: env[`${prefix}_USER`] || 'postgres',
    host: env[`${prefix}_HOST`] || 'localhost',
    port: parseInt(env[`${prefix}_PORT`] || '5432'),
    database: database,
    password: env[`${prefix}_PASSWD`],
    ssl: env[`${prefix}_SSL`] == 'true' ? true : false,
    max: parseInt(env.DB_MAX_CONNECTIONS || '10'), // maximum number of clients per pool
    connectionTimeoutMillis: 5000, // fail a query when no connection is available in time
  }
}

// Builds the API configuration from environment variables. Throws an Error
// with a message fit for the operator when the configuration is unusable.
function loadConfig(env) {
  const modules = parseModules(env.OPI_MODULES)
  const dbs = {}
  for (const module_name of modules) {
    dbs[module_name] = parse_db_config(env, module_name)
  }
  return {
    modules,
    dbs,
    api: {
      host: env.API_HOST || '127.0.0.1',
      port: parseInt(env.API_PORT || '8000'),
      trustedProxies: parseInt(env.API_TRUSTED_PROXY_CNT || '0'),
    },
    rateLimit: {
      enabled: (env.RATE_LIMIT_ENABLE || 'false') === 'true',
      windowMs: parseInt(env.RATE_LIMIT_WINDOW_MS || String(15 * 60 * 1000)),
      max: parseInt(env.RATE_LIMIT_MAX || '100'),
    },
  }
}

module.exports = { MODULES, parseModules, loadConfig, loadEnv }
