// OpenBell v0.8.0 Live Kalshi v2 Connected Account, Order Routing & Hard Risk Rails client.
// Aligned with loopback-only /api/live/* routes and X-OpenBell-CSRF: 1 + Content-Type: application/json guard.
// NEVER logs, stores in localStorage, or echoes private key PEM bytes.
import { type Home, cleanStr, isNum, supportsV080 } from "@/lib/desk";
import { kalshiFeeDollars } from "@/lib/fees";

export type LiveEnvironment = "demo" | "prod";

export type RiskRailsConfig = {
  maxOrderContracts: number;
  maxOrderDollars: number;
  maxDailyLossDollars: number;
  maxOpenExposureDollars: number;
  allowedTickerPrefix: string;
};

export type LiveStatus = {
  configured: boolean;
  environment: LiveEnvironment;
  apiBaseUrl: string;
  keyIdMasked: string | null;
  keyPathConfigured: boolean;
  connected: boolean;
  mode: "paper" | "live";
  liveArmed: boolean;
  killed: boolean;
  killReason: string | null;
  rails: RiskRailsConfig;
  dailyRealizedLossDollars: number;
  openExposureDollars: number;
  ordersPlacedToday: number;
  lastError: string | null;
  source: "server" | "client";
};

export type LiveAccount = {
  configured: boolean;
  connected: boolean;
  balanceDollars: number | null;
  portfolioValueDollars: number | null;
  dailyPnlDollars: number | null;
  note: string;
};

export type LivePosition = {
  ticker: string;
  side: "yes" | "no";
  position: number;
  marketExposureDollars: number;
  realizedPnlDollars: number;
  restingOrdersCount: number;
  totalTradedDollars: number;
};

export type LiveOrder = {
  orderId: string;
  clientOrderId: string;
  ticker: string;
  action: "buy" | "sell";
  side: "yes" | "no";
  status: string;
  count: number;
  remainingCount: number;
  priceCents: number;
  createdTime: string;
};

export type LiveFill = {
  fillId: string;
  orderId: string;
  ticker: string;
  action: "buy" | "sell";
  side: "yes" | "no";
  count: number;
  priceCents: number;
  feeDollars: number;
  createdTime: string;
};

export type LiveDeskBundle = {
  status: LiveStatus;
  account: LiveAccount;
  positions: LivePosition[];
  orders: LiveOrder[];
  fills: LiveFill[];
};

export type LiveConfigPayload = {
  api_key_id?: string;
  private_key_path?: string;
  environment?: LiveEnvironment;
  rails?: {
    max_order_contracts: number;
    max_order_dollars: number;
    max_daily_loss_dollars: number;
    max_open_exposure_dollars: number;
    allowed_ticker_prefix: string;
  };
};

export type LiveOrderPayload = {
  ticker: string;
  action: "buy" | "sell";
  side: "yes" | "no";
  count: number;
  price_cents: number;
  post_only?: boolean;
  dry_run?: boolean;
  client_order_id?: string;
};

export type LiveOrderPreflightResult = {
  allowed: boolean;
  dryRun: boolean;
  ticker: string;
  side: "yes" | "no";
  count: number;
  priceCents: number;
  postOnly: boolean;
  orderNotionalDollars: number;
  projectedExposureDollars: number;
  estFeeDollars: number;
  railViolations: string[];
  source: "server" | "client";
};

export type RailCheckItem = {
  id: string;
  label: string;
  passed: boolean;
  detail: string;
};

export type RailEvaluation = {
  allPassed: boolean;
  orderNotionalDollars: number;
  estFeeDollars: number;
  checks: RailCheckItem[];
};

export const DEFAULT_RAILS: RiskRailsConfig = {
  maxOrderContracts: 25,
  maxOrderDollars: 25,
  maxDailyLossDollars: 50,
  maxOpenExposureDollars: 100,
  allowedTickerPrefix: "KX",
};

