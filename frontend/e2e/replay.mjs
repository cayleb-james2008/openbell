// replay.mjs: OpenBell's share of the step 9 worst cases, fed through browser page.route mocks only (odd, long and
// malformed data must not blank or break the page). Run through `npm run check:ui` on fresh fixture data; shots to .out/replay.
import { BASE, OUT as E2E_OUT, launch, report } from './fixture.mjs'
import fs from 'node:fs'
const OUT = `${E2E_OUT}/replay`
fs.mkdirSync(OUT, { recursive: true })
const b = await launch()
const res = []
const ok = (n, p, d = '') => { res.push({ n, p: !!p, d }); console.log((p ? 'PASS ' : 'FAIL ') + n + (d ? '  ' + d : '')) }
const LONG = 'x'.repeat(260)
const clipped = p => p.evaluate(() => [...document.querySelectorAll('body *')].filter(e => { const r = e.getBoundingClientRect(); if (!r.width || r.right <= innerWidth + 1) return false; for (let q = e.parentElement; q; q = q.parentElement) { const o = getComputedStyle(q).overflowX; if (o !== 'visible') return false } return true }).length)
const textOverflow = (p, sel) => p.evaluate(s => [...document.querySelectorAll(s)].filter(e => e.scrollWidth > e.clientWidth + 1).length, sel)
async function page(w, url, routes, theme = 'light') {
  const ctx = await b.newContext({ viewport: { width: w, height: w < 768 ? 844 : 900 } })
  const p = await ctx.newPage(); p.errs = []
  p.on('pageerror', e => p.errs.push(e.message))
  for (const [pat, fn] of routes) await p.route(pat, fn)
  await p.goto(url); await p.waitForTimeout(2200)
  return { ctx, p }
}
const json = (r, body, status = 200) => r.fulfill({ status, contentType: 'application/json', body: typeof body === 'string' ? body : JSON.stringify(body) })
const patch = (fn) => async r => { const res = await r.fetch(); const j = await res.json(); r.fulfill({ response: res, body: JSON.stringify(fn(j)) }) }

for (const w of [1440, 390]) {
  { const { ctx, p } = await page(w, BASE + '?dataset=fixture', [
      ['**/api/strategies*', patch(j => [...j, { ...j[0], strategy: null }])], ['**/api/markets', patch(j => [{ ...j[0], ticker: null, close_time: null }, ...j])],
      ['**/api/slots*', patch(j => ({ ...j, paper_pot_start: null }))]])
    ok(`openbell ${w}: null strategy, ticker, close time and pot start do not blank the desk`, !p.errs.length && await p.locator('#readings').count() === 1, p.errs.join('|')); await ctx.close() }
  { const { ctx, p } = await page(w, BASE + '?dataset=fixture', [['**/api/strategies*', patch(j => j.map((s, i) => i ? s : { ...s, strategy: 'strategy-' + LONG }))], ['**/api/slots*', patch(j => ({ ...j, slots: j.slots.map((s, i) => i ? s : { ...s, strategy: 'slot-' + LONG }) }))]])
    ok(`openbell ${w}: long unbroken names stay in their rows`, (await clipped(p)) === 0, String(await clipped(p)))
    ok(`openbell ${w}: header chips hold their words`, (await textOverflow(p, '#chip, header [title^="Paper only"]')) === 0); await p.screenshot({ path: `${OUT}/openbell-long-${w}.png` }); await ctx.close() }
  { const { ctx, p } = await page(w, BASE + '?dataset=fixture', [['**/api/strategies*', patch(j => [...j, { ...j[0], strategy: null, standard: null }, { ...j[0], strategy: 5, standard: { min_windows: 'many' } }])]])
    ok(`openbell ${w}: null names and standards still tally in the standing line`, !p.errs.length && (await p.locator('#standing-figure').innerText().catch(() => '')).trim() === '0 of 7', p.errs.join('|') + ' ' + await p.locator('#standing').innerText().catch(() => 'no band')); await ctx.close() }
  // a forward pass is named in the line: the live tape, two proven strategies with long unbroken names
  { const PASS = 'PROVEN on forward evidence (paper only — no orders placed)'
    const { ctx, p } = await page(w, BASE, [['**/api/strategies*', patch(j => j.map((s, i) => i < 2 ? { ...s, verdict: PASS, strategy: 'strategy-' + LONG } : s))]])
    ok(`openbell ${w}: long unbroken proven names stay inside the standing line`, (await p.locator('#standing-figure').innerText().catch(() => '')).trim() === '2 of 5' && (await p.locator('#standing-detail').textContent().catch(() => '')).includes('proven — strategy-x') && (await clipped(p)) === 0 && (await textOverflow(p, '#standing li span')) === 0 && (await p.evaluate(() => { const s = document.querySelector('main > div.grid'); return s.scrollWidth - s.clientWidth })) === 0, 'clipped ' + await clipped(p) + ', the surface scrolls ' + await p.evaluate(() => { const s = document.querySelector('main > div.grid'); return s.scrollWidth - s.clientWidth }) + 'px'); await p.screenshot({ path: `${OUT}/openbell-standing-long-${w}.png` }); await ctx.close() }
  // standards that differ between strategies: no single window count is claimed
  { const { ctx, p } = await page(w, BASE + '?dataset=fixture', [['**/api/strategies*', patch(j => j.map((s, i) => ({ ...s, standard: { ...s.standard, min_windows: 100 + i } })))]])
    const t = await p.locator('#standing-detail').textContent().catch(() => '')
    ok(`openbell ${w}: differing standards say waiting for evidence and claim no window count`, /waiting for evidence/.test(t) && !/below/.test(t), t.replace(/\s+/g, ' ')); await ctx.close() }
  { const { ctx, p } = await page(w, BASE + '?dataset=fixture', [['**/api/strategies*', r => json(r, { not: 'a list' })]])
    ok(`openbell ${w}: a strategies body of the wrong shape says the state is unavailable`, (await p.locator('#standing-label').innerText().catch(() => '')) === 'Strategy state unavailable', p.errs.join('|')); await ctx.close() }
  { const { ctx, p } = await page(w, BASE + '?dataset=fixture', [['**/api/collect', r => json(r, { snapshots_written: 8, errors: ['KXETH15M/open: venue response had no markets array'] })]])
    await p.locator('#collect-btn').click(); await p.waitForTimeout(800)
    ok(`openbell ${w}: a partial collect names the failed read`, /Collected one pass, but 1 market read failed/.test(await p.locator('#collect-status').innerText().catch(() => ''))); await ctx.close() }
}
await b.close()
report('replay', res)
