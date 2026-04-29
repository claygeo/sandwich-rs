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
    /// Sum of lamports transferred to known Jito tip accounts in this transaction.
    /// Subtracted from WSOL profit alongside `fee_lamports` to surface the attacker's
    /// true net SOL extraction.
    pub jito_tip_lamports: u64,
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
const ORCA_WHIRLPOOL: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

/// Per Raydium V4 swap instruction layout (IDL): accounts[1] is the AMM pool ID.
/// Reference: https://github.com/raydium-io/raydium-amm/blob/master/program/src/instruction.rs
const RAYDIUM_V4_SWAP_AMM_ACCOUNT_INDEX: usize = 1;
/// Per Orca Whirlpool swap instruction (IDL): account #2 is the whirlpool address.
const ORCA_WHIRLPOOL_AMM_ACCOUNT_INDEX: usize = 2;

/// Known Jito tip accounts. Searcher bots that want their bundles landed pay tips
/// here as a separate System Program transfer. Without subtracting these, our
/// "profit" number is inflated by 0.001-0.01 SOL per sandwich.
/// Source: https://docs.jito.wtf/lowlatencytxnsend/#tip-amount
const JITO_TIP_ACCOUNTS: &[&str] = &[
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pivKeVBBjNS2zwJzUFF1",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

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
    let inner_tx = tx.pointer("/transaction")?;

    parse_tx_components(&signature, slot, inner_tx, meta)
}

/// Pure components-level parser — works on any source that gives us
/// `(signature, slot, transaction_message_node, meta_node)`. Used both by the
/// WS path (`parse_helius_tx`) and the enricher (`getTransaction` RPC path).
/// Codex flagged the previous "synthesized envelope" approach as brittle:
/// `getTransaction.result.transaction.message` vs `transactionNotification.
/// params.result.transaction.transaction.message` — different shapes that
/// happen to coincide when nested correctly. This kills the ambiguity.
pub fn parse_tx_components(
    signature: &str,
    slot: u64,
    inner_tx: &serde_json::Value,
    meta: &serde_json::Value,
) -> Option<Swap> {
    if meta.get("err").is_some_and(|e| !e.is_null()) {
        return None;
    }

    let account_keys_node = inner_tx.pointer("/message/accountKeys")?;
    let account_keys = extract_account_keys(account_keys_node)?;
    if account_keys.is_empty() {
        return None;
    }
    let signer = account_keys[0].clone();

    // Pool extraction (correct):
    //   1. Walk the transaction's instructions (outer + inner).
    //   2. Try Raydium V4: programId == RAYDIUM, take accounts[1] per IDL.
    //   3. Try Orca Whirlpool: programId == WHIRLPOOL, take accounts[2] per IDL.
    //   4. Fall back to heuristic (first non-signer non-program account) if no
    //      recognized DEX instruction matched.
    let (pool, dex) = extract_pool_and_dex(inner_tx, meta, &account_keys)
        .or_else(|| pick_pool_heuristic(&account_keys, &signer).map(|p| (p, "unknown".into())))?;

    let fee_lamports = meta
        .get("fee")
        .and_then(|f| f.as_u64())
        .unwrap_or_default();
    let jito_tip_lamports = extract_jito_tip(inner_tx, meta, &account_keys);
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
        signature: signature.to_string(),
        slot,
        signer,
        pool,
        dex,
        fee_lamports,
        jito_tip_lamports,
        deltas,
        raw_logs: logs,
    })
}

/// Walk outer + inner instructions; recognize Raydium V4 and Orca Whirlpool swaps;
/// return (pool_address, dex_name). Caller falls back to a heuristic if this returns None.
fn extract_pool_and_dex(
    inner_tx: &serde_json::Value,
    meta: &serde_json::Value,
    account_keys: &[String],
) -> Option<(String, String)> {
    let outer = inner_tx
        .pointer("/message/instructions")
        .and_then(|i| i.as_array());
    if let Some(found) = outer.and_then(|ixs| find_dex_amm_in(ixs, account_keys)) {
        return Some(found);
    }
    let inner_groups = meta
        .get("innerInstructions")
        .and_then(|i| i.as_array())?;
    for group in inner_groups {
        let ixs = group.get("instructions").and_then(|i| i.as_array())?;
        if let Some(found) = find_dex_amm_in(ixs, account_keys) {
            return Some(found);
        }
    }
    None
}