const MUTATION_HEADERS: HeadersInit = {
  "Content-Type": "application/json",
  "X-OpenBell-CSRF": "1",
};

function pickNum(o: Record<string, unknown>, keys: string[]): number | null {
  for (const k of keys) {
    if (isNum(o[k])) return o[k] as number;
  }
  return null;
}

function centsOrDollars(o: Record<string, unknown>, dollarKeys: string[], centKeys: string[]): number | null {
  const d = pickNum(o, dollarKeys);
  if (d !== null) return d;
  const c = pickNum(o, centKeys);
  if (c !== null) return Number((c / 100).toFixed(2));
  return null;
}

export function defaultLiveStatus(source: "server" | "client" = "client"): LiveStatus {
  return {
    configured: false,
    environment: "demo",
    apiBaseUrl: "https://demo-api.kalshi.co/trade-api/v2",
    keyIdMasked: null,
    keyPathConfigured: false,
    connected: false,
    mode: "paper",
    liveArmed: false,
    killed: false,
    killReason: null,
    rails: { ...DEFAULT_RAILS },
    dailyRealizedLossDollars: 0,
    openExposureDollars: 0,
    ordersPlacedToday: 0,
    lastError: null,
    source,
  };
}

export function normalizeLiveStatus(raw: unknown, source: "server" | "client" = "server"): LiveStatus {
  if (!raw || typeof raw !== "object") return defaultLiveStatus(source);
  const o = raw as Record<string, unknown>;
  const rObj = (o.rails && typeof o.rails === "object" ? o.rails : {}) as Record<string, unknown>;
  const envRaw = String(o.environment ?? "demo").toLowerCase();
  const environment: LiveEnvironment = envRaw === "prod" || envRaw === "production" ? "prod" : "demo";
  const liveArmed = Boolean(o.live_armed);
  const modeRaw = String(o.mode ?? (liveArmed ? "live" : "paper")).toLowerCase();
  return {
    configured: Boolean(o.configured),
    environment,
    apiBaseUrl:
      typeof o.api_base_url === "string" && o.api_base_url
        ? o.api_base_url
        : environment === "prod"
          ? "https://api.elections.kalshi.com/trade-api/v2"
          : "https://demo-api.kalshi.co/trade-api/v2",
    keyIdMasked: typeof o.key_id_masked === "string" && o.key_id_masked ? o.key_id_masked : null,
    keyPathConfigured: Boolean(o.key_path_configured ?? o.private_key_configured ?? o.configured),
    connected: Boolean(o.connected),
    mode: modeRaw === "live" || liveArmed ? "live" : "paper",
    liveArmed,
    killed: Boolean(o.killed),
    killReason: typeof o.kill_reason === "string" && o.kill_reason ? o.kill_reason : null,
    rails: {
      maxOrderContracts: pickNum(rObj, ["max_order_contracts"]) ?? DEFAULT_RAILS.maxOrderContracts,
      maxOrderDollars: pickNum(rObj, ["max_order_dollars"]) ?? DEFAULT_RAILS.maxOrderDollars,
      maxDailyLossDollars: pickNum(rObj, ["max_daily_loss_dollars"]) ?? DEFAULT_RAILS.maxDailyLossDollars,
      maxOpenExposureDollars: pickNum(rObj, ["max_open_exposure_dollars"]) ?? DEFAULT_RAILS.maxOpenExposureDollars,
      allowedTickerPrefix:
        typeof rObj.allowed_ticker_prefix === "string" ? rObj.allowed_ticker_prefix : DEFAULT_RAILS.allowedTickerPrefix,
    },
    dailyRealizedLossDollars: pickNum(o, ["daily_realized_loss_dollars", "daily_loss_dollars"]) ?? 0,
    openExposureDollars: pickNum(o, ["open_exposure_dollars"]) ?? 0,
    ordersPlacedToday: Math.max(0, Math.round(pickNum(o, ["orders_placed_today"]) ?? 0)),
    lastError: typeof o.last_error === "string" && o.last_error ? o.last_error : null,
    source,
  };
}

