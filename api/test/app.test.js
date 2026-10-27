const test = require('node:test')
const assert = require('node:assert')
const { createApp } = require('../app')
const { loadConfig } = require('../config')

const ENV = {
  OPI_MODULES: 'brc20,bitmap,sns,runes',
  DB_DATABASE: 'b',
  BITMAP_DB_DATABASE: 'm',
  SNS_DB_DATABASE: 's',
  RUNES_DB_DATABASE: 'r',
}

// A pool double: `handler(sql, params)` returns the rows, or throws.
function fake_pool(handler) {
  const calls = []
  return {
    calls,
    query: async (sql, params = []) => {
      calls.push({ sql, params })
      return { rows: handler(sql, params) }
    },
  }
}

// Starts the app on an ephemeral port, runs `fn(base_url)`, then stops it.
async function with_app(env, pools, fn) {
  const app = createApp(loadConfig(env), pools)
  const server = await new Promise((resolve) => {
    const s = app.listen(0, '127.0.0.1', () => resolve(s))
  })
  try {
    return await fn(`http://127.0.0.1:${server.address().port}`)
  } finally {
    await new Promise((resolve) => server.close(resolve))
  }
}

test('a disabled module answers 404 under its prefix', async () => {
  const brc20 = fake_pool(() => [{ max_block_height: 7 }])
  await with_app({ DB_DATABASE: 'b' }, { brc20 }, async (base) => {
    for (const path of ['/v1/bitmap/block_height', '/v1/sns/ip', '/v1/runes/event', '/v1/runes']) {
      const res = await fetch(base + path)
      assert.strictEqual(res.status, 404, path)
      const module_name = path.split('/')[2]
      assert.deepStrictEqual(await res.json(), { error: `${module_name} is not enabled on this node` })
    }
  })
})

test('an enabled module route reaches its own pool', async () => {
  const brc20 = fake_pool(() => [{ db_version: 7 }])
  const runes = fake_pool(() => [])
  await with_app({ ...ENV, OPI_MODULES: 'brc20,runes' }, { brc20, runes }, async (base) => {
    const res = await fetch(base + '/v1/brc20/db_version')
    assert.strictEqual(res.status, 200)
    assert.strictEqual(await res.text(), '7')
    assert.match(brc20.calls[0].sql, /brc20_indexer_version/)
    assert.strictEqual(runes.calls.length, 0)
  })
})

test('an enabled module keeps its query params and error shape', async () => {
  const brc20 = fake_pool(() => [])
  await with_app({ DB_DATABASE: 'b' }, { brc20 }, async (base) => {
    const res = await fetch(base + '/v1/brc20/event')
    assert.strictEqual(res.status, 400)
    assert.deepStrictEqual(await res.json(), { error: 'inscription_id is required', result: null })
  })
})

test('/v1/status lists each enabled module with its latest height', async () => {
  const pools = {
    brc20: fake_pool((sql) => {
      assert.match(sql, /brc20_block_hashes/)
      return [{ max_block_height: 100 }]
    }),
    bitmap: fake_pool(() => [{ max_block_height: null }]),
    sns: fake_pool(() => {
      throw new Error('database "s" does not exist')
    }),
  }
  await with_app({ ...ENV, OPI_MODULES: 'brc20,bitmap,sns' }, pools, async (base) => {
    const res = await fetch(base + '/v1/status')
    assert.strictEqual(res.status, 200)
    assert.deepStrictEqual(await res.json(), {
      error: null,
      result: {
        modules: {
          brc20: { block_height: 100 },
          bitmap: { block_height: null },
          sns: { error: 'database "s" does not exist' },
        },
      },
    })
  })
})

test('bitmap and sns block_height answer with the height', async () => {
  const pools = {
    bitmap: fake_pool((sql) => {
      assert.match(sql, /bitmap_block_hashes/)
      return [{ max_block_height: 840000 }]
    }),
    sns: fake_pool((sql) => {
      assert.match(sql, /sns_block_hashes/)
      return [{ max_block_height: 840001 }]
    }),
  }
  await with_app({ ...ENV, OPI_MODULES: 'bitmap,sns' }, pools, async (base) => {
    assert.strictEqual(await (await fetch(base + '/v1/bitmap/block_height')).text(), '840000')
    assert.strictEqual(await (await fetch(base + '/v1/sns/block_height')).text(), '840001')
  })
})

test('bitmap block_height answers 500 when the query fails', async () => {
  const bitmap = fake_pool(() => {
    throw new Error('down')
  })
  const log = console.log
  console.log = () => {}
  try {
    await with_app({ ...ENV, OPI_MODULES: 'bitmap' }, { bitmap }, async (base) => {
      const res = await fetch(base + '/v1/bitmap/block_height')
      assert.strictEqual(res.status, 500)
      assert.deepStrictEqual(await res.json(), { error: 'internal error', result: null })
    })
  } finally {
    console.log = log
  }
})

test('the ip route of each enabled module stays', async () => {
  const pools = { runes: fake_pool(() => []) }
  await with_app({ ...ENV, OPI_MODULES: 'runes' }, pools, async (base) => {
    const res = await fetch(base + '/v1/runes/ip')
    assert.strictEqual(res.status, 200)
    assert.match(await res.text(), /127\.0\.0\.1/)
  })
})

test('an async handler failure answers 500 and the process keeps serving', async () => {
  const pools = {
    bitmap: fake_pool(() => {
      throw new Error('down')
    }),
    sns: fake_pool((sql) => {
      if (/sns_namespaces/.test(sql)) return [{ namespace: 'n' }]
      throw new Error('down')
    }),
    runes: fake_pool(() => []),
  }
  const log = console.log
  const error = console.error
  console.log = () => {}
  console.error = () => {}
  try {
    await with_app({ ...ENV, OPI_MODULES: 'bitmap,sns,runes' }, pools, async (base) => {
      for (const path of ['/v1/bitmap/get_hash_of_all_activity', '/v1/sns/get_hash_of_all_activity']) {
        const res = await fetch(base + path)
        assert.strictEqual(res.status, 500, path)
        assert.deepStrictEqual(await res.json(), { error: 'internal error', result: null })
      }
      const ok = await fetch(base + '/v1/runes/ip')
      assert.strictEqual(ok.status, 200)
    })
  } finally {
    console.log = log
    console.error = error
  }
})

test('sns get_registered_namespaces queries without a params argument', async () => {
  const sns = fake_pool(() => [{ namespace: 'n' }])
  await with_app({ ...ENV, OPI_MODULES: 'sns' }, { sns }, async (base) => {
    const res = await fetch(base + '/v1/sns/get_registered_namespaces')
    assert.strictEqual(res.status, 200)
    assert.deepStrictEqual(await res.json(), { error: null, result: [{ namespace: 'n' }] })
    assert.deepStrictEqual(sns.calls[0].params, [])
  })
})

test('the rate limiter applies to every module when enabled', async () => {
  const env = { DB_DATABASE: 'b', RATE_LIMIT_ENABLE: 'true', RATE_LIMIT_MAX: '2' }
  await with_app(env, { brc20: fake_pool(() => []) }, async (base) => {
    const statuses = []
    for (let i = 0; i < 3; i++) {
      statuses.push((await fetch(base + '/v1/brc20/ip')).status)
    }
    assert.deepStrictEqual(statuses, [200, 200, 429])
  })
})
