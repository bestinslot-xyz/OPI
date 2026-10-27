const test = require('node:test')
const assert = require('node:assert')
const { parseModules, loadConfig } = require('../config')

test('parseModules defaults to brc20 when unset', () => {
  assert.deepStrictEqual(parseModules(undefined), ['brc20'])
})

test('parseModules trims, ignores empty entries and dedups', () => {
  assert.deepStrictEqual(parseModules(' sns , ,runes,sns,brc20 '), ['sns', 'runes', 'brc20'])
})

test('parseModules rejects an unknown module', () => {
  assert.throws(() => parseModules('brc20,ordinals'), /unknown module `ordinals` in OPI_MODULES/)
})

test('parseModules rejects an empty list', () => {
  assert.throws(() => parseModules(' , '), /OPI_MODULES names no module/)
})

test('loadConfig requires DB_DATABASE for brc20', () => {
  assert.throws(() => loadConfig({}), /DB_DATABASE is not set/)
})

test('loadConfig requires a database name per enabled module', () => {
  const env = { OPI_MODULES: 'brc20,bitmap,sns,runes', DB_DATABASE: 'b' }
  assert.throws(() => loadConfig(env), /BITMAP_DB_DATABASE is not set/)
  assert.throws(() => loadConfig({ ...env, BITMAP_DB_DATABASE: 'm' }), /SNS_DB_DATABASE is not set/)
  assert.throws(
    () => loadConfig({ ...env, BITMAP_DB_DATABASE: 'm', SNS_DB_DATABASE: 's' }),
    /RUNES_DB_DATABASE is not set/
  )
})

test('loadConfig ignores the settings of disabled modules', () => {
  const config = loadConfig({ OPI_MODULES: 'sns', SNS_DB_DATABASE: 's' })
  assert.deepStrictEqual(config.modules, ['sns'])
  assert.deepStrictEqual(Object.keys(config.dbs), ['sns'])
})

test('loadConfig reads prefixed and unprefixed DB variables', () => {
  const config = loadConfig({
    OPI_MODULES: 'brc20,runes',
    DB_DATABASE: 'b',
    DB_USER: 'bu',
    DB_HOST: 'bh',
    DB_PORT: '5433',
    DB_PASSWD: 'bp',
    DB_SSL: 'true',
    RUNES_DB_DATABASE: 'r',
    RUNES_DB_USER: 'ru',
    DB_MAX_CONNECTIONS: '3',
  })
  assert.deepStrictEqual(config.dbs.brc20, {
    user: 'bu', host: 'bh', port: 5433, database: 'b', password: 'bp', ssl: true, max: 3, connectionTimeoutMillis: 5000,
  })
  assert.deepStrictEqual(config.dbs.runes, {
    user: 'ru', host: 'localhost', port: 5432, database: 'r', password: undefined, ssl: false, max: 3, connectionTimeoutMillis: 5000,
  })
})

test('loadConfig applies API and rate limit defaults', () => {
  const config = loadConfig({ DB_DATABASE: 'b' })
  assert.strictEqual(config.api.host, '127.0.0.1')
  assert.strictEqual(config.api.port, 8000)
  assert.strictEqual(config.api.trustedProxies, 0)
  assert.deepStrictEqual(config.rateLimit, { enabled: false, windowMs: 900000, max: 100 })
})

test('loadConfig reads API and rate limit settings', () => {
  const config = loadConfig({
    DB_DATABASE: 'b',
    API_HOST: '0.0.0.0',
    API_PORT: '9000',
    API_TRUSTED_PROXY_CNT: '2',
    RATE_LIMIT_ENABLE: 'true',
    RATE_LIMIT_WINDOW_MS: '1000',
    RATE_LIMIT_MAX: '5',
  })
  assert.strictEqual(config.api.host, '0.0.0.0')
  assert.strictEqual(config.api.port, 9000)
  assert.strictEqual(config.api.trustedProxies, 2)
  assert.deepStrictEqual(config.rateLimit, { enabled: true, windowMs: 1000, max: 5 })
})
