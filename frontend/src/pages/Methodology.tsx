export function Methodology() {
  return (
    <article className="methodology">
      <h1>How sandwich.rs detects MEV</h1>

      <h2>Detection algorithm</h2>
      <p>
        Every observed swap on Raydium V4 and Orca Whirlpool is buffered in a
        per-pool ring (30s window, 256-event cap). When a new swap arrives, we
        walk that ring backward for a prior swap by the same signer in the same
        pool. If we find one, we then look for at least one different-signer
        swap between them — the victim. The bracket emits as a sandwich if the
        slot distance from front to back is no more than three.
      </p>

      <pre>
{`for new swap S in pool P, by signer A:
  for each prior swap F in ring(P) where signer(F) == A:
    if slot(S) - slot(F) > 3: skip
    for each V in ring(P) between F and S:
      if signer(V) != A and victim_pool(V) == P:
        emit Sandwich { front: F, victim: V, back: S }`}
      </pre>

      <h2>Profit, accurately</h2>
      <p>
        Profit is the attacker's net WSOL across the front and back transactions, with three subtractions:
      </p>
      <ul style={{ marginLeft: 20, marginBottom: 16 }}>
        <li>Solana base + priority fees (from <code>meta.fee</code> per leg).</li>
        <li>Jito tips (System Program transfers to any of the eight known tip accounts).</li>
        <li>Slippage on the attacker's own legs falls out of the WSOL math directly — no separate adjustment needed.</li>
      </ul>
      <p>
        Without these subtractions, a real $5 sandwich with $0.80 of fees and
        $0.20 of Jito tips reads as a loss. With them, the number you see on the
        live feed is the attacker's true take-home in WSOL terms, then
        multiplied by the Pyth Hermes SOL/USD spot (refreshed every 30 seconds).
      </p>

      <h2>Confidence scoring</h2>
      <p>
        Each detection carries a 0–100 score. 50 base for topology alone. +20 if
        the attacker's WSOL deltas have opposite signs across front and back
        (front spends, back recovers). +10 if the victim shares a non-WSOL mint
        with the front in the same direction (the victim is taking the price
        bump the attacker created). +10 if the net profit after fees and tips is
        positive. Confidence ≥ 70 is the recommended display threshold for
        public leaderboards.
      </p>

      <h2>Precision estimate</h2>
      <p>This is the load-bearing number for the project:</p>
      <div className="precision-number">—</div>
      <div className="precision-sub">
        awaiting first <code>scrape-fixtures</code> run against ≥30 labeled
        signatures from Sandwiched.wtf
      </div>

      <h2>What's filtered out</h2>
      <p>
        Failed transactions are dropped before the ring buffer. Self-trades
        (signer matches victim) are explicitly rejected. Slot spans wider than
        three slots are rejected. Cross-pool brackets — where the attacker's
        front and back are in different pools — are rejected as a different
        strategy, not a sandwich.
      </p>

      <h2>What's deferred</h2>
      <p>
        Jupiter v6 aggregator coverage, Meteora pools, and a slot-resume
        replay over the gap window are tracked in the project README. The
        backend already subscribes to two DEXes and identifies pools via each
        program's IDL, so the algorithm itself is DEX-agnostic; coverage is the
        scope, not the architecture.
      </p>
    </article>
  );
}