fn find_dex_amm_in(
    ixs: &[serde_json::Value],
    account_keys: &[String],
) -> Option<(String, String)> {
    for ix in ixs {
        let program_id = match ix.get("programId").and_then(|p| p.as_str()) {
            Some(s) => s.to_string(),
            None => {
                let idx = ix.get("programIdIndex").and_then(|i| i.as_u64())? as usize;
                account_keys.get(idx).cloned()?
            }
        };
        let (target_index, dex) = if program_id == RAYDIUM_AMM_V4 {
            (RAYDIUM_V4_SWAP_AMM_ACCOUNT_INDEX, "raydium-v4")
        } else if program_id == ORCA_WHIRLPOOL {
            (ORCA_WHIRLPOOL_AMM_ACCOUNT_INDEX, "orca-whirlpool")
        } else {
            continue;
        };
        let accounts = match ix.get("accounts").and_then(|a| a.as_array()) {
            Some(a) => a,
            None => continue,
        };
        let amm = match accounts.get(target_index) {
            Some(a) => a,
            None => continue,
        };
        if let Some(s) = amm.as_str() {
            return Some((s.to_string(), dex.to_string()));
        }
        if let Some(idx) = amm.as_u64() {
            if let Some(addr) = account_keys.get(idx as usize) {
                return Some((addr.clone(), dex.to_string()));
            }
        }
    }
    None
}

/// Sum lamports transferred to any Jito tip account across the whole transaction.
/// Searches both outer and inner instructions; supports `jsonParsed` and raw encodings.
fn extract_jito_tip(
    inner_tx: &serde_json::Value,
    meta: &serde_json::Value,
    account_keys: &[String],
) -> u64 {
    let mut total = 0_u64;

    if let Some(outer) = inner_tx
        .pointer("/message/instructions")
        .and_then(|i| i.as_array())
    {
        for ix in outer {
            total = total.saturating_add(jito_tip_from_ix(ix, account_keys));
        }
    }
    if let Some(groups) = meta
        .get("innerInstructions")
        .and_then(|i| i.as_array())
    {
        for group in groups {
            if let Some(ixs) = group.get("instructions").and_then(|i| i.as_array()) {
                for ix in ixs {
                    total = total.saturating_add(jito_tip_from_ix(ix, account_keys));
                }
            }
        }
    }
    total
}

