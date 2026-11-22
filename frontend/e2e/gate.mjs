// gate.mjs: OpenBell v0.5.0 institutional quant desk gate (4 tabs, opaque header, Settings-only Light/Dark,
// #desk-search non-truncation on all 4 tabs, 390px no-orphan wrap, Cmd+K palette, keyboard 1-4,
// interactive price+spread crosshair, Rune Sandbox validate+backtest, Slot Desk equity+drawdown+export,
// Quant Lab calibration/Monte Carlo/walk-forward, v0.5.0 wire-contract E2E, and Update strip).
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

// Mechanical 1-word orphan wrap detector for 390px mobile prose & headings
const ORPHAN_WRAPS = () => {
  const vis = el => {
    const r = el.getBoundingClientRect()
    const s = getComputedStyle(el)
    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'
  }
  const orphans = []
  const candidates = [...document.querySelectorAll('h1,h2,h3,h4,p,button,[role=tab]')].filter(el => {
    if (!vis(el) || el.closest('[aria-hidden="true"]') || el.closest('pre') || el.closest('table')) return false
    if (el.classList.contains('mono') || el.classList.contains('font-mono') || el.classList.contains('truncate')) return false
    const txt = (el.innerText || '').trim().replace(/\s+/g, ' ')
    return txt.split(' ').length >= 4 && !txt.includes('KX')
  })
  for (const el of candidates) {
    // Only inspect leaf-ish prose blocks whose text comes directly or via inline spans
    if (el.querySelector('div,p,ul,table,svg')) continue
    const range = document.createRange()
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT)
    const words = []
    let node
    while ((node = walker.nextNode())) {
      const text = node.textContent || ''
      const re = /\S+/g
      let m
      while ((m = re.exec(text))) {
        range.setStart(node, m.index)
        range.setEnd(node, m.index + m[0].length)
        const rect = range.getBoundingClientRect()
        if (rect.width > 0 && rect.height > 0) {
          words.push({ word: m[0], top: Math.round(rect.top / 4) * 4 })
        }
      }
    }
    if (words.length < 4) continue
    const lines = new Map()
    for (const w of words) {
      const arr = lines.get(w.top) || []
      arr.push(w.word)
      lines.set(w.top, arr)
    }
    if (lines.size >= 2) {
      const sortedTops = [...lines.keys()].sort((a, b) => a - b)
      const lastLine = lines.get(sortedTops[sortedTops.length - 1])
      if (lastLine && lastLine.length === 1 && lastLine[0].length <= 14) {
        orphans.push(`${el.tagName}:"${(el.innerText || '').trim().slice(0, 60)}" -> [${lastLine[0]}]`)
      }
    }
  }
  return orphans
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
      if (dn === 'fixture' && vn === '1440' && theme === 'dark') {
        await p.locator('#tab-quant').click(); await p.waitForTimeout(450)
        await p.screenshot({ path: `${OUT}/quant-lab-1440-dark.png` })
      }
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

