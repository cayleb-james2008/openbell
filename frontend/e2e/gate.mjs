// gate.mjs: OpenBell v0.4.0 quant desk gate (live & fixture tapes, Settings-only Light/Dark theme, SVG label geometry,
// Tape / Strategies / Slots tabs, Market & Strategy Vaul inspectors, and Update strip).
// /api/collect is stubbed in the browser so this offline suite never touches the network; live Kalshi E2E runs separately.
import { BASE, OUT as E2E_OUT, launch, report } from './fixture.mjs'
import fs from 'node:fs'

const FX = BASE + '?dataset=fixture'
const OUT = `${E2E_OUT}/gate`
fs.mkdirSync(OUT, { recursive: true })
const b = await launch()

const MEASURE = () => {
  const vis = el => {
    const r = el.getBoundingClientRect()
    const s = getComputedStyle(el)
    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'
  }
  const cv = document.createElement('canvas'); cv.width = cv.height = 1
  const cx = cv.getContext('2d', { willReadFrequently: true })
  const rgba = c => {
    cx.clearRect(0, 0, 1, 1); cx.fillStyle = '#000'; cx.fillStyle = c; cx.fillRect(0, 0, 1, 1)
    const d = cx.getImageData(0, 0, 1, 1).data
    return [d[0], d[1], d[2], d[3] / 255]
  }
  const lin = v => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4 }
  const L = ([r, g, bb]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(bb)
  const bgOf = el => {
    for (let e = el; e; e = e.parentElement) {
      const c = rgba(getComputedStyle(e).backgroundColor)
      if (c[3] > 0.5) return c.slice(0, 3)
    }
    return [255, 255, 255]
  }
  const inter = [...document.querySelectorAll('a[href],button,input,select,textarea,[role=button],[role=tab],summary,[tabindex]:not([tabindex="-1"])')].filter(vis)
  const name = el => (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('placeholder') || '').trim()
  const min = innerWidth < 768 ? 44 : 24
  const small = inter
    .filter(e => { const r = e.getBoundingClientRect(); return r.width < min || r.height < min })
    .map(e => `${e.tagName}:${name(e).slice(0, 30)}:${Math.round(e.getBoundingClientRect().width)}x${Math.round(e.getBoundingClientRect().height)}`)
  const unnamed = inter
    .filter(e => !name(e) && !(e.id && document.querySelector(`label[for="${e.id}"]`)))
    .map(e => e.outerHTML.slice(0, 90))
  const low = []
  let minFont = 99
  let minRatio = 99
  for (const el of document.querySelectorAll('body *')) {
    if (!vis(el) || el.closest('[aria-hidden="true"]') || ![...el.childNodes].some(n => n.nodeType === 3 && n.textContent.trim())) continue
    const fsPx = parseFloat(getComputedStyle(el).fontSize)
    minFont = Math.min(minFont, fsPx)
    const fg = rgba(getComputedStyle(el).color)
    const bg = bgOf(el)
    const a = fg[3]
    const mix = [0, 1, 2].map(i => fg[i] * a + bg[i] * (1 - a))
    const l1 = L(mix), l2 = L(bg)
    const ratio = (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05)
    minRatio = Math.min(minRatio, ratio)
    if (ratio < (fsPx >= 18.66 ? 3 : 4.5)) low.push(`${ratio.toFixed(2)}:${(el.innerText || '').trim().slice(0, 30)}`)
  }
  const clipped = [...document.querySelectorAll('body *')]
    .filter(e => {
      if (!vis(e) || e.closest('[aria-hidden="true"]')) return false
      const r = e.getBoundingClientRect()
      if (r.right <= innerWidth + 1 && r.left >= -1) return false
      for (let p = e.parentElement; p; p = p.parentElement) {
        const o = getComputedStyle(p).overflowX
        if (o === 'auto' || o === 'scroll' || o === 'hidden' || o === 'clip') return false
      }
      return true
    })
    .map(e => e.tagName + '.' + String(e.className?.baseVal ?? e.className).slice(0, 40) + ' r=' + Math.round(e.getBoundingClientRect().right))
    .slice(0, 5)
  // SVG text label geometry (matches ultra-design probe.mjs)
  const svgs = [...document.querySelectorAll('svg')].filter(s => vis(s) && s.querySelector('text'))
  const svgOut = [], svgOverlap = [], svgSmall = []
  for (const s of svgs) {
    const box = s.getBoundingClientRect()
    const ts = [...s.querySelectorAll('text')].filter(t => t.textContent.trim() && vis(t))
    const rects = ts.map(t => t.getBoundingClientRect())
    ts.forEach((t, i) => {
      const m = t.getScreenCTM()
      const px = parseFloat(getComputedStyle(t).fontSize) * (m ? Math.hypot(m.a, m.b) : 1)
      if (px < 11.9) svgSmall.push(`${t.textContent.slice(0, 20)}:${px.toFixed(1)}px`)
      const q = rects[i]
      if (q.left < box.left - 1 || q.right > box.right + 1 || q.top < box.top - 1 || q.bottom > box.bottom + 1) {
        svgOut.push(t.textContent.slice(0, 20))
      }
      for (let j = i + 1; j < ts.length; j++) {
        const o = rects[j]
        if (Math.min(q.right, o.right) - Math.max(q.left, o.left) > 1 && Math.min(q.bottom, o.bottom) - Math.max(q.top, o.top) > 1) {
          svgOverlap.push(`${t.textContent.slice(0, 14)}x${ts[j].textContent.slice(0, 14)}`)
        }
      }
    })
  }
  return {
    clipped,
    docOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    interactive: inter.length,
    smallTargets: small.length,
    smallSample: small.slice(0, 12),
    unnamed: unnamed.slice(0, 4),
    lowContrast: low.length,
    lowSample: low.slice(0, 6),
    minRatio: Math.round(minRatio * 100) / 100,
    minFont,
    transitionClass: document.querySelectorAll('.transition').length,
    svgBad: svgOut.length + svgOverlap.length + svgSmall.length,
    svgDetail: [...svgOut, ...svgOverlap, ...svgSmall].slice(0, 6),
  }
}

