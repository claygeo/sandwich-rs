use std::collections::HashMap;

use serde::Serialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize)]
pub struct Swap {
    pub signature: String,
    pub slot: u64,
    pub signer: String,
    pub pool: String,
    pub dex: String,
    /// Network fee in lamports (base + priority). Subtracted from WSOL profit calc
    /// so a $5 sandwich with $0.50 priority fee doesn't look like a loss.
    pub fee_lamports: u64,
    /// Token amount delta for the signer, in lamports/smallest unit.
    /// Positive means signer received this mint; negative means signer paid it out.
    pub deltas: Vec<TokenDelta>,
    pub raw_logs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenDelta {
    pub mint: String,
    pub delta: i128,
    pub decimals: u8,
}

const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const RAYDIUM_AMM_V4_AUTHORITY: &str = "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1";

/// Per Raydium V4 swap instruction layout (IDL): accounts[1] is the AMM pool ID.
/// Reference: https://github.com/raydium-io/raydium-amm/blob/master/program/src/instruction.rs
const RAYDIUM_V4_SWAP_AMM_ACCOUNT_INDEX: usize = 1;

pub async fn run(mut rx: mpsc::Receiver<Message>, tx: mpsc::Sender<Swap>) -> anyhow::Result<()> {
    while let Some(msg) = rx.recv().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => match std::str::from_utf8(&b) {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            },
            _ => continue,
        };

        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                warn!(err = %e, "json parse failed");
                continue;
            }
        };

        let Some(method) = v.get("method").and_then(|m| m.as_str()) else {
            continue;
        };

        let swap = match method {
            "transactionNotification" => parse_helius_tx(&v),
            "logsNotification" => parse_logs(&v),
            _ => continue,
        };

        if let Some(swap) = swap {
            debug!(
                sig = %swap.signature,
                slot = swap.slot,
                signer = %swap.signer,
                pool = %swap.pool,
                "swap candidate"
            );
            if tx.send(swap).await.is_err() {
                break;
            }
        }
    }
    Ok(())
}

/// Helius `transactionNotification` — parsed transaction with pre/post balances.
fn parse_helius_tx(v: &serde_json::Value) -> Option<Swap> {
    let result = v.pointer("/params/result")?;
    let signature = result
        .pointer("/signature")
        .and_then(|s| s.as_str())?
        .to_string();
    let slot = result.pointer("/slot").and_then(|s| s.as_u64())?;

    let tx = result.pointer("/transaction")?;
    let meta = tx.pointer("/meta")?;

    if meta.get("err").is_some_and(|e| !e.is_null()) {
        return None;
    }

    let account_keys_node = tx.pointer("/transaction/message/accountKeys")?;
    let account_keys = extract_account_keys(account_keys_node)?;
    if account_keys.is_empty() {
        return None;
    }
    let signer = account_keys[0].clone();

    // Pool extraction (correct):
    //   1. Walk the transaction's instructions.
    //   2. Find the one whose programIdIndex resolves to Raydium V4.
    //   3. Take its accounts[RAYDIUM_V4_SWAP_AMM_ACCOUNT_INDEX]; resolve via accountKeys.
    // Falls back to the previous heuristic if no Raydium ix is found (e.g., a CPI from
    // an aggregator that wraps the swap inside another program).
    let pool = extract_raydium_pool(tx, &account_keys)
        .or_else(|| pick_pool_heuristic(&account_keys, &signer))?;

    let fee_lamports = meta
        .get("fee")
        .and_then(|f| f.as_u64())
        .unwrap_or_default();
    let deltas = compute_signer_deltas(meta, &signer, &account_keys);

    let logs = meta
        .get("logMessages")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Some(Swap {
        signature,
        slot,
        signer,
        pool,
        dex: "raydium-v4".into(),
        fee_lamports,
        deltas,
        raw_logs: logs,
    })
}

/// Walk outer instructions; find any whose program is Raydium V4; return the AMM ID
/// account per the IDL layout. Falls back to inner instructions if no outer match.
fn extract_raydium_pool(tx: &serde_json::Value, account_keys: &[String]) -> Option<String> {
    let outer = tx
        .pointer("/transaction/message/instructions")
        .and_then(|i| i.as_array());
    if let Some(found) = outer.and_then(|ixs| find_raydium_amm_in(ixs, account_keys)) {
        return Some(found);
    }
    // Try inner instructions (CPI invokes via Jupiter / aggregators).
    let inner_groups = tx
        .pointer("/meta/innerInstructions")
        .and_then(|i| i.as_array())?;
    for group in inner_groups {
        let ixs = group.get("instructions").and_then(|i| i.as_array())?;
        if let Some(found) = find_raydium_amm_in(ixs, account_keys) {
            return Some(found);
        }
    }
    None
}

