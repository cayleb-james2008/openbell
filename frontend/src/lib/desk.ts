// OpenBell desk API reads, live SSE stream (with polling fallback), runtime shape/null guards, and quant formatters.
import { useCallback, useEffect, useRef, useState } from "react";

export type Dataset = "live" | "fixture";

export type Collector = {
  collecting: boolean;
  snapshots_on_disk: number;
  lines_on_disk?: number | null;
  markets_seen?: number | null;
  settled_windows?: number | null;
  dropped_lines?: number | null;
  conflicting_settlements?: number | null;
  newest_snapshot_age_secs: number | null;
  series_failed_last_pass?: number | null;
  series_failed_names?: string[] | null;
  series_total?: number | null;
  collect_interval_secs?: number | null;
};

export type Account = {
  balance: number | null;
  day_pnl: number | null;
  open_positions: unknown[] | null;
  note: string;
};

export type Home = {
  account: Account;
  collector: Collector;
  version?: string;
  mode?: string;
  series?: string[];
  capabilities?: string[];
  sse?: boolean;
  library_errors?: { file: string; error: string }[];
  stage_target?: { target_dollars: number; progress_pct?: number };
};

export type Slot = {
  slot: number;
  strategy: string;
  status?: string;
  strikes?: number;
  windows_traded: number;
  stint_windows?: number;
  stint_pnl_dollars?: number;
  equity_dollars?: number;
  paper_pnl_dollars: number;
  rotated_to?: string;
};

export type Trade = {
  close_epoch: number;
  contracts: number;
  pnl_dollars: number;
  price: number;
  slot: number;
  strategy: string;
  ticker?: string;
  side?: string;
  result?: string;
};

export type StrategyTotal = {
  strategy: string;
  paper_pnl_dollars: number;
  windows_traded?: number;
};

export type Slots = {
  trades?: Trade[];
  trades_truncated?: boolean;
  paper_equity_dollars: number;
  paper_pot_start: number;
  slots: Slot[];
  retired: Slot[];
  strategy_totals?: StrategyTotal[];
  stage_target?: { target_dollars: number };
  notes?: string[];
  dataset?: string;
};

export type Equity = {
  paper_pot_start: number;
  pot: [number, number][];
  series: { strategy: string; points: [number, number][] }[];
  dataset?: string;
};

export type Market = {
  fetched_at?: number | null;
  ticker: string | null;
  event_ticker?: string | null;
  series?: string | null;
  close_time: string | null;
  status?: string | null;
  yes_bid: number | null;
  yes_ask: number | null;
  no_bid: number | null;
  no_ask: number | null;
  last_price: number | null;
  result: string | null;
};

export type MarketHistory = {
  ticker: string;
  dataset: string;
  points: Market[];
};

export type Strategy = {
  strategy: string | null;
  side?: string | null;
  windows_traded?: number | null;
  windows_settled?: number | null;
  windows_conflicted?: number | null;
  call_failures?: number | null;
  first_half_cents?: number | null;
  second_half_cents?: number | null;
  standard?: { min_windows?: number | null; min_mean_cents?: number | null } | null;
  mean_cents_per_contract: number | null;
  verdict: unknown;
};

export type StrategySource = {
  strategy: string;
  file: string;
  side: string;
  source: string;
};

export type Report = { report: string };

export type Read<T> = { ok: true; v: T } | { ok: false; msg: string };

export type Reads = {
  home: Read<Home>;
  slots: Read<Slots>;
  equity: Read<Equity>;
  markets: Read<Market[]>;
  strategies: Read<Strategy[]>;
  report: Read<Report>;
};

export type TapeStreamState = {
  mode: "sse" | "polling" | "connecting";
  eventCount: number;
  lastEventAt: number | null;
};

