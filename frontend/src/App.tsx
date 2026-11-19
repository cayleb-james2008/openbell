import { type CSSProperties, useEffect, useState } from "react";
import { MotionConfig } from "motion/react";
import { Bell, Moon, Sun } from "lucide-react";
import { Toaster } from "sonner";
import { AnimatedBackground } from "@/components/ui/animated-background";
import { SegmentedControl, SegmentedControlItem } from "@/components/ui/segmented-control";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Button } from "@/components/ui/button";
import { glide } from "@/motion";
import { type Dataset, type Read, type Home, useDesk } from "@/lib/desk";
import { Lead } from "@/Lead";
import { AccountPanel, SlotsPanel, TapeHealth } from "@/Panels";
import { Markets, Strategies } from "@/Lists";
import { UpdateStrip, VersionRow, useUpdates } from "@/Update";

const SECTIONS = [
  { id: "report", label: "Report", target: "report-card" },
  { id: "pot", label: "Paper pot", target: "pot" },
  { id: "tape", label: "Tape health", target: "tape" },
  { id: "account", label: "Venue account", target: "account" },
  { id: "slots", label: "Slots", target: "slots-card" },
  { id: "strategies", label: "Strategies", target: "strategies" },
  { id: "markets", label: "Markets", target: "markets-card" },
] as const;

const ok = <T,>(r: Read<T> | undefined): T | null => (r && r.ok ? r.v : null);

function readDataset(): Dataset {
  return new URLSearchParams(location.search).get("dataset") === "fixture" ? "fixture" : "live";
}

