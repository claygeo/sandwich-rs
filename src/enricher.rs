//! getTransaction RPC enricher.
//!
//! On free-tier Helius (and on public Solana mainnet WS), `logsSubscribe` gives us
//! `(signature, slot, raw log lines)` and nothing else — no signer, no accountKeys,
//! no parsed inner instructions, no token balance deltas. The detector cannot run on
//! topology alone without at least the signer.
//!
//! This module fills the gap. For each logs-only Swap that arrives, it spawns a
//! bounded-concurrency task that calls Helius's `getTransaction` RPC, then re-parses
//! the response into a fully populated Swap and emits it to the detector.
//!
//! Codex round-2 review surfaced four production landmines fixed here:
//!
//! 1. **Quota burnout.** Free tier = 100k credits/day; Raydium peak = ~50/sec ×
//!    Orca = ~100/sec. Naive 1:1 RPC-per-swap burns the daily budget in ~17min.
//!    Token bucket (`Limiter`) caps sustained rate at 25 req/sec; over-budget
//!    swaps are dropped early with `enrich_dropped_quota_total`.
//!
//! 2. **Commitment mismatch.** WS fires `logsNotification` at `processed` (~400ms),
//!    but the original code queried `getTransaction` with `commitment: confirmed`
//!    (~13s lag). Helius returns `result: null` until confirmed lands, so 30-40%
//!    of swaps were silently dropped on first call. Fix: query at `processed`
//!    AND retry up to 3 times with 600/1200/2400ms backoff if result is null.
//!
//! 3. **Triple-match blackout.** A single 429 on any leg of a sandwich (front,
//!    victim, OR back) makes the bracket invisible. Fix: bounded exponential
//!    backoff with jitter on 429 (400/800/1600ms), counters
//!    `enrich_429_total` and `enrich_dropped_after_retry_total`.
//!
//! 4. **Parser blocks WS reader.** Bounded mpsc fills in 256ms under spike;
//!    parser blocks on send → WS reader blocks on parser → Helius disconnects.
//!    Fix: `try_send` from parser into enricher input; on full, increment
//!    `enrich_input_dropped_total` and discard.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::{mpsc, Semaphore};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::parser::{self, Swap};

const ENRICH_TIMEOUT_SECS: u64 = 8;
const MAX_CONCURRENT_REQUESTS: usize = 16;

const TOKENS_PER_SECOND: u64 = 25;
const TOKEN_BUCKET_CAPACITY: u64 = 50;

const NULL_RETRY_ATTEMPTS: usize = 3;
const RATE_LIMIT_RETRY_ATTEMPTS: usize = 3;

#[derive(Default)]
pub struct EnrichMetrics {
    pub input_dropped: Arc<AtomicU64>,
    pub quota_dropped: Arc<AtomicU64>,
    pub rate_limit_429: Arc<AtomicU64>,
    pub null_retries: Arc<AtomicU64>,
    pub dropped_after_retry: Arc<AtomicU64>,
    pub enriched_ok: Arc<AtomicU64>,
}

#[derive(Clone)]
struct EnrichClient {
    http: reqwest::Client,
    rpc_url: String,
}

/// Token-bucket rate limiter — cheap enough to avoid pulling `governor`.
/// Producer: refill task adds 1 token every `1000/TOKENS_PER_SECOND` ms,
/// capped at `TOKEN_BUCKET_CAPACITY`. Consumer: `try_acquire` returns
/// false when bucket empty (caller drops the request).
#[derive(Clone)]
struct Limiter {
    sem: Arc<Semaphore>,
}

impl Limiter {
    fn new() -> Self {
        let sem = Arc::new(Semaphore::new(TOKEN_BUCKET_CAPACITY as usize));
        let refill = sem.clone();
        tokio::spawn(async move {
            let interval_ms = 1000 / TOKENS_PER_SECOND;
            let mut tick = tokio::time::interval(Duration::from_millis(interval_ms));
            loop {
                tick.tick().await;
                if refill.available_permits() < TOKEN_BUCKET_CAPACITY as usize {
                    refill.add_permits(1);
                }
            }
        });
        Self { sem }
    }

    fn try_acquire(&self) -> bool {
        self.sem.try_acquire().map(|p| p.forget()).is_ok()
    }
}

pub fn run_with_metrics(
    rpc_url: String,
    metrics: Arc<EnrichMetrics>,
    rx: mpsc::Receiver<Swap>,
    tx: mpsc::Sender<Swap>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(run_inner(rpc_url, metrics, rx, tx))
}

pub async fn run(
    rpc_url: String,
    rx: mpsc::Receiver<Swap>,
    tx: mpsc::Sender<Swap>,
) -> Result<()> {
    run_inner(rpc_url, Arc::new(EnrichMetrics::default()), rx, tx).await
}

