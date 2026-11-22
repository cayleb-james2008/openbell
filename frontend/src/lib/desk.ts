// OpenBell desk API reads, runtime shape/null guards, and quant formatters.
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
  const seq = useRef(0);
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

  return { reads, load };
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

export function closeClock(iso: string | null | undefined, nowMs: number): { label: string; isOpen: boolean } {
  if (typeof iso !== "string" || !iso) return { label: "-", isOpen: false };
  const ms = Date.parse(iso);
  if (!Number.isFinite(ms)) return { label: iso.slice(0, 16), isOpen: false };
  const diffSec = Math.floor((ms - nowMs) / 1000);
  if (diffSec > 0) {
    if (diffSec < 3600) {
      const m = Math.floor(diffSec / 60);
      const s = diffSec % 60;
      return { label: `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`, isOpen: true };
    }
    const h = Math.floor(diffSec / 3600);
    const m = Math.floor((diffSec % 3600) / 60);
    return { label: `${h}h ${String(m).padStart(2, "0")}m`, isOpen: true };
  }
  return { label: iso.slice(5, 16).replace("T", " ") + "Z", isOpen: false };
}

export function seriesShort(series: string | null | undefined, ticker?: string | null): string {
  const src = (series || ticker || "").toUpperCase();
  if (src.includes("BTC")) return "BTC";
  if (src.includes("ETH")) return "ETH";
  if (src.includes("SOL")) return "SOL";
  if (src.includes("XRP")) return "XRP";
  if (src.includes("DOGE")) return "DOGE";
  return src.replace(/^KX/, "").replace(/15M.*$/, "") || "15M";
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