function readTheme(): "light" | "dark" {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

function collectorWords(home: Home | null): { word: string; dot: string; line: string } {
  if (!home) return { word: "Collector: checking", dot: "bg-track", line: "Collector: checking" };
  const c = home.collector;
  const failed = c.series_failed_last_pass ?? 0;
  const healthy = c.collecting && c.dropped_lines === 0 && c.conflicting_settlements === 0 && !failed;
  const dot = c.collecting ? (healthy ? "bg-info-foreground" : "bg-dot-amber") : "bg-unread";
  const line = c.collecting ? "Collecting, newest " + ageOf(c.newest_snapshot_age_secs) : "Not collecting";
  return { word: c.collecting ? (healthy ? "Healthy" : "Collecting with issues") : "Stale", dot, line };
}
function ageOf(age: number | null): string {
  if (age === null) return "none yet";
  return age < 90 ? age + "s ago" : age < 5400 ? Math.round(age / 60) + "m ago" : (age / 3600).toFixed(1) + "h ago";
}

function Shell() {
  const [dataset, setDataset] = useState<Dataset>(readDataset);
  const [theme, setTheme] = useState<"light" | "dark">(readTheme);
  const [active, setActive] = useState<string>(SECTIONS[0].id);
  const { reads, load } = useDesk(dataset);
  const upd = useUpdates();

  useEffect(() => {
    const on = () => {
      let cur: string = SECTIONS[0].id;
      for (const s of SECTIONS) {
        const el = document.getElementById(s.target);
        if (el && el.getBoundingClientRect().top <= 160) cur = s.id;
      }
      setActive(cur);
    };
    on();
    window.addEventListener("scroll", on, { passive: true });
    return () => window.removeEventListener("scroll", on);
  }, []);

  const pickDataset = (next: Dataset) => {
    setDataset(next);
    const url = new URL(location.href);
    if (next === "fixture") url.searchParams.set("dataset", "fixture"); else url.searchParams.delete("dataset");
    history.replaceState(null, "", url);
  };
  const pickTheme = (t: "light" | "dark") => {
    document.documentElement.dataset.theme = t;
    try { localStorage.setItem("openbell-theme", t); } catch { /* private window: the choice lasts this visit */ }
    setTheme(t);
  };
  const go = (s: (typeof SECTIONS)[number]) => {
    const el = document.getElementById(s.target);
    if (!el) return;
    setActive(s.id);
    el.scrollIntoView({ block: "start" });
    el.querySelector("h2")?.focus({ preventScroll: true });
  };

  const home = ok(reads?.home ?? undefined);
  const cw = collectorWords(home);
  const allFailed = !!reads && Object.values(reads).every((r) => !r.ok);
  const failMsg = reads && !reads.home.ok ? reads.home.msg : "";
  const fixture = dataset === "fixture";

  return (
    <TooltipProvider>
      <header className="sticky top-0 z-20 border-b border-hairline bg-canvas">
        <div className="mx-auto flex min-h-14 max-w-[1280px] flex-wrap items-center gap-x-5 gap-y-2 px-4 py-2">
          <div className="mr-auto flex items-center gap-2 text-base font-medium text-ink">
            <Bell className="size-5 text-ink" strokeWidth={1.5} aria-hidden />
            OpenBell
          </div>
          <Pill variant="outline" className="text-ink-2"><PillStatus><PillIndicator variant="info" />Paper only</PillStatus>This build places no orders</Pill>
          <Pill id="chip" variant="outline" className="max-w-full text-ink-2" title="Collector health">
            <span id="chip-dot" aria-hidden="true" className={"size-2 rounded-full " + cw.dot} />
            <span id="chip-text" className="min-w-0 break-words">{allFailed ? "Collector: unavailable" : cw.line}</span>
          </Pill>
          <div className="flex items-center gap-2">
            <SegmentedControl aria-label="Which tape the paper figures use" size="sm">
              {(["live", "fixture"] as const).map((d) => (
                <SegmentedControlItem key={d} id={d === "live" ? "seg-live" : "seg-fixture"} aria-pressed={dataset === d} data-checked={dataset === d ? "" : undefined} onClick={() => pickDataset(d)}>
                  {d === "live" ? "Live tape" : "Fixture"}
                </SegmentedControlItem>
              ))}
            </SegmentedControl>
            <div id="settings">
              <SegmentedControl aria-label="Appearance" size="sm" id="themeBtn">
                {(["dark", "light"] as const).map((t) => (
                  <SegmentedControlItem key={t} aria-label={t === "dark" ? "Dark appearance" : "Light appearance"} aria-pressed={theme === t} data-checked={theme === t ? "" : undefined} onClick={() => pickTheme(t)}>
                    {t === "dark" ? <Moon className="size-4" strokeWidth={1.5} aria-hidden /> : <Sun className="size-4" strokeWidth={1.5} aria-hidden />}
                    <span className="sr-only">{t === "dark" ? "Dark" : "Light"}</span>
                  </SegmentedControlItem>
                ))}
              </SegmentedControl>
            </div>
          </div>
        </div>
        <nav aria-label="Sections" className="mx-auto flex max-w-[1280px] flex-nowrap gap-1 overflow-x-auto px-4 pb-2">
          <AnimatedBackground
            defaultValue={active}
            onValueChange={(id) => { const s = SECTIONS.find((x) => x.id === id); if (s) go(s); }}
            className="rounded-md bg-raised"
            transition={glide}
          >
            {SECTIONS.map((s) => (
              <button key={s.id} type="button" data-id={s.id} aria-current={active === s.id ? "location" : undefined}
                className="h-10 shrink-0 cursor-pointer rounded-md px-3 text-sm font-medium text-ink-2 outline-none transition-colors duration-(--dur-press) ease-(--ease-house) hover:text-ink focus-visible:ring-2 focus-visible:ring-blue data-[checked=true]:text-ink">
                {s.label}
              </button>
            ))}
          </AnimatedBackground>
        </nav>
      </header>

      <main id="top" className="mx-auto grid max-w-[1280px] grid-cols-[minmax(0,1fr)] gap-4 px-4 pb-10 pt-4">
        <p className="sr-only" aria-live="polite">{fixture ? "Showing the example tape" : "Showing the live tape"}</p>
        <UpdateStrip u={upd} />
        {fixture && (
          <div role="note" className="flex gap-3 rounded-[10px] border border-hairline bg-board px-4 py-3 text-sm text-ink-2">
            <p><span className="font-medium text-ink">Example tape.</span> Paper figures below come from a labelled fixture of 120 settled windows written for tests. They were not collected from Kalshi. The daily report and the venue account always read the live desk.</p>
          </div>
        )}
        {allFailed && (
          <div id="load-error" role="alert" className="flex flex-wrap items-center gap-3 rounded-[10px] border border-unread/40 bg-unread/10 px-4 py-3 text-sm text-ink">
            <span id="load-error-text" className="min-w-0 flex-1 basis-64">The desk could not load: none of its six reads answered ({failMsg}). Check that openbell-rs is running on this address, then Retry.</span>
            <Button id="retryBtn" variant="outline" onClick={load}>Retry</Button>
          </div>
        )}

        <Lead reads={reads} dataset={dataset} onRetry={load} />

        <div className="grid min-w-0 grid-cols-[minmax(0,1fr)] gap-px overflow-hidden rounded-[10px] border border-hairline bg-hairline lg:grid-cols-2">
          <TapeHealth read={reads?.home ?? null} onRetry={load} onCollected={load} />
          <AccountPanel read={reads?.home ?? null} onRetry={load} />
          <div className="bg-surface lg:col-span-2">
            <SlotsPanel read={reads?.slots ?? null} onRetry={load} notes={ok(reads?.slots ?? undefined)?.notes} />
          </div>
        </div>

        <Strategies read={reads?.strategies ?? null} libraryErrors={home?.library_errors ?? []} dataset={dataset} onRetry={load} />
        <Markets read={reads?.markets ?? null} onRetry={load} />
      </main>

      <footer className="mx-auto grid max-w-[1280px] gap-3 px-4 pb-10 text-sm text-ink-3">
        <VersionRow u={upd} />
        <p>Every figure is venue (read from Kalshi), tape (counted from the collected snapshots), or paper (the slot simulation). Anything not obtained reads unavailable, with the reason.</p>
      </footer>
      <Toaster position="bottom-right" theme={theme}
        style={{ "--normal-bg": "var(--surface)", "--normal-text": "var(--ink)", "--normal-border": "var(--hairline)", "--border-radius": "10px", fontSize: "14px" } as CSSProperties} />
    </TooltipProvider>
  );
}

export function App() {
  return (
    <MotionConfig reducedMotion="user">
      <Shell />
    </MotionConfig>
  );
}