async fn run_inner(
    rpc_url: String,
    metrics: Arc<EnrichMetrics>,
    mut rx: mpsc::Receiver<Swap>,
    tx: mpsc::Sender<Swap>,
) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(ENRICH_TIMEOUT_SECS))
        .build()?;
    let client = EnrichClient { http, rpc_url };
    let limiter = Limiter::new();
    let concurrency = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));

    info!(
        "enricher running (rate cap {}/s, concurrency {}, retry on null/429)",
        TOKENS_PER_SECOND, MAX_CONCURRENT_REQUESTS
    );

    while let Some(swap) = rx.recv().await {
        // Already-parsed swaps (Atlas path) bypass the enricher entirely.
        if !swap.signer.is_empty() && swap.pool != "raydium-v4-unknown" {
            if tx.send(swap).await.is_err() {
                break;
            }
            continue;
        }

        // Fix #1 (codex): shed-load on quota empty before spawning anything.
        if !limiter.try_acquire() {
            metrics.quota_dropped.fetch_add(1, Ordering::Relaxed);
            debug!(sig = %swap.signature, "enrich: quota empty, drop");
            continue;
        }

        let permit = match concurrency.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };
        let client = client.clone();
        let tx = tx.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            let _permit = permit;
            match enrich_with_retries(&client, &swap, metrics.as_ref()).await {
                Ok(Some(enriched)) => {
                    metrics.enriched_ok.fetch_add(1, Ordering::Relaxed);
                    let _ = tx.send(enriched).await;
                }
                Ok(None) => {
                    metrics
                        .dropped_after_retry
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    warn!(sig = %swap.signature, err = %e, "enrich failed");
                }
            }
        });
    }
    Ok(())
}