fn find_raydium_amm_in(
    ixs: &[serde_json::Value],
    account_keys: &[String],
) -> Option<String> {
    for ix in ixs {
        let program_id = match ix.get("programId").and_then(|p| p.as_str()) {
            Some(s) => s.to_string(),
            None => {
                let idx = ix.get("programIdIndex").and_then(|i| i.as_u64())? as usize;
                account_keys.get(idx).cloned()?
            }
        };
        if program_id != RAYDIUM_AMM_V4 {
            continue;
        }
        let accounts = ix.get("accounts").and_then(|a| a.as_array())?;
        let amm = accounts.get(RAYDIUM_V4_SWAP_AMM_ACCOUNT_INDEX)?;
        if let Some(s) = amm.as_str() {
            return Some(s.to_string());
        }
        if let Some(idx) = amm.as_u64() {
            return account_keys.get(idx as usize).cloned();
        }
    }
    None
}

/// Public `logsSubscribe` — no balances, no signer. Topology-only fallback.
fn parse_logs(v: &serde_json::Value) -> Option<Swap> {
    let value = v.pointer("/params/result/value")?;
    let context = v.pointer("/params/result/context")?;
    if value.get("err").is_some_and(|e| !e.is_null()) {
        return None;
    }
    let signature = value
        .get("signature")
        .and_then(|s| s.as_str())?
        .to_string();
    let slot = context.get("slot").and_then(|s| s.as_u64())?;
    let logs = value
        .get("logs")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Some(Swap {
        signature,
        slot,
        signer: String::new(),
        pool: "raydium-v4-unknown".into(),
        dex: "raydium-v4".into(),
        fee_lamports: 0,
        deltas: Vec::new(),
        raw_logs: logs,
    })
}

fn extract_account_keys(node: &serde_json::Value) -> Option<Vec<String>> {
    let arr = node.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for k in arr {
        if let Some(s) = k.as_str() {
            out.push(s.to_string());
        } else if let Some(s) = k.get("pubkey").and_then(|p| p.as_str()) {
            out.push(s.to_string());
        } else {
            return None;
        }
    }
    Some(out)
}

fn pick_pool_heuristic(account_keys: &[String], signer: &str) -> Option<String> {
    account_keys
        .iter()
        .find(|k| {
            k.as_str() != signer
                && k.as_str() != RAYDIUM_AMM_V4
                && k.as_str() != RAYDIUM_AMM_V4_AUTHORITY
                && !is_known_program(k)
        })
        .cloned()
}

fn is_known_program(addr: &str) -> bool {
    matches!(
        addr,
        "11111111111111111111111111111111"
            | "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
            | "ComputeBudget111111111111111111111111111111"
            | "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
            | "SysvarRent111111111111111111111111111111111"
            | "SysvarC1ock11111111111111111111111111111111"
            | "Vote111111111111111111111111111111111111111"
            | "BPFLoaderUpgradeab1e11111111111111111111111"
            | "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
            | "5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"   // Raydium V4 AMM authority
            | "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin"   // Serum DEX V3
            | "srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX"    // Serum legacy
            | "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4"    // Jupiter v6
            | "JUP4Fb2cqiRUcaTHdrPC8h2gNsA2ETXiPDD33WcGuJB"    // Jupiter v4
            | "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"    // Orca Whirlpool
    )
}

/// Compute per-mint balance delta for the signer's accounts.
/// Returns one TokenDelta per mint where the signer's owned token account changed.
fn compute_signer_deltas(
    meta: &serde_json::Value,
    signer: &str,
    account_keys: &[String],
) -> Vec<TokenDelta> {
    let pre = meta
        .get("preTokenBalances")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();
    let post = meta
        .get("postTokenBalances")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();

    let mut by_account: HashMap<usize, (Option<&serde_json::Value>, Option<&serde_json::Value>)> =
        HashMap::new();
    for entry in pre.iter() {
        if let Some(idx) = entry.get("accountIndex").and_then(|i| i.as_u64()) {
            by_account.entry(idx as usize).or_insert((None, None)).0 = Some(entry);
        }
    }
    for entry in post.iter() {
        if let Some(idx) = entry.get("accountIndex").and_then(|i| i.as_u64()) {
            by_account.entry(idx as usize).or_insert((None, None)).1 = Some(entry);
        }
    }

    let mut deltas = Vec::new();
    for (_idx, (pre_e, post_e)) in by_account {
        let owner = post_e
            .or(pre_e)
            .and_then(|e| e.get("owner"))
            .and_then(|o| o.as_str())
            .unwrap_or("");
        if owner != signer {
            // Some Helius shapes report owner as an index; fall back if needed.
            if let Some(owner_idx) = post_e
                .or(pre_e)
                .and_then(|e| e.get("owner"))
                .and_then(|o| o.as_u64())
            {
                if account_keys.get(owner_idx as usize).map(|s| s.as_str()) != Some(signer) {
                    continue;
                }
            } else {
                continue;
            }
        }
        let mint = post_e
            .or(pre_e)
            .and_then(|e| e.get("mint"))
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        if mint.is_empty() {
            continue;
        }
        let pre_amt = parse_token_amount(pre_e).unwrap_or(0);
        let post_amt = parse_token_amount(post_e).unwrap_or(0);
        let decimals = post_e
            .or(pre_e)
            .and_then(|e| e.pointer("/uiTokenAmount/decimals"))
            .and_then(|d| d.as_u64())
            .unwrap_or(0) as u8;
        let delta = post_amt - pre_amt;
        if delta != 0 {
            deltas.push(TokenDelta {
                mint,
                delta,
                decimals,
            });
        }
    }
    deltas
}

