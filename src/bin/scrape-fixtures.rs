// sandwich-rs fixtures scraper
//
// Takes a CSV with columns `victim_sig,front_sig,back_sig` (and optional
// case_type / notes), looks up each transaction via Helius `getTransaction`,
// extracts attacker, pool, slots, fees, and writes the fully-populated CSV
// that backtest.rs consumes.
//
// Run:
//   HELIUS_API_KEY=... cargo run --release --bin scrape-fixtures -- \
//       fixtures/seed.csv fixtures/known-sandwiches.csv
//
// Seed CSV (operator-curated from sandwiched.wtf or solscan):
//   case_type,victim_sig,front_sig,back_sig,notes
//   positive,5j7s8...,xyz...,abc...,raydium-v4 sample 2026-04-28
//
// Output CSV (backtest.rs format):
//   case_type,victim_sig,front_sig,back_sig,attacker,victim_signer,pool,
//   front_slot,back_slot,profit_sol_expected,notes

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct SeedRow {
    case_type: String,
    victim_sig: String,
    front_sig: String,
    back_sig: String,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Serialize)]
struct OutRow {
    case_type: String,
    victim_sig: String,
    front_sig: String,
    back_sig: String,
    attacker: String,
    victim_signer: String,
    pool: String,
    front_slot: u64,
    back_slot: u64,
    profit_sol_expected: f64,
    notes: String,
}

#[derive(Deserialize)]
struct GetTransactionResp {
    result: Option<GetTransactionResult>,
}

#[derive(Deserialize)]
struct GetTransactionResult {
    slot: u64,
    transaction: GtTransaction,
    meta: GtMeta,
}

#[derive(Deserialize)]
struct GtTransaction {
    message: GtMessage,
}

#[derive(Deserialize)]
struct GtMessage {
    #[serde(rename = "accountKeys")]
    account_keys: Vec<serde_json::Value>,
    #[serde(default)]
    instructions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct GtMeta {
    #[serde(default)]
    err: Option<serde_json::Value>,
    #[serde(default)]
    fee: u64,
    #[serde(rename = "preTokenBalances", default)]
    pre_token_balances: Vec<serde_json::Value>,
    #[serde(rename = "postTokenBalances", default)]
    post_token_balances: Vec<serde_json::Value>,
}

const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const ORCA_WHIRLPOOL: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: scrape-fixtures <input.csv> <output.csv>\n  \
             input columns: case_type,victim_sig,front_sig,back_sig,notes"
        );
        return ExitCode::from(2);
    }

    let helius_key = std::env::var("HELIUS_API_KEY").unwrap_or_default();
    if helius_key.is_empty() {
        eprintln!("HELIUS_API_KEY env var required");
        return ExitCode::from(2);
    }
    let rpc_url = format!("https://mainnet.helius-rpc.com/?api-key={helius_key}");

    match run(&args[1], &args[2], &rpc_url).await {
        Ok((ok, fail)) => {
            println!("\nscrape complete: {ok} ok, {fail} failed");
            if ok == 0 {
                ExitCode::from(3)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("scrape failed: {e:?}");
            ExitCode::from(1)
        }
    }
}

