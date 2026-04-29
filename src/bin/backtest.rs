// sandwich-rs backtest harness
// Reads a CSV of labeled known sandwiches, replays them through the detector,
// reports precision/recall.
//
// CSV format (header row required):
//   case_type,victim_sig,front_sig,back_sig,attacker,victim_signer,pool,front_slot,back_slot,profit_sol_expected,notes
//
// case_type: "positive" (should be detected) or "negative" (should NOT be detected)
// profit_sol_expected: optional, comma-skip with empty string if unknown
//
// Run:
//   cargo run --bin backtest -- fixtures/known-sandwiches.csv
//
// Ship gate: precision >= 70%, recall >= 50% on >=30 positive cases.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use tokio::sync::mpsc;

use sandwich_rs::detector::{self, PoolState, Sandwich};
use sandwich_rs::parser::{Swap, TokenDelta};

const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[derive(Debug, serde::Deserialize)]
struct Row {
    case_type: String,
    victim_sig: String,
    front_sig: String,
    back_sig: String,
    attacker: String,
    victim_signer: String,
    pool: String,
    front_slot: u64,
    back_slot: u64,
    #[serde(default)]
    profit_sol_expected: Option<f64>,
    #[serde(default)]
    notes: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/known-sandwiches.csv".to_string());

    match run(&path).await {
        Ok(report) => {
            println!("\n{}", report);
            if report.passes_ship_gate() {
                println!("\nSHIP GATE: PASS\n");
                ExitCode::SUCCESS
            } else {
                println!("\nSHIP GATE: FAIL  (need precision ≥0.70, recall ≥0.50, n_positive ≥30)\n");
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("backtest failed: {e:?}");
            ExitCode::from(2)
        }
    }
}

async fn run(path: &str) -> Result<Report> {
    if !Path::new(path).exists() {
        anyhow::bail!(
            "fixture not found: {}\n\
             Create it with header:\n\
             case_type,victim_sig,front_sig,back_sig,attacker,victim_signer,pool,front_slot,back_slot,profit_sol_expected,notes\n\
             then add ≥30 positive rows scraped from Sandwiched.wtf or similar.",
            path
        );
    }

    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .context("open csv")?;

    let mut positives: Vec<Row> = Vec::new();
    let mut negatives: Vec<Row> = Vec::new();
    for r in rdr.deserialize::<Row>() {
        let r = r.context("parse row")?;
        match r.case_type.to_lowercase().as_str() {
            "positive" | "pos" | "true" | "1" => positives.push(r),
            "negative" | "neg" | "false" | "0" => negatives.push(r),
            other => {
                eprintln!("warn: unknown case_type {other:?} on victim_sig={}", r.victim_sig);
            }
        }
    }

    let mut tp = 0;
    let mut fn_ = 0;
    let mut fp = 0;
    let mut tn = 0;
    let mut details: Vec<String> = Vec::new();

    for row in &positives {
        let detected = detect_one(row).await?;
        if let Some(s) = &detected {
            if s.attacker == row.attacker
                && s.front.signature == row.front_sig
                && s.back.signature == row.back_sig
            {
                tp += 1;
                details.push(format!("  ✓ TP {} (conf {})", row.victim_sig, s.confidence));
            } else {
                fn_ += 1;
                details.push(format!(
                    "  ✗ FN {} (detected wrong triple — attacker={}, conf={})",
                    row.victim_sig, s.attacker, s.confidence
                ));
            }
        } else {
            fn_ += 1;
            details.push(format!("  ✗ FN {} (no detection)", row.victim_sig));
        }
    }

    for row in &negatives {
        let detected = detect_one(row).await?;
        if detected.is_some() {
            fp += 1;
            details.push(format!(
                "  ✗ FP {} ({})",
                row.victim_sig,
                row.notes.as_deref().unwrap_or("")
            ));
        } else {
            tn += 1;
            details.push(format!(
                "  ✓ TN {} ({})",
                row.victim_sig,
                row.notes.as_deref().unwrap_or("")
            ));
        }
    }

    let n_pos = positives.len();
    let n_neg = negatives.len();
    let precision = if tp + fp == 0 {
        0.0
    } else {
        tp as f64 / (tp + fp) as f64
    };
    let recall = if tp + fn_ == 0 {
        0.0
    } else {
        tp as f64 / (tp + fn_) as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };

    Ok(Report {
        n_positive: n_pos,
        n_negative: n_neg,
        tp,
        fp,
        fn_,
        tn,
        precision,
        recall,
        f1,
        details,
    })
}

async fn detect_one(row: &Row) -> Result<Option<Sandwich>> {
    let state: PoolState = Arc::new(DashMap::new());
    let (in_tx, in_rx) = mpsc::channel::<Swap>(8);
    let (out_tx, mut out_rx) = mpsc::channel::<Sandwich>(8);

    let h = tokio::spawn(detector::run(state, in_rx, out_tx));

    let mid_slot = (row.front_slot + row.back_slot) / 2;
    let token_mint = "TokenMint000000000000000000000000000000000000".to_string();

    // Synthetic deltas: front buys TOKEN with WSOL, back sells TOKEN for WSOL.
    let profit_lamports = row
        .profit_sol_expected
        .map(|sol| (sol * 1_000_000_000.0) as i128)
        .unwrap_or(0);
    let front_wsol = -1_000_000_000_i128; // attacker spends 1 SOL to buy TOKEN
    let back_wsol = 1_000_000_000_i128 + profit_lamports;

    in_tx
        .send(Swap {
            signature: row.front_sig.clone(),
            slot: row.front_slot,
            signer: row.attacker.clone(),
            pool: row.pool.clone(),
            dex: "raydium-v4".into(),
            fee_lamports: 0,
            deltas: vec![
                delta(WSOL_MINT, front_wsol),
                delta(&token_mint, 500_000),
            ],
            raw_logs: vec![],
        })
        .await?;
    in_tx
        .send(Swap {
            signature: row.victim_sig.clone(),
            slot: mid_slot,
            signer: row.victim_signer.clone(),
            pool: row.pool.clone(),
            dex: "raydium-v4".into(),
            fee_lamports: 0,
            deltas: vec![
                delta(WSOL_MINT, -2_000_000_000),
                delta(&token_mint, 800_000),
            ],
            raw_logs: vec![],
        })
        .await?;
    in_tx
        .send(Swap {
            signature: row.back_sig.clone(),
            slot: row.back_slot,
            signer: row.attacker.clone(),
            pool: row.pool.clone(),
            dex: "raydium-v4".into(),
            fee_lamports: 0,
            deltas: vec![
                delta(WSOL_MINT, back_wsol),
                delta(&token_mint, -500_000),
            ],
            raw_logs: vec![],
        })
        .await?;
    drop(in_tx);
    h.await??;

    Ok(out_rx.try_recv().ok())
}

fn delta(mint: &str, d: i128) -> TokenDelta {
    TokenDelta {
        mint: mint.into(),
        delta: d,
        decimals: 9,
    }
}

struct Report {
    n_positive: usize,
    n_negative: usize,
    tp: usize,
    fp: usize,
    fn_: usize,
    tn: usize,
    precision: f64,
    recall: f64,
    f1: f64,
    details: Vec<String>,
}

impl Report {
    fn passes_ship_gate(&self) -> bool {
        self.precision >= 0.70 && self.recall >= 0.50 && self.n_positive >= 30
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "===== sandwich-rs BACKTEST =====")?;
        writeln!(f, "positives:  {}", self.n_positive)?;
        writeln!(f, "negatives:  {}", self.n_negative)?;
        writeln!(f, "TP / FP / FN / TN: {} / {} / {} / {}", self.tp, self.fp, self.fn_, self.tn)?;
        writeln!(f, "precision:  {:.3}", self.precision)?;
        writeln!(f, "recall:     {:.3}", self.recall)?;
        writeln!(f, "F1:         {:.3}", self.f1)?;
        writeln!(f)?;
        writeln!(f, "--- per-case ---")?;
        for d in &self.details {
            writeln!(f, "{d}")?;
        }
        Ok(())
    }
}
