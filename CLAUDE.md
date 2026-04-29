# sandwich-rs — Claude operating notes

## Design System
Always read `DESIGN.md` before making any visual or UI decisions. All font choices, colors, spacing, layout, and motion are defined there. Do not deviate without explicit user approval. In `/qa-design-review` mode, flag any code that doesn't match `DESIGN.md`.

## Pipeline (mandatory)
- `/plan-eng-review` before any non-trivial architecture change. No exceptions.
- `/codex` outside voice (via Agent subagent — never the codex CLI) before any judgment call: library pick, target system, data-source pick.
- `/qa` after every meaningful frontend or full-pipeline change. Loop until clean.
- `/review` before merging.
- `/investigate` for any bug — root cause first, no quick fixes.

## Decision policy
Operator delegated full decision authority. **Never stop to ask.** When facing a real choice (architecture, scope, library, design direction), spawn an outside-voice Agent subagent for `/codex`-style adversarial review. Use that verdict. Act immediately.

## Stack
- **Backend:** Rust (tokio + tokio-tungstenite). Helius `transactionSubscribe` enhanced WS (NOT public mainnet, NOT `getTransaction` followups). Bounded mpsc → parser pool → detector → sqlx writer (NOT PostgREST). axum HTTP `:8080` for `/healthz`, `/events` (SSE), `/stats`.
- **DB:** Supabase project `mqlnirjtuiwreohbpeli` (region us-east-1). Schema: `transactions` + `sandwich_attempts` (split, NOT flat). RLS: anon read-only, writes via service key. Realtime publication on `sandwich_attempts`.
- **Frontend:** Vite + React + TypeScript. `@supabase/supabase-js` for realtime sub. Pages: `/` (live feed), `/leaderboard`, `/methodology`. Deploy: Netlify.
- **Backtest:** `cargo run --bin backtest -- fixtures/known-sandwiches.csv`. **SHIP BLOCKER** — must hit ≥70% precision / ≥50% recall before live agent ships.
- **VPS:** Hetzner EU `77.42.83.22:2222` for v1. Migrate to US-east in v1.5 (Helius RTT penalty).

## Hard rules
- DB password lives only in `.env` (gitignored). Never echo or commit.
- No emojis in UI or commits unless explicitly requested.
- No Co-Authored-By Claude trailer on commits (operator preference).
- Frontend follows DESIGN.md verbatim — coral accent, no neon, no card grids, table-as-content.
- Backend follows the architectural landmine codex flagged: parser is a SEPARATE task from socket reader, bounded mpsc enforces backpressure on the reader, never on the event loop.

## Out of scope (v1)
- USD profit estimation via Pyth/Birdeye (v1.5)
- Jupiter v6 / Orca / Meteora subscriptions (v1.5)
- Auth, saved searches, push notifications (v2)
- Mobile-optimized layout (desktop-first, mobile = hero + last 5)
- MEV-protection product (different game)

## Repo layout
```
sandwich-rs/
├── src/                  # Rust backend
│   ├── main.rs           # Channel topology + tokio::select!
│   ├── ws.rs             # Helius WS reader
│   ├── parser.rs         # Enhanced-tx → Swap
│   ├── detector.rs       # Per-pool ring buffer + matcher
│   ├── db.rs             # sqlx writer (NEW)
│   ├── http.rs           # axum endpoints (NEW)
│   ├── telegram.rs       # Optional alerter
│   └── config.rs         # Env loader
├── src/bin/
│   └── backtest.rs       # Precision/recall on labeled CSV
├── migrations/           # SQL DDL
├── frontend/             # Vite + React + TS
├── ops/                  # systemd unit, deploy scripts
└── .context/             # Codex outputs preserved for posterity
```
