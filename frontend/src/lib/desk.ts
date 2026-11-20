// The desk's reads. Six independent GETs, each shape-checked; a failed read shows its own error and the rest stay current.
import { useCallback, useEffect, useRef, useState } from "react";

export type Dataset = "live" | "fixture";

export type Collector = {
  collecting: boolean;
  snapshots_on_disk: number;
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
export type Account = { balance: number | null; day_pnl: number | null; open_positions: unknown[] | null; note: string };
export type Home = {
  account: Account;
  collector: Collector;
  library_errors?: { file: string; error: string }[];
  stage_target?: { target_dollars: number };
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
export type Slots = {
  paper_equity_dollars: number;
  paper_pot_start: number;
  slots: Slot[];
  retired: Slot[];
  stage_target?: { target_dollars: number };
  notes?: string[];
};
export type Equity = { paper_pot_start: number; pot: [number, number][]; series: { strategy: string; points: [number, number][] }[] };
export type Market = {
  ticker: string;
  close_time: string;
  yes_bid: number | null;
  yes_ask: number | null;
  no_bid: number | null;
  no_ask: number | null;
  last_price: number | null;
  result: string | null;
};
export type Strategy = {
  strategy: string;
  side: string;
  windows_traded: number;
  standard?: { min_windows: number };
  mean_cents_per_contract: number | null;
  verdict: string;
};
export type Report = { report: string };

export type Read<T> = { ok: true; v: T } | { ok: false; msg: string };
export type Reads = { home: Read<Home>; slots: Read<Slots>; equity: Read<Equity>; markets: Read<Market[]>; strategies: Read<Strategy[]>; report: Read<Report> };

const SHAPES: Record<keyof Reads, (x: unknown) => boolean> = {
  home: (x) => {
    const h = x as Home | null;
    return !!h && typeof h.account === "object" && h.account !== null && typeof h.collector === "object" && h.collector !== null
      && typeof h.collector.collecting === "boolean" && typeof h.collector.snapshots_on_disk === "number";
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
      markets: "/api/markets",
      strategies: "/api/strategies" + q,
      report: "/api/report",
    };
    const keys = Object.keys(urls) as (keyof Reads)[];
    const entries = await Promise.all(keys.map(async (k) => {
      try {
        const body = await getJson(urls[k]);
        if (!SHAPES[k](body)) throw new Error("answered with an unexpected shape");
        return [k, { ok: true, v: body }] as const;
      } catch (e) {
        return [k, { ok: false, msg: e instanceof Error ? e.message : String(e) }] as const;
      }
    }));
    if (mine === seq.current) setReads(Object.fromEntries(entries) as unknown as Reads);
  }, [dataset]);
  useEffect(() => {
    load();
    const id = setInterval(load, 30000);
    return () => clearInterval(id);
  }, [load]);
  return { reads, load };
}

export const money = (v: number) => (v < 0 ? "-$" + Math.abs(v).toFixed(2) : "$" + v.toFixed(2));
export const moneyWhole = (v: number) => (v < 0 ? "-$" + Math.abs(Math.round(v)) : "$" + Math.round(v));
export function ageWords(age: number | null | undefined): string {
  if (age === null || age === undefined) return "none yet";
  return age < 90 ? age + "s ago" : age < 5400 ? Math.round(age / 60) + "m ago" : (age / 3600).toFixed(1) + "h ago";
}
export const dayLabel = (x: number) => new Date(x * 1000).toISOString().slice(0, 10);