function normalizeLiveAccount(raw: unknown): LiveAccount {
  if (!raw || typeof raw !== "object") {
    return {
      configured: false,
      connected: false,
      balanceDollars: null,
      portfolioValueDollars: null,
      dailyPnlDollars: null,
      note: "Configure Kalshi v2 API credentials in Desk Settings to connect your account.",
    };
  }
  const o = raw as Record<string, unknown>;
  const inner = (o.account && typeof o.account === "object" ? o.account : o) as Record<string, unknown>;
  const balanceDollars = centsOrDollars(
    inner,
    ["balance_dollars", "balance_usd", "available_balance_dollars"],
    ["balance_cents", "balance"],
  );
  // If `balance` came from a Kalshi v2 cents integer (e.g., 125000) vs small dollars, check explicit balance_dollars first above.
  const portfolioValueDollars = centsOrDollars(
    inner,
    ["portfolio_value_dollars", "portfolio_value_usd"],
    ["portfolio_value_cents", "portfolio_value"],
  );
  const dailyPnlDollars = centsOrDollars(
    inner,
    ["daily_pnl_dollars", "day_pnl_dollars", "day_pnl"],
    ["daily_pnl_cents"],
  );
  return {
    configured: Boolean(o.configured ?? inner.configured ?? balanceDollars !== null),
    connected: Boolean(o.connected ?? inner.connected ?? balanceDollars !== null),
    balanceDollars,
    portfolioValueDollars,
    dailyPnlDollars,
    note:
      typeof o.note === "string" && o.note
        ? o.note
        : typeof inner.note === "string" && inner.note
          ? inner.note
          : balanceDollars !== null
            ? "Connected to Kalshi v2 REST portfolio."
            : "Configure Kalshi v2 API credentials in Desk Settings to connect.",
  };
}

function normalizePositions(raw: unknown): LivePosition[] {
  const arr = Array.isArray(raw)
    ? raw
    : raw && typeof raw === "object"
      ? ((raw as Record<string, unknown>).market_positions ??
          (raw as Record<string, unknown>).positions ??
          [])
      : [];
  if (!Array.isArray(arr)) return [];
  return arr
    .filter((x): x is Record<string, unknown> => !!x && typeof x === "object")
    .map((x) => {
      const qty = pickNum(x, ["position", "quantity", "contracts", "count"]) ?? 0;
      const sideRaw = String(x.side ?? (qty < 0 ? "no" : "yes")).toLowerCase();
      return {
        ticker: cleanStr(x.ticker ?? x.market_ticker, "unknown"),
        side: sideRaw === "no" ? "no" : "yes",
        position: Math.abs(Math.round(qty)),
        marketExposureDollars:
          centsOrDollars(x, ["market_exposure_dollars", "exposure_dollars"], ["market_exposure", "market_exposure_cents"]) ?? 0,
        realizedPnlDollars:
          centsOrDollars(x, ["realized_pnl_dollars"], ["realized_pnl", "realized_pnl_cents"]) ?? 0,
        restingOrdersCount: Math.max(0, Math.round(pickNum(x, ["resting_orders_count"]) ?? 0)),
        totalTradedDollars:
          centsOrDollars(x, ["total_traded_dollars"], ["total_traded", "total_traded_cents"]) ?? 0,
      };
    });
}

