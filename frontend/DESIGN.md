# OpenBell front end: DESIGN (v0.4.0 Kalshi 15m Quant Research & Paper Compounding Desk)

## v0.4.0 Architecture Summary
- **Complete frontend restart**: Replaced the two-column report-card layout with a focused Kalshi 15-minute Quant Research & Paper Compounding Desk built on Cayleb's House Frontend system (`8/8` libraries active in `ROLE-COVERAGE.md`, self-hosted Geist 400/500/600 & Geist Mono `.woff2`, calm Square UI nested hairline surfaces).
- **Settings-only Appearance**: Light is the crisp default surface; Dark is the full second theme toggled exclusively inside the Desk Settings `Vaul` sheet (`#settings` / `#themeBtn`), never on the top command bar.
- **Live-first operation**: Zero fixture badge or fixture switcher in live mode. `?dataset=fixture` remains available strictly for automated tests and CI verification.
- **Hero Telemetry & Compounding Strip (`#readings`)**: 4 tabular `Geist Mono` `SlidingNumber` KPI tiles (Paper Compounding Pot + Stage 1 Tremor `ProgressCircle`, Proven Strategies standing tally, Active 15m Markets, Live Tape Snapshots), hover-inspectable Paper Compounding Curve SVG (`role="img"`, ≥12px SVG labels, `motion.path` `pathLength`), Tremor `Tracker` of recent settled outcomes, and 5-series crypto coverage `CategoryBar`. Motion Primitives `BorderTrail` and `TextShimmer` run strictly while `POST /api/collect` is in flight.
- **Three-tab Workbench (`COSS UI Tabs` + `AnimatedBackground` glide + `/` global filter + `1`/`2`/`3` hotkeys)**:
  - `Live 15m Tape`: Open 15m contracts sorted first with live countdowns, YES/NO implied probability bars, bid/ask/spread, and clickable Vaul Market Inspector backed by `GET /api/markets/history`.
  - `Rune Strategies`: Ranked leaderboard with Tremor `SparkAreaChart` + evidence `Tracker`, null/malformed guards, `j`/`k`/`Enter` navigation, and Vaul Strategy Inspector showing live `.rn` script source (`GET /api/strategy/source`) inside a Cult UI `Expandable`.
  - `Slot Desk`: Compound-sized slot allocation, 3-strike Tremor `Tracker`, verbatim Daily Report inside a Cult UI `Expandable`, and Read-only Venue Account.

---

# Historical Notes (round 1-3)

## Job
An honest paper-trading desk for Kalshi's 15-minute crypto markets: it collects the venue's public data, scores Rune strategies on paper, and writes a daily report.
It places no orders, and every figure says whether it is venue, tape or paper, or reads unavailable with the reason.
The first screen answers five questions: 1. How did paper trading do (the daily report)? 2. What is the paper pot and its curve? 3. Is the collector healthy and how fresh is the tape? 4. Which strategies and slots are active and how are they doing? 5. What does the venue account say, or why is it unavailable?

## Native theme and faces
Console, native DARK (graphite, three lightness steps). Light "paper" is a full second theme. Host Grotesk 500 for what a person reads; Atkinson Hyperlegible Mono (slashed zero) for every figure the instrument measured (values, ids, times, units).
Amber (`--blue` role) = a person / the one primary control / the current step. Cool (`info` role, `text-info-foreground`) = an instrument (a reading, a trace). No shadow, no glow, no gradient: separation is 1px hairlines and the three steps.