const errs = []
const checks = []
const ok = (name, pass, detail = '') => {
  checks.push({ name, pass: !!pass, detail })
  console.log((pass ? 'PASS ' : 'FAIL ') + name + (detail ? '  ' + detail : ''))
}

async function open(w, h, theme = 'light', extra = {}) {
  const ctx = await b.newContext({ viewport: { width: w, height: h }, colorScheme: theme, hasTouch: w < 768, ...extra })
  await ctx.addInitScript((t) => { try { localStorage.setItem('openbell-theme', t) } catch {} }, theme)
  const p = await ctx.newPage()
  p.__allow = /$^/
  p.on('pageerror', e => errs.push(`${w}-${theme}: pageerror ${e.message}`))
  p.on('console', m => { if (m.type() === 'error' && !p.__allow.test(m.text())) errs.push(`${w}-${theme}: ${m.text().slice(0, 140)} @ ${m.location().url}`) })
  await p.route('**/api/collect', r => r.fulfill({ status: 200, contentType: 'application/json', body: '{"snapshots_written":5}' }))
  return { ctx, p }
}

const ready = p => p.waitForSelector('#readings #pot-figure', { timeout: 15000 })
const stratRows = p => p.locator('#strategies tbody tr button[data-strategy-row]').count()
const drawerTitle = p => p.locator('[data-strategy-drawer] h2').first().innerText().catch(() => '')

// 1. Responsive measurements across live & fixture, light & dark, 1440/768/390/320
const measures = []
for (const [dn, url] of [['live', BASE], ['fixture', FX]]) {
  for (const theme of ['light', 'dark']) {
    for (const [vn, w, h] of [['1440', 1440, 900], ['768', 768, 1024], ['390', 390, 844], ['320', 320, 640]]) {
      const { ctx, p } = await open(w, h, theme)
      await p.goto(url); await ready(p); await p.waitForTimeout(700)
      const m = await p.evaluate(MEASURE)
      m.size = vn; m.theme = theme; m.data = dn
      measures.push(m)
      if (vn === '1440' || vn === '390') await p.screenshot({ path: `${OUT}/${dn}-${vn}-${theme}.png` })
      await ctx.close()
    }
  }
}
for (const m of measures) {
  ok(
    `measure ${m.data} ${m.size} ${m.theme}`,
    m.docOverflow <= 0 && m.clipped.length === 0 && m.smallTargets === 0 && m.lowContrast === 0 && m.unnamed.length === 0 && m.transitionClass === 0 && m.svgBad === 0,
    `overflow=${m.docOverflow} clipped=${m.clipped.join('|')} small=${m.smallTargets}(${m.smallSample.join('|')}) low=${m.lowContrast}(${m.lowSample.join('|')}) unnamed=${m.unnamed.join('|')} tr=${m.transitionClass} svg=${m.svgBad}(${m.svgDetail.join('|')}) minRatio=${m.minRatio}`,
  )
}