fn parse_token_amount(entry: Option<&serde_json::Value>) -> Option<i128> {
    let s = entry?
        .pointer("/uiTokenAmount/amount")
        .and_then(|a| a.as_str())?;
    s.parse::<i128>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_logs_notification() {
        let fixture = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "logsNotification",
            "params": {
                "result": {
                    "context": { "slot": 250_000_000_u64 },
                    "value": {
                        "signature": "5j7s8vTsample",
                        "err": null,
                        "logs": [
                            "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke [1]",
                            "Program log: ray_log: ...",
                            "Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 success"
                        ]
                    },
                    "subscription": 1
                }
            }
        })
        .to_string();

        let (raw_tx, raw_rx) = mpsc::channel::<Message>(8);
        let (swap_tx, mut swap_rx) = mpsc::channel::<Swap>(8);
        raw_tx.send(Message::Text(fixture)).await.unwrap();
        drop(raw_tx);

        let h = tokio::spawn(run(raw_rx, swap_tx));
        let swap = swap_rx.recv().await.expect("swap emitted");
        h.await.unwrap().unwrap();

        assert_eq!(swap.slot, 250_000_000);
        assert_eq!(swap.signature, "5j7s8vTsample");
        assert_eq!(swap.dex, "raydium-v4");
        assert_eq!(swap.raw_logs.len(), 3);
        assert!(swap.signer.is_empty(), "logs fallback has no signer");
    }

    #[tokio::test]
    async fn drops_failed_transactions() {
        let fixture = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "logsNotification",
            "params": {
                "result": {
                    "context": { "slot": 1 },
                    "value": {
                        "signature": "fail",
                        "err": { "InstructionError": [0, "Custom"] },
                        "logs": []
                    },
                    "subscription": 1
                }
            }
        })
        .to_string();

        let (raw_tx, raw_rx) = mpsc::channel::<Message>(8);
        let (swap_tx, mut swap_rx) = mpsc::channel::<Swap>(8);
        raw_tx.send(Message::Text(fixture)).await.unwrap();
        drop(raw_tx);

        let h = tokio::spawn(run(raw_rx, swap_tx));
        h.await.unwrap().unwrap();
        assert!(swap_rx.try_recv().is_err(), "failed tx must be dropped");
    }

    #[tokio::test]
    async fn parses_helius_transaction_notification() {
        let signer = "AttackerPubkey11111111111111111111111111111";
        let pool = "PoolAccount22222222222222222222222222222222";
        let mint = "So11111111111111111111111111111111111111112";
        let fixture = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "transactionNotification",
            "params": {
                "subscription": 1,
                "result": {
                    "signature": "helius_test_sig",
                    "slot": 250_000_001_u64,
                    "transaction": {
                        "transaction": {
                            "message": {
                                "accountKeys": [
                                    { "pubkey": signer, "signer": true, "writable": true, "source": "transaction" },
                                    { "pubkey": pool,   "signer": false, "writable": true, "source": "transaction" },
                                    { "pubkey": "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8", "signer": false, "writable": false, "source": "transaction" }
                                ]
                            }
                        },
                        "meta": {
                            "err": null,
                            "preTokenBalances": [
                                { "accountIndex": 0, "mint": mint, "owner": signer, "uiTokenAmount": { "amount": "1000000000", "decimals": 9 } }
                            ],
                            "postTokenBalances": [
                                { "accountIndex": 0, "mint": mint, "owner": signer, "uiTokenAmount": { "amount": "500000000", "decimals": 9 } }
                            ],
                            "logMessages": ["Program log: swap"]
                        }
                    }
                }
            }
        })
        .to_string();

        let (raw_tx, raw_rx) = mpsc::channel::<Message>(8);
        let (swap_tx, mut swap_rx) = mpsc::channel::<Swap>(8);
        raw_tx.send(Message::Text(fixture)).await.unwrap();
        drop(raw_tx);

        let h = tokio::spawn(run(raw_rx, swap_tx));
        let swap = swap_rx.recv().await.expect("swap emitted");
        h.await.unwrap().unwrap();

        assert_eq!(swap.signature, "helius_test_sig");
        assert_eq!(swap.slot, 250_000_001);
        assert_eq!(swap.signer, signer);
        assert_eq!(swap.pool, pool);
        assert_eq!(swap.deltas.len(), 1);
        assert_eq!(swap.deltas[0].mint, mint);
        assert_eq!(swap.deltas[0].delta, -500_000_000_i128);
    }
}
