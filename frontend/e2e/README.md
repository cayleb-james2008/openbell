# OpenBell UI checks

`npm run check:ui`, run from `frontend/`, builds the frontend and the binary. It then starts OpenBell on a free port against the seeded tapes in `fixtures/data` and runs two scripts, each on fresh data:

- `gate.mjs`: the Ledger gate, 61 checks. It measures the live and fixture tapes at 1440, 768, 390 and 320 px in light and dark. It then covers filters, search, keyboard, drawer, collect, sections, the error, empty and loading states, the phone layout, console errors and reduced motion.
- `replay.mjs`: worst-case data fed in through `page.route` mocks, 16 checks.

No run touches the network. The collector's `curl` is replaced on the app's PATH by `fixtures/kalshi-curl.mjs`, which answers market reads with one recorded public pass (`fixtures/kalshi-markets.json`) and fails anything else. The updater is off. The check fails unless every check passes and each script ran its full count. It stops everything it started, also when interrupted.

Setup, once:

- `npm ci` in `frontend/`. Playwright is the frontend's own `playwright-core` devDependency.
- A Rust toolchain with this crate's dependencies already fetched (`cargo build --offline` must work).
- Chrome for Testing 151 at `~/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome` (what `npx playwright@1.62.0 install chromium` puts there). To use another Chromium build, set `E2E_CHROME` to its path.

Flags, after `--` (`npm run check:ui -- --skip-build`):

- `--skip-build` reuses the last build.
- `--shots` adds screenshots at 1440 and 390, light and dark, on fresh data.
- `--lints` adds the taste, impeccable and Hallmark lints. They only report and never fail the check. The tools live outside this repo; `lints.sh` lists their default paths and the env vars that override them.

Screenshots, `result.json` files, logs and the run's data go to `e2e/.out/`, which git ignores. To bring the fixture up by hand, run `node e2e/fixture.mjs start` (it prints the URL); stop it with `node e2e/fixture.mjs stop`.