// 2. Settings-only Light/Dark switch (#67) with opposite browser colorScheme + luminance check
{
  const { ctx, p } = await open(1440, 900, 'light')
  await p.goto(BASE); await ready(p); await p.waitForTimeout(500)
  ok('no Light/Dark buttons visible on top bar at rest',
    (await p.locator('header').getByRole('button', { name: 'Dark', exact: true }).count()) === 0 &&
    (await p.locator('header').getByRole('button', { name: 'Light', exact: true }).count()) === 0)
  ok('sticky header uses opaque surface with zero backdrop blur',
    await p.locator('header').evaluate(el => {
      const s = getComputedStyle(el)
      return (!s.backdropFilter || s.backdropFilter === 'none') && (!s.webkitBackdropFilter || s.webkitBackdropFilter === 'none')
    }))
  // Verify #desk-search placeholder fits comfortably across all 3 tabs at 1440px
  const phFits = []
  for (const tid of ['#tab-tape', '#tab-strategies', '#tab-slots']) {
    await p.locator(tid).click(); await p.waitForTimeout(100)
    const info = await p.locator('#desk-search').evaluate(el => {
      const s = getComputedStyle(el)
      const cx = document.createElement('canvas').getContext('2d')
      cx.font = `${s.fontWeight} ${s.fontSize} ${s.fontFamily}`
      const textW = cx.measureText(el.placeholder).width
      const availW = el.clientWidth - parseFloat(s.paddingLeft) - parseFloat(s.paddingRight)
      return { ph: el.placeholder, textW: Math.round(textW), availW: Math.round(availW), ok: textW <= availW }
    })
    phFits.push(info)
  }
  await p.locator('#tab-tape').click(); await p.waitForTimeout(100)
  ok('search placeholder fits without mid-word clipping on all 3 tabs', phFits.every(x => x.ok), JSON.stringify(phFits))
  ok('live mode shows zero Example fixture pill', (await p.locator('text=Example fixture').count()) === 0)
  const lum = () => p.evaluate(() => {
    const cx = document.createElement('canvas').getContext('2d')
    cx.fillStyle = getComputedStyle(document.body).backgroundColor
    cx.fillRect(0, 0, 1, 1)
    const [r, g, bl] = cx.getImageData(0, 0, 1, 1).data
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl
  })
  const lightLum = await lum()
  await p.locator('#settings-btn').click(); await p.waitForTimeout(400)
  ok('Settings Vaul sheet opens with Appearance Light/Dark', await p.locator('[data-settings-drawer]').isVisible() && await p.locator('#themeBtn').isVisible())
  await p.locator('#themeBtn').getByRole('button', { name: 'Dark', exact: true }).click(); await p.waitForTimeout(250)
  const darkLum = await lum()
  ok('Dark switch inside Settings updates <html data-theme>, localStorage, and darkens page bg',
    (await p.evaluate(() => document.documentElement.dataset.theme === 'dark' && localStorage.getItem('openbell-theme') === 'dark')) && darkLum < lightLum,
    `lightLum=${lightLum.toFixed(1)} darkLum=${darkLum.toFixed(1)}`)
  await p.locator('#themeBtn').getByRole('button', { name: 'Light', exact: true }).click(); await p.waitForTimeout(200)
  ok('Light switch restores light theme', (await p.evaluate(() => document.documentElement.dataset.theme === 'light')))
  await p.keyboard.press('Escape'); await p.waitForTimeout(400)
  ok('Escape closes Settings sheet', (await p.locator('[data-settings-drawer]').count()) === 0)
  await ctx.close()
}