const SHAPES: Record<keyof Reads, (x: unknown) => boolean> = {
  home: (x) => {
    const h = x as Home | null;
    return (
      !!h &&
      typeof h.account === "object" &&
      h.account !== null &&
      typeof h.collector === "object" &&
      h.collector !== null &&
      typeof h.collector.collecting === "boolean" &&
      typeof h.collector.snapshots_on_disk === "number"
    );
  },
  slots: (x) => {
    const s = x as Slots | null;
    return !!s && typeof s.paper_equity_dollars === "number" && Array.isArray(s.slots) && Array.isArray(s.retired);
  },
  equity: (x) => {
    const e = x as Equity | null;
    return !!e && Array.isArray(e.pot) && Array.isArray(e.series);
  },
  markets: (x) => Array.isArray(x),
  strategies: (x) => Array.isArray(x),
  report: (x) => !!x && typeof (x as Report).report === "string",
};

export function semverAtLeast(version: string | undefined, target: [number, number, number]): boolean {
  if (typeof version !== "string") return false;
  const m = version.trim().match(/^v?(\d+)\.(\d+)\.(\d+)/);
  if (!m) return false;
  const [maj, min, pat] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (maj !== target[0]) return maj > target[0];
  if (min !== target[1]) return min > target[1];
  return pat >= target[2];
}

export function supportsV050(home: Home | null, capability?: string): boolean {
  if (!home) return false;
  if (capability && Array.isArray(home.capabilities)) {
    if (home.capabilities.includes(capability)) return true;
  }
  if (capability === "sse" && home.sse === true) return true;
  return semverAtLeast(home.version, [0, 5, 0]);
}

export async function getJson(url: string, init?: RequestInit): Promise<unknown> {
  const r = await fetch(url, init);
  if (!r.ok) throw new Error(`${url} answered ${r.status}`);
  return r.json();
}

export async function fetchMarketHistory(ticker: string, dataset: Dataset): Promise<MarketHistory> {
  const qs = new URLSearchParams({ ticker });
  if (dataset === "fixture") qs.set("dataset", "fixture");
  const body = (await getJson(`/api/markets/history?${qs.toString()}`)) as MarketHistory | null;
  if (!body || !Array.isArray(body.points)) throw new Error("Market history answered with an unexpected shape");
  return body;
}

export async function fetchStrategySource(name: string): Promise<StrategySource> {
  const qs = new URLSearchParams({ name });
  const body = (await getJson(`/api/strategy/source?${qs.toString()}`)) as StrategySource | null;
  if (!body || typeof body.source !== "string") throw new Error("Strategy source answered with an unexpected shape");
  return body;
}

