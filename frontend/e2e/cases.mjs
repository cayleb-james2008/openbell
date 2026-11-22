// cases.mjs: kit/replay.mjs worst-case suite for OpenBell v0.4.0.
export default ({ LONG, patch, malformed, down, json }) => {
  const PASS = 'PROVEN on forward evidence (paper only - no orders placed)'
  const text = (p, sel) => p.locator(sel).textContent().catch(() => '')
  return [
    {
      name: 'null list entry and non-string verdicts in strategies table do not crash',
      routes: [['**/api/strategies*', patch(j => [null, ...j.map((s, i) => i === 0 ? { ...s, verdict: null } : i === 1 ? { ...s, verdict: 99, windows_traded: null, mean_cents_per_contract: 'bad' } : s)])]],
      check: async p => {
        await p.locator('#tab-strategies').click(); await p.waitForTimeout(250)
        const rows = await p.locator('#strategies tbody tr button[data-strategy-row]').count()
        if (rows !== 5) return 'expected 5 valid strategy rows, got ' + rows
        if ((await text(p, '#standing-figure')).trim() !== '0 of 5') return 'standing: ' + await text(p, '#standing')
        return true
      },
    },
    {
      name: 'null names and standards still tally',
      routes: [['**/api/strategies*', patch(j => [...j, { ...j[0], strategy: null, standard: null }, { ...j[0], strategy: 5, standard: { min_windows: 'many' } }])]],
      check: async p => (await text(p, '#standing-figure')).trim() === '0 of 7' || 'figure: ' + await text(p, '#standing'),
    },
    {
      name: 'long unbroken proven names and tickers stay inside bounds',
      path: '/',
      shot: true,
      routes: [
        ['**/api/strategies*', patch(j => j.map((s, i) => i < 2 ? { ...s, verdict: PASS, strategy: 'strategy-' + LONG } : s))],
        ['**/api/slots*', patch(j => ({ ...j, slots: j.slots.map((s, i) => i ? s : { ...s, strategy: 'slot-' + LONG }) }))],
        ['**/api/markets*', patch(j => j.map((m, i) => i ? m : { ...m, ticker: 'KX-' + LONG }))],
      ],
      check: async p => {
        if ((await text(p, '#standing-figure')).trim() !== '2 of 5') return 'figure: ' + await text(p, '#standing')
        const overflow = await p.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
        return overflow <= 0 || `doc overflow ${overflow}px`
      },
    },
    {
      name: 'differing standards claim no window count',
      routes: [['**/api/strategies*', patch(j => j.map((s, i) => ({ ...s, standard: { ...s.standard, min_windows: 100 + i } })))]],
      check: async p => {
        const t = await text(p, '#standing-detail')
        return (/waiting for evidence/.test(t) && !/below/.test(t)) || 'tally: ' + t
      },
    },
    {
      name: 'a body of the wrong shape says the state is unavailable',
      routes: [['**/api/strategies*', r => json(r, { not: 'a list' })]],
      check: async p => (await text(p, '#standing-label')).trim() === 'Strategy state unavailable' || 'label: ' + await text(p, '#standing'),
    },
    {
      name: 'a body that does not parse says the state is unavailable',
      routes: [['**/api/strategies*', malformed]],
      check: async p => (await text(p, '#standing-label')).trim() === 'Strategy state unavailable' || 'label: ' + await text(p, '#standing'),
    },
    {
      name: 'an unreachable server says the state is unavailable',
      routes: [['**/api/strategies*', down]],
      check: async p => (await text(p, '#standing-label')).trim() === 'Strategy state unavailable' || 'label: ' + await text(p, '#standing'),
    },
    {
      name: 'an empty library says so',
      routes: [['**/api/strategies*', r => json(r, [])]],
      check: async p => (await text(p, '#standing-label')).includes('No strategies in the library') || 'label: ' + await text(p, '#standing'),
    },
  ]
}
