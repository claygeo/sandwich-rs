import type { Stats24h } from "../types";
import { fmtUsd, fmtPct, fmtNumber } from "../lib/format";

export function HeroStatBand({ stats }: { stats: Stats24h | null }) {
  if (!stats) {
    return (
      <div className="hero">
        <h1>sandwich.rs</h1>
        <div className="number">$0</div>
        <div className="subtitle">collecting first 24h of data…</div>
      </div>
    );
  }
  const usd = fmtUsd(stats.total_profit_usd ?? 0);
  const count = fmtNumber(stats.sandwich_count);
  const trend =
    stats.delta_pct != null && Number.isFinite(stats.delta_pct)
      ? `${stats.delta_pct >= 0 ? "↗" : "↘"} ${fmtPct(Math.abs(stats.delta_pct))}`
      : null;

  return (
    <div className="hero">
      <h1>sandwich.rs</h1>
      <div className="number">{usd}</div>
      <div className="subtitle">
        {count} sandwiches · {stats.unique_attackers} attackers · {stats.unique_victim_pools} pools
        {trend && (
          <>
            {" · "}
            <span className="trend">{trend} vs prior 24h</span>
          </>
        )}
      </div>
    </div>
  );
}