function normalizeOrders(raw: unknown): LiveOrder[] {
  const arr = Array.isArray(raw)
    ? raw
    : raw && typeof raw === "object"
      ? ((raw as Record<string, unknown>).orders ?? [])
      : [];
  if (!Array.isArray(arr)) return [];
  return arr
    .filter((x): x is Record<string, unknown> => !!x && typeof x === "object")
    .map((x) => {
      const sideRaw = String(x.side ?? "yes").toLowerCase();
      const side: "yes" | "no" = sideRaw === "no" ? "no" : "yes";
      const actionRaw = String(x.action ?? "buy").toLowerCase();
      const action: "buy" | "sell" = actionRaw === "sell" ? "sell" : "buy";
      const priceCents =
        pickNum(x, ["price_cents", side === "yes" ? "yes_price" : "no_price", "yes_price", "no_price"]) ?? 50;
      const count = Math.max(0, Math.round(pickNum(x, ["count", "initial_count", "contracts"]) ?? 0));
      const remainingCount = Math.max(0, Math.round(pickNum(x, ["remaining_count", "remaining"]) ?? count));
      return {
        orderId: cleanStr(x.order_id ?? x.id, ""),
        clientOrderId: cleanStr(x.client_order_id, ""),
        ticker: cleanStr(x.ticker ?? x.market_ticker, "unknown"),
        action,
        side,
        status: cleanStr(x.status, "resting"),
        count,
        remainingCount,
        priceCents: Math.round(priceCents),
        createdTime: typeof x.created_time === "string" ? x.created_time : "",
      };
    })
    .filter((x) => x.orderId.length > 0);
}

function normalizeFills(raw: unknown): LiveFill[] {
  const arr = Array.isArray(raw)
    ? raw
    : raw && typeof raw === "object"
      ? ((raw as Record<string, unknown>).fills ?? [])
      : [];
  if (!Array.isArray(arr)) return [];
  return arr
    .filter((x): x is Record<string, unknown> => !!x && typeof x === "object")
    .map((x, idx) => {
      const sideRaw = String(x.side ?? "yes").toLowerCase();
      const side: "yes" | "no" = sideRaw === "no" ? "no" : "yes";
      const actionRaw = String(x.action ?? "buy").toLowerCase();
      const priceCents =
        pickNum(x, ["price_cents", side === "yes" ? "yes_price" : "no_price", "yes_price", "no_price"]) ?? 50;
      return {
        fillId: cleanStr(x.trade_id ?? x.fill_id ?? x.id, `fill-${idx}`),
        orderId: cleanStr(x.order_id, ""),
        ticker: cleanStr(x.ticker ?? x.market_ticker, "unknown"),
        action: actionRaw === "sell" ? "sell" : "buy",
        side,
        count: Math.max(0, Math.round(pickNum(x, ["count", "contracts"]) ?? 0)),
        priceCents: Math.round(priceCents),
        feeDollars: centsOrDollars(x, ["fee_dollars", "fee_cost_dollars"], ["fee_cents"]) ?? 0,
        createdTime: typeof x.created_time === "string" ? x.created_time : "",
      };
    });
}

