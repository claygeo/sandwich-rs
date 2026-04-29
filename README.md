# sandwich-rs

Real-time Solana MEV sandwich detector. Subscribes to Helius's enhanced WebSocket, ring-buffers per-pool swap activity, and identifies front-run / victim / back-run triples within ≤3 slots. Persists to Supabase, exposes an SSE feed and a `/stats` endpoint.

```
WS  →  parser pool  →  detector (per-pool ring buffer, 30s window)
                              │
                              ├──→  sqlx writer  →  Postgres (transactions + sandwich_attempts)
                              ├──→  axum SSE     →  GET /events
                              └──→  Telegram (optional)
```

The WebSocket reader does *only* I/O. Parsing happens on a separate task with bounded backpressure on the channel — when parsers fall behind, the socket reader yields, never the event loop. This is the whole game during a busy slot.

## What ships in v1

- Subscribes to **Raydium AMM v4** on Solana mainnet. (Jupiter v6 / Orca / Meteora come in v1.5.)
- Detection algorithm:
  - Walk the per-pool ring buffer for a prior swap by the same signer.
  - Confirm a different-signer swap sits between front and back, in the same pool.
  - `slot(back) - slot(front)` must be ≤ 3.
  - Score confidence: 50 base + 20 (opposite WSOL signs on attacker) + 10 (victim shares non-WSOL mint, same direction as front) + 10 (positive aggregate profit).
- Profit estimate (when Helius enhanced-tx is available): `signed_sum(attacker.WSOL deltas across front + back)`.
- Idempotent persistence: `ON CONFLICT (signature) DO NOTHING` on `transactions`, `ON CONFLICT (front_sig, back_sig) DO NOTHING` on `sandwich_attempts`. Restart-safe.
- HTTP endpoints on `:8080`:
  - `GET /healthz` — 200 if last WS frame within 60s, 503 otherwise.
  - `GET /events` — Server-Sent Events; replays the most recent 50 sandwiches then streams new ones.
  - `GET /stats` — singleton 24h aggregate (count, profit_sol, profit_usd, unique attackers, unique victim pools, delta vs prior 24h).

## What's deferred

- USD profit (needs Pyth/Birdeye spot). v1.5.
- Jupiter v6, Orca Whirlpool, Meteora subscriptions. v1.5.
- US-east region migration (Hetzner EU adds ~120ms RTT to Helius). v1.5 once precision is proven.
- Auto-scrape ground-truth from Sandwiched.wtf. v2.
- Public dashboard frontend. v2 once detector precision is verified.

## Run locally

```bash
cp .env.example .env
# fill HELIUS_API_KEY, SUPABASE_POOLER_URL, SUPABASE_URL, SUPABASE_ANON_KEY
RUST_LOG=sandwich_rs=info cargo run --release
```

Without DB or Helius the agent still runs, falling back to public mainnet `logsSubscribe` and skipping persistence.

## Backtest

```bash
cargo run --release --bin backtest -- fixtures/known-sandwiches.csv
```

Reads a CSV of labeled known sandwiches (positives) and decoy patterns (negatives), replays each through the actual detector, and reports precision / recall / F1. **Ship gate:** precision ≥ 0.70, recall ≥ 0.50, n_positive ≥ 30. Below threshold the binary exits non-zero.