export function useDesk(dataset: Dataset) {
  const [reads, setReads] = useState<Reads | null>(null);
  const [stream, setStream] = useState<TapeStreamState>({ mode: "polling", eventCount: 0, lastEventAt: null });
  const seq = useRef(0);
  const sseRefreshTimer = useRef<number | null>(null);

  const load = useCallback(async () => {
    const mine = ++seq.current;
    const q = dataset === "fixture" ? "?dataset=fixture" : "";
    const urls: Record<keyof Reads, string> = {
      home: "/api/home",
      slots: "/api/slots" + q,
      equity: "/api/equity" + q,
      markets: "/api/markets" + q,
      strategies: "/api/strategies" + q,
      report: "/api/report",
    };
    const keys = Object.keys(urls) as (keyof Reads)[];
    const entries = await Promise.all(
      keys.map(async (k) => {
        try {
          const body = await getJson(urls[k]);
          if (!SHAPES[k](body)) throw new Error("answered with an unexpected shape");
          return [k, { ok: true, v: body }] as const;
        } catch (e) {
          return [k, { ok: false, msg: e instanceof Error ? e.message : String(e) }] as const;
        }
      }),
    );
    if (mine === seq.current) setReads(Object.fromEntries(entries) as unknown as Reads);
  }, [dataset]);

  useEffect(() => {
    load();
    const id = setInterval(load, 30000);
    return () => clearInterval(id);
  }, [load]);

  const home = reads?.home.ok ? reads.home.v : null;
  const sseEnabled = dataset === "live" && supportsV050(home, "sse") && typeof EventSource !== "undefined";

  useEffect(() => {
    if (!sseEnabled) {
      setStream({ mode: "polling", eventCount: 0, lastEventAt: null });
      return;
    }

    let closed = false;
    setStream((prev) => ({ ...prev, mode: "connecting" }));
    const es = new EventSource("/api/tape/stream");

    const scheduleReload = () => {
      if (sseRefreshTimer.current !== null) return;
      sseRefreshTimer.current = window.setTimeout(() => {
        sseRefreshTimer.current = null;
        if (!closed) void load();
      }, 400);
    };

    const handleEvent = (ev: MessageEvent<string>) => {
      if (closed) return;
      setStream((prev) => ({
        mode: "sse",
        eventCount: prev.eventCount + 1,
        lastEventAt: Date.now(),
      }));
      try {
        const parsed = JSON.parse(ev.data) as {
          markets?: Market[];
          snapshot?: Market;
          snapshots_written?: number;
        };
        if (Array.isArray(parsed.markets)) {
          setReads((prev) =>
            prev ? { ...prev, markets: { ok: true, v: parsed.markets as Market[] } } : prev,
          );
          return;
        }
        if (parsed.snapshot && typeof parsed.snapshot === "object" && parsed.snapshot.ticker) {
          const snap = parsed.snapshot;
          setReads((prev) => {
            if (!prev || !prev.markets.ok) return prev;
            const rest = prev.markets.v.filter((m) => m?.ticker !== snap.ticker);
            return { ...prev, markets: { ok: true, v: [snap, ...rest] } };
          });
          return;
        }
        if (isNum(parsed.snapshots_written) && parsed.snapshots_written > 0) {
          scheduleReload();
        }
      } catch {
        scheduleReload();
      }
    };

    es.onopen = () => {
      if (!closed) {
        setStream((prev) => ({ ...prev, mode: "sse" }));
      }
    };
    es.onmessage = handleEvent;
    es.addEventListener("tape", handleEvent as EventListener);
    es.addEventListener("snapshot", handleEvent as EventListener);
    es.addEventListener("collect", handleEvent as EventListener);

    es.onerror = () => {
      if (closed) return;
      // Clean fallback to 30s polling if the stream closes or is unavailable.
      es.close();
      closed = true;
      setStream((prev) => ({ ...prev, mode: "polling" }));
    };

    return () => {
      closed = true;
      es.close();
      if (sseRefreshTimer.current !== null) {
        window.clearTimeout(sseRefreshTimer.current);
        sseRefreshTimer.current = null;
      }
    };
  }, [sseEnabled, load]);

  return { reads, load, stream };
}

export const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

export const cleanStr = (v: unknown, fallback = "unnamed"): string => {
  if (typeof v === "string") {
    const t = v.trim();
    return t || fallback;
  }
  if (typeof v === "number" && Number.isFinite(v)) return String(v);
  return fallback;
};

export const money = (v: unknown): string =>
  isNum(v) ? (v < 0 ? "-$" + Math.abs(v).toFixed(2) : "$" + v.toFixed(2)) : "unavailable";

export const moneySigned = (v: unknown): string =>
  isNum(v) ? (v > 0 ? "+$" + v.toFixed(2) : v < 0 ? "-$" + Math.abs(v).toFixed(2) : "$0.00") : "unavailable";

export const moneyWhole = (v: unknown): string =>
  isNum(v) ? (v < 0 ? "-$" + Math.abs(Math.round(v)) : "$" + Math.round(v)) : "unavailable";

export const cents = (v: unknown): string =>
  isNum(v) ? Math.round(v * 100) + "c" : "-";

export const signedCents = (v: unknown): string =>
  isNum(v) ? (v > 0 ? "+" : "") + v.toFixed(2) + "c" : "-";

export function ageWords(age: unknown): string {
  if (!isNum(age)) return "none yet";
  if (age < 5) return "just now";
  if (age < 90) return Math.round(age) + "s ago";
  if (age < 5400) return Math.round(age / 60) + "m ago";
  return (age / 3600).toFixed(1) + "h ago";
}