// Real-time fail-closed Risk Rails pre-check matching src/risk_rails.rs
export function evaluateOrderRails(
  status: LiveStatus,
  ticker: string,
  count: number,
  priceCents: number,
  action: "buy" | "sell" = "buy",
  postOnly = false,
): RailEvaluation {
  const cleanTicker = ticker.trim().toUpperCase();
  const c = Number.isFinite(count) ? Math.floor(count) : 0;
  const p = Number.isFinite(priceCents) ? Math.round(priceCents) : 0;
  const wholeCentValid = Number.isInteger(priceCents) && p >= 1 && p <= 99;
  const orderNotionalDollars = c > 0 && p >= 1 && p <= 99 ? Number(((c * p) / 100).toFixed(2)) : 0;
  const estFeeDollars =
    c > 0 && p >= 1 && p <= 99 ? Number(kalshiFeeDollars(c, p / 100, postOnly).toFixed(2)) : 0;

  const r = status.rails;
  const prefixes = r.allowedTickerPrefix
    .split(",")
    .map((s) => s.trim().toUpperCase())
    .filter((s) => s.length > 0);
  const matchedPrefix = prefixes.find((pref) => cleanTicker.startsWith(pref));
  const tickerCharsOk = cleanTicker.length > 0 && /^[A-Z0-9_-]+$/.test(cleanTicker);
  const prefixOk = tickerCharsOk && (prefixes.length === 0 || Boolean(matchedPrefix));
  const contractsOk = c >= 1 && c <= r.maxOrderContracts;
  const orderDollarsOk = orderNotionalDollars > 0 && orderNotionalDollars <= r.maxOrderDollars + 1e-9;
  const isSell = action === "sell";
  const exposureAfter = Number(
    (isSell
      ? Math.max(0, status.openExposureDollars - orderNotionalDollars)
      : status.openExposureDollars + orderNotionalDollars
    ).toFixed(2),
  );
  const exposureOk = isSell || exposureAfter <= r.maxOpenExposureDollars + 1e-9;
  const dailyLossOk = status.dailyRealizedLossDollars < r.maxDailyLossDollars - 1e-9;
  const armedOk = status.configured && status.liveArmed && !status.killed;

  const checks: RailCheckItem[] = [
    {
      id: "armed",
      label: "Live Arm State",
      passed: armedOk,
      detail: !status.configured
        ? "API credentials not configured"
        : status.killed
          ? "Kill switch active — re-arm required"
          : status.liveArmed
            ? `Armed (${status.environment.toUpperCase()})`
            : "Disarmed (Paper Mode)",
    },
    {
      id: "prefix",
      label: "Ticker Prefix",
      passed: prefixOk,
      detail: !cleanTicker
        ? "Select or enter a ticker"
        : !tickerCharsOk
          ? "Ticker contains invalid characters"
          : prefixOk
            ? `Matches ${matchedPrefix ?? "*"}`
            : `Must start with ${prefixes.join(", ")}`,
    },
    {
      id: "price",
      label: "Whole-Cent Limit",
      passed: wholeCentValid,
      detail: wholeCentValid ? `${p}c (1..99c)` : "Limit must be 1..99c integer",
    },
    {
      id: "contracts",
      label: "Max Contracts",
      passed: contractsOk,
      detail: `${c} / ${r.maxOrderContracts} cts`,
    },
    {
      id: "order_dollars",
      label: "Max Order $",
      passed: orderDollarsOk,
      detail: `$${orderNotionalDollars.toFixed(2)} / $${r.maxOrderDollars.toFixed(2)}`,
    },
    {
      id: "open_exposure",
      label: "Open Exposure",
      passed: exposureOk,
      detail: isSell
        ? `$${exposureAfter.toFixed(2)} / $${r.maxOpenExposureDollars.toFixed(2)} (reduces)`
        : `$${exposureAfter.toFixed(2)} / $${r.maxOpenExposureDollars.toFixed(2)}`,
    },
    {
      id: "daily_loss",
      label: "Daily Loss Rail",
      passed: dailyLossOk,
      detail: `$${status.dailyRealizedLossDollars.toFixed(2)} / $${r.maxDailyLossDollars.toFixed(2)}`,
    },
  ];

  return {
    allPassed: checks.every((item) => item.passed),
    orderNotionalDollars,
    estFeeDollars,
    checks,
  };
}

export async function fetchLiveBundle(home: Home | null): Promise<LiveDeskBundle> {
  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    return {
      status: defaultLiveStatus("client"),
      account: normalizeLiveAccount(null),
      positions: [],
      orders: [],
      fills: [],
    };
  }
  try {
    const [stRes, accRes, posRes, ordRes, fillRes] = await Promise.all([
      fetch("/api/live/status").then((r) => (r.ok ? r.json() : null)).catch(() => null),
      fetch("/api/live/account").then((r) => (r.ok ? r.json() : null)).catch(() => null),
      fetch("/api/live/positions").then((r) => (r.ok ? r.json() : null)).catch(() => null),
      fetch("/api/live/orders").then((r) => (r.ok ? r.json() : null)).catch(() => null),
      fetch("/api/live/fills").then((r) => (r.ok ? r.json() : null)).catch(() => null),
    ]);
    return {
      status: stRes ? normalizeLiveStatus(stRes, "server") : defaultLiveStatus("client"),
      account: normalizeLiveAccount(accRes),
      positions: normalizePositions(posRes),
      orders: normalizeOrders(ordRes),
      fills: normalizeFills(fillRes),
    };
  } catch {
    return {
      status: defaultLiveStatus("client"),
      account: normalizeLiveAccount(null),
      positions: [],
      orders: [],
      fills: [],
    };
  }
}