// 2. Settings-only Light/Dark switch (#67), opaque header, & #desk-search non-truncation on ALL 4 tabs
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
  const phFits = []
  for (const tid of ['#tab-tape', '#tab-strategies', '#tab-slots', '#tab-quant']) {
    await p.locator(tid).click(); await p.waitForTimeout(120)
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
  ok('search placeholder fits without mid-word clipping on all 4 tabs', phFits.every(x => x.ok), JSON.stringify(phFits))
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

// 3. Full 4-tab Workbench E2E: Tape + Price/Spread Crosshair, Rune Sandbox + Backtest, Slot Desk + Drawdown + Export, Quant Lab, Cmd+K & 1-4 shortcuts
{
  const { ctx, p } = await open(1440, 900)
  await p.goto(FX); await ready(p); await p.waitForTimeout(550)
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
  const stratPayload = await p.evaluate(() => fetch('/api/strategies?dataset=fixture').then(r => r.json()))
  const stratTotal = Array.isArray(stratPayload) ? stratPayload.length : 0
  const tally = await p.locator('#standing-detail li').evaluateAll(ls => ls.map(l => l.innerText.replace(/\s+/g, ' ').trim()))
  ok('standing line counts 0 forward-proven on example tape across full library',
    stratTotal >= 5 && (await p.locator('#standing-figure').textContent()).trim() === `0 of ${stratTotal}` && tally.some(t => /passed on the example tape only/.test(t)),
    `total=${stratTotal} tally=${JSON.stringify(tally)}`)

  // Live 15m Tape: dynamic series chips + inline Price & Spread crosshair + Market Inspector drawer
  const chipsPresent = await Promise.all(['All', 'BTC', 'ETH', 'SOL', 'XRP'].map(s => p.locator(`[data-series-chip="${s}"]`).count()))
  ok('Live 15m Tape shows dynamic multi-series chips (All, BTC, ETH, SOL, XRP)', chipsPresent.every(c => c === 1), JSON.stringify(chipsPresent))
  const inlineSvg = p.locator('#tape-inline-chart svg[role=img]').first()
  const ibox = await inlineSvg.boundingBox()
  if (ibox) {
    await p.mouse.move(ibox.x + ibox.width * 0.4, ibox.y + ibox.height * 0.4)
    await p.waitForTimeout(120)
  }
  const crossTxt = await p.locator('#tape-inline-chart [data-market-crosshair-tooltip]').innerText().catch(() => '')
  ok('interactive SVG price+spread chart crosshair shows YES/NO bid/ask, implied prob & spread',
    /YES\s+\S+\/\S+/.test(crossTxt) && /NO\s+\S+\/\S+/.test(crossTxt) && /Prob\s+[\d.]+%/.test(crossTxt) && /Spread\s+\d+c/.test(crossTxt),
    crossTxt)

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

  // Keyboard shortcuts 1-4 switch all 4 tabs cleanly
  await p.locator('body').click({ position: { x: 5, y: 300 } })
  await p.keyboard.press('4'); await p.waitForTimeout(200)
  const k4Ok = await p.locator('#quant-lab').isVisible()
  await p.keyboard.press('3'); await p.waitForTimeout(200)
  const k3Ok = await p.locator('#slots-card').isVisible()
  await p.keyboard.press('2'); await p.waitForTimeout(200)
  const k2Ok = await p.locator('#strategies').isVisible()
  ok('keyboard shortcuts 2/3/4 switch Strategies, Slot Desk & Quant Lab tabs', k4Ok && k3Ok && k2Ok)

  // Strategies tab + filters + Sandbox Editor + Validate + Backtest + j/k/Enter + Inspector
  ok('Strategies leaderboard renders full strategy library', (await stratRows(p)) === stratTotal && stratTotal >= 5, String(await stratRows(p)))
  const filterBtns = await p.locator('#strategies [role="group"][aria-label="Filter strategies by verdict"] button').evaluateAll(bs =>
    bs.map(b => ({ label: b.innerText.trim(), n: Number((b.innerText.match(/\((\d+)\)/) || [])[1] || 0) })),
  )
  for (const { label, n } of [...filterBtns.slice(1), filterBtns[0]]) {
    await p.getByRole('button', { name: label, exact: true }).click(); await p.waitForTimeout(150)
    ok(`strategy filter ${label.split(' ')[0]}`, (await stratRows(p)) === n, `${label} -> ${await stratRows(p)}`)
  }

  // Interactive Strategy Sandbox Editor & Backtest on Tape
  await p.locator('#open-sandbox-btn').click(); await p.waitForTimeout(250)
  ok('Strategy Sandbox editor opens inline', await p.locator('#rune-sandbox #rune-editor').isVisible())
  await p.locator('#sandbox-validate-btn').click(); await p.waitForTimeout(250)
  ok('Validate Strategy reports compile diagnostics', (await p.locator('#sandbox-diagnostics').innerText()).includes('Compile passed'))
  await p.locator('#sandbox-backtest-btn').click(); await p.waitForTimeout(350)
  const sbTxt = await p.locator('[data-sandbox-results]').innerText()
  ok('Backtest on Tape renders paper P&L, Sharpe, Max Drawdown & Brier',
    sbTxt.includes('Paper Net P&L') && sbTxt.includes('Sharpe') && sbTxt.includes('Max Drawdown') && sbTxt.includes('Brier Score'),
    sbTxt.slice(0, 120))
  const sandboxRuneLeak = await p.evaluate(() => /\brune\b/i.test(document.body.innerText))
  ok('zero user-facing Rune jargon on Strategies + Sandbox view', !sandboxRuneLeak)
  await p.screenshot({ path: `${OUT}/strategies-sandbox-1440.png` })
  await p.locator('#open-sandbox-btn').click(); await p.waitForTimeout(150)

  await p.locator('#desk-search').fill('late'); await p.waitForTimeout(150)
  const lateMatches = await stratRows(p)
  ok('global search filters strategies', lateMatches >= 2 && lateMatches < stratTotal, String(lateMatches))
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
  await p.locator('[data-strategy-drawer]').getByText(/Strategy script source/).click(); await p.waitForTimeout(350)
  ok('Cult UI Expandable reveals live strategy script source', (await p.locator('[data-strategy-drawer] pre').innerText()).includes('allow_entry'))
  await p.screenshot({ path: `${OUT}/strategy-drawer-1440.png` })
  await p.keyboard.press('j'); await p.waitForTimeout(300)
  ok('j follows inside Strategy Inspector', (await drawerTitle(p)) !== secondStrat, await drawerTitle(p))
  await p.keyboard.press('Escape'); await p.waitForTimeout(450)
  ok('Escape closes Strategy Inspector', (await p.locator('[data-strategy-drawer]').count()) === 0)

  // Slot Desk tab: Equity + Drawdown Underwater subplot, Stint Attribution, 3-strike tracker, Export .md/.csv, Expandable Report, Venue Account
  await p.locator('#tab-slots').click(); await p.waitForTimeout(350)
  ok('Slot Desk renders active slots with 3-strike tracker', (await p.locator('#slots-card tbody tr').count()) >= 5)
  const ddSvg = p.locator('[data-slot-equity-dd] svg[role=img]').first()
  ok('Slot Desk renders compounding equity + drawdown underwater SVG & stint attribution',
    (await ddSvg.count()) === 1 && (await p.locator('#stint-attribution').isVisible()))
  const [dlMd] = await Promise.all([p.waitForEvent('download'), p.locator('#export-report-md').click()])
  const [dlCsv] = await Promise.all([p.waitForEvent('download'), p.locator('#export-report-csv').click()])
  ok('Slot Desk exports Daily Report in .md and .csv formats',
    dlMd.suggestedFilename().endsWith('.md') && dlCsv.suggestedFilename().endsWith('.csv'),
    `${dlMd.suggestedFilename()} | ${dlCsv.suggestedFilename()}`)
  await p.locator('#report-body').getByText('Show full report').click(); await p.waitForTimeout(400)
  ok('full daily report expands in place', await p.locator('#report').isVisible())
  ok('read-only Venue Account panel renders', await p.locator('#account').isVisible())
  const slotsM = await p.evaluate(MEASURE)
  ok('Slot Desk passes full geometry, contrast & SVG checks at 1440',
    slotsM.docOverflow <= 0 && slotsM.lowContrast === 0 && slotsM.svgBad === 0 && slotsM.smallTargets === 0,
    `low=${slotsM.lowContrast} svg=${slotsM.svgBad} small=${slotsM.smallTargets}`)
  await p.screenshot({ path: `${OUT}/slots-1440-light.png` })

  // Quant Lab (4th tab): Calibration Reliability SVG + Wilson error bars + Brier/ECE, Monte Carlo 1,000-path Fan + Reseed, Walk-Forward IS vs OOS
  await p.locator('#tab-quant').click(); await p.waitForTimeout(500)
  const brierTxt = (await p.locator('#quant-brier').innerText()).trim()
  const eceTxt = (await p.locator('#quant-ece').innerText()).trim()
  const wilsonCount = await p.locator('[data-calibration-chart] [data-wilson-bar]').count()
  ok('Quant Lab renders Calibration Reliability SVG with 95% Wilson error bars, Brier & ECE',
    wilsonCount >= 1 && /^\d+\.\d{4}$/.test(brierTxt) && /^\d+\.\d{2}%$/.test(eceTxt),
    `wilsonBars=${wilsonCount} brier=${brierTxt} ece=${eceTxt}`)
  const mcSeedBefore = await p.locator('#mc-seed-input').inputValue()
  await p.locator('#mc-reseed-btn').click(); await p.waitForTimeout(300)
  const mcSeedAfter = await p.locator('#mc-seed-input').inputValue()
  ok('Quant Lab renders 1,000-path Monte Carlo Fan Chart and Run/Reseed updates seed',
    (await p.locator('[data-montecarlo-chart] svg[role=img]').count()) === 1 && mcSeedAfter !== mcSeedBefore,
    `seed ${mcSeedBefore} -> ${mcSeedAfter}`)
  const wfRows = await p.locator('#quant-walkforward tbody tr').count()
  const wfText = await p.locator('#quant-walkforward').innerText()
  ok('Quant Lab renders Walk-Forward IS vs OOS table with Sharpe, Sortino, Profit Factor, Max DD & Fee Drag',
    wfRows >= 2 && wfText.includes('IS (') && wfText.includes('OOS (') && wfText.includes('Portfolio'),
    `rows=${wfRows}`)
  const quantM = await p.evaluate(MEASURE)
  ok('Quant Lab passes full geometry, contrast & SVG checks at 1440',
    quantM.docOverflow <= 0 && quantM.lowContrast === 0 && quantM.svgBad === 0 && quantM.smallTargets === 0,
    `low=${quantM.lowContrast}(${quantM.lowSample.join('|')}) svg=${quantM.svgBad}(${quantM.svgDetail.join('|')}) small=${quantM.smallTargets}`)
  await p.screenshot({ path: `${OUT}/quant-lab-1440-light.png` })

  // Global Command Palette (Ctrl+K / Cmd+K)
  await p.keyboard.press('Control+k'); await p.waitForTimeout(250)
  ok('Ctrl+K opens global Command Palette', await p.locator('[data-command-palette]').isVisible())
  const cmdkRuneLeak = await p.evaluate(() => /\brune\b/i.test(document.body.innerText))
  ok('zero user-facing Rune jargon inside Command Palette', !cmdkRuneLeak)
  await p.screenshot({ path: `${OUT}/cmdk-1440-light.png` })
  await p.locator('[data-command-palette] input').fill('favourite-late'); await p.waitForTimeout(200)
  await p.keyboard.press('Enter'); await p.waitForTimeout(600)
  ok('Command Palette jumps directly to strategy inspector',
    (await p.locator('[data-command-palette]').count()) === 0 && (await drawerTitle(p)) === 'favourite-late',
    await drawerTitle(p))
  await p.keyboard.press('Escape'); await p.waitForTimeout(400)

  // Collect now (stubbed)
  await p.locator('#collect-btn').focus(); await p.keyboard.press('Enter')
  await p.waitForSelector('#collect-status', { timeout: 5000 }).then(() => ok('Collect now reports live snapshot toast/status', true), () => ok('Collect now reports live snapshot toast/status', false))
  ok('Enter on Collect now does not open drawer', (await p.locator('[data-strategy-drawer]').count()) === 0)

  // Return to live tape clears fixture notice
  await p.getByRole('button', { name: 'Return to live tape' }).click(); await p.waitForTimeout(700)
  ok('Return to live tape clears fixture param & notice', !p.url().includes('dataset=fixture') && (await p.locator('text=Example tape.').count()) === 0)
  await ctx.close()
}

// 4. 390x844 mobile sweep across ALL 4 tabs: zero overflow, zero 1-word orphan wraps, 44px targets, bottom Settings sheet
{
  const { ctx, p } = await open(390, 844)
  await p.goto(FX); await ready(p); await p.waitForTimeout(450)
  const orphanIssues = []
  const tabMeasures = []
  for (const [tid, label] of [['#tab-tape', 'tape'], ['#tab-strategies', 'strategies'], ['#tab-slots', 'slots'], ['#tab-quant', 'quant']]) {
    await p.locator(tid).click(); await p.waitForTimeout(350)
    const m = await p.evaluate(MEASURE)
    const orph = await p.evaluate(ORPHAN_WRAPS)
    tabMeasures.push({ label, ok: m.docOverflow <= 0 && m.clipped.length === 0 && m.smallTargets === 0 && m.lowContrast === 0 && m.svgBad === 0, m })
    for (const o of orph) orphanIssues.push(`${label}: ${o}`)
    if (label === 'quant') await p.screenshot({ path: `${OUT}/quant-lab-390-light.png` })
  }
  ok('all 4 tabs pass 390x844 touch, contrast, overflow & SVG geometry',
    tabMeasures.every(x => x.ok),
    tabMeasures.filter(x => !x.ok).map(x => `${x.label}:overflow=${x.m.docOverflow},small=${x.m.smallTargets}(${x.m.smallSample.join('|')}),low=${x.m.lowContrast},svg=${x.m.svgBad}(${x.m.svgDetail.join('|')})`).join(' ; '))
  ok('zero 1-word orphan wraps at 390x844 across all 4 tabs', orphanIssues.length === 0, orphanIssues.slice(0, 6).join(' | '))

  await p.locator('#settings-btn').click(); await p.waitForTimeout(500)
  const box = await p.locator('[data-settings-drawer]').boundingBox()
  ok('phone Settings sheet rises from bottom', !!box && box.y > 40 && Math.round(box.width) === 390, JSON.stringify(box))
  await p.screenshot({ path: `${OUT}/settings-390.png` })
  await p.getByRole('button', { name: 'Close settings', exact: true }).click(); await p.waitForTimeout(450)
  await ctx.close()
}

// 5. v0.5.0 API Wire-Contract E2E (proves live SSE, Calibration, Monte Carlo, Walk-Forward, Validate, Backtest & Export wiring)
{
  const { ctx, p } = await open(1440, 900)
  const hits = new Set()
  await p.route('**/api/home', async r => {
    const res = await r.fetch()
    const j = await res.json()
    j.version = '0.5.0'
    j.capabilities = ['sse', 'calibration', 'walkforward', 'montecarlo', 'sandbox', 'export']
    return r.fulfill({ response: res, body: JSON.stringify(j) })
  })
  await p.route('**/api/tape/stream', r => {
    hits.add('sse')
    return r.fulfill({
      status: 200,
      headers: { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' },
      body: 'event: tape\ndata: {"snapshots_written":1}\n\n',
    })
  })
  await p.route('**/api/analytics/calibration*', r => {
    hits.add('calibration')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        brier_score: 0.165,
        ece: 0.032,
        total_windows: 240,
        buckets: [
          { bin_index: 2, low: 0.2, high: 0.3, mean_predicted: 0.25, realized_rate: 0.24, wilson_low: 0.15, wilson_high: 0.36, count: 60 },
          { bin_index: 7, low: 0.7, high: 0.8, mean_predicted: 0.76, realized_rate: 0.78, wilson_low: 0.71, wilson_high: 0.84, count: 180 },
        ],
      }),
    })
  })
  await p.route('**/api/analytics/walkforward*', r => {
    hits.add('walkforward')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        in_sample_ratio: 0.6,
        rows: [{
          strategy: 'Portfolio (All Slots)',
          is_portfolio: true,
          in_sample: { windows: 60, net_pnl_dollars: 24.5, sharpe: 1.82, sortino: 2.41, profit_factor: 1.75, max_drawdown_pct: 4.2, fee_drag_pct: 7.4, max_win_streak: 7, max_loss_streak: 2 },
          out_of_sample: { windows: 40, net_pnl_dollars: 15.1, sharpe: 1.54, sortino: 2.05, profit_factor: 1.61, max_drawdown_pct: 5.1, fee_drag_pct: 7.9, max_win_streak: 5, max_loss_streak: 2 },
        }],
      }),
    })
  })
  await p.route('**/api/analytics/montecarlo*', r => {
    hits.add('montecarlo')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        paths: 1000,
        seed: 777,
        start_equity: 100,
        steps: 3,
        percentiles: { p5: [100, 96, 94, 92], p25: [100, 101, 104, 108], p50: [100, 105, 112, 121], p75: [100, 110, 122, 136], p95: [100, 118, 138, 162] },
        terminal: { p5: 92, p25: 108, p50: 121, p75: 136, p95: 162 },
        dd_prob_10: 0.12,
        dd_prob_25: 0.015,
        elapsed_ms: 9,
      }),
    })
  })
  await p.route('**/api/strategy/validate', r => {
    hits.add('validate')
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ valid: true, name: 'late-favourite-edge', side: 'yes', diagnostics: [{ level: 'info', message: 'Server sandbox compile OK' }], elapsed_ms: 4 }) })
  })
  await p.route('**/api/backtest', r => {
    hits.add('backtest')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        valid: true,
        strategy: 'late-favourite-edge',
        side: 'yes',
        summary: { net_pnl_dollars: 18.4, sharpe: 1.71, sortino: 2.33, max_drawdown_pct: 3.9, brier_score: 0.161, win_rate_pct: 68.0, windows_traded: 25, mean_cents_per_contract: 3.1, fee_drag_pct: 6.8 },
        equity_curve: [[1700000000, 0], [1700000900, 18.4]],
        trades: [{ close_epoch: 1700000900, ticker: 'KXBTC15M-MOCK', side: 'yes', price: 0.74, contracts: 5, pnl_dollars: 1.15, result: 'YES' }],
        elapsed_ms: 11,
      }),
    })
  })
  await p.route('**/api/export/report*', r => {
    hits.add('export')
    return r.fulfill({ status: 200, headers: { 'Content-Type': 'text/markdown', 'Content-Disposition': 'attachment; filename="openbell-server-report.md"' }, body: '# Venue truth line 1\n' })
  })

  await p.goto(BASE); await ready(p); await p.waitForTimeout(450)
  await p.locator('#tab-strategies').click(); await p.waitForTimeout(150)
  await p.locator('#open-sandbox-btn').click(); await p.waitForTimeout(150)
  await p.locator('#sandbox-validate-btn').click(); await p.waitForTimeout(150)
  await p.locator('#sandbox-backtest-btn').click(); await p.waitForTimeout(200)
  await p.locator('#tab-slots').click(); await p.waitForTimeout(200)
  await Promise.all([p.waitForEvent('download'), p.locator('#export-report-md').click()])
  await p.locator('#tab-quant').click(); await p.waitForTimeout(350)
  const serverEngineShown = (await p.locator('#quant-calibration').innerText()).includes('Server Quant Engine')
  ok('v0.5.0 live API contract integrates SSE, validate, backtest, export, calibration, walkforward & montecarlo',
    ['sse', 'calibration', 'walkforward', 'montecarlo', 'validate', 'backtest', 'export'].every(k => hits.has(k)) && serverEngineShown,
    `hits=${[...hits].sort().join(',')} serverEngine=${serverEngineShown}`)
  await ctx.close()
}