export const dayLabel = (x: unknown): string =>
  isNum(x) ? new Date(x * 1000).toISOString().slice(0, 10) : "-";

export const timeUtcLabel = (x: unknown): string =>
  isNum(x) ? new Date(x * 1000).toISOString().slice(11, 16) + "Z" : "-";

export const timeUtcFullLabel = (x: unknown): string =>
  isNum(x) ? new Date(x * 1000).toISOString().slice(11, 19) + "Z" : "-";

export function closeClock(
  iso: string | null | undefined,
  nowMs: number,
): { label: string; isOpen: boolean; secsRemaining: number | null } {
  if (typeof iso !== "string" || !iso) return { label: "-", isOpen: false, secsRemaining: null };
  const ms = Date.parse(iso);
  if (!Number.isFinite(ms)) return { label: iso.slice(0, 16), isOpen: false, secsRemaining: null };
  const diffSec = Math.floor((ms - nowMs) / 1000);
  if (diffSec > 0) {
    if (diffSec < 3600) {
      const m = Math.floor(diffSec / 60);
      const s = diffSec % 60;
      return {
        label: `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`,
        isOpen: true,
        secsRemaining: diffSec,
      };
    }
    const h = Math.floor(diffSec / 3600);
    const m = Math.floor((diffSec % 3600) / 60);
    return { label: `${h}h ${String(m).padStart(2, "0")}m`, isOpen: true, secsRemaining: diffSec };
  }
  return { label: iso.slice(5, 16).replace("T", " ") + "Z", isOpen: false, secsRemaining: 0 };
}

const CANONICAL_SERIES = ["BTC", "ETH", "SOL", "XRP", "DOGE"];

export function seriesShort(series: string | null | undefined, ticker?: string | null): string {
  const src = (series || ticker || "").toUpperCase();
  if (src.includes("BTC")) return "BTC";
  if (src.includes("ETH")) return "ETH";
  if (src.includes("SOL")) return "SOL";
  if (src.includes("XRP")) return "XRP";
  if (src.includes("DOGE")) return "DOGE";
  const cleaned = src.replace(/^KX/, "").replace(/15M.*$/, "").replace(/[^A-Z0-9]/g, "");
  return cleaned || "15M";
}

export function deriveSeriesChips(markets: Market[], homeSeries?: string[]): string[] {
  const seen = new Set<string>(["BTC", "ETH", "SOL", "XRP"]);
  if (Array.isArray(homeSeries)) {
    for (const s of homeSeries) {
      if (typeof s === "string" && s.trim()) seen.add(seriesShort(s));
    }
  }
  for (const m of markets) {
    const sym = seriesShort(m.series, m.ticker);
    if (sym && sym !== "15M") seen.add(sym);
  }
  const ordered = [...seen].sort((a, b) => {
    const ia = CANONICAL_SERIES.indexOf(a);
    const ib = CANONICAL_SERIES.indexOf(b);
    if (ia >= 0 && ib >= 0) return ia - ib;
    if (ia >= 0) return -1;
    if (ib >= 0) return 1;
    return a.localeCompare(b);
  });
  return ["All", ...ordered];
}

export function marketIsOpen(m: Market): boolean {
  const r = typeof m.result === "string" ? m.result.trim().toLowerCase() : "";
  if (r === "yes" || r === "no") return false;
  const st = typeof m.status === "string" ? m.status.trim().toLowerCase() : "";
  if (st === "open" || st === "active") return true;
  if (typeof m.close_time === "string") {
    const ms = Date.parse(m.close_time);
    if (Number.isFinite(ms) && ms > Date.now()) return true;
  }
  return false;
}

export function impliedYesProb(m: Market): number | null {
  const bid = isNum(m.yes_bid) ? m.yes_bid : null;
  const ask = isNum(m.yes_ask) ? m.yes_ask : null;
  if (bid !== null && ask !== null && ask >= bid && (bid > 0 || ask < 1)) {
    return Math.min(1, Math.max(0, (bid + ask) / 2));
  }
  if (isNum(m.last_price) && m.last_price >= 0 && m.last_price <= 1) return m.last_price;
  if (ask !== null && ask >= 0 && ask <= 1) return ask;
  if (bid !== null && bid >= 0 && bid <= 1) return bid;
  return null;
}