async function parseErrorMessage(r: Response, fallback: string): Promise<string> {
  const body = (await r.json().catch(() => ({}))) as {
    error?: string;
    rail_violations?: string[];
  };
  if (Array.isArray(body.rail_violations) && body.rail_violations.length > 0) {
    return body.rail_violations.join("; ");
  }
  return body.error || fallback;
}

export async function saveLiveConfig(payload: LiveConfigPayload, home: Home | null): Promise<LiveStatus> {
  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    throw new Error("Live Kalshi v2 API requires OpenBell v0.8.0+ server.");
  }
  const r = await fetch("/api/live/config", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify(payload),
  });
  if (!r.ok) throw new Error(await parseErrorMessage(r, `Save config answered ${r.status}`));
  return normalizeLiveStatus(await r.json(), "server");
}

export async function armLiveTrading(home: Home | null): Promise<LiveStatus> {
  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    throw new Error("Live Kalshi v2 API requires OpenBell v0.8.0+ server.");
  }
  const r = await fetch("/api/live/arm", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify({ confirm_arm: true }),
  });
  if (!r.ok) throw new Error(await parseErrorMessage(r, `Arm live trading answered ${r.status}`));
  return normalizeLiveStatus(await r.json(), "server");
}

export async function disarmLiveTrading(home: Home | null): Promise<LiveStatus> {
  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    return defaultLiveStatus("client");
  }
  const r = await fetch("/api/live/disarm", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify({}),
  });
  if (!r.ok) throw new Error(await parseErrorMessage(r, `Disarm answered ${r.status}`));
  return normalizeLiveStatus(await r.json(), "server");
}

export async function preflightLiveOrder(
  payload: LiveOrderPayload,
  status: LiveStatus,
  home: Home | null,
): Promise<LiveOrderPreflightResult> {
  const count = Math.max(1, Math.round(payload.count));
  const priceCents = Math.min(99, Math.max(1, Math.round(payload.price_cents)));
  const postOnly = Boolean(payload.post_only);
  const estFeeDollars = kalshiFeeDollars(count, priceCents / 100, postOnly);
  const fallbackEval = evaluateOrderRails(status, payload.ticker, count, priceCents, payload.action, postOnly);

  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    return {
      allowed: fallbackEval.allPassed,
      dryRun: true,
      ticker: payload.ticker,
      side: payload.side,
      count,
      priceCents,
      postOnly,
      orderNotionalDollars: fallbackEval.orderNotionalDollars,
      projectedExposureDollars: Number((status.openExposureDollars + fallbackEval.orderNotionalDollars).toFixed(2)),
      estFeeDollars,
      railViolations: fallbackEval.checks.filter((c) => !c.passed).map((c) => `${c.label}: ${c.detail}`),
      source: "client",
    };
  }

  try {
    const r = await fetch("/api/live/order", {
      method: "POST",
      headers: MUTATION_HEADERS,
      body: JSON.stringify({
        ticker: payload.ticker,
        action: payload.action,
        side: payload.side,
        count,
        price_cents: priceCents,
        post_only: postOnly,
        dry_run: true,
      }),
    });
    const body = (await r.json().catch(() => ({}))) as {
      ok?: boolean;
      error?: string;
      rail_violations?: string[];
      order_notional_dollars?: number;
      projected_exposure_dollars?: number;
      risk_check?: {
        allowed?: boolean;
        order_notional_dollars?: number;
        projected_exposure_dollars?: number;
        rail_violations?: string[];
      };
    };
    const rc = body.risk_check;
    const notional = isNum(rc?.order_notional_dollars)
      ? rc.order_notional_dollars
      : isNum(body.order_notional_dollars)
        ? body.order_notional_dollars
        : fallbackEval.orderNotionalDollars;
    const projected = isNum(rc?.projected_exposure_dollars)
      ? rc.projected_exposure_dollars
      : isNum(body.projected_exposure_dollars)
        ? body.projected_exposure_dollars
        : Number((status.openExposureDollars + notional).toFixed(2));
    const violations = Array.isArray(rc?.rail_violations)
      ? rc.rail_violations
      : Array.isArray(body.rail_violations)
        ? body.rail_violations
        : r.ok
          ? []
          : [body.error || `Preflight rejected (${r.status})`];

    return {
      allowed: r.ok && Boolean(body.ok ?? true) && violations.length === 0,
      dryRun: true,
      ticker: payload.ticker,
      side: payload.side,
      count,
      priceCents,
      postOnly,
      orderNotionalDollars: notional,
      projectedExposureDollars: projected,
      estFeeDollars,
      railViolations: violations,
      source: "server",
    };
  } catch {
    return {
      allowed: fallbackEval.allPassed,
      dryRun: true,
      ticker: payload.ticker,
      side: payload.side,
      count,
      priceCents,
      postOnly,
      orderNotionalDollars: fallbackEval.orderNotionalDollars,
      projectedExposureDollars: Number((status.openExposureDollars + fallbackEval.orderNotionalDollars).toFixed(2)),
      estFeeDollars,
      railViolations: fallbackEval.checks.filter((c) => !c.passed).map((c) => `${c.label}: ${c.detail}`),
      source: "client",
    };
  }
}

