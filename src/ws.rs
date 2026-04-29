use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

/// Subscribe to a list of programs over WS.
///
/// `use_helius=true` uses Helius's `transactionSubscribe` (parsed transactions inline,
/// no `getTransaction` followup needed). `use_helius=false` falls back to public
/// `logsSubscribe` and downstream stages must do RPC followups themselves.
pub async fn run(
    url: String,
    programs: Vec<String>,
    use_helius: bool,
    tx: mpsc::Sender<Message>,
) -> Result<()> {
    let mut backoff = Duration::from_secs(1);
    loop {
        let frame_received = Arc::new(AtomicBool::new(false));
        match connect_and_run(&url, &programs, use_helius, &tx, frame_received.clone()).await {
            Ok(()) => {
                info!("ws stream ended normally, reconnecting in 1s");
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                warn!(err = ?e, backoff_secs = backoff.as_secs(), "ws errored, reconnecting");
            }
        }
        tokio::time::sleep(backoff).await;
        // Codex #6: only escalate backoff if no real frames were received this cycle.
        // Otherwise a flapping connection that recovers briefly never gets back to the
        // healthy 1s cadence, and we miss minutes of mainnet on each blip.
        if frame_received.load(Ordering::Relaxed) {
            backoff = Duration::from_secs(1);
        } else {
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

async fn connect_and_run(
    url: &str,
    programs: &[String],
    use_helius: bool,
    tx: &mpsc::Sender<Message>,
    frame_received: Arc<AtomicBool>,
) -> Result<()> {
    let (ws, _) = connect_async(url).await.context("ws connect")?;
    info!(%url, helius = use_helius, "ws connected");
    let (mut write, mut read) = ws.split();

    if use_helius {
        let sub = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "transactionSubscribe",
            "params": [
                { "accountInclude": programs },
                {
                    "commitment": "processed",
                    "encoding": "jsonParsed",
                    "transactionDetails": "full",
                    "showRewards": false,
                    "maxSupportedTransactionVersion": 0
                }
            ]
        });
        write
            .send(Message::Text(sub.to_string()))
            .await
            .context("ws send transactionSubscribe")?;
        info!(programs = ?programs, "subscribed to Helius transactionSubscribe");
    } else {
        for (i, program) in programs.iter().enumerate() {
            let sub = serde_json::json!({
                "jsonrpc": "2.0",
                "id": i + 1,
                "method": "logsSubscribe",
                "params": [
                    { "mentions": [program] },
                    { "commitment": "processed" }
                ]
            });
            write
                .send(Message::Text(sub.to_string()))
                .await
                .context("ws send logsSubscribe")?;
            info!(%program, "subscribed to logsSubscribe");
        }
    }

    while let Some(msg) = read.next().await {
        let msg = msg.context("ws read")?;
        frame_received.store(true, Ordering::Relaxed);
        if let Message::Ping(p) = &msg {
            let _ = write.send(Message::Pong(p.clone())).await;
        }
        if tx.send(msg).await.is_err() {
            warn!("downstream parser dropped, exiting ws_reader");
            return Ok(());
        }
    }
    Ok(())
}