// 6. Error, empty, and loading states
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
  ok('retry recovers strategies table', (await stratRows(p)) >= 5, String(await stratRows(p)))
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

// 7. Update strip (loopback + non-loopback shared host, BANNER-FIX-BRIEF)
{
  const { ctx, p } = await open(1440, 900)
  p.__allow = /409|500/
  let snap = { mode: 'notify', current: '0.5.0', latest: '0.6.0', available: true, can_apply: true, dev_build: false, notes: 'Faster collector.', release_url: 'https://github.com/cayleb-james2008/openbell/releases/tag/v0.6.0', checked_at: Math.floor(Date.now() / 1000) }
  let failApply = true
  await p.route('**/api/update', r => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(snap) }))
  await p.route('**/api/update/apply', r => {
    if (failApply) return r.fulfill({ status: 409, contentType: 'application/json', body: JSON.stringify({ error: 'the download does not match its checksum, so it was discarded' }) })
    snap = { ...snap, available: false, staged: '0.6.0', can_apply: false }
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
  const snap = { mode: 'notify', current: '0.5.0', latest: '0.6.0', available: true, can_apply: true, dev_build: false, notes: 'Notes.', release_url: 'https://github.com/cayleb-james2008/openbell/releases/tag/v0.6.0', checked_at: Math.floor(Date.now() / 1000) }
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

// 8. Reduced motion honored
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