export async function submitLiveOrder(
  payload: LiveOrderPayload,
  home: Home | null,
): Promise<{ orderId: string; status: string }> {
  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    throw new Error("Live Kalshi v2 order routing requires OpenBell v0.8.0+ server.");
  }
  const r = await fetch("/api/live/order", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify(payload),
  });
  if (!r.ok) throw new Error(await parseErrorMessage(r, `Order rejected (${r.status})`));
  const body = (await r.json().catch(() => ({}))) as {
    order?: { order_id?: string; status?: string };
    order_id?: string;
    status?: string;
  };
  return {
    orderId: cleanStr(body.order?.order_id ?? body.order_id, "submitted"),
    status: cleanStr(body.order?.status ?? body.status, "resting"),
  };
}

export async function cancelLiveOrder(orderId: string, home: Home | null): Promise<void> {
  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    throw new Error("Live Kalshi v2 order cancel requires OpenBell v0.8.0+ server.");
  }
  const r = await fetch(`/api/live/orders/${encodeURIComponent(orderId)}`, {
    method: "DELETE",
    headers: MUTATION_HEADERS,
  });
  if (!r.ok) throw new Error(await parseErrorMessage(r, `Cancel order answered ${r.status}`));
}

export async function triggerKillSwitch(
  home: Home | null,
): Promise<{ cancelledCount: number; status: LiveStatus }> {
  if (!supportsV080(home, "live_kalshi") && !supportsV080(home, "risk_rails")) {
    return { cancelledCount: 0, status: defaultLiveStatus("client") };
  }
  const r = await fetch("/api/live/kill-switch", {
    method: "POST",
    headers: MUTATION_HEADERS,
    body: JSON.stringify({ reason: "operator_kill_switch" }),
  });
  if (!r.ok) throw new Error(await parseErrorMessage(r, `Kill switch answered ${r.status}`));
  const body = (await r.json().catch(() => ({}))) as {
    cancelled_count?: number;
    status?: unknown;
  };
  return {
    cancelledCount: isNum(body.cancelled_count) ? body.cancelled_count : 0,
    status: body.status ? normalizeLiveStatus(body.status, "server") : defaultLiveStatus("server"),
  };
}