export function marketSpreadCents(m: Market): number | null {
  if (isNum(m.yes_bid) && isNum(m.yes_ask) && m.yes_ask >= m.yes_bid && (m.yes_bid > 0 || m.yes_ask < 1)) {
    return Math.round((m.yes_ask - m.yes_bid) * 100);
  }
  return null;
}

export type VerdictKind = "proven_forward" | "proven_example" | "rejected" | "testing";

export function classifyVerdict(verdict: unknown): { kind: VerdictKind; label: string; text: string } {
  const raw = typeof verdict === "string" ? verdict.trim() : "";
  if (!raw) return { kind: "testing", label: "Testing", text: "Verdict unavailable" };
  if (/^PROVEN on forward evidence/i.test(raw)) {
    return { kind: "proven_forward", label: "Proven", text: raw };
  }
  if (/^PROVEN on the example tape/i.test(raw)) {
    return { kind: "proven_example", label: "Example pass", text: raw };
  }
  if (/^NOT PROVEN|^KILL|^REJECT/i.test(raw)) {
    return { kind: "rejected", label: "Rejected", text: raw };
  }
  return { kind: "testing", label: "Testing", text: raw };
}

export type StandingInfo = {
  state: "loading" | "error" | "empty" | "ready";
  provenForward: number;
  provenExample: number;
  rejected: number;
  testing: number;
  total: number;
  provenNames: string[];
  commonMinWindows: number | null;
  lines: string[];
};

export function computeStanding(read: Read<Strategy[]> | undefined): StandingInfo {
  if (!read) {
    return { state: "loading", provenForward: 0, provenExample: 0, rejected: 0, testing: 0, total: 0, provenNames: [], commonMinWindows: null, lines: [] };
  }
  if (!read.ok) {
    return { state: "error", provenForward: 0, provenExample: 0, rejected: 0, testing: 0, total: 0, provenNames: [], commonMinWindows: null, lines: [] };
  }
  const rows = read.v.filter((s): s is Strategy => !!s && typeof s === "object");
  if (rows.length === 0) {
    return { state: "empty", provenForward: 0, provenExample: 0, rejected: 0, testing: 0, total: 0, provenNames: [], commonMinWindows: null, lines: [] };
  }
  let provenForward = 0;
  let provenExample = 0;
  let rejected = 0;
  let testing = 0;
  const provenNames: string[] = [];
  const mins = new Set<number>();
  let allHaveMin = true;
  for (const s of rows) {
    const v = classifyVerdict(s.verdict);
    if (v.kind === "proven_forward") {
      provenForward++;
      provenNames.push(cleanStr(s.strategy, "unnamed"));
    } else if (v.kind === "proven_example") {
      provenExample++;
    } else if (v.kind === "rejected") {
      rejected++;
    } else {
      testing++;
    }
    const mw = s.standard && typeof s.standard === "object" ? s.standard.min_windows : null;
    if (isNum(mw) && mw > 0) mins.add(mw);
    else allHaveMin = false;
  }
  const commonMinWindows = allHaveMin && mins.size === 1 ? [...mins][0] : null;
  const lines: string[] = [];
  if (provenForward > 0) {
    lines.push(`${provenForward} proven: ${provenNames.join(", ")}`);
  }
  if (provenExample > 0) {
    lines.push(`${provenExample} passed on the example tape only`);
  }
  if (rejected > 0) {
    lines.push(`${rejected} rejected`);
  }
  if (testing > 0) {
    lines.push(commonMinWindows ? `${testing} below ${commonMinWindows} traded windows` : `${testing} waiting for evidence`);
  }
  return {
    state: "ready",
    provenForward,
    provenExample,
    rejected,
    testing,
    total: rows.length,
    provenNames,
    commonMinWindows,
    lines,
  };
}

