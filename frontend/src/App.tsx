import { type CSSProperties, useEffect, useState } from "react";
import { MotionConfig } from "motion/react";
import { Bell, CircleHelp, Moon, Sun } from "lucide-react";
import { Toaster } from "sonner";
import { AnimatedBackground } from "@/components/ui/animated-background";
import { SegmentedControl, SegmentedControlItem } from "@/components/ui/segmented-control";
import { Pill, PillIndicator, PillStatus } from "@/components/kibo-ui/pill";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Dialog, DialogContent, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { sectionGlide } from "@/motionProps";
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

const PURPOSE = "A paper-trading desk for Kalshi’s 15-minute crypto markets: it collects the venue’s public data, tests strategies on paper, and reports what happened. It never places orders.";
const WIDE = "(min-width: 768px)";

const ok = <T,>(r: Read<T> | undefined): T | null => (r && r.ok ? r.v : null);

function readDataset(): Dataset {
  return new URLSearchParams(location.search).get("dataset") === "fixture" ? "fixture" : "live";
}

function readTheme(): "light" | "dark" {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

function useWide() {
  const [wide, setWide] = useState(() => matchMedia(WIDE).matches);
  useEffect(() => {
    const mq = matchMedia(WIDE);
    const on = () => setWide(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return wide;
}

function collectorWords(home: Home | null): { dot: string; line: string } {
  if (!home) return { dot: "bg-track", line: "Collector: checking" };
  const c = home.collector;
  const failed = c.series_failed_last_pass ?? 0;
  const healthy = c.collecting && c.dropped_lines === 0 && c.conflicting_settlements === 0 && !failed;
  const dot = c.collecting ? (healthy ? "bg-info-foreground" : "bg-dot-amber") : "bg-unread";
  const line = c.collecting ? "Collecting, newest " + ageOf(c.newest_snapshot_age_secs) : "Not collecting";
  return { dot, line };
}
function ageOf(age: number | null): string {
  if (age === null) return "none yet";
  return age < 90 ? age + "s ago" : age < 5400 ? Math.round(age / 60) + "m ago" : (age / 3600).toFixed(1) + "h ago";
}

// The tag meanings, once, in a Popover from 768 px and in a Dialog on a phone.
function HowToRead() {
  const wide = useWide();
  const body = (
    <dl className="grid gap-3 text-sm">
      <div><dt className="font-medium text-ink"><Pill variant="outline" className="text-info-foreground">venue</Pill></dt><dd className="mt-1 text-ink-2">What Kalshi reports, read from the venue.</dd></div>
      <div><dt className="font-medium text-ink"><Pill variant="outline" className="text-ink-2">tape</Pill></dt><dd className="mt-1 text-ink-2">Counted from the snapshots the collector wrote.</dd></div>
      <div><dt className="font-medium text-ink"><Pill variant="outline" className="text-warning-foreground">paper</Pill></dt><dd className="mt-1 text-ink-2">The pretend-money simulation. Paper only, not venue money.</dd></div>
      <dd className="text-ink-2">Anything not obtained reads unavailable, with the reason.</dd>
    </dl>
  );
  const trigger = (
    <CircleHelp className="size-5" strokeWidth={1.5} aria-hidden />
  );
  if (wide) {
    return (
      <Popover>
        <PopoverTrigger aria-label="How to read this" className="inline-flex size-11 cursor-pointer items-center justify-center rounded-md text-ink-2 outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) hover:text-ink focus-visible:ring-2 focus-visible:ring-blue">{trigger}</PopoverTrigger>
        <PopoverContent className="w-96 rounded-[10px] border border-hairline bg-surface p-4 text-ink">
          <p className="mb-3 text-sm font-medium text-ink">How to read this</p>
          {body}
        </PopoverContent>
      </Popover>
    );
  }
  return (
    <Dialog>
      <DialogTrigger aria-label="How to read this" className="inline-flex size-11 cursor-pointer items-center justify-center rounded-md text-ink-2 outline-none focus-visible:ring-2 focus-visible:ring-blue">{trigger}</DialogTrigger>
      <DialogContent className="rounded-[10px] border border-hairline bg-surface p-5 text-ink">
        <DialogTitle className="mb-3 text-base font-medium">How to read this</DialogTitle>
        {body}
      </DialogContent>
    </Dialog>
  );
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
  const segItem = "!h-11 !min-w-11";

  return (
    <TooltipProvider>
      <header className="sticky top-0 z-20 border-b border-hairline bg-canvas">
        <div className="mx-auto flex min-h-14 max-w-[1280px] flex-wrap items-center gap-x-5 gap-y-2 px-4 py-2">
          <div className="mr-auto flex items-center gap-1 text-base font-medium text-ink">
            <Bell className="size-5 text-ink" strokeWidth={1.5} aria-hidden />
            <span className="mr-1">OpenBell</span>
            <HowToRead />
          </div>
          <Pill variant="outline" className="!h-7 text-ink-2"><PillStatus><PillIndicator variant="info" />Paper only</PillStatus>This build places no orders</Pill>
          <Pill id="chip" variant="outline" className="!h-7 max-w-full text-ink-2" title="Collector health">
            <span id="chip-dot" aria-hidden="true" className={"size-2 rounded-full " + cw.dot} />
            <span id="chip-text" className="min-w-0 break-words">{allFailed ? "Collector: unavailable" : cw.line}</span>
          </Pill>
          <div className="flex items-center gap-2">
            <SegmentedControl aria-label="Which tape the paper figures use">
              {(["live", "fixture"] as const).map((d) => (
                <SegmentedControlItem key={d} id={d === "live" ? "seg-live" : "seg-fixture"} className={segItem} aria-pressed={dataset === d} data-checked={dataset === d ? "" : undefined} onClick={() => pickDataset(d)}>
                  {d === "live" ? "Live tape" : "Fixture"}
                </SegmentedControlItem>
              ))}
            </SegmentedControl>
            <div id="settings">
              <SegmentedControl aria-label="Appearance" id="themeBtn">
                {(["dark", "light"] as const).map((t) => (
                  <SegmentedControlItem key={t} className={segItem} aria-label={t === "dark" ? "Dark appearance" : "Light appearance"} aria-pressed={theme === t} data-checked={theme === t ? "" : undefined} onClick={() => pickTheme(t)}>
                    {t === "dark" ? <Moon className="size-4" strokeWidth={1.5} aria-hidden /> : <Sun className="size-4" strokeWidth={1.5} aria-hidden />}
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
            {...sectionGlide}
          >
            {SECTIONS.map((s) => (
              <button key={s.id} type="button" data-id={s.id} aria-current={active === s.id ? "location" : undefined}
                className="h-11 shrink-0 cursor-pointer rounded-md px-3 text-sm font-medium text-ink-2 outline-none transition-[color,background-color,border-color] duration-(--dur-press) ease-(--ease-house) hover:text-ink focus-visible:ring-2 focus-visible:ring-blue data-[checked=true]:text-ink">
                {s.label}
              </button>
            ))}
          </AnimatedBackground>
        </nav>
      </header>

      <p className="mx-auto max-w-[1280px] px-4 pt-3 text-sm text-ink-2">{PURPOSE}</p>

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
            <Button id="retryBtn" variant="outline" className="min-h-11" onClick={load}>Retry</Button>
          </div>
        )}

        {/* One instrument surface: bands divided by 1px rules on the hairline ground. */}
        <div className="grid min-w-0 grid-cols-[minmax(0,1fr)] gap-px overflow-hidden rounded-[10px] border border-hairline bg-hairline">
          <Lead reads={reads} dataset={dataset} onRetry={load} />
          <div className="grid min-w-0 grid-cols-[minmax(0,1fr)] gap-px bg-hairline lg:grid-cols-2">
            <TapeHealth read={reads?.home ?? null} onRetry={load} onCollected={load} />
            <AccountPanel read={reads?.home ?? null} onRetry={load} />
          </div>
          <SlotsPanel read={reads?.slots ?? null} onRetry={load} notes={ok(reads?.slots ?? undefined)?.notes} />
          <Strategies read={reads?.strategies ?? null} libraryErrors={home?.library_errors ?? []} dataset={dataset} onRetry={load} />
          <Markets read={reads?.markets ?? null} onRetry={load} />
        </div>
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