/// Codex #2 + #3: retry on null result (commitment lag) AND on 429.
/// Backoffs: 600/1200/2400ms for null, 400/800/1600ms for 429, both with jitter.
async fn enrich_with_retries(
    client: &EnrichClient,
    raw: &Swap,
    metrics: &EnrichMetrics,
) -> Result<Option<Swap>> {
    for attempt in 0..(NULL_RETRY_ATTEMPTS + RATE_LIMIT_RETRY_ATTEMPTS) {
        match enrich_one(client, raw).await {
            Ok(EnrichResult::Ok(swap)) => return Ok(Some(swap)),
            Ok(EnrichResult::NullResult) if attempt < NULL_RETRY_ATTEMPTS => {
                metrics.null_retries.fetch_add(1, Ordering::Relaxed);
                let backoff = 600 * (1u64 << attempt as u32);
                sleep(Duration::from_millis(backoff + jitter_ms())).await;
            }
            Ok(EnrichResult::NullResult) => {
                debug!(sig = %raw.signature, "enrich: null result after retries");
                return Ok(None);
            }
            Ok(EnrichResult::RateLimited) if attempt < RATE_LIMIT_RETRY_ATTEMPTS => {
                metrics.rate_limit_429.fetch_add(1, Ordering::Relaxed);
                let backoff = 400 * (1u64 << attempt as u32);
                sleep(Duration::from_millis(backoff + jitter_ms())).await;
            }
            Ok(EnrichResult::RateLimited) => {
                warn!(sig = %raw.signature, "enrich: 429 after retries — quota saturated");
                return Ok(None);
            }
            Ok(EnrichResult::ParseFailed) => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

enum EnrichResult {
    Ok(Swap),
    NullResult,
    RateLimited,
    ParseFailed,
}

async fn enrich_one(client: &EnrichClient, raw: &Swap) -> Result<EnrichResult> {
    // Match the WS commitment so we don't query for state that hasn't landed.
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [raw.signature, {
            "encoding": "jsonParsed",
            "maxSupportedTransactionVersion": 0,
            "commitment": "processed"
        }]
    });
    let resp = client
        .http
        .post(&client.rpc_url)
        .json(&body)
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(EnrichResult::RateLimited);
    }
    if !resp.status().is_success() {
        return Err(anyhow!("rpc HTTP {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await?;
    if v.get("error").is_some_and(|e| !e.is_null()) {
        let msg = v.get("error").map(|e| e.to_string()).unwrap_or_default();
        // Helius wraps rate limiting into a JSON-RPC error sometimes.
        if msg.contains("429") || msg.to_lowercase().contains("rate") {
            return Ok(EnrichResult::RateLimited);
        }
        return Err(anyhow!("rpc error: {}", msg));
    }
    let result = match v.get("result") {
        Some(r) if !r.is_null() => r,
        _ => return Ok(EnrichResult::NullResult),
    };

    // Codex #5 fix: the getTransaction result has shape
    //   { slot, transaction: { message, ... }, meta: { ... } }
    // No "transactionNotification" envelope synthesis; call the components-level
    // parser directly with the right nodes.
    let tx_node = match result.get("transaction") {
        Some(t) => t,
        None => return Ok(EnrichResult::ParseFailed),
    };
    let meta_node = match result.get("meta") {
        Some(m) => m,
        None => return Ok(EnrichResult::ParseFailed),
    };

    match parser::parse_tx_components(&raw.signature, raw.slot, tx_node, meta_node) {
        Some(s) => Ok(EnrichResult::Ok(s)),
        None => Ok(EnrichResult::ParseFailed),
    }
}

fn jitter_ms() -> u64 {
    // Cheap deterministic-ish jitter without pulling `rand`.
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    n % 200
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn raw_logs_swap(sig: &str, slot: u64) -> Swap {
        Swap {
            signature: sig.into(),
            slot,
            signer: String::new(),
            pool: "raydium-v4-unknown".into(),
            dex: "raydium-v4".into(),
            fee_lamports: 0,
            jito_tip_lamports: 0,
            deltas: Vec::new(),
            raw_logs: Vec::new(),
        }
    }

    fn rpc_response_for(sig: &str, slot: u64) -> serde_json::Value {
        let signer = "Att1111111111111111111111111111111111111111";
        let pool = "Pool1111111111111111111111111111111111111111";
        let raydium = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "slot": slot,
                "transaction": {
                    "message": {
                        "accountKeys": [
                            { "pubkey": signer, "signer": true, "writable": true, "source": "transaction" },
                            { "pubkey": pool,   "signer": false, "writable": true, "source": "transaction" },
                            { "pubkey": raydium,"signer": false, "writable": false, "source": "transaction" }
                        ],
                        "instructions": [
                            { "programId": raydium, "accounts": [signer, pool, "Other11111111111111111111111111111111111111"] }
                        ]
                    }
                },
                "meta": {
                    "err": null,
                    "fee": 5_000,
                    "preTokenBalances": [],
                    "postTokenBalances": [],
                    "logMessages": []
                }
            },
            "_sig_for_test": sig
        })
    }

    #[tokio::test]
    async fn rate_limit_returns_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let client = EnrichClient {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            rpc_url: server.uri(),
        };
        let raw = raw_logs_swap("ratelimit_sig", 100);
        let r = enrich_one(&client, &raw).await.unwrap();
        assert!(matches!(r, EnrichResult::RateLimited));
    }

    #[tokio::test]
    async fn null_result_returns_null_then_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": null
            })))
            .mount(&server)
            .await;

        let client = EnrichClient {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            rpc_url: server.uri(),
        };
        let raw = raw_logs_swap("null_sig", 100);
        // single-call returns NullResult
        let r = enrich_one(&client, &raw).await.unwrap();
        assert!(matches!(r, EnrichResult::NullResult));

        // run-with-retries gives up after NULL_RETRY_ATTEMPTS
        let metrics = EnrichMetrics::default();
        let r = enrich_with_retries(&client, &raw, &metrics).await.unwrap();
        assert!(r.is_none());
        assert!(metrics.null_retries.load(Ordering::Relaxed) >= 1);
    }

    #[tokio::test]
    async fn success_returns_enriched_swap() {
        let server = MockServer::start().await;
        let body = rpc_response_for("ok_sig", 250_000_001);
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getTransaction",
                "params": ["ok_sig", {
                    "encoding": "jsonParsed",
                    "maxSupportedTransactionVersion": 0,
                    "commitment": "processed"
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = EnrichClient {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            rpc_url: server.uri(),
        };
        let raw = raw_logs_swap("ok_sig", 250_000_001);
        let r = enrich_one(&client, &raw).await.unwrap();
        match r {
            EnrichResult::Ok(s) => {
                assert_eq!(s.signature, "ok_sig");
                assert_eq!(s.slot, 250_000_001);
                assert_eq!(s.dex, "raydium-v4");
                assert!(!s.signer.is_empty());
                assert_eq!(s.fee_lamports, 5_000);
            }
            other => panic!("expected Ok, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[tokio::test]
    async fn token_bucket_caps_concurrency() {
        let lim = Limiter::new();
        // First TOKEN_BUCKET_CAPACITY tries succeed
        let mut succeeded = 0;
        for _ in 0..TOKEN_BUCKET_CAPACITY {
            if lim.try_acquire() {
                succeeded += 1;
            }
        }
        assert_eq!(succeeded, TOKEN_BUCKET_CAPACITY);
        // Next try fails (bucket empty, refill is async)
        assert!(!lim.try_acquire());
    }
}
