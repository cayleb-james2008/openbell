// gate.mjs: OpenBell Ledger gate, live and fixture tapes. /api/collect is stubbed in the browser, so the gate never
// triggers a collection pass. Run through `npm run check:ui`, which builds and starts the fixture first; shots to .out/gate.
import { BASE, OUT as E2E_OUT, launch, report } from './fixture.mjs'
import fs from 'node:fs'
const FX = BASE + '?dataset=fixture', OUT = `${E2E_OUT}/gate`
fs.mkdirSync(OUT, { recursive: true })
const b = await launch()
const MEASURE = () => {
  const vis = el => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none' }
  const cv = document.createElement('canvas'); cv.width = cv.height = 1
  const cx = cv.getContext('2d', { willReadFrequently: true })
  const rgba = c => { cx.clearRect(0, 0, 1, 1); cx.fillStyle = '#000'; cx.fillStyle = c; cx.fillRect(0, 0, 1, 1); const d = cx.getImageData(0, 0, 1, 1).data; return [d[0], d[1], d[2], d[3] / 255] }
  const lin = v => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4 }
  const L = ([r, g, bb]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(bb)
  const bgOf = el => { for (let e = el; e; e = e.parentElement) { const c = rgba(getComputedStyle(e).backgroundColor); if (c[3] > 0.5) return c.slice(0, 3) } return [255, 255, 255] }
  const inter = [...document.querySelectorAll('a[href],button,input,select,textarea,[role=button],summary,[tabindex]:not([tabindex="-1"])')].filter(vis)
  const name = el => (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('placeholder') || '').trim()
  const min = innerWidth < 768 ? 44 : 24; const small = inter.filter(e => { const r = e.getBoundingClientRect(); return r.width < min || r.height < min }).map(e => `${e.tagName}:${name(e).slice(0, 30)}:${Math.round(e.getBoundingClientRect().width)}x${Math.round(e.getBoundingClientRect().height)}`)
  const unnamed = inter.filter(e => !name(e) && !(e.id && document.querySelector(`label[for="${e.id}"]`))).map(e => e.outerHTML.slice(0, 90))
  const low = []; let minFont = 99; let minRatio = 99
  for (const el of document.querySelectorAll('body *')) {
    if (!vis(el) || ![...el.childNodes].some(n => n.nodeType === 3 && n.textContent.trim())) continue
    const fs = parseFloat(getComputedStyle(el).fontSize); minFont = Math.min(minFont, fs)
    const fg = rgba(getComputedStyle(el).color); const bg = bgOf(el)
    const a = fg[3]; const mix = [0, 1, 2].map(i => fg[i] * a + bg[i] * (1 - a))
    const l1 = L(mix), l2 = L(bg); const ratio = (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05)
    minRatio = Math.min(minRatio, ratio)
    if (ratio < (fs >= 18.66 ? 3 : 4.5)) low.push(`${ratio.toFixed(2)}:${(el.innerText || '').trim().slice(0, 30)}`)
  }
  const clipped = [...document.querySelectorAll('body *')].filter(e => { if (!vis(e)) return false; const r = e.getBoundingClientRect(); if (r.right <= innerWidth + 1 && r.left >= -1) return false; for (let p = e.parentElement; p; p = p.parentElement) { const o = getComputedStyle(p).overflowX; if (o === 'auto' || o === 'scroll' || o === 'hidden' || o === 'clip') return false } return true }).map(e => e.tagName + '.' + String(e.className?.baseVal ?? e.className).slice(0, 40) + ' r=' + Math.round(e.getBoundingClientRect().right)).slice(0, 5)
  return { clipped, docOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth, interactive: inter.length, smallTargets: small.length, smallSample: small.slice(0, 20), unnamed: unnamed.slice(0, 4), lowContrast: low.length, lowSample: low.slice(0, 6), minRatio: Math.round(minRatio * 100) / 100, minFont }
}

