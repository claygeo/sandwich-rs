import type { SandwichRow } from "../types";
import { fmtNumber, fmtUsd, truncate } from "../lib/format";

type AttackerStat = {
  attacker: string;
  count: number;
  profit_usd: number;
  pools: Set<string>;
};

export function LeaderboardTable({ rows }: { rows: SandwichRow[] }) {
  const ranks = aggregate(rows).slice(0, 10);
  if (ranks.length === 0) return null;
  return (
    <div className="section">
      <h2 className="section-header">Top attackers · 24h</h2>
      <table className="feed-table" aria-label="Top attackers leaderboard">
        <thead>
          <tr>
            <th>rank</th>
            <th>attacker</th>
            <th className="num">sandwiches</th>
            <th className="num">profit (USD)</th>
            <th className="num">pools</th>
          </tr>
        </thead>
        <tbody>
          {ranks.map((r, i) => (
            <tr key={r.attacker}>
              <td>{i + 1}</td>
              <td>
                <a
                  href={`https://solscan.io/account/${r.attacker}`}
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  {truncate(r.attacker)}
                </a>
              </td>
              <td className="num">{fmtNumber(r.count)}</td>
              <td className="num profit-positive">{fmtUsd(r.profit_usd)}</td>
              <td className="num">{r.pools.size}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function aggregate(rows: SandwichRow[]): AttackerStat[] {
  const cutoff = Date.now() - 24 * 60 * 60 * 1000;
  const map = new Map<string, AttackerStat>();
  for (const r of rows) {
    if (new Date(r.detected_at).getTime() < cutoff) continue;
    const cur = map.get(r.attacker_signer) ?? {
      attacker: r.attacker_signer,
      count: 0,
      profit_usd: 0,
      pools: new Set(),
    };
    cur.count += 1;
    cur.profit_usd += r.profit_usd ?? 0;
    cur.pools.add(r.pool);
    map.set(r.attacker_signer, cur);
  }
  return [...map.values()].sort((a, b) => b.profit_usd - a.profit_usd);
}