// 3. Workbench tabs, Live 15m Tape inspector, Rune Strategies inspector, Slot Desk & Daily Report (desktop)
{
  const { ctx, p } = await open(1440, 900)
  await p.goto(FX); await ready(p); await p.waitForTimeout(500)
  ok('fixture notice visible only on ?dataset=fixture', (await p.locator('text=Example tape.').count()) === 1)
  ok('hero telemetry strip + Collect now sit above the fold at 1440', await p.evaluate(() =>
    ['#pot-figure', '#standing', '#markets-kpi', '#snapshots-kpi', '#collect-btn'].every(s => {
      const r = document.querySelector(s)?.getBoundingClientRect()
      return !!r && r.top >= 0 && r.bottom <= innerHeight
    })))
  const fontsOk = await p.evaluate(async () => {
    await document.fonts.ready
    return document.fonts.check('14px "Geist"') && [...document.fonts].some(f => f.family.includes('Geist Mono') && f.status === 'loaded')
  })
  ok('self-hosted Geist and Geist Mono woff2 loaded', fontsOk)
  // Standing line honesty on fixture
  const tally = await p.locator('#standing-detail li').evaluateAll(ls => ls.map(l => l.innerText.replace(/\s+/g, ' ').trim()))
  ok('standing line counts 0 of 5 forward-proven on example tape',
    (await p.locator('#standing-figure').textContent()).trim() === '0 of 5' && tally.some(t => /passed on the example tape only/.test(t)),
    JSON.stringify(tally))

  // Live 15m Tape: search via '/' shortcut + Market Inspector Vaul drawer
  await p.locator('body').click({ position: { x: 5, y: 300 } })
  ok('global search kbd hint shows /', (await p.locator('header kbd').first().innerText().catch(() => '')).trim() === '/')
  await p.keyboard.press('/'); await p.waitForTimeout(100)
  ok('slash key focuses #desk-search', await p.evaluate(() => document.activeElement?.id === 'desk-search'))
  await p.locator('#desk-search').fill('KXBTC'); await p.waitForTimeout(200)
  const firstMarket = p.locator('#markets-card button[data-market-row]').first()
  ok('tape filter matches KXBTC', (await firstMarket.innerText()).includes('KXBTC'))
  await firstMarket.click(); await p.waitForTimeout(600)
  ok('clicking a market opens Market Inspector drawer with real snapshot history',
    await p.locator('[data-market-drawer]').isVisible() && (await p.locator('[data-market-drawer] svg[role=img]').count()) >= 1)
  await p.keyboard.press('Escape'); await p.waitForTimeout(450)
  await p.locator('#desk-search').fill(''); await p.waitForTimeout(150)

  // Rune Strategies tab + filters + j/k/Enter + Strategy Inspector with Cult UI Expandable Rune source
  await p.locator('#tab-strategies').click(); await p.waitForTimeout(300)
  ok('five strategies in Rune Strategies leaderboard', (await stratRows(p)) === 5, String(await stratRows(p)))
  for (const [f, n] of [['Proven (2)', 2], ['Rejected (1)', 1], ['Testing (2)', 2], ['All (5)', 5]]) {
    await p.getByRole('button', { name: f, exact: true }).click(); await p.waitForTimeout(150)
    ok(`strategy filter ${f}`, (await stratRows(p)) === n, String(await stratRows(p)))
  }
  await p.locator('#desk-search').fill('late'); await p.waitForTimeout(150)
  ok('global search filters strategies', (await stratRows(p)) === 2, String(await stratRows(p)))
  await p.locator('#desk-search').fill('zzz'); await p.waitForTimeout(150)
  ok('empty search shows clear status', /No strategy matches/.test(await p.locator('#strategies [role=status]').innerText()))
  await p.locator('#desk-search').fill(''); await p.waitForTimeout(150)

  // Keyboard j / k / Enter + Strategy Inspector drawer + Rune script Expandable
  await p.locator('body').click({ position: { x: 5, y: 300 } })
  await p.keyboard.press('j'); await p.waitForTimeout(100)
  const firstStrat = await p.locator('#strategies tr[aria-selected="true"] button').innerText()
  ok('j highlights the first strategy row', !!firstStrat, firstStrat)
  await p.keyboard.press('j'); await p.waitForTimeout(100)
  const secondStrat = await p.locator('#strategies tr[aria-selected="true"] button').innerText()
  ok('second j steps to the next strategy row', !!secondStrat && secondStrat !== firstStrat, secondStrat)
  await p.keyboard.press('Enter'); await p.waitForTimeout(600)
  ok('Enter opens Strategy Inspector drawer', (await drawerTitle(p)) === secondStrat, await drawerTitle(p))
  ok('Strategy Inspector draws SVG equity curve', (await p.locator('[data-strategy-drawer] svg[role=img]').count()) >= 1)
  await p.locator('[data-strategy-drawer]').getByText(/Rune script source/).click(); await p.waitForTimeout(350)
  ok('Cult UI Expandable reveals live Rune .rn source', (await p.locator('[data-strategy-drawer] pre').innerText()).includes('allow_entry'))
  await p.screenshot({ path: `${OUT}/strategy-drawer-1440.png` })
  await p.keyboard.press('j'); await p.waitForTimeout(300)
  ok('j follows inside Strategy Inspector', (await drawerTitle(p)) !== secondStrat, await drawerTitle(p))
  await p.keyboard.press('Escape'); await p.waitForTimeout(450)
  ok('Escape closes Strategy Inspector', (await p.locator('[data-strategy-drawer]').count()) === 0)

  // Slot Desk tab + Daily Report Expandable + Venue Account
  await p.locator('#tab-slots').click(); await p.waitForTimeout(300)
  ok('Slot Desk renders active slots with 3-strike tracker', (await p.locator('#slots-card tbody tr').count()) >= 5)
  await p.locator('#report-body').getByText('Show full report').click(); await p.waitForTimeout(400)
  ok('full daily report expands in place', await p.locator('#report').isVisible())
  ok('read-only Venue Account panel renders', await p.locator('#account').isVisible())

  // Collect now (stubbed)
  await p.locator('#collect-btn').focus(); await p.keyboard.press('Enter')
  await p.waitForSelector('#collect-status', { timeout: 5000 }).then(() => ok('Collect now reports live snapshot toast/status', true), () => ok('Collect now reports live snapshot toast/status', false))
  ok('Enter on Collect now does not open drawer', (await p.locator('[data-strategy-drawer]').count()) === 0)

  // Return to live tape clears fixture notice
  await p.getByRole('button', { name: 'Return to live tape' }).click(); await p.waitForTimeout(700)
  ok('Return to live tape clears fixture param & notice', !p.url().includes('dataset=fixture') && (await p.locator('text=Example tape.').count()) === 0)
  await ctx.close()
}

