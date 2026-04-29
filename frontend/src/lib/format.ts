const usdFmt = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

const numberFmt = new Intl.NumberFormat("en-US");

const pctFmt = new Intl.NumberFormat("en-US", {
  minimumFractionDigits: 1,
  maximumFractionDigits: 1,
});

export function fmtUsd(n: number): string {
  if (!Number.isFinite(n)) return "$0.00";
  return usdFmt.format(n);
}

export function fmtNumber(n: number): string {
  return numberFmt.format(n);
}

export function fmtPct(n: number): string {
  return `${pctFmt.format(n)}%`;
}

/** Truncate a Solana signature/address to first6…last6. */
export function truncate(s: string, head = 6, tail = 6): string {
  if (!s) return "";
  if (s.length <= head + tail + 1) return s;
  return `${s.slice(0, head)}…${s.slice(-tail)}`;
}

export function fmtTime(iso: string): string {
  const d = new Date(iso);
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const mm = String(d.getUTCMinutes()).padStart(2, "0");
  const ss = String(d.getUTCSeconds()).padStart(2, "0");
  return `${hh}:${mm}:${ss}`;
}

export function fmtPool(pool: string, dex: string): string {
  const tag = dex === "raydium-v4" ? "RAY" : dex === "orca-whirlpool" ? "ORCA" : dex.slice(0, 4).toUpperCase();
  return `${tag}·${truncate(pool, 4, 4)}`;
}
