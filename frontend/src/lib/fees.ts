/** Kalshi fee rounded up to whole cents: ceil((postOnly ? 0.0175 : 0.07) * C * P * (1 - P) * 100) / 100. */
export function kalshiFeeDollars(contracts: number, price: number, postOnly = false): number {
  const c = Math.max(0, Math.floor(contracts));
  const p = Math.min(0.99, Math.max(0.01, Math.round(price * 100) / 100));
  if (c === 0) return 0;
  const rate = postOnly ? 0.0175 : 0.07;
  return Math.ceil(rate * c * p * (1 - p) * 100 - 1e-9) / 100;
}