const errs = [], checks = []
const ok = (name, pass, detail = '') => { checks.push({ name, pass: !!pass, detail }); console.log((pass ? 'PASS ' : 'FAIL ') + name + (detail ? '  ' + detail : '')) }
async function open(w, h, theme = 'light', extra = {}) {
  const ctx = await b.newContext({ viewport: { width: w, height: h }, colorScheme: theme, hasTouch: w < 768, ...extra })
  await ctx.addInitScript((t) => { try { localStorage.setItem('openbell-theme', t) } catch {} }, theme)
  const p = await ctx.newPage()
  p.__allow = /$^/
  p.on('pageerror', e => errs.push(`${w}-${theme}: pageerror ${e.message}`))
  p.on('console', m => { if (m.type() === 'error' && !p.__allow.test(m.text())) errs.push(`${w}-${theme}: ${m.text().slice(0, 140)} @ ${m.location().url}`) })
  await p.route('**/api/collect', r => r.fulfill({ status: 200, contentType: 'application/json', body: '{"written":5}' }))
  return { ctx, p }
}
const ready = p => p.waitForSelector('#strategies table, #strategies [role=alert]', { timeout: 15000 })
const rowsShown = p => p.locator('#strategies tbody tr button[data-strategy-row]').count()
const drawerTitle = p => p.locator('[data-strategy-drawer] h2').first().innerText().catch(() => '')

// 1. measurements, live and fixture
const measures = []
for (const [dn, url] of [['live', BASE], ['fixture', FX]]) for (const theme of ['light', 'dark']) for (const [vn, w, h] of [['1440', 1440, 900], ['768', 768, 1024], ['390', 390, 844], ['320', 320, 640]]) {
  const { ctx, p } = await open(w, h, theme)
  await p.goto(url); await ready(p); await p.waitForTimeout(800)
  const m = await p.evaluate(MEASURE); m.size = vn; m.theme = theme; m.data = dn
  measures.push(m)
  if (vn === '1440' || vn === '390') await p.screenshot({ path: `${OUT}/${dn}-${vn}-${theme}.png` })
  await ctx.close()
}
for (const m of measures) ok(`measure ${m.data} ${m.size} ${m.theme}`, m.docOverflow <= 0 && m.clipped.length === 0 && m.smallTargets === 0 && m.lowContrast === 0 && m.unnamed.length === 0,
  `overflow ${m.docOverflow} clipped ${m.clipped.join(' | ')} small ${m.smallTargets} ${m.smallSample.join(' | ')} low ${m.lowContrast} ${m.lowSample.join(' | ')} unnamed ${m.unnamed.join(' | ')} minFont ${m.minFont} minRatio ${m.minRatio}`)

