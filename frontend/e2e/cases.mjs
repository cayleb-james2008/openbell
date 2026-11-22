// cases.mjs: kit/replay.mjs worst-case suite for OpenBell (standing line, Strategies null/malformed rows, Markets, Slots, and long unbroken strings).
// Run: CHROME_PATH=<chromium> PLAYWRIGHT=<frontend>/node_modules/playwright-core/index.mjs node replay.mjs '<url>?dataset=fixture' cases.mjs out
export default ({ LONG, patch, malformed, down, overflowing, json }) => {
  const PASS = 'PROVEN on forward evidence (paper only — no orders placed)'
  const text = (p, sel) => p.locator(sel).innerText().catch(() => '')
  return [
    { name: 'null list entry and non-string verdicts in strategies table do not crash',
      routes: [['**/api/strategies*', patch(j => [null, ...j.map((s, i) => i === 0 ? { ...s, verdict: null } : i === 1 ? { ...s, verdict: 99, windows_traded: null, mean_cents_per_contract: 'bad' } : s)])]],
      check: async p => {
        const rows = await p.locator('#strategies tbody tr button[data-strategy-row]').count()
        if (rows !== 5) return 'expected 5 valid strategy rows, got ' + rows
        if ((await text(p, '#standing-figure')).trim() !== '0 of 5') return 'standing: ' + await text(p, '#standing')
        return true
      } },
    { name: 'null names and standards still tally',
      routes: [['**/api/strategies*', patch(j => [...j, { ...j[0], strategy: null, standard: null }, { ...j[0], strategy: 5, standard: { min_windows: 'many' } }])]],
      check: async p => (await text(p, '#standing-figure')).trim() === '0 of 7' || 'figure: ' + await text(p, '#standing') },
    { name: 'long unbroken proven names (live tape) stay inside the line and rows', path: '/', shot: true,
      routes: [
        ['**/api/strategies*', patch(j => j.map((s, i) => i < 2 ? { ...s, verdict: PASS, strategy: 'strategy-' + LONG } : s))],
        ['**/api/slots*', patch(j => ({ ...j, slots: j.slots.map((s, i) => i ? s : { ...s, strategy: 'slot-' + LONG }) }))],
        ['**/api/markets*', patch(j => j.map((m, i) => i ? m : { ...m, ticker: 'KX-' + LONG }))],
      ],
      check: async p => {
        if ((await text(p, '#standing-figure')).trim() !== '2 of 5') return 'figure: ' + await text(p, '#standing')
        return (await overflowing(p, '#standing li span')) === 0 || 'a tally span is wider than its box'
      } },
    { name: 'differing standards claim no window count',
      routes: [['**/api/strategies*', patch(j => j.map((s, i) => ({ ...s, standard: { ...s.standard, min_windows: 100 + i } })))]],
      check: async p => { const t = await p.locator('#standing-detail').textContent().catch(() => ''); return (/waiting for evidence/.test(t) && !/below/.test(t)) || 'tally: ' + t.replace(/\s+/g, ' ') } },
    { name: 'a body of the wrong shape says the state is unavailable', routes: [['**/api/strategies*', r => json(r, { not: 'a list' })]],
      check: async p => (await text(p, '#standing-label')) === 'Strategy state unavailable' || 'label: ' + await text(p, '#standing') },
    { name: 'a body that does not parse says the state is unavailable', routes: [['**/api/strategies*', malformed]],
      check: async p => (await text(p, '#standing-label')) === 'Strategy state unavailable' || 'label: ' + await text(p, '#standing') },
    { name: 'an unreachable server says the state is unavailable', routes: [['**/api/strategies*', down]],
      check: async p => (await text(p, '#standing-label')) === 'Strategy state unavailable' || 'label: ' + await text(p, '#standing') },
    { name: 'an empty library says so', routes: [['**/api/strategies*', r => json(r, [])]],
      check: async p => (await text(p, '#standing-label')).includes('No strategies in the library') || 'label: ' + await text(p, '#standing') },
  ]
}