## Region map (desktop 1440 x 900, then phone 390 x 844)
- Top bar (sticky): wordmark; "Paper only" Kibo pill (the honesty chip); collector chip with a cool dot and "newest Ns ago" (answers Q3); Live tape / Fixture segmented control (COSS SegmentedControl); theme segmented control (sun/moon); version chip. Why: the honesty and freshness claims must be visible on every scroll.
- Section tabs (sticky under the bar): Report, Paper pot, Tape health, Venue account, Slots, Strategies, Markets. Motion Primitives AnimatedBackground pill glides to the active section and tracks scroll. Replaces the "Jump to" links. Phone: the strip scrolls sideways inside itself.
- Lead surface (one hairline panel, answers Q1 and Q2): top-left, the daily report verdict set at display size (Host Grotesk 500, sentence case) with the venue and tape Kibo pills; top-right, the paper pot as a reading in the mono face at display size with the start value beside it, a "paper" pill, and a Tremor ProgressCircle for the stage-one target; full width under them, the labelled paper equity trace (inline SVG, axes and legend). Why: the verdict and the pot are the two things a person opens this app for, so they are the largest things on screen.
- Below the lead: the report's remaining lines, verbatim, in a Cult UI Expandable (collapsed to six lines). Why: the full text is a record, not a headline.
- Bento from one surface with 1px seams (answers Q3, Q5, Q4): Tape health (counts, freshness, Collect now with a border trail while it runs) and Venue account (four readings, unavailable as one muted panel when no key), then Slots (one row per slot with Tremor Tracker strike blocks, windows, P&L, equity).
- Strategies (COSS Table with an InputGroup search and Kbd hint) and Markets (collapsed panel: COSS Table with search). Why: lists take tables before cards.
- Phone (390): one column in the same order; tables keep their columns and scroll inside their own box, never the page.

## Signature object
The reading and its trail: the paper pot as a single large figure in the measured face, read against its start and drawn as the trace beneath it. Round 1 has the reading and the trace; the audit trail on a 1px spine (collector passes, slot windows, strategy results) is round 2.

## Library rows (Library | need of this product | part used | file)
| Library | Need of this product | Part used | File |
|---|---|---|---|
| Motion Primitives | section tabs glide to the chosen section; the pot figure rolls when it changes; a collect that is running shows that it runs | AnimatedBackground, SlidingNumber, BorderTrail | components/ui/animated-background.tsx, sliding-number.tsx, border-trail.tsx |
| COSS UI | search in a list; Live/Fixture and theme choice; a state chip; a table of rows; the pressable Collect now | InputGroup + Kbd, SegmentedControl, Badge, Table, Button, Tooltip | components/ui/input-group.tsx, kbd.tsx, segmented-control.tsx, badge.tsx, table.tsx, button.tsx, tooltip.tsx |
| Cult UI | the report's rest is long and opens in place | Expandable, ExpandableTrigger, ExpandableContent | components/ui/expandable.tsx |
| Kibo UI | the venue / tape / paper tags and status states | Pill, PillIndicator | components/kibo-ui/pill/index.tsx |
| Tremor | the stage target is one percentage that matters; strikes out of three are a row of states | ProgressCircle, Tracker | components/ProgressCircle/ProgressCircle.tsx, components/Tracker/Tracker.tsx |
| Vaul | a phone section or filter sheet | not in round 1 (planned: section sheet below 640 px) | - |
| Sonner | a collect or an update finished where the person is not looking | Toaster, toast.success, toast.error | package `sonner` (Toaster in src/App.tsx) |
| Motion | the equity trace is drawn once; the rest of the motion comes from the presets | motion/react `motion.path` pathLength, once | package `motion` |
No "no need here" row: Vaul is pending (not refused).

## Motion values
Only from `@/motion` (src/motion.ts copied to rune/used) and the `--dur-*` / `--ease-house` tokens. Nothing plays an entrance on first load. Numbers that change roll. Loops only on running work (the collect border trail).

