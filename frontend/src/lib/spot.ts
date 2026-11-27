// OpenBell Spot Oracle client, SSE `event: spot` stream merger, history reader, and strike divergence calculator.
// Aligned with Zone A endpoints:
//   GET /api/spot/status
//   GET /api/spot/history?asset=<ASSET>&limit=<LIMIT>
//   SSE /api/tape/stream (`event: spot`)
import { useCallback, useEffect, useRef, useState } from "react";
import { type Market, impliedYesProb, isNum, subscribeSpotSseFrame } from "@/lib/desk";

export const CANONICAL_SPOT_ASSETS = ["BTC", "ETH", "SOL", "XRP", "DOGE"] as const;

export type SpotAssetStatus = {
  asset: string;
  price: number | null;
  delta_30s_bps: number | null;
  vol_bps: number | null;
  updated_ms: number | null;
  source: string;
  stale: boolean;
};

export type SpotStatusResponse = {
  assets: SpotAssetStatus[];
  offline: boolean;
};

export type SpotHistoryTick = {
  ts_ms: number;
  price: number;
  source: string;
};

export type SpotHistoryResponse = {
  ok: boolean;
  asset: string;
  ticks: SpotHistoryTick[];
};

export type SpotKalshiDivergence = {
  spotPrice: number | null;
  floorStrike: number | null;
  distanceBps: number | null;
  spotImpliedProb: number | null;
  kalshiImpliedProb: number | null;
  divergenceCents: number | null;
  label: string;
  tone: "success" | "warning" | "info" | "neutral";
};

function cleanPositive(v: unknown): number | null {
  return isNum(v) && v > 0 ? v : null;
}

function cleanSigned(v: unknown): number | null {
  return isNum(v) ? v : null;
}

function cleanAssetSymbol(raw: unknown): string {
  if (typeof raw !== "string") return "";
  const cleaned = raw.trim().toUpperCase().replace(/[^A-Z0-9]/g, "");
  return cleaned;
}

function offlineAsset(asset: string): SpotAssetStatus {
  return {
    asset,
    price: null,
    delta_30s_bps: null,
    vol_bps: null,
    updated_ms: null,
    source: "offline",
    stale: true,
  };
}

export function defaultOfflineSpotStatus(): SpotStatusResponse {
  return {
    assets: CANONICAL_SPOT_ASSETS.map((asset) => offlineAsset(asset)),
    offline: true,
  };
}

export function normalizeSpotAsset(raw: unknown): SpotAssetStatus | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const asset = cleanAssetSymbol(o.asset ?? o.symbol);
  if (!asset) return null;
  const price = cleanPositive(o.price);
  const delta_30s_bps = cleanSigned(o.delta_30s_bps);
  const volRaw = cleanSigned(o.vol_bps);
  const vol_bps = volRaw !== null && volRaw >= 0 ? volRaw : null;
  const updatedRaw = cleanPositive(o.updated_ms);
  const updated_ms =
    updatedRaw !== null && updatedRaw <= 8_640_000_000_000_000 ? updatedRaw : null;
  const source =
    typeof o.source === "string" && o.source.trim() ? o.source.trim() : price === null ? "offline" : "spot";
  const stale = typeof o.stale === "boolean" ? o.stale : price === null;
  return {
    asset,
    price,
    delta_30s_bps,
    vol_bps,
    updated_ms,
    source,
    stale,
  };
}

function orderSpotAssets(byAsset: Map<string, SpotAssetStatus>): SpotAssetStatus[] {
  const ordered: SpotAssetStatus[] = [];
  for (const sym of CANONICAL_SPOT_ASSETS) {
    ordered.push(byAsset.get(sym) ?? offlineAsset(sym));
  }
  const extras = [...byAsset.keys()]
    .filter((k) => !(CANONICAL_SPOT_ASSETS as readonly string[]).includes(k))
    .sort();
  for (const sym of extras) {
    const item = byAsset.get(sym);
    if (item) ordered.push(item);
  }
  return ordered;
}

export function normalizeSpotStatus(raw: unknown): SpotStatusResponse {
  if (!raw || typeof raw !== "object") return defaultOfflineSpotStatus();
  const o = raw as Record<string, unknown>;
  const rawAssets = Array.isArray(o.assets) ? o.assets : [];
  const byAsset = new Map<string, SpotAssetStatus>();
  for (const item of rawAssets) {
    const norm = normalizeSpotAsset(item);
    if (norm) byAsset.set(norm.asset, norm);
  }
  const assets = orderSpotAssets(byAsset);
  const hasAnyPrice = assets.some((a) => a.price !== null);
  const offline = typeof o.offline === "boolean" ? o.offline : !hasAnyPrice;
  return { assets, offline };
}