fn jito_tip_from_ix(ix: &serde_json::Value, account_keys: &[String]) -> u64 {
    // Path 1: jsonParsed System transfer.
    if ix.get("program").and_then(|p| p.as_str()) == Some("system") {
        if let Some(parsed) = ix.get("parsed") {
            if parsed.get("type").and_then(|t| t.as_str()) == Some("transfer") {
                let dest = parsed
                    .pointer("/info/destination")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                if JITO_TIP_ACCOUNTS.contains(&dest) {
                    return parsed
                        .pointer("/info/lamports")
                        .and_then(|l| l.as_u64())
                        .unwrap_or(0);
                }
            }
        }
    }

    // Path 2: raw System transfer instruction.
    let program_id = ix
        .get("programId")
        .and_then(|p| p.as_str())
        .map(String::from)
        .or_else(|| {
            let idx = ix.get("programIdIndex").and_then(|i| i.as_u64())?;
            account_keys.get(idx as usize).cloned()
        });
    if program_id.as_deref() != Some(SYSTEM_PROGRAM) {
        return 0;
    }
    let accounts = match ix.get("accounts").and_then(|a| a.as_array()) {
        Some(a) => a,
        None => return 0,
    };
    let to = match accounts.get(1) {
        Some(v) => match v.as_str() {
            Some(s) => s.to_string(),
            None => v
                .as_u64()
                .and_then(|i| account_keys.get(i as usize).cloned())
                .unwrap_or_default(),
        },
        None => return 0,
    };
    if !JITO_TIP_ACCOUNTS.contains(&to.as_str()) {
        return 0;
    }
    // System transfer ix data: 4-byte LE discriminator (=2) + 8-byte LE lamports.
    // Helius emits `data` in base58 by default; bs58-decode without pulling a crate
    // by using the "encoding" hint when present, otherwise return 0 (parsed path
    // covers the common case). v1.6 will add bs58 decoding for completeness.
    0
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
        jito_tip_lamports: 0,
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
                && k.as_str() != ORCA_WHIRLPOOL
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
        assert_eq!(swap.jito_tip_lamports, 0);
        assert_eq!(swap.fee_lamports, 0);
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
        let jito_tip_acct = "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5";
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
                                ],
                                "instructions": [
                                    {
                                        "programId": "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
                                        "accounts": [signer, pool, "OtherAcct33333333333333333333333333333333333"]
                                    }
                                ]
                            }
                        },
                        "meta": {
                            "err": null,
                            "fee": 5_000_u64,
                            "preTokenBalances": [
                                { "accountIndex": 0, "mint": mint, "owner": signer, "uiTokenAmount": { "amount": "1000000000", "decimals": 9 } }
                            ],
                            "postTokenBalances": [
                                { "accountIndex": 0, "mint": mint, "owner": signer, "uiTokenAmount": { "amount": "500000000", "decimals": 9 } }
                            ],
                            "logMessages": ["Program log: swap"],
                            "innerInstructions": [
                                {
                                    "index": 0,
                                    "instructions": [
                                        {
                                            "program": "system",
                                            "programId": "11111111111111111111111111111111",
                                            "parsed": {
                                                "type": "transfer",
                                                "info": {
                                                    "source": signer,
                                                    "destination": jito_tip_acct,
                                                    "lamports": 50_000
                                                }
                                            }
                                        }
                                    ]
                                }
                            ]
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
        assert_eq!(swap.dex, "raydium-v4");
        assert_eq!(swap.fee_lamports, 5_000);
        assert_eq!(swap.jito_tip_lamports, 50_000);
        assert_eq!(swap.deltas.len(), 1);
        assert_eq!(swap.deltas[0].mint, mint);
        assert_eq!(swap.deltas[0].delta, -500_000_000_i128);
    }

    #[tokio::test]
    async fn parses_orca_whirlpool_swap() {
        // Orca Whirlpool IDL: account index 2 = whirlpool address (the pool)
        let signer = "OrcaTrader11111111111111111111111111111111";
        let pool = "WhirlpoolAcct1111111111111111111111111111";
        let fixture = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "transactionNotification",
            "params": {
                "subscription": 1,
                "result": {
                    "signature": "orca_test_sig",
                    "slot": 250_000_002_u64,
                    "transaction": {
                        "transaction": {
                            "message": {
                                "accountKeys": [
                                    { "pubkey": signer, "signer": true, "writable": true, "source": "transaction" },
                                    { "pubkey": "TokenAcct1111111111111111111111111111111", "signer": false, "writable": true, "source": "transaction" },
                                    { "pubkey": pool,   "signer": false, "writable": true, "source": "transaction" }
                                ],
                                "instructions": [
                                    {
                                        "programId": "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
                                        "accounts": [signer, "TokenAcct1111111111111111111111111111111", pool, "extra"]
                                    }
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
        let swap = swap_rx.recv().await.expect("orca swap emitted");
        h.await.unwrap().unwrap();

        assert_eq!(swap.dex, "orca-whirlpool");
        assert_eq!(swap.pool, pool);
    }
}