## Parity inventory (old page, `git show HEAD:assets/index.html`)
Status column: planned round 1 = kept in this rebuild unless a note says otherwise. Filled in at the end of round 1.
| # | Item (old page) | Behaviour to keep | Status |
|---|---|---|---|
| P1 | Theme: `localStorage` `openbell-theme`, dark default, toggle, label "Appearance: Dark/Light" | same key, same default, toggle persists; control is a SegmentedControl with sun and moon | kept (label text changes to the segmented control's visible names; aria-label keeps "Appearance") |
| P2 | Dataset toggle Live tape / Fixture, `?dataset=fixture` in the URL, caption | same key in the URL, aria-pressed, the fixture banner text "Example tape" | kept |
| P3 | Fixture banner: "Example tape. Paper figures below come from a labelled fixture of 120 settled windows written for tests..." | same text | kept |
| P4 | Collector chip: "Collector: checking / Collecting, newest Ns ago / Not collecting / Collector: unavailable" with a dot | same wording | kept |
| P5 | Paper only note "Paper only: this build places no orders" | same text | kept |
| P6 | Purpose and "How to read this: venue / tape / paper" | kept as the legend popover (the ART asks for it) | changed: moved into a Popover behind a Tooltip-labelled key |
| P7 | Load error: "The desk could not load: none of its six reads answered (...). Check that openbell-rs is running on this address, then Retry." + Retry | same text, Retry button | kept |
| P8 | Per-panel error: "This part did not load (msg). The rest of the page is unaffected." + Retry | same text on each panel, data-err hooks | kept |
| P9 | Shape check per read (home, slots, equity, markets, strategies, report) | same checks | kept |
| P10 | Poll every 30 s (`setInterval(load, 30000)`) | same | kept |
| P11 | Daily report headline (first line) and the rest verbatim, "Show full report / Collapse report" after six lines | Expandable with the same labels | kept (labels now "Show full report" / "Collapse report") |
| P12 | Venue account: Balance, Day P&L, Open positions, Capital at risk; "unavailable" when null; capital at risk "$0.00 (no open positions)" or "see positions"; note from server | same mapping; unavailable reads as the word, never zero | kept |
| P13 | Venue account always-visible footer line "Account figures need a Kalshi API key..." | shown as one muted panel with the server note; the four readings in mono | changed: one muted panel when unavailable (ART) |
| P14 | Tape health: Tape lines, Markets seen, Settled windows, Dropped lines, Conflicting settlements, Newest snapshot; state line with Healthy/Stale/Empty/Collecting text | same readings and the same sentences (renderTape) | kept |
| P15 | Failed series note "N of M market reads failed on the last pass" and "Failed series: ..." | same | kept |
| P16 | Collect now (POST /api/collect), "Collecting…" while in flight, status "Could not collect right now. Nothing was written. Try again in a minute. Server said: ..." | same; result also raised as a Sonner toast | kept, plus toast |
| P17 | Paper pot figure, change against start ("+$x against the $100.00 start (paper)"), stage target line (reached / % of the way / to go), progress bar | same sentences; progress as ProgressCircle plus the sentence | kept |
| P18 | Equity chart: pot (heavy line), strategy curves, dashed start line, dollar axis labels, day labels, note, legend with last values or "no trades" | inline SVG with the same labels and data | kept |
| P19 | Equity empty states: "No paper trades yet, so there is no pot line..." and "Not enough collected tape to draw the curve yet. Two points are needed." | same text | kept |
| P20 | Slots: one card per slot (status pill, strategy name, strikes N of 3 with pips, "This stint, N windows", "Slot total", paper tag); retired slots ("Retired after N windows, rotated to X.", "Stint P&L") | same facts, Tracker for strikes | kept |
| P21 | Slots empty text "No slots yet. The strategy library in strategies/ is empty..." and notes list | same | kept |
| P22 | Slots note: "Paper simulation: the pot is re-split evenly across N slots..." | same | kept |
| P23 | Strategies table: Strategy, Side, Windows traded of 100 (bar), Mean cents per contract, Verdict (PROVEN/NOT PROVEN pill; fixture PROVEN reads "meets the bar on the example tape (not forward evidence)") | same columns and wording; search added | kept, plus search (ART: lists get search) |
| P24 | Strategies empty text "No strategies in the library..." and library errors ("X did not load, so it is not scored.") | same | kept |
| P25 | Strategies explainer: "A strategy is proven only after 100 forward windows..." | same | kept |
| P26 | Markets: collapsed toggle with "Collected tape, N markets", Show first 40 note, table of Market, Closes, Yes bid/ask, No bid/ask, Last, Result; empty text "No snapshots collected yet..." | same, with search | kept |
| P27 | Footer: version line "OpenBell <version>" link to release_url (only when given), Check for updates, check result | same, with the update rules in the brief | kept, rules updated per COMMON |
| P28 | Footer note "Every figure is venue (...), tape (...), or paper (...)" | same | kept |
| P29 | Update banner (show/hide, Update now, Restart now, What's new, Dismiss, messages) | COMMON rules 1 to 8 | kept, to the brief |
| P30 | Keyboard: focus on headings after jumping (data-live-link) | section tabs focus the section heading | kept |
| P31 | Reduced motion respected | presets + CSS | kept |

## Pinned hooks (must stay readable)
From `.loop/checks/theme.sh` (served HTML): `data-theme`, `[data-theme="light"]`, `localStorage.getItem('openbell-theme') || 'dark'`, `localStorage.setItem('openbell-theme'`, `id="settings"`, `id="themeBtn"`, `id="i-sun"`, `id="i-moon"`, no emoji.
From `.loop/checks/equity.sh` (served HTML and API): `/api/equity` string in the page, `<svg`, the word `paper`, no external resource; the `http` text rule is updated in its own commit to flag loaded resources only.
From `.loop/checks/report.sh`, `stage.sh`, `robust.sh`: API text only (`Stage target (paper):`, `Balance (venue): unavailable`, `Paper only, not venue money`, the report lines). Unaffected by the front end.
From `tests/cli.rs`: `--version` only. Unaffected.
Old element ids kept as React DOM ids (in the rendered page): settings, themeBtn, i-sun/i-moon (sprite), chip, chip-dot, chip-text, live-group, seg-live, seg-fixture, fixture-banner, load-error, load-error-text, retryBtn, update-banner, upd-*, report-card, report, report-head, report-toggle, pot, pot-figure, pot-change, stage-track, stage-line, chart-card, chart, chart-note, legend, tape, c-snaps, c-markets, c-settled, c-dropped, c-conflicts, c-age, c-dot, c-note, c-fail, account, balance, pnl, positions, risk, account-note, slots-card, slots, slots-note, slot-notes, strategies, strategies-body, library-errors, markets-card, markets-toggle, markets-count, markets-panel, markets-note, collect-btn, collect-status, markets, version-row, version-link, check-updates, check-result. Round 1 keeps all of them that the new layout has a place for.

## What was not checked yet (round 1)
Not yet: Vaul phone sheet; audit trail; Tape health spark; screen-reader pass; contrast sweep and sloplint on the built page; Strategies and Markets at 320 px; the cargo test and full kit. Listed in the hand-over.

## Round 1 status (as built)
Built: top bar, Live/Fixture and theme segmented controls, update strip and version line (rules from COMMON), section tabs with AnimatedBackground glide and scroll tracking, daily report verdict and pot reading and labelled equity trace (inline SVG, drawn once), report rest in an Expandable, Tape health (with Collect now, border trail while it runs, Sonner result), Venue account (unavailable panel), Slots table with Tracker strikes, Strategies and Markets tables with search.
Deviations from the parity table, decided in round 1 (reason: the new layout; the owner should see them):
- P2 the old "live desk" collapse (live report and account hidden under Fixture until Show) is removed: the live report and account always show, labelled venue and tape, as the ART brief says.
- P6 the "How to read this" paragraph and the legend popover are NOT built yet (round 2).
- P11 the six-line preview of the full report is replaced by the Expandable trigger alone (round 2 may restore the preview).
- P13 the unavailable account panel lists the server note once, inside the muted panel.
- Not built yet: the audit trail on a 1px spine (signature round 2), the Tape health SparkChart (no snapshot time series endpoint yet), the Vaul phone sheet, the Strategies verdict legend popover, the purpose line (`/#purpose`).
Pinned-hook status: `.loop/checks/equity.sh` `http` rule updated in its own commit (loaded resources only). The remaining raw-HTML greps (`<svg`, `[data-theme="light"]`, `localStorage.setItem('openbell-theme'`, `id="settings"`, `id="themeBtn"`, `id="i-sun"`, `id="i-moon"`) read the served shell, which is a React bundle: they fail until the checks read the rendered DOM. Decision for the lead, not changed here.

## Round 2 status
- One instrument surface: the lead bands, the tape/account pair, slots, strategies and markets are bands on one hairline grid (gap-px on --hairline, bands on --surface, one outer 10px radius).
- Verdict split at the first " — ": the head is display type, the explanation is 16px ink-2 (wording verbatim).
- Audit trail (src/Trail.tsx): the report's own lines as rows; only the lines the report wrote, split at its " · " joins; venue/tape/paper pills only where a line carries them; the one timestamp in its cool time column; the full verbatim text stays behind "Show full report".
- "How to read this": a ? key next to the wordmark; Popover from 768 px, Dialog below. The purpose sentence is one quiet line under the top bar.
- Venue account: the server's no-key sentence once, four readings as mono "unavailable" rows with a "no key" chip.
- Equity empty state: frame with axis hairlines, start line and "paper", the sentence on top.
- Phone (<768): tables become stacked rows (table.stack in index.css, cells carry data-label); the section tabs are a horizontal row with the glide.
- Vaul "Sections" sheet: NO NEED HERE. The section row already scrolls in one line at 390 px; a sheet would add a tap to reach the same tabs.
- Not done in round 2: Hallmark sloplint still reports FAILs (see the round-2 hand-over); contrast and tap-target measurements not run; LOCAL-EDITS.md not created (no part edits made).
- Checks rewritten in their own commits (theme.sh, equity.sh labelled-SVG rule), each with the reason in its message.

## Round 3 status
- Trail: compact rows (~36 px), the Slots and Strategy verdicts groups behind one disclosure row each with a one-line summary counted from the report's own text. Only "Slot N" lines count as slots.
- Vaul: "no need here" for the section tabs (one scroll row at 390 px). The legend stays in a Popover/Dialog, not a drawer, so Vaul is not used.
- Theme overrides (src/theme.css, by hand): --raised light gets a hue (zero chroma was a REFLEX); --input is set to a 3:1 edge colour in both themes. src/tokens.css resets the default palette and shadows, and maps the few palette names the parts use to role tokens.
- Copied parts: every hand edit is in rune/used/LOCAL-EDITS.md and sync-used.mjs skips those files.
- Measured (fixture and live, 320/390/768/1440, light and dark): 0 small-text contrast failures under 4.5:1, 0 controls under 44 px, 0 focusable controls without a visible ring, 0 sideways overflow.
- Hallmark lint: 2 FAIL and 9 WARN remain. FAIL: the ring-offset literal in Tailwind's own base rule; the `.transition` candidate, which the five motion parts create with their `transition=` prop names. WARN: dark-variant background classes of the card table variant, which the app never renders.
- Open defect: the verdict pills in the strategies table show a line through their text in the render. Not yet traced.

## Final sections (fe final, 2026-11-19)

### Colour scheme (owner palette, `_fe/palette-claude.css`)
- Light is the default (ivory), dark the full second theme (warm near-black). Fonts, radii and the shadow recipe stay Console.
- Clay (`--blue`, #d97757) is the one primary key, the current selection and the live state. Blue is the instrument colour: `--dot-blue` #6a9bcc for dots and instrument trace lines; text uses a darker blue (`--info-foreground` #2f5f94 light, #9dc0e4 dark) because #6a9bcc is about 3:1 on ivory and fails text.
- Sage and red are state only (gain/loss, retired, not proven). The equity trace: pot in ink, instrument lines in blue and greys.
- Focus ring: `--focus` (#b35634 light, #e08a6d dark). Clay alone is 2.8:1 on ivory, under the 3:1 ring rule, so the ring is the darker token; `--ring` maps to it.
- `theme.css` is edited by hand. `_fe/gen-theme.py` OPENBELL is stale (it lacks `--input` and `--focus`); do not regenerate without porting those.
- The kit pin `theme.sh` now asserts light as the default (own commit).

### Library rows (Library | need of this product | part used | file)
| Library | Need of this product | Part used | File |
|---|---|---|---|
| Motion Primitives | section tabs glide to the chosen section; a running collect shows it runs; the tape state word morphs | AnimatedBackground, BorderTrail, TextMorph | components/ui/animated-background.tsx, border-trail.tsx, text-morph.tsx |
| COSS UI | search in a list; Live/Fixture and theme choice; the list of rows; the pressable Collect now and Retry | InputGroup, Button, Table, SegmentedControl, Tooltip | components/ui/input-group.tsx, button.tsx, table.tsx, segmented-control.tsx, tooltip.tsx |
| Cult UI | the report's rest, the trail's groups and the update notes open in place | Expandable, ExpandableTrigger, ExpandableContent | components/ui/expandable.tsx |
| Kibo UI | venue / tape / paper tags and the slot and verdict states | Pill, PillStatus, PillIndicator | components/kibo-ui/pill/index.tsx |
| Tremor | the stage target is one percentage; strikes and windows are rows of states | ProgressCircle, Tracker | components/ProgressCircle/ProgressCircle.tsx, components/Tracker/Tracker.tsx |
| Vaul | no need here: the section tabs are one scroll row at 390 px, and a sheet would add a tap to reach the same tabs | none | none |
| Sonner | a collect result, or an update result, where the person is not looking | Toaster, toast.success, toast.error | package `sonner` (App.tsx, Panels.tsx, Update.tsx) |
| Motion | the equity lines are drawn once; the rest comes from the presets | motion/react `motion.path` pathLength, once (`drawOnce`) | package `motion`, src/motionProps.ts |

SlidingNumber (Motion Primitives) is in rune/used but no longer imported: its rolling digit strips rendered overlapping glyphs in the built page, so the pot and tape counts are plain mono readings.

### Local edits to parts
`rune/used/LOCAL-EDITS.md` is unchanged this round: no part was edited. The focus ring, the phone table and the palette are in tokens.css, theme.css and index.css, not in copied parts.

### Measured (fixture and live, 2026-11-19, built page)
- Phone page height (390 px, light): live 4,695 px; fixture 4,827 px. Round start was 4,855 px (fixture). Target ~3,500 px is NOT reached. Live breakdown: sticky header 169, purpose 92, lead 492, chart 448, trail 678, tape and account 759, slots 768, strategies 907, markets 114, footer 204.
- Sideways overflow: scrollWidth equals the viewport at 320, 390, 768 and 1440, both themes, with no element outside its own scroller (the section-tab row is the only one).
- Tap targets: 0 controls under 44 px (buttons, links, tabs, switches, inputs) at 320 to 1440, both themes.
- Contrast, both themes, 390 and 1440, fixture: 211 to 218 text nodes, lowest 4.72:1 (text rule 4.5). Input edges (search fields) 3:1 or better on the wrapper.
- Hallmark render lint (`sloplint.mjs --scope app --render`): 0 FAIL, 0 REFLEX, 0 WARN.
- `cargo test`: 52 unit and 1 integration test passed.

### Pinned hooks (unchanged, plus the default)
Kept as before (see "Pinned hooks" above). `theme.sh` now reads `getItem('openbell-theme')||'light'`, asserted in its own commit.

### Parity
The parity inventory above stands. This round changed no behaviour and no wording: the rolling digits became plain text, the phone trail shows full labels, and the Collected markets preview on phone shows 4 rows instead of 8 (the same "Showing the first N of M" note).

### What was not checked
- A screen-reader pass on the rebuilt phone rows. The live dataset at 320 px beyond the overflow and target measures. The kit's proof.sh queries the live Kalshi public API.

## v0.3.0 alignment (2026-11-21)
- **Typography**: Self-hosted `Geist` (`400`, `500`) and `Geist Mono` (`400–500`) `.woff2` bundled inline in `assets/index.html` (`frontend/src/fonts.css`, `theme.css`, `tokens.css`), replacing `Host Grotesk` / `Atkinson Hyperlegible Mono` for full House Frontend parity with zero external network calls.
- **All 8 House Frontend libraries active**: Vaul `Drawer` (`StrategyDrawer` in `Lists.tsx`, `250 ms` house enter/dismiss timing) and COSS UI `Kbd` (`/` shortcut hint on search inputs) join Motion Primitives, COSS UI, Cult UI, Kibo UI, Tremor (`ProgressCircle` + `Tracker`), Sonner, and Motion (`0` "no need here" rows in `ROLE-COVERAGE.md`).
- **Lead hierarchy & fixture badge**: Desktop separates the Daily report (`venue` / `tape`) and `Strategies proven on forward evidence` (`paper`) across a vertical 1px hairline so the promotion standing never reads as a sub-line of `Day P&L (venue): unavailable`, while preserving first-fold bounds at `1440x900` and `390x844`; `?dataset=fixture` carries an explicit in-frame `example fixture` Kibo Pill.
- **Resilience & self-update**: Null list entries and non-string `verdict` values in `/api/strategies` render safely without crashing or `NaN`; `UpdateStrip` and `VersionRow` enforce loopback-only `Update now` / `Check for updates`, deterministic focus handoff (`Dismiss` -> `Show update`, `Update now` -> `Restart now`), and plain failure copy (`plainUpdateError` + `Details` disclosure).
