use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::parser::Swap;

#[derive(Debug, Clone, Serialize)]
pub struct Sandwich {
    pub front: Swap,
    pub victim: Swap,
    pub back: Swap,
    pub attacker: String,
    pub victim_signer: String,
    pub pool: String,
    pub dex: String,
    pub slot_span: i32,
    pub profit_lamports: Option<i128>,
    pub confidence: u8,
}

pub type PoolState = Arc<DashMap<String, VecDeque<(Instant, Swap)>>>;

const RING_WINDOW_SECS: u64 = 30;
const RING_CAP: usize = 256;
const MAX_SLOT_SPAN: u64 = 3;

const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Detector: walks per-pool ring buffer and emits Sandwich on front/victim/back match.
///
/// Algorithm (v1):
///   For each new swap S in pool P:
///     - Look back in pool P's ring for a prior swap S_front where signer(S_front) == signer(S).
///     - Collect ALL swaps V_i between S_front and S where signer(V_i) != signer(S).
///       Emit one Sandwich per victim (a single A→V1→V2→B bracket records two rows).
///     - slot(S) - slot(S_front) must be ≤ MAX_SLOT_SPAN (3 slots).
///     - Confidence scoring:
///         50 base (topology only)
///         +20 if attacker WSOL deltas have opposite signs (front sells/buys, back buys/sells)
///         +10 if victim shares a non-WSOL mint with front, same-direction
///         +10 if compound profit on attacker's WSOL is positive
///
/// Profit: signed sum of attacker WSOL deltas across front+back, MINUS network+priority
/// fees on both transactions. Positive = attacker netted SOL.
pub async fn run(
    state: PoolState,
    mut rx: mpsc::Receiver<Swap>,
    tx: mpsc::Sender<Sandwich>,
) -> anyhow::Result<()> {
    info!("detector running (real algorithm: front+victim+back triple match)");

    while let Some(swap) = rx.recv().await {
        // Logs-fallback swaps have no signer/pool — skip; topology cannot match.
        if swap.signer.is_empty() || swap.pool == "raydium-v4-unknown" {
            continue;
        }

        let now = Instant::now();
        let detected = {
            let mut ring = state.entry(swap.pool.clone()).or_default();
            let detected = try_detect_sandwiches(&swap, &ring);
            ring.push_back((now, swap.clone()));
            while ring
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t).as_secs() > RING_WINDOW_SECS)
            {
                ring.pop_front();
            }
            while ring.len() > RING_CAP {
                ring.pop_front();
            }
            detected
        };

        debug!(pool = %swap.pool, slot = swap.slot, "ring updated");

        for sandwich in detected {
            info!(
                attacker = %sandwich.attacker,
                victim = %sandwich.victim_signer,
                pool = %sandwich.pool,
                slot_span = sandwich.slot_span,
                confidence = sandwich.confidence,
                profit_lamports = ?sandwich.profit_lamports,
                "SANDWICH"
            );
            if tx.send(sandwich).await.is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Returns every (front, victim_i, back) triple that satisfies the sandwich pattern.
/// Multi-victim brackets and back-to-back attacker sequences both produce multiple rows.
fn try_detect_sandwiches(
    candidate_back: &Swap,
    ring: &VecDeque<(Instant, Swap)>,
) -> Vec<Sandwich> {
    let attacker = &candidate_back.signer;
    let mut out = Vec::new();

    for i in (0..ring.len()).rev() {
        let (_t_front, front) = &ring[i];
        if front.signer != *attacker {
            continue;
        }
        if front.signature == candidate_back.signature {
            continue;
        }
        let slot_span = candidate_back.slot.saturating_sub(front.slot);
        if slot_span == 0 || slot_span > MAX_SLOT_SPAN {
            continue;
        }

        for (_t_v, victim) in ring.iter().skip(i + 1) {
            if victim.signer == *attacker || victim.signer.is_empty() {
                continue;
            }
            if !(front.slot..=candidate_back.slot).contains(&victim.slot) {
                continue;
            }
            if victim.pool != candidate_back.pool {
                continue;
            }

            let confidence = score_confidence(front, victim, candidate_back);
            let profit = compute_profit_lamports(front, candidate_back);

            out.push(Sandwich {
                front: front.clone(),
                victim: victim.clone(),
                back: candidate_back.clone(),
                attacker: attacker.clone(),
                victim_signer: victim.signer.clone(),
                pool: candidate_back.pool.clone(),
                dex: candidate_back.dex.clone(),
                slot_span: slot_span as i32,
                profit_lamports: profit,
                confidence,
            });
        }

        // Once we've found a same-signer front for this back-leg and emitted all victims,
        // stop walking further back. An older same-signer swap is a different bracket
        // and gets its own back-leg event when it appears.
        if !out.is_empty() {
            break;
        }
    }
    out
}

fn score_confidence(front: &Swap, victim: &Swap, back: &Swap) -> u8 {
    let mut score = 50_u8;

    let front_wsol = signer_delta_for_mint(front, WSOL_MINT);
    let back_wsol = signer_delta_for_mint(back, WSOL_MINT);
    if let (Some(f), Some(b)) = (front_wsol, back_wsol) {
        if f != 0 && b != 0 && f.signum() != b.signum() {
            score = score.saturating_add(20);
        }
    }

    let front_other = signer_delta_for_other_mint(front);
    let victim_other = signer_delta_for_other_mint(victim);
    if let (Some((mf, df)), Some((mv, dv))) = (front_other, victim_other) {
        if mf == mv && df != 0 && dv != 0 && df.signum() == dv.signum() {
            score = score.saturating_add(10);
        }
    }

    if let Some(p) = compute_profit_lamports(front, back) {
        if p > 0 {
            score = score.saturating_add(10);
        }
    }

    score.min(100)
}

fn signer_delta_for_mint(swap: &Swap, mint: &str) -> Option<i128> {
    swap.deltas.iter().find(|d| d.mint == mint).map(|d| d.delta)
}

fn signer_delta_for_other_mint(swap: &Swap) -> Option<(String, i128)> {
    swap.deltas
        .iter()
        .find(|d| d.mint != WSOL_MINT)
        .map(|d| (d.mint.clone(), d.delta))
}

/// Profit on attacker's WSOL across the bracket, net of network + priority fees AND
/// Jito tips (parsed by the parser from the same transaction's inner instructions).
/// Codex flagged: ignoring these means a real $5 sandwich with $0.50 of fees and a
/// $0.20 Jito tip reads as a meaningful loss instead of a small win.
fn compute_profit_lamports(front: &Swap, back: &Swap) -> Option<i128> {
    let f = signer_delta_for_mint(front, WSOL_MINT)?;
    let b = signer_delta_for_mint(back, WSOL_MINT)?;
    let fees = front.fee_lamports as i128 + back.fee_lamports as i128;
    let tips = front.jito_tip_lamports as i128 + back.jito_tip_lamports as i128;
    Some(f + b - fees - tips)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::TokenDelta;

    fn mk_swap(sig: &str, slot: u64, signer: &str, pool: &str, deltas: Vec<TokenDelta>) -> Swap {
        Swap {
            signature: sig.into(),
            slot,
            signer: signer.into(),
            pool: pool.into(),
            dex: "raydium-v4".into(),
            fee_lamports: 0,
            jito_tip_lamports: 0,
            deltas,
            raw_logs: vec![],
        }
    }

    fn delta(mint: &str, d: i128) -> TokenDelta {
        TokenDelta {
            mint: mint.into(),
            delta: d,
            decimals: 9,
        }
    }

    const ATTACKER: &str = "AttackerPubkey11111111111111111111111111111";
    const VICTIM: &str = "VictimPubkey1111111111111111111111111111111";
    const POOL: &str = "PoolAcct1111111111111111111111111111111111";
    const POOL_B: &str = "PoolAcctB111111111111111111111111111111111";
    const TOKEN: &str = "TokenMint11111111111111111111111111111111111";

    async fn drive(swaps: Vec<Swap>) -> Vec<Sandwich> {
        let state: PoolState = Arc::new(DashMap::new());
        let (in_tx, in_rx) = mpsc::channel::<Swap>(swaps.len() + 1);
        let (out_tx, mut out_rx) = mpsc::channel::<Sandwich>(swaps.len() + 1);
        for s in swaps {
            in_tx.send(s).await.unwrap();
        }
        drop(in_tx);
        let h = tokio::spawn(run(state, in_rx, out_tx));
        h.await.unwrap().unwrap();
        let mut out = Vec::new();
        while let Ok(s) = out_rx.try_recv() {
            out.push(s);
        }
        out
    }

    #[tokio::test]
    async fn detects_classic_sandwich() {
        let out = drive(vec![
            mk_swap("front", 100, ATTACKER, POOL, vec![delta(WSOL_MINT, -1_000_000_000), delta(TOKEN, 500_000)]),
            mk_swap("victim", 101, VICTIM, POOL, vec![delta(WSOL_MINT, -2_000_000_000), delta(TOKEN, 800_000)]),
            mk_swap("back", 102, ATTACKER, POOL, vec![delta(WSOL_MINT, 1_200_000_000), delta(TOKEN, -500_000)]),
        ])
        .await;
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(s.attacker, ATTACKER);
        assert_eq!(s.victim_signer, VICTIM);
        assert_eq!(s.front.signature, "front");
        assert_eq!(s.victim.signature, "victim");
        assert_eq!(s.back.signature, "back");
        assert_eq!(s.slot_span, 2);
        assert_eq!(s.profit_lamports, Some(200_000_000));
        assert!(s.confidence >= 80, "should hit ≥80 confidence (got {})", s.confidence);
    }

    #[tokio::test]
    async fn ignores_back_only() {
        let out = drive(vec![
            mk_swap("only", 100, ATTACKER, POOL, vec![]),
            mk_swap("victim", 101, VICTIM, POOL, vec![]),
        ])
        .await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn ignores_different_pools() {
        let out = drive(vec![
            mk_swap("front", 100, ATTACKER, POOL, vec![]),
            mk_swap("victim", 101, VICTIM, POOL, vec![]),
            mk_swap("back_wrong_pool", 102, ATTACKER, POOL_B, vec![]),
        ])
        .await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn ignores_slot_span_too_wide() {
        let out = drive(vec![
            mk_swap("front", 100, ATTACKER, POOL, vec![]),
            mk_swap("victim", 101, VICTIM, POOL, vec![]),
            mk_swap("back_too_late", 105, ATTACKER, POOL, vec![]),
        ])
        .await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn ignores_self_sandwich_no_victim() {
        let out = drive(vec![
            mk_swap("a", 100, ATTACKER, POOL, vec![]),
            mk_swap("b", 101, ATTACKER, POOL, vec![]),
            mk_swap("c", 102, ATTACKER, POOL, vec![]),
        ])
        .await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn ignores_logs_fallback_no_signer() {
        let out = drive(vec![
            mk_swap("a", 100, "", "raydium-v4-unknown", vec![]),
            mk_swap("b", 101, "", "raydium-v4-unknown", vec![]),
            mk_swap("c", 102, "", "raydium-v4-unknown", vec![]),
        ])
        .await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn topology_only_gives_base_confidence() {
        // No deltas — pure topology match. Confidence should be exactly 50.
        let out = drive(vec![
            mk_swap("front", 100, ATTACKER, POOL, vec![]),
            mk_swap("victim", 101, VICTIM, POOL, vec![]),
            mk_swap("back", 102, ATTACKER, POOL, vec![]),
        ])
        .await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].confidence, 50);
        assert_eq!(out[0].profit_lamports, None);
    }

    #[tokio::test]
    async fn multi_victim_bracket_emits_one_per_victim() {
        // Codex #5: A → V1 → V2 → B should produce two Sandwich rows.
        let v2 = "Victim2222222222222222222222222222222222222";
        let out = drive(vec![
            mk_swap("front", 100, ATTACKER, POOL, vec![delta(WSOL_MINT, -1_000_000_000), delta("T", 500_000)]),
            mk_swap("victim1", 101, VICTIM, POOL, vec![delta(WSOL_MINT, -500_000_000), delta("T", 200_000)]),
            mk_swap("victim2", 101, v2, POOL, vec![delta(WSOL_MINT, -700_000_000), delta("T", 280_000)]),
            mk_swap("back", 102, ATTACKER, POOL, vec![delta(WSOL_MINT, 1_500_000_000), delta("T", -500_000)]),
        ])
        .await;
        assert_eq!(out.len(), 2, "two victims => two sandwich rows");
        let victims: Vec<_> = out.iter().map(|s| s.victim_signer.as_str()).collect();
        assert!(victims.contains(&VICTIM));
        assert!(victims.contains(&v2));
        for s in &out {
            assert_eq!(s.front.signature, "front");
            assert_eq!(s.back.signature, "back");
            assert_eq!(s.attacker, ATTACKER);
        }
    }

    #[tokio::test]
    async fn profit_subtracts_fees() {
        // Codex #1: profit = (-1B + 1.2B) - (front_fee + back_fee) = 200M - 50M - 50M = 100M lamports
        let mut front = mk_swap("front", 100, ATTACKER, POOL, vec![delta(WSOL_MINT, -1_000_000_000), delta("T", 500_000)]);
        front.fee_lamports = 50_000_000;
        let victim = mk_swap("victim", 101, VICTIM, POOL, vec![delta(WSOL_MINT, -2_000_000_000), delta("T", 800_000)]);
        let mut back = mk_swap("back", 102, ATTACKER, POOL, vec![delta(WSOL_MINT, 1_200_000_000), delta("T", -500_000)]);
        back.fee_lamports = 50_000_000;
        let out = drive(vec![front, victim, back]).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].profit_lamports, Some(100_000_000), "profit must net out fees");
    }
}
