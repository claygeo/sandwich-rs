use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::detector::Sandwich;

pub async fn run(cfg: Config, mut rx: mpsc::Receiver<Sandwich>) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let enabled = cfg.telegram_bot_token.is_some() && cfg.telegram_chat_id.is_some();
    if !enabled {
        info!("telegram disabled (set TELEGRAM_BOT_TOKEN + TELEGRAM_CHAT_ID to enable)");
    }

    while let Some(s) = rx.recv().await {
        let sol = s
            .profit_lamports
            .map(|p| format!("{:.4}", p as f64 / 1_000_000_000.0))
            .unwrap_or_else(|| "—".into());

        let msg = format!(
            "Sandwich on {} (slot span {})\n\
             attacker: {}\n\
             pool: {}\n\
             profit: {} SOL  (confidence {})\n\
             https://solscan.io/tx/{} ← victim",
            s.dex, s.slot_span, s.attacker, s.pool, sol, s.confidence, s.victim.signature
        );
        info!("{}", msg);

        if let (Some(token), Some(chat_id)) = (&cfg.telegram_bot_token, &cfg.telegram_chat_id) {
            let url = format!("https://api.telegram.org/bot{token}/sendMessage");
            match client
                .post(&url)
                .json(&serde_json::json!({ "chat_id": chat_id, "text": msg }))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => warn!(status = %r.status(), "telegram non-2xx"),
                Err(e) => error!(err = ?e, "telegram send failed"),
            }
        }
    }
    Ok(())
}