// 2. interactions on the fixture tape (desktop)
{
  const { ctx, p } = await open(1440, 900)
  await p.goto(FX); await ready(p)
  ok('fixture banner', await p.locator('text=Example tape.').count() === 1)
  ok('no row highlighted at rest', await p.locator('#strategies tr[aria-selected="true"]').count() === 0)
  ok('five strategies', await rowsShown(p) === 5, String(await rowsShown(p)))
  for (const [f, n] of [['Proven', 2], ['Not proven', 1], ['Waiting for evidence', 2], ['All', 5]]) {
    await p.getByRole('button', { name: f, exact: true }).click(); await p.waitForTimeout(150)
    ok(`filter ${f}`, await rowsShown(p) === n, String(await rowsShown(p)))
  }
  await p.getByRole('searchbox', { name: 'Search strategies' }).fill('late'); await p.waitForTimeout(150)
  ok('search late', await rowsShown(p) === 2, String(await rowsShown(p)))
  await p.getByRole('searchbox', { name: 'Search strategies' }).fill('zzz'); await p.waitForTimeout(150)
  ok('search empty state', await p.locator('#strategies [role=status]').innerText().then(t => /No strategy matches/.test(t)))
  await p.getByRole('button', { name: 'Show all strategies' }).click(); await p.waitForTimeout(150)
  ok('show all restores', await rowsShown(p) === 5)
  // keyboard: j / k / Enter
  await p.locator('body').click({ position: { x: 5, y: 300 } })
  await p.keyboard.press('j'); await p.waitForTimeout(100)
  ok('j highlights the first row', await p.locator('#strategies tr[aria-selected="true"] button').innerText() === 'favourite-floor')
  await p.keyboard.press('j'); await p.keyboard.press('j'); await p.keyboard.press('k'); await p.waitForTimeout(100)
  ok('j j k lands on the second row', await p.locator('#strategies tr[aria-selected="true"] button').innerText() === 'favourite-late')
  await p.keyboard.press('Enter'); await p.waitForTimeout(700)
  ok('Enter opens the drawer', (await drawerTitle(p)) === 'favourite-late', await drawerTitle(p))
  ok('drawer draws the paper line', await p.locator('[data-strategy-drawer] svg[role=img]').count() >= 1)
  ok('drawer lists paper trades', await p.locator('[data-strategy-drawer] li').count() > 0)
  await p.screenshot({ path: `${OUT}/drawer-1440.png` })
  await p.keyboard.press('j'); await p.waitForTimeout(300)
  ok('j follows inside the drawer', (await drawerTitle(p)) === 'longshot-late', await drawerTitle(p))
  await p.keyboard.press('Escape'); await p.waitForTimeout(600)
  ok('Escape closes the drawer', await p.locator('[data-strategy-drawer]').count() === 0)
  await p.getByRole('button', { name: 'Open mid-window-momentum' }).click(); await p.waitForTimeout(700)
  ok('click opens an empty strategy', (await p.locator('[data-strategy-drawer]').innerText()).includes('No paper trades for this strategy'))
  await p.getByRole('button', { name: 'Close', exact: true }).click(); await p.waitForTimeout(600)
  // Enter on Collect now collects and does not open the drawer
  await p.locator('#collect-btn').focus(); await p.keyboard.press('Enter')
  await p.waitForSelector('text=Collected one pass into the tape.', { timeout: 5000 }).then(() => ok('collect toast (stubbed)', true), () => ok('collect toast (stubbed)', false))
  ok('Enter on Collect does not open the drawer', await p.locator('[data-strategy-drawer]').count() === 0)
  // markets
  const mc = await p.locator('#markets-count').innerText()
  await p.getByRole('searchbox', { name: 'Search markets' }).fill('KXBTC'); await p.waitForTimeout(200)
  ok('market search filters', (await p.locator('#markets-card [role=row]').allInnerTexts()).slice(1).every(t => t.includes('KXBTC')), mc)
  await p.getByRole('searchbox', { name: 'Search markets' }).fill('nothing-like-this'); await p.waitForTimeout(150)
  ok('market search empty', await p.locator('#markets-card [role=status]').count() === 1)
  // report trail
  await p.locator('#report-body').getByText('Show full report').click(); await p.waitForTimeout(500)
  ok('full report opens', await p.locator('#report').isVisible())
  // section nav
  await p.locator('nav[aria-label=Sections] button[data-id=markets]').click(); await p.waitForTimeout(500)
  ok('section nav scrolls', Math.abs(await p.locator('#markets-card').evaluate(e => e.getBoundingClientRect().top)) < 200)
  // tape switch
  await p.locator('#seg-live').click(); await p.waitForTimeout(1200)
  ok('live tape switch', !p.url().includes('dataset=fixture') && await p.locator('text=Example tape.').count() === 0)
  // theme
  await p.getByRole('button', { name: 'Dark appearance' }).click()
  ok('theme toggles', (await p.evaluate(() => document.documentElement.dataset.theme)) === 'dark')
  // how to read
  await p.getByRole('button', { name: 'How to read this' }).click(); await p.waitForTimeout(400)
  ok('how to read opens', await p.locator('text=Anything not obtained reads unavailable').first().isVisible())
  await ctx.close()
}
// 3. collect failure, error, empty and loading states
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /50[03]/
  await p.unroute('**/api/collect'); await p.route('**/api/collect', r => r.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"venue unreachable"}' }))
  await p.goto(FX); await ready(p)
  await p.locator('#collect-btn').click()
  await p.waitForSelector('#collect-status', { timeout: 5000 }).catch(() => {})
  ok('collect failure says what to do', /Nothing was written\. Try again/.test(await p.locator('#collect-status').innerText().catch(() => '')))
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /500/
  await p.route('**/api/strategies*', r => r.fulfill({ status: 500, body: 'no' }))
  await p.goto(FX); await ready(p)
  ok('one read failing shows its own error', await p.locator('#strategies [role=alert]').count() === 1 && await p.locator('#slots-card table').count() === 1)
  await p.screenshot({ path: `${OUT}/error-strategies-1440.png` })
  await p.unroute('**/api/strategies*')
  await p.locator('#strategies [role=alert] button').click(); await p.waitForTimeout(1200)
  ok('retry recovers', await rowsShown(p) === 5)
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /500/
  await p.route('**/api/**', r => r.request().url().includes('/api/update') ? r.continue() : r.fulfill({ status: 500, body: 'no' }))
  await p.goto(FX); await p.waitForSelector('#load-error', { timeout: 10000 }).catch(() => {})
  ok('all reads failing: one banner with Retry', await p.locator('#load-error').count() === 1)
  await p.screenshot({ path: `${OUT}/error-all-1440.png` })
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  await p.route('**/api/strategies*', r => r.fulfill({ status: 200, contentType: 'application/json', body: '[]' }))
  await p.route('**/api/markets*', r => r.fulfill({ status: 200, contentType: 'application/json', body: '[]' }))
  await p.goto(FX); await ready(p); await p.waitForTimeout(400)
  ok('empty library message', (await p.locator('#strategies').innerText()).includes('No strategies in the library'))
  ok('empty tape message', (await p.locator('#markets-card').innerText()).includes('No snapshots collected yet'))
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  await p.route('**/api/home', async r => { await new Promise(s => setTimeout(s, 2500)); r.continue() })
  await p.goto(FX); await p.waitForTimeout(600)
  ok('loading skeletons', await p.locator('#readings span[aria-hidden].bg-track').count() > 0)
  await p.screenshot({ path: `${OUT}/loading-1440.png` })
  await ctx.close()
}
// 4. phone: drawer from the bottom, how to read as a dialog
{
  const { ctx, p } = await open(390, 844)
  await p.goto(FX); await ready(p)
  await p.getByRole('button', { name: 'Open favourite-floor' }).click(); await p.waitForTimeout(800)
  const box = await p.locator('[data-strategy-drawer]').boundingBox()
  ok('phone drawer rises from the bottom', !!box && box.y > 50 && Math.round(box.width) === 390, JSON.stringify(box))
  await p.screenshot({ path: `${OUT}/drawer-390.png` })
  await p.getByRole('button', { name: 'Close', exact: true }).click(); await p.waitForTimeout(600)
  await p.getByRole('button', { name: 'How to read this' }).click(); await p.waitForTimeout(500)
  ok('phone how-to-read dialog', await p.locator('[role=dialog]').isVisible())
  await ctx.close()
}
// 5. reduced motion
{
  const { ctx, p } = await open(1440, 900, 'light', { reducedMotion: 'reduce' })
  await p.goto(FX); await ready(p); await p.waitForTimeout(1500)
  const moving = await p.evaluate(() => document.getAnimations().filter(a => a.playState === 'running' && a.effect?.getTiming().iterations === Infinity).length)
  ok('reduced motion: no endless motion', moving === 0, String(moving))
  await ctx.close()
}
ok('no console errors', errs.length === 0, errs.slice(0, 5).join(' || '))
fs.writeFileSync(`${OUT}/result.json`, JSON.stringify({ checks, measures, errs }, null, 1))
report('gate', checks)
await b.close()