export function mergeSpotAssetUpdate(
  prev: SpotStatusResponse,
  update: SpotStatusResponse | SpotAssetStatus,
): SpotStatusResponse {
  if ("assets" in update) {
    return update;
  }
  const byAsset = new Map<string, SpotAssetStatus>();
  for (const a of prev.assets) {
    byAsset.set(a.asset, a);
  }
  byAsset.set(update.asset, update);
  const assets = orderSpotAssets(byAsset);
  const hasLive = assets.some((a) => a.price !== null && !a.stale);
  return {
    assets,
    offline: !hasLive && !assets.some((a) => a.price !== null),
  };
}

export async function fetchSpotStatus(signal?: AbortSignal): Promise<SpotStatusResponse> {
  const r = await fetch("/api/spot/status", { signal });
  if (!r.ok) {
    throw new Error(`Spot status answered ${r.status}`);
  }
  const body = (await r.json()) as unknown;
  return normalizeSpotStatus(body);
}

export async function fetchSpotHistory(
  asset: string,
  limit = 200,
  signal?: AbortSignal,
): Promise<SpotHistoryResponse> {
  const cleanAsset = cleanAssetSymbol(asset) || "BTC";
  const clampedLimit = Math.max(1, Math.min(500, Math.round(limit)));
  const qs = new URLSearchParams({ asset: cleanAsset, limit: String(clampedLimit) });
  const r = await fetch(`/api/spot/history?${qs.toString()}`, { signal });
  if (!r.ok) {
    throw new Error(`Spot history answered ${r.status}`);
  }
  const body = (await r.json()) as Record<string, unknown> | null;
  const rawTicks = Array.isArray(body?.ticks) ? body.ticks : [];
  const ticks: SpotHistoryTick[] = [];
  for (const item of rawTicks) {
    if (!item || typeof item !== "object") continue;
    const t = item as Record<string, unknown>;
    const ts_ms = cleanPositive(t.ts_ms);
    const price = cleanPositive(t.price);
    if (ts_ms === null || price === null) continue;
    const source = typeof t.source === "string" && t.source.trim() ? t.source.trim() : "spot";
    ticks.push({ ts_ms, price, source });
  }
  ticks.sort((a, b) => a.ts_ms - b.ts_ms);
  return {
    ok: Boolean(body?.ok ?? true),
    asset: cleanAssetSymbol(body?.asset) || cleanAsset,
    ticks,
  };
}

export function computeSpotDistanceBps(
  spotPrice: number | null | undefined,
  floorStrike: number | null | undefined,
): number | null {
  const spot = cleanPositive(spotPrice);
  const strike = cleanPositive(floorStrike);
  if (spot === null || strike === null) return null;
  const dist = ((spot - strike) / strike) * 10_000;
  return Number.isFinite(dist) ? dist : null;
}

export function estimateSpotImpliedProb(
  distanceBps: number | null,
  volBps?: number | null,
): number | null {
  if (!isNum(distanceBps)) return null;
  const scale = isNum(volBps) && volBps >= 8 ? volBps : 25;
  const z = Math.max(-8, Math.min(8, distanceBps / scale));
  const prob = 1 / (1 + Math.exp(-1.7 * z));
  if (!Number.isFinite(prob)) return null;
  return Math.min(0.99, Math.max(0.01, prob));
}

export function computeSpotKalshiDivergence(
  spotPrice: number | null | undefined,
  market: Market | null | undefined,
  volBps?: number | null,
): SpotKalshiDivergence {
  const spot = cleanPositive(spotPrice) ?? cleanPositive(market?.spot_price);
  const strike = cleanPositive(market?.floor_strike);
  const distanceBps = computeSpotDistanceBps(spot, strike);
  const askProb =
    market && isNum(market.yes_ask) && market.yes_ask >= 0 && market.yes_ask <= 1
      ? market.yes_ask
      : null;
  const kalshiImpliedProb = askProb ?? (market ? impliedYesProb(market) : null);

  if (distanceBps === null || kalshiImpliedProb === null) {
    return {
      spotPrice: spot,
      floorStrike: strike,
      distanceBps,
      spotImpliedProb: null,
      kalshiImpliedProb,
      divergenceCents: null,
      label: "N/A",
      tone: "neutral",
    };
  }

  const spotImpliedProb = estimateSpotImpliedProb(distanceBps, volBps);
  if (spotImpliedProb === null) {
    return {
      spotPrice: spot,
      floorStrike: strike,
      distanceBps,
      spotImpliedProb: null,
      kalshiImpliedProb,
      divergenceCents: null,
      label: "N/A",
      tone: "neutral",
    };
  }

  const divergenceCents = (spotImpliedProb - kalshiImpliedProb) * 100;
  if (!Number.isFinite(divergenceCents)) {
    return {
      spotPrice: spot,
      floorStrike: strike,
      distanceBps,
      spotImpliedProb: null,
      kalshiImpliedProb,
      divergenceCents: null,
      label: "N/A",
      tone: "neutral",
    };
  }

  const sign = divergenceCents > 0 ? "+" : "";
  const tone: SpotKalshiDivergence["tone"] =
    Math.abs(divergenceCents) >= 8
      ? divergenceCents > 0
        ? "success"
        : "warning"
      : "info";

  return {
    spotPrice: spot,
    floorStrike: strike,
    distanceBps,
    spotImpliedProb,
    kalshiImpliedProb,
    divergenceCents,
    label: `${sign}${divergenceCents.toFixed(1)}c`,
    tone,
  };
}

