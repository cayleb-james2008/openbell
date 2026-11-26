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
  const textClipped = []
  const textRange = document.createRange()
  for (const el of document.querySelectorAll('header button, header input, aside button, nav button, [role=tab], h1, h2, h3')) {
    if (!vis(el) || el.closest('[aria-hidden="true"]')) continue
    const cs = getComputedStyle(el)
    if (cs.textOverflow === 'ellipsis' || el.classList.contains('truncate')) continue
    const er = el.getBoundingClientRect()
    const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT)
    let tn
    while ((tn = w.nextNode())) {
      if (!tn.textContent || !tn.textContent.trim()) continue
      textRange.selectNodeContents(tn)
      const tr = textRange.getBoundingClientRect()
      if (tr.width > 0 && (tr.right > er.right + 2 || tr.left < er.left - 2)) {
        textClipped.push(`${el.tagName}:${(tn.textContent || '').trim().slice(0, 24)}`)
        break
      }
    }
  }
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
    textClipped: textClipped.slice(0, 5),
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
    m.docOverflow <= 0 && m.clipped.length === 0 && m.textClipped.length === 0 && m.smallTargets === 0 && m.lowContrast === 0 && m.unnamed.length === 0 && m.transitionClass === 0 && m.svgBad === 0,
    `overflow=${m.docOverflow} clipped=${m.clipped.join('|')} textClip=${m.textClipped.join('|')} small=${m.smallTargets}(${m.smallSample.join('|')}) low=${m.lowContrast}(${m.lowSample.join('|')}) unnamed=${m.unnamed.join('|')} tr=${m.transitionClass} svg=${m.svgBad}(${m.svgDetail.join('|')}) minRatio=${m.minRatio}`,
  )
}

