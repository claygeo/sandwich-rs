import { useEffect, useRef } from "react";
import type { SandwichRow } from "../types";
import { fmtPool, fmtTime, fmtUsd, truncate } from "../lib/format";

export function FeedTable({ rows }: { rows: SandwichRow[] }) {
  // Track the most recently inserted row by id to apply the "new-row" flash class once.
  const seen = useRef<Set<string>>(new Set());
  const newest = rows[0]?.id;
  useEffect(() => {
    if (newest && !seen.current.has(newest)) {
      seen.current.add(newest);
    }
  }, [newest]);

  if (rows.length === 0) {
    return (
      <div className="empty" role="status" aria-live="polite">
        <span className="pulsing-dot" />
        Watching mainnet — no sandwiches detected yet
      </div>
    );
  }

  return (
    <table className="feed-table" aria-label="Live feed of detected sandwiches">
      <thead>
        <tr>
          <th>time</th>
          <th>pool</th>
          <th className="num">victim loss</th>
          <th className="num">attacker profit</th>
          <th>attacker</th>
          <th>tx</th>
        </tr>
      </thead>
      <tbody aria-live="polite">
        {rows.slice(0, 50).map((r, idx) => {
          const isNew = idx === 0 && r.id === newest;
          return <Row key={r.id} row={r} isNew={isNew} />;
        })}
      </tbody>
    </table>
  );
}

function Row({ row, isNew }: { row: SandwichRow; isNew: boolean }) {
  const profitUsd = row.profit_usd ?? 0;
  const profitDisplay =
    profitUsd === 0 && row.profit_usd == null ? "—" : fmtUsd(profitUsd);
  const profitClass =
    profitUsd > 0 ? "profit-positive" : profitUsd < 0 ? "profit-negative" : "";

  // Victim "loss" is unknown without a separate calc; for v1 we surface a dash
  // and show attacker profit as the load-bearing number. Schema has the data
  // when v1.6 adds it.
  const victimDisplay = "—";

  return (
    <tr className={isNew ? "new-row" : undefined}>
      <td>{fmtTime(row.detected_at)}</td>
      <td>{fmtPool(row.pool, row.dex)}</td>
      <td className="num victim-loss">{victimDisplay}</td>
      <td className={`num ${profitClass}`}>{profitDisplay}</td>
      <td className="truncate">
        <a
          href={`https://solscan.io/account/${row.attacker_signer}`}
          target="_blank"
          rel="noopener noreferrer"
        >
          {truncate(row.attacker_signer)}
        </a>
      </td>
      <td>
        <a
          href={`https://solscan.io/tx/${row.victim_sig}`}
          target="_blank"
          rel="noopener noreferrer"
          title={row.victim_sig}
        >
          ↗ {truncate(row.victim_sig, 4, 6)}
        </a>
      </td>
    </tr>
  );
}
