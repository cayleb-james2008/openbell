// kalshi-curl.mjs: stands in for `curl` on the fixture's PATH, so the collector never reaches the venue.
// A public markets read gets the recorded pass in kalshi-markets.json, its close times moved so the open window
// closes at the next quarter hour; anything else (the updater, any other host) fails as unreachable, exit 7.
import fs from 'node:fs'
const url = process.argv.slice(2).find(a => a.startsWith('https://api.elections.kalshi.com/trade-api/v2/markets?'))
if (!url) { process.stderr.write('fixture curl: no network in the UI check\n'); process.exit(7) }
const q = new URL(url).searchParams, rec = JSON.parse(fs.readFileSync(new URL('kalshi-markets.json', import.meta.url), 'utf8'))
const quarter = 15 * 60_000, shift = Math.ceil(Date.now() / quarter) * quarter - Date.parse(rec.anchor)
const markets = (rec.series[q.get('series_ticker')]?.[q.get('status')] || [])
  .map(m => ({ ...m, close_time: new Date(Date.parse(m.close_time) + shift).toISOString().replace('.000Z', 'Z') }))
process.stdout.write(JSON.stringify({ markets, cursor: '' }))