// 2. Settings-only Light/Dark switch (#67), opaque header, & #desk-search non-truncation on ALL 5 workbench views
{
  const { ctx, p } = await open(1440, 900, 'light')
  await p.goto(BASE); await ready(p); await p.waitForTimeout(500)
  ok('no Light/Dark buttons visible on top bar or left rail at rest',
    (await p.locator('header, aside').getByRole('button', { name: 'Dark', exact: true }).count()) === 0 &&
    (await p.locator('header, aside').getByRole('button', { name: 'Light', exact: true }).count()) === 0)
  ok('sticky header uses opaque surface with zero backdrop blur',
    await p.locator('header').evaluate(el => {
      const s = getComputedStyle(el)
      return (!s.backdropFilter || s.backdropFilter === 'none') && (!s.webkitBackdropFilter || s.webkitBackdropFilter === 'none')
    }))
  const phFits = []
  for (const tid of ['#tab-tape', '#tab-strategies', '#tab-slots', '#tab-quant', '#tab-live']) {
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
  ok('search placeholder fits without mid-word clipping on all 5 views', phFits.every(x => x.ok), JSON.stringify(phFits))
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
  const liveSettingsOk =
    (await p.locator('#settings-live-config').isVisible()) &&
    (await p.locator('#live-api-key-id').count()) === 1 &&
    (await p.locator('#live-pem-path').count()) === 1 &&
    (await p.locator('#rail-max-contracts').count()) === 1 &&
    (await p.locator('#save-live-config-btn').count()) === 1
  ok('Settings Vaul sheet includes Kalshi API & Hard Risk Rails configuration panel', liveSettingsOk)
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
  ok('left desk rail, Risk/Kill-Switch header & live market table sit above the fold at 1440', await p.evaluate(() => {
    const controlsVisible = ['#pot-figure', '#standing', '#collect-btn', '#kill-switch-btn', '#run-edge-audit-btn', '#tab-live'].every(s => {
      const r = document.querySelector(s)?.getBoundingClientRect()
      return !!r && r.top >= 0 && r.bottom <= innerHeight
    })
    const tableRow = document.querySelector('#markets-card tbody tr')?.getBoundingClientRect()
    return controlsVisible && !!tableRow && tableRow.top >= 0 && tableRow.top < innerHeight - 24
  }))
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

  // v0.9.0: zero outdated "paper-only / no orders" claims + Live & Rails view, collapsible Order Ticket & Rule Editor drawers
  const noStalePaperOnly = await p.evaluate(() => !/paper-only|no orders|zero order paths|zero live order paths|places no orders/i.test(document.body.innerText))
  ok('zero outdated paper-only or no-orders copy across desk chrome (v0.9.0)', noStalePaperOnly)
  await p.locator('#drawer-order-ticket-btn').click(); await p.waitForTimeout(350)
  const drawerTicketOk =
    (await p.locator('[data-order-ticket-drawer]').isVisible()) &&
    (await p.locator('[data-order-ticket-drawer] #live-order-ticket').isVisible()) &&
    (await p.locator('[data-order-ticket-drawer] #live-risk-rails-check [data-risk-rail]').count()) === 7
  await p.keyboard.press('Escape'); await p.waitForTimeout(350)
  await p.locator('#drawer-rule-editor-btn').click(); await p.waitForTimeout(350)
  const drawerEditorOk =
    (await p.locator('[data-rule-editor-drawer]').isVisible()) &&
    (await p.locator('[data-rule-editor-drawer] #rune-editor').isVisible())
  await p.keyboard.press('Escape'); await p.waitForTimeout(350)
  ok('collapsible Order Ticket & Strategy Rule Editor drawers open and close via Escape', drawerTicketOk && drawerEditorOk)
  await p.locator('#tab-live').click(); await p.waitForTimeout(250)
  const liveTicketOk =
    (await p.locator('#live-account-strip').isVisible()) &&
    (await p.locator('#trading-mode-paper').isVisible()) &&
    (await p.locator('#trading-mode-live').isVisible()) &&
    (await p.locator('#kill-switch-btn').isVisible()) &&
    (await p.locator('#live-order-ticket').isVisible()) &&
    (await p.locator('#live-risk-rails-check [data-risk-rail]').count()) === 7
  ok('Live & Rails view renders Account Strip, Order Ticket, 7 Hard Risk Rails & Emergency Kill Switch', liveTicketOk)
  const runnerDeckOk =
    (await p.locator('#shadow-runner-deck').isVisible()) &&
    (await p.locator('#runner-mode-badge').isVisible()) &&
    (await p.locator('#runner-slots-card [data-runner-slot-row]').count()) >= 1 &&
    (await p.locator('#runner-open-quotes-card [role="progressbar"]').count()) >= 1 &&
    (await p.locator('#runner-strategy-stats-card [data-runner-stat-strategy]').count()) >= 1 &&
    (await p.locator('#runner-ledger-card [data-runner-ledger-row]').count()) >= 1 &&
    (await p.locator('#runner-start-live-btn').isDisabled())
  ok('ShadowRunnerPanel renders mode badge, slot config, queue-ahead quotes, strategy rollup, ledger & fail-closed Live Auto gate', runnerDeckOk)
  await p.screenshot({ path: `${OUT}/live-desk-1440-light.png` })
  await p.locator('#tab-tape').click(); await p.waitForTimeout(200)

  // Live 15m Tape: Auto-Collector strip (Off / 60s / 180s + 200-window Readiness Gate bar), dynamic series chips, Price & Spread crosshair, Market Inspector
  const autoCollectorOk =
    (await p.locator('#auto-collector-strip [data-collector-mode="off"]').count()) === 1 &&
    (await p.locator('#auto-collector-strip [data-collector-mode="60"]').count()) === 1 &&
    (await p.locator('#auto-collector-strip [data-collector-mode="180"]').count()) === 1 &&
    (await p.locator('#readiness-progress [role="progressbar"]').count()) === 1
  ok('Live 15m Tape renders Auto-Collector Off/60s/180s controls and 200-window Readiness Gate bar', autoCollectorOk)
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

  // Keyboard shortcuts 1-5 switch all 5 workbench views cleanly
  await p.locator('body').click({ position: { x: 5, y: 300 } })
  await p.keyboard.press('5'); await p.waitForTimeout(200)
  const k5Ok = await p.locator('#live-account-strip').isVisible()
  await p.keyboard.press('4'); await p.waitForTimeout(200)
  const k4Ok = await p.locator('#quant-lab').isVisible()
  await p.keyboard.press('3'); await p.waitForTimeout(200)
  const k3Ok = await p.locator('#slots-card').isVisible()
  await p.keyboard.press('2'); await p.waitForTimeout(200)
  const k2Ok = await p.locator('#strategies').isVisible()
  ok('keyboard shortcuts 2/3/4/5 switch Strategies, Slots, Quant Lab & Live views', k5Ok && k4Ok && k3Ok && k2Ok)

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
  ok('Connected Kalshi Account panel renders', await p.locator('#account').isVisible())
  const slotsM = await p.evaluate(MEASURE)
  ok('Slot Desk passes full geometry, contrast & SVG checks at 1440',
    slotsM.docOverflow <= 0 && slotsM.lowContrast === 0 && slotsM.svgBad === 0 && slotsM.smallTargets === 0,
    `low=${slotsM.lowContrast} svg=${slotsM.svgBad} small=${slotsM.smallTargets}`)
  await p.screenshot({ path: `${OUT}/slots-1440-light.png` })

  // Quant Lab (4th view): 1-Click Full Quant Edge Audit, $500/Day Scale Planner, 5-Gate Readiness Scorecard, PAV Isotonic Recalibration, Calibration Reliability, Monte Carlo Fan, Walk-Forward IS vs OOS
  await p.locator('#run-edge-audit-btn').click(); await p.waitForTimeout(650)
  const edgeAuditClear = await p.evaluate(() => {
    const hdr = document.querySelector('header')?.getBoundingClientRect()
    const h2 = document.querySelector('#quant-edge-audit h2')?.getBoundingClientRect()
    const verd = document.querySelector('#edge-audit-verdict')?.getBoundingClientRect()
    return !!hdr && !!h2 && !!verd && h2.top >= hdr.bottom - 1 && verd.top >= hdr.bottom - 1
  })
  const edgeAuditOk =
    edgeAuditClear &&
    (await p.locator('#quant-edge-audit').isVisible()) &&
    (await p.locator('#edge-audit-verdict').isVisible()) &&
    (await p.locator('#quant-edge-audit [data-edge-audit-stage]').count()) === 5
  ok('1-click Full Quant Edge Audit button opens Quant Lab and renders 5-stage Go/No-Go Scorecard below sticky header', edgeAuditOk)
  const scaleReady =
    (await p.locator('#quant-scale-planner').isVisible()) &&
    (await p.locator('[data-scale-trajectory-chart] svg[role=img]').count()) === 1 &&
    (await p.locator('[data-scale-trajectory-chart] [data-target-ref-line]').count()) === 1
  ok('Quant Lab renders $500/Day Scale Planner controls, 180-day trajectory SVG & target reference line', scaleReady)
  await p.locator('#scale-sizing-mode').selectOption('quarter_kelly')
  await p.locator('#scale-assumed-edge').fill('4')
  await p.locator('#scale-plan-run-btn').click(); await p.waitForTimeout(300)
  const sensLabel = (await p.locator('#scale-mode-pill').innerText()).trim()
  await p.locator('#scale-assumed-edge').fill('')
  await p.locator('#scale-plan-run-btn').click(); await p.waitForTimeout(300)
  const empLabel = (await p.locator('#scale-mode-pill').innerText()).trim()
  ok('Scale Planner sensitivity override toggles Empirical Tape <-> Sensitivity (Assumed Edge)',
    sensLabel.includes('Sensitivity') && empLabel.includes('Empirical Tape'),
    `${sensLabel} -> ${empLabel}`)
  const gateCount = await p.locator('[data-readiness-scorecard] [data-readiness-gate]').count()
  const verdictTxt = (await p.locator('#readiness-verdict-pill').innerText()).trim()
  ok('5-Gate Readiness Scorecard renders 5 mechanical gates with honest Paper Research Only verdict',
    gateCount === 5 && verdictTxt.includes('Paper Research Only'),
    `gates=${gateCount} verdict=${verdictTxt}`)
  await p.locator('#quant-scale-planner').scrollIntoViewIfNeeded(); await p.waitForTimeout(150)
  await p.screenshot({ path: `${OUT}/scale-planner-1440-light.png` })

  // Strategy Tournament, Optimal 4-Slot Lineup, 12x12 Correlation Matrix SVG & Edge-by-Regime Heatmap SVG
  const tournamentRows = await p.locator('#tournament-table [data-tournament-row]').count()
  ok('Quant Lab renders 12-strategy OOS Tournament leaderboard and Optimal 4-Slot Lineup',
    tournamentRows >= 12 && (await p.locator('#tournament-lineup').isVisible()),
    `rows=${tournamentRows}`)
  await p.locator('#quant-tournament').scrollIntoViewIfNeeded(); await p.waitForTimeout(150)
  await p.screenshot({ path: `${OUT}/tournament-1440-light.png` })

  const corrCells = await p.locator('#correlation-matrix-svg [data-corr-cell]').count()
  ok('Quant Lab renders 12x12 Strategy Return Correlation Heatmap SVG with readout',
    corrCells >= 144 && (await p.locator('#corr-hover-readout').isVisible()),
    `cells=${corrCells}`)
  const regCells = await p.locator('#regime-heatmap-svg [data-regime-cell]').count()
  ok('Quant Lab renders 5x4 Edge-by-Regime Heatmap SVG with Wilson CI readout',
    regCells === 20 && (await p.locator('#regime-hover-readout').isVisible()),
    `cells=${regCells}`)
  await p.locator('#quant-correlation').scrollIntoViewIfNeeded(); await p.waitForTimeout(150)
  await p.screenshot({ path: `${OUT}/correlation-regime-1440-light.png` })

  const recalChartOk =
    (await p.locator('[data-recalibration-chart] svg[role=img]').count()) === 1 &&
    (await p.locator('#recal-brier').innerText()).includes(' to ') &&
    (await p.locator('#ev-gate-impact tbody tr').count()) === 3
  ok('Quant Lab renders PAV Isotonic Recalibration Before vs After SVG & EV-After-Fees Gate impact', recalChartOk)
  await p.locator('#quant-recalibration').scrollIntoViewIfNeeded(); await p.waitForTimeout(150)
  await p.screenshot({ path: `${OUT}/recalibration-1440-light.png` })

  const quantRuneLeak = await p.evaluate(() => /\brune\b/i.test(document.body.innerText))
  ok('zero user-facing Rune jargon across Quant Lab (v0.6.0)', !quantRuneLeak)

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

// 4. 390x844 mobile sweep across ALL 5 views: zero overflow, zero 1-word orphan wraps, 44px targets, bottom Settings sheet
{
  const { ctx, p } = await open(390, 844)
  await p.goto(FX); await ready(p); await p.waitForTimeout(450)
  const orphanIssues = []
  const tabMeasures = []
  for (const [tid, label] of [['#tab-tape', 'tape'], ['#tab-strategies', 'strategies'], ['#tab-slots', 'slots'], ['#tab-quant', 'quant'], ['#tab-live', 'live']]) {
    await p.locator(tid).click(); await p.waitForTimeout(350)
    const m = await p.evaluate(MEASURE)
    const orph = await p.evaluate(ORPHAN_WRAPS)
    tabMeasures.push({ label, ok: m.docOverflow <= 0 && m.clipped.length === 0 && m.textClipped.length === 0 && m.smallTargets === 0 && m.lowContrast === 0 && m.svgBad === 0, m })
    for (const o of orph) orphanIssues.push(`${label}: ${o}`)
    if (label === 'quant') await p.screenshot({ path: `${OUT}/quant-lab-390-light.png` })
  }
  ok('all 5 views pass 390x844 touch, contrast, overflow & SVG geometry',
    tabMeasures.every(x => x.ok),
    tabMeasures.filter(x => !x.ok).map(x => `${x.label}:overflow=${x.m.docOverflow},textClip=${x.m.textClipped.join('|')},small=${x.m.smallTargets}(${x.m.smallSample.join('|')}),low=${x.m.lowContrast},svg=${x.m.svgBad}(${x.m.svgDetail.join('|')})`).join(' ; '))
  ok('zero 1-word orphan wraps at 390x844 across all 5 views', orphanIssues.length === 0, orphanIssues.slice(0, 6).join(' | '))

  await p.locator('#settings-btn').click(); await p.waitForTimeout(500)
  const box = await p.locator('[data-settings-drawer]').boundingBox()
  ok('phone Settings sheet rises from bottom', !!box && box.y > 40 && Math.round(box.width) === 390, JSON.stringify(box))
  await p.screenshot({ path: `${OUT}/settings-390.png` })
  await p.getByRole('button', { name: 'Close settings', exact: true }).click(); await p.waitForTimeout(450)
  await ctx.close()
}

// 5. v0.8.0 API Wire-Contract E2E (proves live Kalshi v2 Account/Config/Arm/Order/Cancel/Kill-Switch + CSRF, SSE, Auto-Collector Schedule, Tournament, Recalibration, Scale Plan, Calibration, Monte Carlo, Walk-Forward, Validate, Backtest & Export wiring)
{
  const { ctx, p } = await open(1440, 900)
  const hits = new Set()
  let csrfChecked = 0
  const checkCsrf = r => {
    const h = r.request().headers()
    if (h['x-openbell-csrf'] === '1' && (h['content-type'] || '').includes('application/json')) csrfChecked++
  }
  let liveState = {
    configured: false,
    environment: 'demo',
    api_base_url: 'https://demo-api.kalshi.co/trade-api/v2',
    key_id_masked: null,
    key_path_configured: false,
    connected: false,
    mode: 'paper',
    live_armed: false,
    killed: false,
    kill_reason: null,
    rails: {
      max_order_contracts: 25,
      max_order_dollars: 25,
      max_daily_loss_dollars: 50,
      max_open_exposure_dollars: 100,
      allowed_ticker_prefix: 'KX',
    },
    daily_realized_loss_dollars: 0,
    open_exposure_dollars: 5.5,
    orders_placed_today: 0,
    last_error: null,
  }
  let liveOrders = [
    { order_id: 'ord-demo-1', client_order_id: 'ob-1', ticker: 'KXBTC15M-MOCK', action: 'buy', side: 'yes', status: 'resting', count: 5, remaining_count: 5, price_cents: 55, created_time: '2026-11-22T09:00:00Z' },
  ]
  let schedState = {
    enabled: false,
    interval_secs: 60,
    effective_interval_secs: 60,
    min_interval_secs: 30,
    max_interval_secs: 900,
    backoff_secs: 0,
    rate_limited: false,
    consecutive_errors: 0,
    in_flight: false,
    runs_total: 2,
    last_run_epoch: 1700000900,
    next_run_epoch: null,
    next_run_in_secs: null,
    last_written: 12,
    last_deduped: 40,
    last_error: null,
    settled_windows: 140,
    readiness_target_windows: 200,
    readiness_progress_pct: 70,
    snapshots_on_disk: 520,
    markets_seen: 140,
    series: ['KXBTC15M', 'KXETH15M', 'KXSOL15M', 'KXXRP15M'],
    mode: 'public_readonly_get',
  }
  let runnerState = {
    mode: 'stopped',
    live_armed: false,
    killed: false,
    tick_interval_ms: 2000,
    cancel_before_close_secs: 30,
    bankroll_dollars: 250,
    active_slots: [
      {
        slot_id: 1,
        strategy: 'late-favourite-edge',
        series: ['KXBTC15M', 'KXETH15M'],
        max_contracts: 10,
        execution_mode: 'maker',
        auto_pause_drawdown_dollars: 25,
        enabled: true,
        paused: false,
        pause_reason: null,
        realized_pnl_dollars: 3.45,
        drawdown_dollars: 0.4,
        quotes_posted: 8,
        fills: 5,
        open_quotes_count: 1,
        last_signal_epoch: 1700000900,
      },
    ],
    open_quotes: [],
    stats: {
      quotes_posted: 8,
      fills: 5,
      fill_rate_pct: 62.5,
      realized_pnl_dollars: 3.45,
      unrealized_exposure_dollars: 0,
      avg_fill_slippage_cents: 0.0,
      avg_queue_wait_secs: 31,
    },
  }
  const runnerLedgerEntries = [
    {
      id: 'run-led-1',
      seq: 1,
      ts: 1700000800,
      event: 'OrderFilled',
      mode: 'shadow',
      slot_id: 1,
      strategy: 'late-favourite-edge',
      ticker: 'KXBTC15M-MOCK',
      series: 'KXBTC15M',
      order_id: 'shd-1',
      side: 'yes',
      execution_mode: 'maker',
      limit_cents: 66,
      fill_cents: 66,
      contracts: 10,
      slippage_cents: 0.0,
      queue_wait_secs: 31,
      fee_dollars: 0.04,
      realized_pnl_dollars: 3.45,
      result: 'yes',
      reason: 'Queue depleted',
    },
  ]
  const runnerSummary = [
    {
      strategy: 'late-favourite-edge',
      quotes_posted: 8,
      quotes_canceled: 2,
      fills: 5,
      windows_settled: 5,
      fill_rate_pct: 62.5,
      wins: 4,
      losses: 1,
      realized_pnl_dollars: 3.45,
      total_fees_dollars: 0.2,
      avg_fill_slippage_cents: 0.0,
      avg_queue_wait_secs: 31,
      paused_count: 0,
    },
  ]
  await p.route('**/api/home', async r => {
    const res = await r.fetch()
    const j = await res.json()
    j.version = '0.9.0'
    j.capabilities = ['sse', 'calibration', 'walkforward', 'montecarlo', 'sandbox', 'export', 'recalibration', 'scale_plan', 'tournament', 'auto_collector', 'live_kalshi', 'risk_rails', 'edge_audit', 'shadow_runner']
    j.collector = {
      ...j.collector,
      auto_collect_enabled: schedState.enabled,
      auto_collect_interval_secs: schedState.interval_secs,
      auto_collect_backoff_secs: schedState.backoff_secs,
      readiness_target_windows: 200,
      readiness_progress_pct: 70,
    }
    return r.fulfill({ response: res, body: JSON.stringify(j) })
  })
  await p.route('**/api/live/status*', r => {
    hits.add('live-status')
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(liveState) })
  })
  await p.route('**/api/live/account*', r => {
    hits.add('live-account')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        configured: liveState.configured,
        connected: liveState.connected,
        balance_dollars: liveState.configured ? 1250.0 : null,
        portfolio_value_dollars: liveState.configured ? 1312.5 : null,
        daily_pnl_dollars: liveState.configured ? 18.25 : null,
        note: liveState.configured ? 'Connected to Kalshi v2 Demo.' : 'Not configured.',
      }),
    })
  })
  await p.route('**/api/live/positions*', r => {
    hits.add('live-positions')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        positions: liveState.configured
          ? [{ ticker: 'KXBTC15M-MOCK', side: 'yes', position: 10, market_exposure_dollars: 5.5, realized_pnl_dollars: 2.1, resting_orders_count: 1 }]
          : [],
      }),
    })
  })
  await p.route('**/api/live/orders/*', r => {
    if (r.request().method() === 'DELETE') {
      checkCsrf(r)
      hits.add('live-cancel')
      const id = r.request().url().split('/').pop()
      liveOrders = liveOrders.filter(o => o.order_id !== id)
      return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ ok: true, order_id: id }) })
    }
    return r.continue()
  })
  await p.route('**/api/live/orders', r => {
    hits.add('live-orders')
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ orders: liveState.configured ? liveOrders : [] }) })
  })
  await p.route('**/api/live/fills*', r => {
    hits.add('live-fills')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        fills: liveState.configured
          ? [{ trade_id: 'fill-1', order_id: 'ord-0', ticker: 'KXBTC15M-MOCK', action: 'buy', side: 'yes', count: 10, price_cents: 55, fee_dollars: 0.17, created_time: '2026-11-22T08:55:00Z' }]
          : [],
      }),
    })
  })
  await p.route('**/api/live/config*', r => {
    checkCsrf(r)
    hits.add('live-config')
    const b = JSON.parse(r.request().postData() || '{}')
    liveState = {
      ...liveState,
      configured: true,
      connected: true,
      environment: b.environment || 'demo',
      key_id_masked: 'demo-key-…89ab',
      key_path_configured: true,
      rails: b.rails || liveState.rails,
    }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(liveState) })
  })
  await p.route('**/api/live/arm*', r => {
    checkCsrf(r)
    hits.add('live-arm')
    liveState = { ...liveState, live_armed: true, killed: false, mode: 'live' }
    runnerState = { ...runnerState, live_armed: true, killed: false }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(liveState) })
  })
  await p.route('**/api/live/disarm*', r => {
    checkCsrf(r)
    hits.add('live-disarm')
    liveState = { ...liveState, live_armed: false, mode: 'paper' }
    runnerState = { ...runnerState, live_armed: false, mode: runnerState.mode === 'live_auto' ? 'stopped' : runnerState.mode }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(liveState) })
  })
  await p.route('**/api/live/order', r => {
    checkCsrf(r)
    hits.add('live-order')
    const b = JSON.parse(r.request().postData() || '{}')
    const ord = {
      order_id: 'ord-demo-2',
      client_order_id: b.client_order_id || 'ob-2',
      ticker: b.ticker,
      action: b.action,
      side: b.side,
      status: 'resting',
      count: b.count,
      remaining_count: b.count,
      price_cents: b.price_cents,
      created_time: '2026-11-22T09:05:00Z',
    }
    liveOrders = [ord, ...liveOrders]
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ ok: true, order: ord }) })
  })
  await p.route('**/api/live/kill-switch*', r => {
    checkCsrf(r)
    hits.add('live-kill')
    const n = liveOrders.length
    liveOrders = []
    liveState = { ...liveState, live_armed: false, killed: true, kill_reason: 'operator_kill_switch', mode: 'paper' }
    runnerState = { ...runnerState, mode: 'stopped', live_armed: false, killed: true, open_quotes: [] }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ ok: true, disarmed: true, cancelled_count: n, errors: [], status: liveState }) })
  })
  await p.route('**/api/runner/status*', r => {
    hits.add('runner-status')
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(runnerState) })
  })
  await p.route('**/api/runner/config*', r => {
    checkCsrf(r)
    hits.add('runner-config')
    const b = JSON.parse(r.request().postData() || '{}')
    runnerState = {
      ...runnerState,
      cancel_before_close_secs: Number(b.cancel_before_close_secs) || 30,
      bankroll_dollars: Number(b.bankroll_dollars) || 250,
      active_slots: Array.isArray(b.slots) ? b.slots.map((s, i) => ({
        ...s,
        paused: false,
        pause_reason: null,
        realized_pnl_dollars: runnerState.active_slots[i]?.realized_pnl_dollars ?? 0,
        drawdown_dollars: 0,
        quotes_posted: runnerState.active_slots[i]?.quotes_posted ?? 0,
        fills: runnerState.active_slots[i]?.fills ?? 0,
        open_quotes_count: 0,
        last_signal_epoch: null,
      })) : runnerState.active_slots,
    }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(runnerState) })
  })
  await p.route('**/api/runner/start*', r => {
    checkCsrf(r)
    hits.add('runner-start')
    const b = JSON.parse(r.request().postData() || '{}')
    if (b.mode === 'live_auto' && (!liveState.live_armed || liveState.killed || !b.confirm_auto_live)) {
      return r.fulfill({ status: 403, contentType: 'application/json', body: JSON.stringify({ error: 'live_auto requires armed state and confirm_auto_live' }) })
    }
    runnerState = {
      ...runnerState,
      mode: b.mode === 'live_auto' ? 'live_auto' : 'shadow',
      open_quotes: [
        {
          order_id: b.mode === 'live_auto' ? 'live-q-1' : 'shd-q-1',
          slot_id: 1,
          strategy: 'late-favourite-edge',
          ticker: 'KXBTC15M-MOCK',
          series: 'KXBTC15M',
          side: 'yes',
          mode: b.mode === 'live_auto' ? 'live' : 'shadow',
          execution_mode: 'maker',
          limit_cents: 68,
          contracts: 10,
          remaining_contracts: 10,
          queue_ahead_initial: 100,
          queue_ahead_remaining: 25,
          est_fill_eta_secs: 35,
          posted_epoch: 1700000900,
          close_epoch: 1700001200,
          status: 'resting',
        },
      ],
    }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(runnerState) })
  })
  await p.route('**/api/runner/stop*', r => {
    checkCsrf(r)
    hits.add('runner-stop')
    const b = JSON.parse(r.request().postData() || '{}')
    runnerState = {
      ...runnerState,
      mode: 'stopped',
      open_quotes: b.cancel_resting === false ? runnerState.open_quotes : [],
    }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(runnerState) })
  })
  await p.route('**/api/runner/ledger*', r => {
    hits.add('runner-ledger')
    const u = new URL(r.request().url())
    const strat = u.searchParams.get('strategy')
    const mode = u.searchParams.get('mode')
    const entries = runnerLedgerEntries.filter(e => (!strat || e.strategy === strat) && (!mode || e.mode === mode))
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ entries, summary_by_strategy: runnerSummary }) })
  })
  await p.route('**/api/collector/status*', r => {
    hits.add('collector-status')
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(schedState) })
  })
  await p.route('**/api/collector/schedule*', async r => {
    hits.add('collector-schedule')
    const reqBody = JSON.parse(r.request().postData() || '{}')
    schedState = { ...schedState, enabled: Boolean(reqBody.enabled), interval_secs: Number(reqBody.interval_secs) || 60, effective_interval_secs: Number(reqBody.interval_secs) || 60 }
    return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(schedState) })
  })
  await p.route('**/api/analytics/tournament*', r => {
    hits.add('tournament')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        slippage_cents: 1,
        is_fraction: 0.6,
        oos_fraction: 0.4,
        oos_windows_total: 96,
        honesty_note: 'Ranked strictly on OOS (40%) tape after fees + 1c slippage.',
        rankings: [
          { rank: 1, strategy: 'late-favourite-edge', side: 'yes', oos_windows: 48, is_windows: 72, oos_win_rate: 0.812, oos_net_expectancy_cents: 3.45, oos_net_pnl_dollars: 16.56, oos_sharpe: 1.92, oos_max_drawdown_pct: 4.1, oos_positive: true, series_breakdown: [{ series: 'BTC', windows: 28, win_rate: 0.82, net_expectancy_cents: 3.8, net_pnl_dollars: 10.6 }, { series: 'ETH', windows: 20, win_rate: 0.8, net_expectancy_cents: 2.9, net_pnl_dollars: 5.9 }] },
          { rank: 2, strategy: 'mid-band-reversion', side: 'no', oos_windows: 42, is_windows: 64, oos_win_rate: 0.643, oos_net_expectancy_cents: 1.85, oos_net_pnl_dollars: 7.77, oos_sharpe: 1.24, oos_max_drawdown_pct: 6.2, oos_positive: true, series_breakdown: [{ series: 'SOL', windows: 42, win_rate: 0.64, net_expectancy_cents: 1.85, net_pnl_dollars: 7.77 }] },
          { rank: 3, strategy: 'spread-squeeze-no', side: 'no', oos_windows: 35, is_windows: 52, oos_win_rate: 0.514, oos_net_expectancy_cents: -1.10, oos_net_pnl_dollars: -3.85, oos_sharpe: -0.45, oos_max_drawdown_pct: 11.8, oos_positive: false, series_breakdown: [] },
        ],
        optimal_lineup: {
          strategies: ['late-favourite-edge', 'mid-band-reversion'],
          max_correlation_limit: 0.65,
          max_pairwise_abs_corr: 0.22,
          positive_candidate_count: 2,
          combined_oos_expectancy_cents: 2.65,
          is_fallback: true,
          honesty_note: 'Honest fallback: only 2 of 3 strategies pass positive OOS expectancy after fees + 1c slippage.',
        },
        correlation_matrix: {
          strategies: ['late-favourite-edge', 'mid-band-reversion', 'spread-squeeze-no'],
          matrix: [[1.0, 0.22, -0.35], [0.22, 1.0, 0.71], [-0.35, 0.71, 1.0]],
          shared_windows: [[48, 30, 24], [30, 42, 20], [24, 20, 35]],
          threshold: 0.65,
        },
        regime_heatmap: {
          price_buckets: ['10-30c', '30-50c', '50-70c', '70-90c', '90-99c'],
          time_buckets: ['>600s', '300-600s', '120-300s', '<120s'],
          cells: [
            { price_bucket: '70-90c', time_bucket: '120-300s', n: 32, wins: 27, realised_win_rate: 0.844, avg_fee_cents: 1.3, net_expectancy_cents: 3.9, wilson_lower_95: 0.68, wilson_upper_95: 0.93, low_sample: false },
            { price_bucket: '50-70c', time_bucket: '<120s', n: 14, wins: 7, realised_win_rate: 0.5, avg_fee_cents: 1.7, net_expectancy_cents: -2.4, wilson_lower_95: 0.27, wilson_upper_95: 0.73, low_sample: true },
          ],
        },
      }),
    })
  })
  await p.route('**/api/analytics/recalibration*', r => {
    hits.add('recalibration')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        windows_settled: 240,
        windows_conflicted: 0,
        slippage_cents: 1.0,
        portfolio: {
          population: 'portfolio',
          is_count: 144,
          oos_count: 96,
          total_count: 240,
          is_fraction: 0.6,
          oos_fraction: 0.4,
          oos_before: { sample_size: 96, brier_score: 0.194, log_loss: 0.562, ece: 0.068, mce: 0.11 },
          oos_after: { sample_size: 96, brier_score: 0.171, log_loss: 0.514, ece: 0.031, mce: 0.05 },
          oos_improved: true,
          min_detectable_edge: 0.098,
          honesty_note: 'Monotone PAV fitted strictly on IS (60%) and evaluated on OOS (40%).',
          oos_buckets: [
            { bucket_index: 0, label: '0-5%', lower_bound: 0.0, upper_bound: 0.05, midpoint: 0.025, n: 0, wins: 0, realised_rate: null, wilson_lower_95: null, wilson_upper_95: null, mean_pred_p: null, gap: null, calibrated_p: 0.05, brier_contrib: 0, log_loss_contrib: 0 },
            { bucket_index: 13, label: '65-70%', lower_bound: 0.65, upper_bound: 0.70, midpoint: 0.675, n: 40, wins: 27, mean_pred_p: 0.675, realised_rate: 0.675, wilson_lower_95: 0.52, wilson_upper_95: 0.80, gap: 0.0, calibrated_p: 0.68, brier_contrib: 0.08, log_loss_contrib: 0.25 },
            { bucket_index: 15, label: '75-80%', lower_bound: 0.75, upper_bound: 0.80, midpoint: 0.775, n: 56, wins: 45, mean_pred_p: 0.775, realised_rate: 0.804, wilson_lower_95: 0.68, wilson_upper_95: 0.89, gap: 0.029, calibrated_p: 0.79, brier_contrib: 0.09, log_loss_contrib: 0.26 },
          ],
          entry_gate: {
            slippage_margin_cents: 1.0,
            total_evaluated: 96,
            accepted_count: 56,
            refused_count: 40,
            acceptance_rate: 0.583,
            baseline_win_rate: 0.75,
            accepted_win_rate: 0.804,
            refused_win_rate: 0.675,
            baseline_total_pnl_dollars_1ct: 14.2,
            accepted_total_pnl_dollars_1ct: 22.8,
            refused_total_pnl_dollars_1ct: -8.6,
            pnl_delta_dollars_1ct: 8.6,
            cells: [],
          },
        },
        market: { population: 'market', is_count: 0, oos_count: 0, total_count: 0, oos_buckets: [] },
        by_strategy: [],
      }),
    })
  })
  await p.route('**/api/analytics/scale-plan*', r => {
    hits.add('scale-plan')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        mode_notice: 'Paper research only.',
        sizing_mode_resolved: 'equal_slots',
        strategy_scope: 'portfolio',
        target_daily_dollars: 500,
        current_empirical_daily_dollars: -4.2,
        required_bankroll_equal_slots: null,
        required_bankroll_quarter_kelly: null,
        required_bankroll_half_kelly: null,
        required_bankroll_reason: 'OOS expectancy after fees and slippage is <= 0; refusing to fabricate positive bankroll scaling.',
        empirical_summary: {
          windows_settled_total: 240,
          windows_settled_oos: 96,
          total_trades: 240,
          oos_trades: 96,
          oos_net_expectancy_cents: -0.65,
          oos_ece: 0.031,
          max_drawdown_pct: 9.4,
        },
        liquidity_ceiling: {
          windows_per_day_per_series: 96,
          max_contracts_per_window: 250,
          active_series_count: 4,
          single_series_max_daily_dollars: 0,
          multi_series_max_daily_dollars: 0,
          fill_ratio_at_target: 0,
          note: 'No positive edge.',
        },
        compounding_projection: {
          mode_label: 'Empirical Tape',
          is_empirical: true,
          seed: 42,
          paths: 1000,
          horizon_days: 180,
          starting_bankroll: 1000,
          target_daily_dollars: 500,
          prob_reach_target: 0,
          median_days_to_target: null,
          prob_dd_gte_25_pct: 0.31,
          curve: [
            { day: 0, equity_p10: 1000, equity_p50: 1000, equity_p90: 1000, daily_run_rate_p10: 0, daily_run_rate_p50: 0, daily_run_rate_p90: 0 },
            { day: 90, equity_p10: 920, equity_p50: 960, equity_p90: 1020, daily_run_rate_p10: -6, daily_run_rate_p50: -4, daily_run_rate_p90: 2 },
            { day: 180, equity_p10: 840, equity_p50: 925, equity_p90: 1040, daily_run_rate_p10: -8, daily_run_rate_p50: -4, daily_run_rate_p90: 3 },
          ],
        },
        sensitivity_projection: null,
        readiness_gates: {
          verdict: 'PAPER_RESEARCH_ONLY',
          passed_count: 3,
          total_gates: 5,
          gates: [
            { id: 'sample_depth', label: 'Sample depth >= 200 settled trades', passed: true, observed: 240, threshold: 200, unit: 'trades', detail: '240 settled paper trades recorded.' },
            { id: 'oos_net_expectancy', label: 'OOS net expectancy > 0 after fees & slippage', passed: false, observed: -0.65, threshold: 0.0, unit: 'cents/contract', detail: 'OOS net expectancy is -0.65c/contract.' },
            { id: 'oos_ece', label: 'OOS calibration ECE < 0.05', passed: true, observed: 0.031, threshold: 0.05, unit: 'ECE', detail: 'OOS ECE is 0.0310.' },
            { id: 'max_drawdown_pct', label: 'Max paper drawdown < 20%', passed: true, observed: 9.4, threshold: 20.0, unit: '%', detail: 'Max drawdown is 9.4%.' },
            { id: 'liquidity_fill_ratio', label: 'Liquidity fill ratio >= 75% at target', passed: false, observed: 0.0, threshold: 0.75, unit: 'ratio', detail: 'Book capacity covers 0.0% of target.' },
          ],
          blocking_reasons: ['OOS net expectancy > 0 after fees & slippage: OOS net expectancy is -0.65c/contract.'],
        },
      }),
    })
  })
  await p.route('**/api/analytics/edge-audit*', r => {
    hits.add('edge-audit')
    return r.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        mode_notice: 'Paper edge audit — strictly read-only simulation, no venue capital at risk.',
        dataset: 'live',
        verdict: 'NO_GO',
        verdict_label: 'NO-GO — Paper Research Only',
        blocking_reasons: ['OOS net expectancy > 0 after fees & slippage: OOS net expectancy is -0.65c/contract.'],
        slippage_cents: 1,
        strategies_total: 12,
        strategies_oos_positive: 2,
        strategies_forward_proven: 0,
        windows_settled_total: 240,
        windows_settled_is: 144,
        windows_settled_oos: 96,
        windows_conflicted: 0,
        top_strategy: { strategy: 'late-favourite-edge', side: 'yes', oos_trades: 48, oos_win_rate: 0.812, oos_avg_entry_price: 0.76, oos_avg_fee_cents: 1.28, oos_net_expectancy_cents: 3.45, oos_sharpe: 1.92, oos_max_drawdown_pct: 4.1 },
        lineup: {
          slots: [
            { slot_number: 1, strategy: 'late-favourite-edge', side: 'yes', oos_trades: 48, oos_net_expectancy_cents: 3.45, oos_sharpe: 1.92, oos_max_drawdown_pct: 4.1, max_abs_corr_with_lineup: 0.22 },
            { slot_number: 2, strategy: 'mid-band-reversion', side: 'no', oos_trades: 42, oos_net_expectancy_cents: 1.85, oos_sharpe: 1.24, oos_max_drawdown_pct: 6.2, max_abs_corr_with_lineup: 0.22 },
          ],
          correlation_cap: 0.65,
          max_abs_correlation: 0.22,
          mean_abs_correlation: 0.22,
          lineup_complete: false,
          fallback_used: true,
          combined_oos_expectancy_cents: 2.65,
          honesty_note: 'Honest fallback: only 2 of 12 strategies pass positive OOS expectancy.',
        },
        recalibration: {
          is_count: 144, oos_count: 96, oos_brier_before: 0.194, oos_brier_after: 0.171,
          oos_ece_before: 0.068, oos_ece_after: 0.031, oos_improved: true, min_detectable_edge: 0.098,
          ev_gate_total: 96, ev_gate_accepted: 56, ev_gate_refused: 40, ev_gate_accepted_win_rate: 0.804,
          ev_gate_before_pnl_dollars: 14.2, ev_gate_after_pnl_dollars: 22.8, ev_gate_pnl_delta_dollars: 8.6,
        },
        fee_ev: { avg_entry_price: 0.76, gross_edge_cents: 5.73, kalshi_fee_drag_cents: 1.28, slippage_drag_cents: 1, net_ev_cents: 3.45, realized_oos_win_rate_pct: 81.2, break_even_win_rate_pct: 78.3 },
        monte_carlo: { seed: 42, paths: 1000, start_equity_dollars: 100, p5_final_equity_dollars: 92, p50_final_equity_dollars: 121, p95_final_equity_dollars: 162, median_max_drawdown_pct: 5.1, prob_dd_gte_10_pct: 0.12, prob_dd_gte_25_pct: 0.015, prob_profit: 0.74 },
        readiness: {
          verdict: 'PAPER_RESEARCH_ONLY', gates_passed: 3, gates_total: 5,
          gates: [
            { id: 'sample_depth', label: 'Sample depth >= 200 settled trades', passed: true, observed: 240, threshold: 200, unit: 'trades', detail: '240 settled paper trades recorded.' },
            { id: 'oos_net_expectancy', label: 'OOS net expectancy > 0 after fees & slippage', passed: false, observed: -0.65, threshold: 0.0, unit: 'cents/contract', detail: 'OOS net expectancy is -0.65c/contract.' },
            { id: 'oos_ece', label: 'OOS calibration ECE < 0.05', passed: true, observed: 0.031, threshold: 0.05, unit: 'ECE', detail: 'OOS ECE is 0.0310.' },
            { id: 'max_drawdown_pct', label: 'Max paper drawdown < 20%', passed: true, observed: 9.4, threshold: 20.0, unit: '%', detail: 'Max drawdown is 9.4%.' },
            { id: 'liquidity_fill_ratio', label: 'Liquidity fill ratio >= 75% at target', passed: false, observed: 0.0, threshold: 0.75, unit: 'ratio', detail: 'Book capacity covers 0.0% of target.' },
          ],
          observed_median_depth: 250, fill_ratio_at_target: 0.0,
        },
        funnel: [
          { id: 'tournament', title: '12 Strategies OOS Tournament', passed: true, metric: '+3.45c/ct', detail: '2/12 positive OOS across 96 OOS windows' },
          { id: 'lineup', title: 'Uncorrelated 4-Slot Lineup', passed: false, metric: '2/4 slots', detail: 'Only 2 qualified' },
          { id: 'recalibration', title: 'PAV Isotonic Recalibration', passed: true, metric: 'OOS Improved', detail: 'EV gate accepted 56/96 OOS quotes' },
          { id: 'net_ev', title: 'Kalshi Fee & Slippage Net EV', passed: true, metric: '+3.45c net/ct', detail: 'Slippage 1.0c' },
          { id: 'readiness', title: 'Monte Carlo DD & 5-Gate Readiness', passed: false, metric: '3/5 gates', detail: 'OOS net expectancy <= 0' },
        ],
      }),
    })
  })
  await p.route('**/api/tape/stream', r => {
    hits.add('sse')
    return r.fulfill({
      status: 200,
      headers: { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' },
      body: 'event: tape\ndata: {"snapshots_written":1}\n\nevent: runner\ndata: {"kind":"quote_posted","slot_id":1,"strategy":"late-favourite-edge","ticker":"KXBTC15M-MOCK"}\n\n',
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
  // Full Live Kalshi v2 flow: Settings Config -> Live & Rails View -> Arm Confirmation Modal -> Submit Order -> Cancel Order -> Kill Switch
  await p.locator('#settings-btn').click(); await p.waitForTimeout(350)
  await p.locator('#live-api-key-id').fill('demo-key-123456789ab')
  await p.locator('#live-pem-path').fill('/tmp/mock-kalshi-demo.pem')
  await p.locator('#save-live-config-btn').click(); await p.waitForTimeout(450)
  await p.getByRole('button', { name: 'Close settings', exact: true }).click(); await p.waitForTimeout(500)
  await p.locator('#tab-live').click(); await p.waitForTimeout(250)
  const balanceShown = (await p.locator('#live-balance-val').innerText()).includes('$1250.00') ||
    (await p.locator('#live-balance-val').innerText()).includes('$1,250.00')
  const runnerLockedBeforeArm = await p.locator('#runner-start-live-btn').isDisabled()
  await p.locator('#runner-save-config-btn').click(); await p.waitForTimeout(250)
  await p.locator('#runner-start-shadow-btn').click(); await p.waitForTimeout(250)
  const runnerShadowActive =
    (await p.locator('#runner-mode-badge').innerText()).includes('Shadow') &&
    (await p.locator('#runner-open-quotes-card [role="progressbar"]').count()) >= 1
  await p.locator('#runner-ledger-strategy-filter').selectOption('late-favourite-edge'); await p.waitForTimeout(200)
  await p.locator('[data-runner-ledger-mode="shadow"]').click(); await p.waitForTimeout(200)
  await p.locator('#runner-stop-btn').click(); await p.waitForTimeout(250)
  await p.locator('#trading-mode-live').click(); await p.waitForTimeout(300)
  await p.locator('#arm-live-checkbox').check()
  await p.locator('#confirm-arm-live-btn').click(); await p.waitForTimeout(400)
  const armedPillOk = (await p.locator('#live-mode-pill').innerText()).includes('ARMED')
  await p.locator('#runner-start-live-btn').click(); await p.waitForTimeout(250)
  await p.locator('#runner-live-confirm-checkbox').check()
  await p.locator('#runner-confirm-live-btn').click(); await p.waitForTimeout(300)
  const runnerLiveActive = (await p.locator('#runner-mode-badge').innerText()).includes('Live Auto')
  await p.locator('#live-ticket-ticker').fill('KXBTC15M-MOCK')
  await p.locator('#live-ticket-count').fill('5')
  await p.locator('#live-ticket-price').fill('55')
  await p.waitForTimeout(100)
  await p.locator('#submit-live-order-btn').click(); await p.waitForTimeout(300)
  await p.screenshot({ path: `${OUT}/live-armed-1440-light.png` })
  await p.locator('[data-cancel-order="ord-demo-1"]').click(); await p.waitForTimeout(250)
  await p.locator('#kill-switch-btn').click(); await p.waitForTimeout(300)
  const disarmedAfterKill = /KILLED|DISARMED/.test(await p.locator('#live-mode-pill').innerText())
  const runnerHaltedAfterKill = /Halted|Stopped/.test(await p.locator('#runner-mode-badge').innerText())
  ok('Shadow Runner API wire contract & SSE runner stream enforce fail-closed Live Auto gating, config/start/stop/ledger & kill-switch halt',
    ['runner-status', 'runner-config', 'runner-start', 'runner-stop', 'runner-ledger'].every(k => hits.has(k)) &&
      runnerLockedBeforeArm && runnerShadowActive && runnerLiveActive && runnerHaltedAfterKill,
    `locked=${runnerLockedBeforeArm} shadow=${runnerShadowActive} live=${runnerLiveActive} halt=${runnerHaltedAfterKill}`)
  await p.locator('#tab-tape').click(); await p.waitForTimeout(200)

  await p.locator('#auto-collector-strip [data-collector-mode="60"]').click(); await p.waitForTimeout(250)
  const schedUpdated = (await p.locator('#auto-collector-status-pill').innerText()).includes('Auto') &&
    (await p.locator('#readiness-progress').innerText()).includes('140 / 200')
  await p.locator('#tab-strategies').click(); await p.waitForTimeout(150)
  await p.locator('#open-sandbox-btn').click(); await p.waitForTimeout(150)
  await p.locator('#sandbox-validate-btn').click(); await p.waitForTimeout(150)
  await p.locator('#sandbox-backtest-btn').click(); await p.waitForTimeout(200)
  await p.locator('#tab-slots').click(); await p.waitForTimeout(200)
  await Promise.all([p.waitForEvent('download'), p.locator('#export-report-md').click()])
  await p.locator('#run-edge-audit-btn').click(); await p.waitForTimeout(450)
  const scorecardText = await p.locator('#readiness-scorecard').innerText()
  const evGateText = await p.locator('#ev-gate-impact').innerText()
  const tournamentText = await p.locator('#quant-tournament').innerText()
  const edgeAuditText = await p.locator('#quant-edge-audit').innerText()
  const serverEngineShown =
    balanceShown &&
    armedPillOk &&
    disarmedAfterKill &&
    csrfChecked >= 5 &&
    schedUpdated &&
    edgeAuditText.includes('Server Edge Audit') &&
    (await p.locator('#quant-calibration').innerText()).includes('Server Quant Engine') &&
    (await p.locator('#quant-scale-planner').innerText()).includes('Server Scale Engine') &&
    (await p.locator('#quant-recalibration').innerText()).includes('Server PAV Engine') &&
    tournamentText.includes('Server Tournament Engine') &&
    tournamentText.includes('2/4 slots qualified') &&
    (await p.locator('#scale-req-equal').innerText()).trim() === 'N/A' &&
    scorecardText.includes('240 trades') &&
    scorecardText.includes('-0.65c/ct') &&
    scorecardText.includes('3.10%') &&
    evGateText.includes('+$8.60')
  ok('v0.9.0 live API contract integrates Kalshi v2 config/arm/order/cancel/kill-switch + CSRF, SSE, edge-audit, auto-collector, tournament, recalibration, scale-plan, validate, backtest, export, calibration, walkforward & montecarlo',
    ['edge-audit', 'live-status', 'live-account', 'live-positions', 'live-orders', 'live-fills', 'live-config', 'live-arm', 'live-order', 'live-cancel', 'live-kill', 'sse', 'collector-status', 'collector-schedule', 'tournament', 'recalibration', 'scale-plan', 'calibration', 'walkforward', 'montecarlo', 'validate', 'backtest', 'export'].every(k => hits.has(k)) && serverEngineShown,
    `hits=${[...hits].sort().join(',')} csrf=${csrfChecked} bal=${balanceShown} armed=${armedPillOk} kill=${disarmedAfterKill} serverEngine=${serverEngineShown}`)
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
  await p.waitForFunction(() => /Nothing was written/.test(document.querySelector('#collect-status')?.textContent || ''), { timeout: 5000 }).catch(() => {})
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