async fn run(input: &str, output: &str, rpc_url: &str) -> Result<(usize, usize)> {
    if !Path::new(input).exists() {
        return Err(anyhow!("input not found: {input}"));
    }

    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(input)
        .context("open input csv")?;
    let mut wtr = csv::Writer::from_path(output).context("open output csv")?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let mut ok = 0;
    let mut fail = 0;
    for r in rdr.deserialize::<SeedRow>() {
        let r = r.context("parse seed row")?;
        match resolve(&client, rpc_url, &r).await {
            Ok(out) => {
                wtr.serialize(&out).context("write row")?;
                println!("  ok: {}", out.victim_sig);
                ok += 1;
            }
            Err(e) => {
                eprintln!("  FAIL {}: {:?}", r.victim_sig, e);
                fail += 1;
            }
        }
        // Rate-limit the free Helius tier
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    wtr.flush().context("flush csv")?;
    Ok((ok, fail))
}

async fn resolve(client: &reqwest::Client, rpc_url: &str, r: &SeedRow) -> Result<OutRow> {
    let front = get_tx(client, rpc_url, &r.front_sig).await.context("front")?;
    let victim = get_tx(client, rpc_url, &r.victim_sig).await.context("victim")?;
    let back = get_tx(client, rpc_url, &r.back_sig).await.context("back")?;

    let front_signer = signer_of(&front).context("front signer")?;
    let back_signer = signer_of(&back).context("back signer")?;
    let victim_signer = signer_of(&victim).context("victim signer")?;
    if front_signer != back_signer {
        return Err(anyhow!(
            "attacker mismatch: front={front_signer}, back={back_signer}"
        ));
    }
    if front_signer == victim_signer {
        return Err(anyhow!("attacker == victim — not a sandwich"));
    }

    let pool = pool_of(&front)
        .or_else(|| pool_of(&back))
        .or_else(|| pool_of(&victim))
        .ok_or_else(|| anyhow!("no recognizable DEX pool in any of the 3 transactions"))?;

    let front_wsol = wsol_delta(&front, &front_signer);
    let back_wsol = wsol_delta(&back, &back_signer);
    let profit_lamports =
        front_wsol + back_wsol - front.meta.fee as i128 - back.meta.fee as i128;
    let profit_sol = profit_lamports as f64 / 1_000_000_000.0;

    Ok(OutRow {
        case_type: r.case_type.clone(),
        victim_sig: r.victim_sig.clone(),
        front_sig: r.front_sig.clone(),
        back_sig: r.back_sig.clone(),
        attacker: front_signer,
        victim_signer,
        pool,
        front_slot: front.slot,
        back_slot: back.slot,
        profit_sol_expected: profit_sol,
        notes: r.notes.clone().unwrap_or_default(),
    })
}

async fn get_tx(
    client: &reqwest::Client,
    rpc_url: &str,
    sig: &str,
) -> Result<GetTransactionResult> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [sig, {
            "encoding": "jsonParsed",
            "maxSupportedTransactionVersion": 0,
            "commitment": "confirmed"
        }]
    });
    let resp: GetTransactionResp = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .context("rpc post")?
        .error_for_status()
        .context("rpc status")?
        .json()
        .await
        .context("rpc json")?;
    let res = resp
        .result
        .ok_or_else(|| anyhow!("no result for sig {sig}"))?;
    if res.meta.err.as_ref().is_some_and(|e| !e.is_null()) {
        return Err(anyhow!("transaction failed onchain"));
    }
    Ok(res)
}

fn signer_of(tx: &GetTransactionResult) -> Result<String> {
    let first = tx
        .transaction
        .message
        .account_keys
        .first()
        .ok_or_else(|| anyhow!("no accountKeys"))?;
    if let Some(s) = first.as_str() {
        return Ok(s.to_string());
    }
    if let Some(s) = first.get("pubkey").and_then(|p| p.as_str()) {
        return Ok(s.to_string());
    }
    Err(anyhow!("could not extract signer from accountKeys[0]"))
}

fn pool_of(tx: &GetTransactionResult) -> Option<String> {
    let keys: Vec<String> = tx
        .transaction
        .message
        .account_keys
        .iter()
        .filter_map(|k| {
            k.as_str()
                .map(String::from)
                .or_else(|| k.get("pubkey").and_then(|p| p.as_str()).map(String::from))
        })
        .collect();
    for ix in &tx.transaction.message.instructions {
        let prog = ix
            .get("programId")
            .and_then(|p| p.as_str())
            .map(String::from)
            .or_else(|| {
                ix.get("programIdIndex")
                    .and_then(|i| i.as_u64())
                    .and_then(|i| keys.get(i as usize).cloned())
            });
        let (target_idx, _name) = match prog.as_deref() {
            Some(p) if p == RAYDIUM_AMM_V4 => (1usize, "raydium-v4"),
            Some(p) if p == ORCA_WHIRLPOOL => (2usize, "orca-whirlpool"),
            _ => continue,
        };
        let accs = ix.get("accounts")?.as_array()?;
        let amm = accs.get(target_idx)?;
        if let Some(s) = amm.as_str() {
            return Some(s.to_string());
        }
        if let Some(idx) = amm.as_u64() {
            return keys.get(idx as usize).cloned();
        }
    }
    None
}

fn wsol_delta(tx: &GetTransactionResult, owner: &str) -> i128 {
    let pre_for = |b: &serde_json::Value| -> Option<(String, i128)> {
        let mint = b.get("mint")?.as_str()?.to_string();
        if mint != WSOL_MINT {
            return None;
        }
        let owner_str = b.get("owner")?.as_str()?;
        if owner_str != owner {
            return None;
        }
        let amt: i128 = b
            .pointer("/uiTokenAmount/amount")?
            .as_str()?
            .parse()
            .ok()?;
        Some((mint, amt))
    };
    let pre_amt: i128 = tx
        .meta
        .pre_token_balances
        .iter()
        .filter_map(pre_for)
        .map(|(_, a)| a)
        .sum();
    let post_amt: i128 = tx
        .meta
        .post_token_balances
        .iter()
        .filter_map(pre_for)
        .map(|(_, a)| a)
        .sum();
    post_amt - pre_amt
}