// 4. Error, empty, and loading states
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /50[03]/
  await p.unroute('**/api/collect')
  await p.route('**/api/collect', r => r.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"venue unreachable"}' }))
  await p.goto(FX); await ready(p)
  await p.locator('#collect-btn').click()
  await p.waitForSelector('#collect-status', { timeout: 5000 }).catch(() => {})
  ok('collect failure says Nothing was written', /Nothing was written/.test(await p.locator('#collect-status').innerText().catch(() => '')))
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /500/
  await p.route('**/api/strategies*', r => r.fulfill({ status: 500, body: 'no' }))
  await p.goto(FX); await ready(p)
  ok('strategies failure marks standing unavailable while pot still loads',
    (await p.locator('#standing-label').innerText()) === 'Strategy state unavailable' && (await p.locator('#pot-figure').count()) === 1)
  await p.locator('#tab-strategies').click(); await p.waitForTimeout(200)
  await p.unroute('**/api/strategies*')
  await p.locator('#strategies [role=alert] button').click(); await p.waitForTimeout(900)
  ok('retry recovers strategies table', (await stratRows(p)) === 5)
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /500/
  await p.route('**/api/**', r => r.request().url().includes('/api/update') ? r.continue() : r.fulfill({ status: 500, body: 'no' }))
  await p.goto(FX); await p.waitForSelector('#load-error', { timeout: 10000 }).catch(() => {})
  ok('all reads failing shows #load-error with Retry', (await p.locator('#load-error').count()) === 1)
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  await p.route('**/api/strategies*', r => r.fulfill({ status: 200, contentType: 'application/json', body: '[]' }))
  await p.route('**/api/markets*', r => r.fulfill({ status: 200, contentType: 'application/json', body: '[]' }))
  await p.goto(FX); await ready(p); await p.waitForTimeout(300)
  ok('empty tape shows Collect live tape onboarding', (await p.locator('#markets-card').innerText()).includes('No snapshots collected yet'))
  ok('empty library named in standing tile', (await p.locator('#standing-label').innerText()).includes('No strategies in the library'))
  await ctx.close()
}

// 5. Phone drawer direction (390x844)
{
  const { ctx, p } = await open(390, 844)
  await p.goto(FX); await ready(p); await p.waitForTimeout(400)
  await p.locator('#settings-btn').click(); await p.waitForTimeout(500)
  const box = await p.locator('[data-settings-drawer]').boundingBox()
  ok('phone Settings sheet rises from bottom', !!box && box.y > 40 && Math.round(box.width) === 390, JSON.stringify(box))
  await p.screenshot({ path: `${OUT}/settings-390.png` })
  await p.getByRole('button', { name: 'Close settings', exact: true }).click(); await p.waitForTimeout(450)
  await ctx.close()
}