export function formatSpotUsd(price: number | null | undefined): string {
  const v = cleanPositive(price);
  if (v === null) return "N/A";
  if (v >= 1000) {
    return "$" + v.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  }
  if (v >= 10) return "$" + v.toFixed(2);
  if (v >= 1) return "$" + v.toFixed(3);
  return "$" + v.toFixed(4);
}

export function formatSpotUsdCompact(price: number | null | undefined): string {
  const v = cleanPositive(price);
  if (v === null) return "N/A";
  if (v >= 1000) return "$" + (v / 1000).toFixed(1) + "k";
  if (v >= 10) return "$" + v.toFixed(1);
  if (v >= 1) return "$" + v.toFixed(2);
  return "$" + v.toFixed(3);
}

export function formatBps(bps: number | null | undefined): string {
  if (!isNum(bps)) return "N/A";
  const rounded = Math.abs(bps) < 0.05 ? 0 : bps;
  const sign = rounded > 0 ? "+" : "";
  return `${sign}${rounded.toFixed(1)} bps`;
}

export function parseSpotSsePayload(rawData: string): SpotStatusResponse | SpotAssetStatus | null {
  try {
    const parsed = JSON.parse(rawData) as Record<string, unknown>;
    if (Array.isArray(parsed.assets)) {
      return normalizeSpotStatus(parsed);
    }
    if (parsed.spot && typeof parsed.spot === "object") {
      const inner = parsed.spot as Record<string, unknown>;
      if (Array.isArray(inner.assets)) {
        return normalizeSpotStatus(inner);
      }
      const singleInner = normalizeSpotAsset(inner);
      if (singleInner) return singleInner;
    }
    const payload =
      parsed.status && typeof parsed.status === "object" ? parsed.status : parsed;
    return normalizeSpotAsset(payload);
  } catch {
    return null;
  }
}

export function subscribeSpotStream(
  enabled: boolean,
  onUpdate: (update: SpotStatusResponse | SpotAssetStatus) => void,
): () => void {
  if (!enabled) {
    return () => {};
  }
  return subscribeSpotSseFrame((rawData) => {
    const parsed = parseSpotSsePayload(rawData);
    if (parsed) onUpdate(parsed);
  });
}

export function useSpotStatus(
  options: { enabled?: boolean; pollMs?: number; sseEnabled?: boolean } = {},
) {
  const { enabled = true, pollMs = 5000, sseEnabled = true } = options;
  const [spotStatus, setSpotStatus] = useState<SpotStatusResponse>(() =>
    defaultOfflineSpotStatus(),
  );
  const [loading, setLoading] = useState(false);
  const seqRef = useRef(0);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    const reqId = ++seqRef.current;
    try {
      const st = await fetchSpotStatus();
      if (reqId !== seqRef.current) return;
      setSpotStatus(st);
    } catch {
      if (reqId !== seqRef.current) return;
      setSpotStatus(defaultOfflineSpotStatus());
    } finally {
      if (reqId === seqRef.current) setLoading(false);
    }
  }, [enabled]);

  useEffect(() => {
    if (!enabled) {
      setSpotStatus(defaultOfflineSpotStatus());
      setLoading(false);
      return;
    }
    let alive = true;
    setLoading(true);
    void refresh();

    const pollId = window.setInterval(() => {
      if (alive) void refresh();
    }, Math.max(1500, pollMs));

    return () => {
      alive = false;
      window.clearInterval(pollId);
    };
  }, [enabled, pollMs, refresh]);

  useEffect(() => {
    if (!enabled || !sseEnabled) return;
    return subscribeSpotStream(true, (update) => {
      setSpotStatus((prev) => mergeSpotAssetUpdate(prev, update));
    });
  }, [enabled, sseEnabled]);

  return {
    spotStatus,
    loading,
    refresh,
  };
}
