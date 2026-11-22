// replay.mjs: OpenBell v0.4.0 worst-case replay suite (null rows, malformed fields, 260-char strings, partial collects).
import { BASE, OUT as E2E_OUT, launch, report } from './fixture.mjs'
import fs from 'node:fs'

const OUT = `${E2E_OUT}/replay`
fs.mkdirSync(OUT, { recursive: true })
const b = await launch()
const res = []
const ok = (n, p, d = '') => { res.push({ n, p: !!p, d }); console.log((p ? 'PASS ' : 'FAIL ') + n + (d ? '  ' + d : '')) }
const LONG = 'x'.repeat(260)

const clipped = p => p.evaluate(() => [...document.querySelectorAll('body *')].filter(e => {
  if (e.closest('[aria-hidden="true"]')) return false
  const r = e.getBoundingClientRect()
  if (!r.width || r.right <= innerWidth + 1) return false
  for (let q = e.parentElement; q; q = q.parentElement) {
    const o = getComputedStyle(q).overflowX
    if (o !== 'visible') return false
  }
  return true
}).length)

async function openPage(w, url, routes) {
  const ctx = await b.newContext({ viewport: { width: w, height: w < 768 ? 844 : 900 } })
  const p = await ctx.newPage(); p.errs = []
  p.on('pageerror', e => p.errs.push(e.message))
  for (const [pat, fn] of routes) await p.route(pat, fn)
  await p.goto(url); await p.waitForTimeout(1600)
  return { ctx, p }
}
const json = (r, body, status = 200) => r.fulfill({ status, contentType: 'application/json', body: typeof body === 'string' ? body : JSON.stringify(body) })
const patch = (fn) => async r => { const res = await r.fetch(); const j = await res.json(); r.fulfill({ response: res, body: JSON.stringify(fn(j)) }) }
const baseStrategies = await fetch(BASE + 'api/strategies?dataset=fixture').then(r => r.json())
const baseCount = Array.isArray(baseStrategies) ? baseStrategies.length : 5

for (const w of [1440, 390]) {
  {
    const { ctx, p } = await openPage(w, BASE + '?dataset=fixture', [
      ['**/api/strategies*', patch(j => [...j, { ...j[0], strategy: null }])],
      ['**/api/markets*', patch(j => [{ ...j[0], ticker: null, close_time: null }, ...j])],
      ['**/api/slots*', patch(j => ({ ...j, paper_pot_start: null }))],
    ])
    ok(`openbell ${w}: null strategy, ticker, close time and pot start do not blank the desk`, !p.errs.length && (await p.locator('#readings').count()) === 1, p.errs.join('|'))
    await ctx.close()
  }
  {
    const { ctx, p } = await openPage(w, BASE + '?dataset=fixture', [
      ['**/api/strategies*', patch(j => j.map((s, i) => i ? s : { ...s, strategy: 'strategy-' + LONG }))],
      ['**/api/slots*', patch(j => ({ ...j, slots: j.slots.map((s, i) => i ? s : { ...s, strategy: 'slot-' + LONG }) }))],
      ['**/api/markets*', patch(j => j.map((m, i) => i ? m : { ...m, ticker: 'KX-' + LONG }))],
    ])
    ok(`openbell ${w}: long unbroken tickers stay inside viewport`, (await clipped(p)) === 0, String(await clipped(p)))
    await p.locator('#tab-strategies').click(); await p.waitForTimeout(300)
    ok(`openbell ${w}: long unbroken strategy names stay inside viewport`, (await clipped(p)) === 0, String(await clipped(p)))
    await p.screenshot({ path: `${OUT}/openbell-long-${w}.png` })
    await ctx.close()
  }
  {
    const { ctx, p } = await openPage(w, BASE + '?dataset=fixture', [
      ['**/api/strategies*', patch(j => [...j, { ...j[0], strategy: null, standard: null }, { ...j[0], strategy: 5, standard: { min_windows: 'many' } }])],
    ])
    ok(`openbell ${w}: null names and standards still tally in standing tile`,
      !p.errs.length && (await p.locator('#standing-figure').textContent().catch(() => '')).trim() === `0 of ${baseCount + 2}`,
      p.errs.join('|'))
    await ctx.close()
  }
  {
    const PASS = 'PROVEN on forward evidence (paper only - no orders placed)'
    const { ctx, p } = await openPage(w, BASE, [
      ['**/api/strategies*', patch(j => j.map((s, i) => i < 2 ? { ...s, verdict: PASS, strategy: 'strategy-' + LONG } : s))],
    ])
    ok(`openbell ${w}: long unbroken proven names stay inside standing tile`,
      (await p.locator('#standing-figure').textContent().catch(() => '')).trim() === `2 of ${baseCount}` &&
      (await p.locator('#standing-detail').textContent().catch(() => '')).includes('2 proven: strategy-x') &&
      (await clipped(p)) === 0,
      'clipped=' + await clipped(p))
    await ctx.close()
  }
  {
    const { ctx, p } = await openPage(w, BASE + '?dataset=fixture', [
      ['**/api/strategies*', patch(j => j.map((s, i) => ({ ...s, standard: { ...s.standard, min_windows: 100 + i } })))],
    ])
    const t = await p.locator('#standing-detail').textContent().catch(() => '')
    ok(`openbell ${w}: differing standards claim no window count`, /waiting for evidence/.test(t) && !/below/.test(t), t)
    await ctx.close()
  }
  {
    const { ctx, p } = await openPage(w, BASE + '?dataset=fixture', [['**/api/strategies*', r => json(r, { not: 'a list' })]])
    ok(`openbell ${w}: wrong-shape strategies says Strategy state unavailable`, (await p.locator('#standing-label').innerText().catch(() => '')) === 'Strategy state unavailable')
    await ctx.close()
  }
  {
    const { ctx, p } = await openPage(w, BASE + '?dataset=fixture', [
      ['**/api/strategies*', patch(j => [null, ...j.map((s, i) => i === 0 ? { ...s, verdict: null } : i === 1 ? { ...s, verdict: 42, windows_traded: null, mean_cents_per_contract: 'bad' } : s)])],
    ])
    await p.locator('#tab-strategies').click(); await p.waitForTimeout(300)
    const bodyTxt = await p.locator('body').innerText().catch(() => '')
    ok(`openbell ${w}: null entry and non-string verdict render without crash or NaN`,
      !p.errs.length && (await p.locator('#strategies tbody tr button[data-strategy-row]').count()) === baseCount && !/\bNaN\b|\bundefined\b/.test(bodyTxt),
      p.errs.join('|'))
    await ctx.close()
  }
  {
    const { ctx, p } = await openPage(w, BASE + '?dataset=fixture', [
      ['**/api/collect', r => json(r, { snapshots_written: 8, errors: ['KXETH15M/open: venue response had no markets array'] })],
    ])
    await p.locator('#collect-btn').click(); await p.waitForTimeout(600)
    ok(`openbell ${w}: partial collect names failed read count`, /1 market read failed/.test(await p.locator('#collect-status').innerText().catch(() => '')))
    await ctx.close()
  }
}

await b.close()
report('replay', res)