// 6. Update strip (loopback + non-loopback shared host, BANNER-FIX-BRIEF)
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /409|500/
  let snap = { mode: 'notify', current: '0.4.0', latest: '0.5.0', available: true, can_apply: true, dev_build: false, notes: 'Faster collector.', release_url: 'https://github.com/cayleb-james2008/openbell/releases/tag/v0.5.0', checked_at: Math.floor(Date.now() / 1000) }
  let failApply = true
  await p.route('**/api/update', r => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(snap) }))
  await p.route('**/api/update/apply', r => {
    if (failApply) return r.fulfill({ status: 409, contentType: 'application/json', body: JSON.stringify({ error: 'the download does not match its checksum, so it was discarded' }) })
    snap = { ...snap, available: false, staged: '0.5.0', can_apply: false }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(snap) })
  })
  await p.goto(FX); await ready(p); await p.waitForSelector('#update-banner', { timeout: 5000 })
  ok('update banner shows Update now on loopback', (await p.getByRole('button', { name: 'Update now', exact: true }).count()) === 1)
  await p.getByRole('button', { name: 'Dismiss', exact: true }).click(); await p.waitForTimeout(200)
  ok('Dismiss hides banner and focuses Show update', (await p.locator('#update-banner').count()) === 0 && await p.evaluate(() => document.activeElement?.textContent?.trim() === 'Show update'))
  await p.getByRole('button', { name: 'Show update', exact: true }).click(); await p.waitForTimeout(200)
  ok('Show update restores banner and focuses status', (await p.locator('#update-banner').count()) === 1 && await p.evaluate(() => document.activeElement?.closest('#update-banner') !== null))
  await p.getByRole('button', { name: 'Update now', exact: true }).click(); await p.waitForTimeout(300)
  const errTxt = await p.locator('#update-banner').innerText()
  ok('failed apply shows plain error, Try again, and hides raw error in Details', errTxt.includes('The download did not pass its safety check') && !errTxt.includes('discarded') && (await p.getByRole('button', { name: 'Try again', exact: true }).count()) === 1)
  failApply = false
  await p.getByRole('button', { name: 'Try again', exact: true }).click(); await p.waitForTimeout(300)
  ok('staged install focuses Restart now', await p.evaluate(() => document.activeElement?.textContent?.trim() === 'Restart now'))
  await ctx.close()
}
{
  const { ctx, p } = await open(1440, 900)
  const sharedOrigin = 'http://openbell.shared:18890'
  const snap = { mode: 'notify', current: '0.4.0', latest: '0.5.0', available: true, can_apply: true, dev_build: false, notes: 'Notes.', release_url: 'https://github.com/cayleb-james2008/openbell/releases/tag/v0.5.0', checked_at: Math.floor(Date.now() / 1000) }
  await p.route(sharedOrigin + '/**', async r => {
    const u = new URL(r.request().url())
    if (u.pathname === '/api/update') return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(snap) })
    const upstream = await fetch(new URL(u.pathname + u.search, BASE).href, { method: r.request().method(), headers: { Host: new URL(BASE).host } })
    const buf = Buffer.from(await upstream.arrayBuffer())
    return r.fulfill({ status: upstream.status, headers: Object.fromEntries(upstream.headers.entries()), body: buf })
  })
  await p.goto(sharedOrigin + '/?dataset=fixture'); await ready(p); await p.waitForSelector('#update-banner', { timeout: 5000 })
  ok('non-loopback shared host hides Update now and shows plain host guidance',
    (await p.getByRole('button', { name: 'Update now', exact: true }).count()) === 0 &&
    (await p.locator('#update-banner').innerText()).includes('To update, open OpenBell on the computer it runs on.'))
  await ctx.close()
}

// 7. Reduced motion honored
{
  const { ctx, p } = await open(1440, 900, 'light', { reducedMotion: 'reduce' })
  await p.goto(FX); await ready(p)
  await p.locator('#settings-btn').click(); await p.waitForTimeout(250)
  const dur = await p.locator('[data-settings-drawer]').evaluate(e => getComputedStyle(e).transitionDuration)
  ok('prefers-reduced-motion cuts Vaul drawer duration', /1e-05s|0\.00001s|0s|0\.01ms/.test(dur), dur)
  await ctx.close()
}

ok('zero browser console/page errors', errs.length === 0, errs.slice(0, 5).join(' | '))
await b.close()
report('gate', checks)