function csvCell(v: unknown): string {
  const s = v === null || v === undefined ? "" : String(v);
  return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function buildLocalCsvReport(slots: Slots | null, reportText: string): string {
  const firstLine =
    reportText
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.length > 0) || "OpenBell paper-only daily report";
  const lines: string[] = [
    `# ${firstLine.replace(/[\r\n]+/g, " ")}`,
    "record_type,slot,strategy,status,strikes,windows_traded,price_cents,contracts,pnl_dollars,equity_dollars,close_epoch_utc",
  ];
  if (slots) {
    for (const s of slots.slots) {
      lines.push(
        [
          "active_slot",
          s.slot,
          s.strategy,
          s.status || "active",
          `${s.strikes ?? 0}/3`,
          s.windows_traded,
          "",
          "",
          (isNum(s.stint_pnl_dollars) ? s.stint_pnl_dollars : s.paper_pnl_dollars).toFixed(2),
          isNum(s.equity_dollars) ? s.equity_dollars.toFixed(2) : "",
          "",
        ]
          .map(csvCell)
          .join(","),
      );
    }
    for (const r of slots.retired) {
      lines.push(
        [
          "retired_stint",
          r.slot,
          r.strategy,
          r.rotated_to ? `rotated_to:${r.rotated_to}` : "retired",
          "3/3",
          r.windows_traded,
          "",
          "",
          (isNum(r.paper_pnl_dollars) ? r.paper_pnl_dollars : 0).toFixed(2),
          "",
          "",
        ]
          .map(csvCell)
          .join(","),
      );
    }
    for (const t of slots.trades || []) {
      lines.push(
        [
          "paper_trade",
          t.slot,
          t.strategy,
          "settled",
          "",
          1,
          Math.round((t.price || 0) * 100),
          t.contracts,
          t.pnl_dollars.toFixed(2),
          "",
          new Date(t.close_epoch * 1000).toISOString(),
        ]
          .map(csvCell)
          .join(","),
      );
    }
  }
  return lines.join("\n") + "\n";
}

function triggerFileDownload(filename: string, content: string, mime: string) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export async function exportDailyReport(
  format: "md" | "csv",
  dataset: Dataset,
  home: Home | null,
  reportText: string,
  slots: Slots | null,
): Promise<{ filename: string; source: "server" | "client"; bytes: number }> {
  const day = new Date().toISOString().slice(0, 10);
  const fallbackName = `openbell-daily-report-${day}.${format}`;
  const mime = format === "csv" ? "text/csv;charset=utf-8" : "text/markdown;charset=utf-8";

  if (supportsV050(home, "export")) {
    try {
      const qs = new URLSearchParams({ format });
      if (dataset === "fixture") qs.set("dataset", "fixture");
      const r = await fetch(`/api/export/report?${qs.toString()}`);
      if (r.ok) {
        const ct = (r.headers.get("content-type") || "").toLowerCase();
        let content = "";
        let filename = fallbackName;
        if (ct.includes("application/json")) {
          const j = (await r.json()) as { content?: string; report?: string; filename?: string };
          content = typeof j.content === "string" ? j.content : typeof j.report === "string" ? j.report : "";
          if (typeof j.filename === "string" && j.filename.trim()) filename = j.filename.trim();
        } else {
          content = await r.text();
          const disp = r.headers.get("content-disposition") || "";
          const m = disp.match(/filename="?([^";]+)"?/i);
          if (m && m[1]) filename = m[1];
        }
        if (content.length > 0) {
          triggerFileDownload(filename, content, mime);
          return { filename, source: "server", bytes: content.length };
        }
      }
    } catch {
      /* Fall back cleanly to local export from loaded report/slots */
    }
  }

  const content =
    format === "md"
      ? (reportText || "# OpenBell Paper-Only Daily Report\n\nNo report text loaded yet.\n")
      : buildLocalCsvReport(slots, reportText);
  triggerFileDownload(fallbackName, content, mime);
  return { filename: fallbackName, source: "client", bytes: content.length };
}
