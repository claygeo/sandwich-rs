# Codex outside-voice review (2026-04-28)

Decision input for project pick. Preserved verbatim.

---

**PICK: A — Solana MEV Mempool Watcher**

**Why A wins:**
- Rust earns its keep here. Tokio + raw WS + zero-copy parsing of Jito ShredStream / Helius LaserStream is the exact workload Rust dominates. You can *feel* the latency budget.
- Mempool watchers are catnip for crypto-native hiring. Jump/Jane Street/SIG/Cumberland/Wintermute crews read these READMEs unprompted. "I detect sandwiches in <50ms p99" is a conversation starter; "I built a matching engine" is page 3 of every bootcamp grad's portfolio.
- Pairs with solhunt narrative: adversarial agents on EVM, now adversarial detection on SVM. Coherent.
- Saturday-fun is real — you watch a live feed, find your first sandwich within hours, post the screenshot on X. Dopamine loop.
- v0 is 1 weekend. v1 (writing back-runs to Jito bundles) is the natural evolution — same repo, same crates.

**Traps in the rejected options:**
- **B (matching engine):** Rite of passage, not a differentiator. 9000 of these on GitHub. Senior reads it and yawns.
- **C (Anchor program):** Anchor is Rust-flavored, not Rust. You'll spend 80% of time on IDLs and program-derived addresses, not systems work. Hireability != Rust signal.
- **D (Rust port of solhunt):** Premature optimization. The Python/TS solhunt isn't bottlenecked on language. Porting working code is a dev tax, not a portfolio piece.
- **E (Leptos dashboard):** Frontend in Rust impresses Rust nerds, nobody who hires quants. Trap.

**v0 spec (one weekend):**
- Subscribe to Helius enhanced WS (`accountSubscribe` on Raydium/Orca AMM pools + Jupiter v6 program logs).
- Ring-buffer last N seconds of swaps per pool. Detect pattern: large buy -> victim swap -> large sell, same signer or same slot, on same pool.
- Print classified events to stdout + Telegram webhook. JSON schema versioned.
- **Demo proof:** terminal screencast, 60s of live mainnet, 3+ sandwiches caught with profit estimates and Solscan links.
- **README hero shot:** annotated screenshot of one detected sandwich with attacker tx, victim tx, $extracted.

**Crates (use these exactly):**
`tokio`, `tokio-tungstenite`, `solana-client`, `solana-sdk`, `serde_json` w/ `simd-json` for hot path, `dashmap` for the per-pool ring buffer, `teloxide` for TG, `tracing` + `tracing-subscriber`.

**The architectural mistake you will make:** parsing every WS message in the same task that reads the socket. Don't. Split into a bounded `mpsc` with a dedicated parser pool — backpressure on the socket reader kills you the moment Solana has a busy slot.

Ship by Sunday night.

---

## Deviations from codex's spec in v0 commit

- **No `solana-client` / `solana-sdk` / `teloxide`.** Pulling in `solana-client` adds 200+ transitive crates to compile. v0 doesn't need on-chain reads (we're WS-passive). `teloxide` is overkill — a single `reqwest::Client::post` to the Telegram Bot API does the job. Both can be added when we need them.
- **No `simd-json` yet.** Premature optimization. `serde_json` parses our v0 message volume (hundreds/sec from one Raydium subscription) without breaking a sweat. Swap when we measure parsing as the bottleneck.
- **No Helius API key for v0.** Public mainnet WS at `api.mainnet-beta.solana.com` works for `logsSubscribe`. We'll move to Helius/Triton in v0.5 for higher rate limits + parsed transaction data.
- **Detection logic deferred to v0.5.** Parsing signers requires `getTransaction` follow-ups or Helius enhanced WS — both pull more weight. v0 wires the pipeline + ring buffer; the algorithm is written out in `detector.rs` comments and ready to drop in.