The `fixtures/known-sandwiches.csv` shipped with this repo has 1 positive + 1 negative as a smoke test. Populate it with real signatures from [Sandwiched.wtf](https://sandwiched.wtf) before running the gate.

## Deploy (Hetzner VPS)

```bash
VPS_HOST=77.42.83.22 VPS_PORT=2222 ops/deploy.sh
```

Idempotent. First run creates the `sandwich` system user, installs to `/opt/sandwich-rs/`, drops a systemd unit at `/etc/systemd/system/sandwich-rs.service`, seeds `/etc/sandwich-rs/env` from the example, and tails `journalctl -u sandwich-rs -f`. Edit the env file with real secrets, then `systemctl restart sandwich-rs`.

## Architecture decisions (locked)

- **`sqlx` direct Postgres** to Supabase pooler (port 6543, transaction mode), not PostgREST. Saves a 30–80ms HTTP roundtrip per insert.
- **Helius `transactionSubscribe`** when `HELIUS_API_KEY` is set, falling back to public `logsSubscribe`. Helius gives parsed inner instructions + pre/post token balances inline; public WS would need an extra `getTransaction` followup per swap, which kills throughput on a busy slot.
- **Split schema**: `transactions` (raw observed swaps) and `sandwich_attempts` (joined triples). Audit trail beats a single flat table.
- **Bounded mpsc with backpressure on the socket reader.** Architectural landmine: parsing in the same task that reads the WebSocket fails the moment Solana has a busy slot. Don't.

## Tech

Rust 1.85+ · `tokio` · `tokio-tungstenite` · `axum` · `sqlx` · `dashmap` · `tracing` · Supabase Postgres · Hetzner.

## Why Rust

A single Raydium subscription emits hundreds of frames per second during peak Solana activity. The hot path is JSON deserialization + cross-pool state lookups + an idempotent batched insert. Rust gives us bounded channels with backpressure semantics that don't lie, zero-copy parsing options when we measure parsing as the bottleneck (`simd-json` is the next swap), and a memory model where the per-pool ring buffer never surprises us. Equivalent Python or Node implementations spend their cycles in GC.

## Caveats and known limitations (be honest)

These are documented because the v1 ship-blocker review caught them before deploy:

1. **Backtest fixtures are synthetic.** The CSV-driven harness builds 3-swap streams with hand-crafted topology. Of course it hits 100% precision. The ship gate's real role is *not* validating algorithm correctness against the bench — that's the unit test suite — it's enforcing the ground-truth populating discipline. Until 30+ real Helius `getTransaction` blobs from labeled Sandwiched.wtf signatures are in `fixtures/`, the precision number on the README is lying. v1.5: a `tools/scrape-sandwiched-wtf` binary that fetches and persists real-data fixtures.

2. **Jito tips are not subtracted from profit.** The detector subtracts `meta.fee` (network base + priority fee combined) but not Jito tips, which are paid as separate SOL transfers to a Jito tip account. Real attackers tip 0.001–0.01 SOL routinely. v1.5 adds tip-account scanning over the transaction's transfers.

3. **Pool extraction works for direct Raydium V4 swaps and CPI-via-Jupiter routes.** The parser walks both outer and inner instructions to find `programId == Raydium V4` and pulls `accounts[1]` per the IDL. For aggregator routes through unknown intermediaries, falls back to a non-program-non-signer heuristic. Unrecognized DEXes (Phoenix, Lifinity, etc.) are not yet supported and yield `pool == "raydium-v4-unknown"`, which the detector ignores.

4. **No slot-resume on reconnect.** WS reader uses exponential backoff (1s → 30s with reset on first frame received) but does not request a starting slot from `lastSlot - N`. A 30s outage is silently lost. Defensible for v1 because the detector is event-driven and Helius's `transactionSubscribe` doesn't offer slot-resume out of the box, but a v1.5 disk-persisted resume marker is on the roadmap.

5. **Multi-victim brackets emit one row per victim.** Schema unique constraint is `(front_sig, victim_sig, back_sig)` for this reason. Aggregating to `total_profit / total_victims` is straightforward in SQL.

## Pre-deploy review

This backend went through a multi-pass adversarial review before it was considered ready to deploy:

- Engineering plan reviewed (`/plan-eng-review`): scope locked at HOLD, 6 architecture issues resolved, test plan written.
- Architecture challenged via outside-voice subagent: 7 ship-blockers identified, 6 fixed (the seventh — synthetic-vs-real backtest — is a v1.5 expansion documented above).
- 12/12 unit tests green: parser handles Helius enhanced-tx and `logsNotification` fallback; detector handles single victim, multi-victim, slot-span limit, different pools, self-sandwich, logs-fallback skip, base-confidence topology, and fee-subtracted profit math.
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo build --release` clean, lto=thin, single codegen-unit, stripped symbols.

## Status

Backend ready to deploy. Operator next steps:
1. Get a Helius free API key from [dashboard.helius.dev](https://dashboard.helius.dev).
2. Fill `/etc/sandwich-rs/env` on VPS (or `.env` locally) with Helius key + Supabase credentials.
3. Run `ops/deploy.sh` from a dev box that can SSH to the VPS.
4. Watch `journalctl -u sandwich-rs -f` for the first detected sandwich.
5. Populate `fixtures/known-sandwiches.csv` with ≥30 real cases from [Sandwiched.wtf](https://sandwiched.wtf), then run `cargo run --release --bin backtest` to drop the real precision number into the README.
