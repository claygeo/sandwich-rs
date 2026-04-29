use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub ws_url: String,
    pub use_helius: bool,
    pub watched_programs: Vec<String>,
    pub db_url: Option<String>,
    pub http_bind: String,
    pub state_dir: PathBuf,
    pub enable_pyth: bool,
    /// HTTPS RPC URL used by the enricher (getTransaction followups). Built from
    /// HELIUS_API_KEY when set, falls back to public mainnet.
    pub rpc_url: String,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
}

const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const ORCA_WHIRLPOOL: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
const SOLANA_MAINNET_WS: &str = "wss://api.mainnet-beta.solana.com";

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let helius_key = env::var("HELIUS_API_KEY").ok().filter(|s| !s.is_empty());
        let explicit_ws = env::var("SANDWICH_WS_URL").ok().filter(|s| !s.is_empty());
        // Atlas (transactionSubscribe with parsed inner ix) is on Helius's paid tier.
        // Free tier gets standard WS at mainnet.helius-rpc.com with logsSubscribe.
        // Default OFF to keep free-tier operators running without 403 retry loops.
        let want_atlas = env::var("SANDWICH_USE_ATLAS")
            .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
            .unwrap_or(false);

        let (ws_url, use_helius) = match (explicit_ws, helius_key, want_atlas) {
            (Some(url), _, _) => (url, false),
            (None, Some(key), true) => (
                format!("wss://atlas-mainnet.helius-rpc.com/?api-key={key}"),
                true,
            ),
            (None, Some(key), false) => (
                format!("wss://mainnet.helius-rpc.com/?api-key={key}"),
                false,
            ),
            (None, None, _) => (SOLANA_MAINNET_WS.to_string(), false),
        };

        let watched_programs = env::var("WATCHED_PROGRAMS")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.split(',').map(|p| p.trim().to_string()).collect())
            .unwrap_or_else(|| {
                vec![RAYDIUM_AMM_V4.to_string(), ORCA_WHIRLPOOL.to_string()]
            });

        let db_url = env::var("SUPABASE_POOLER_URL")
            .ok()
            .or_else(|| env::var("SUPABASE_DB_URL").ok())
            .filter(|s| !s.is_empty());

        let http_bind = env::var("SANDWICH_HTTP_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let state_dir = env::var("SANDWICH_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/var/lib/sandwich-rs"));
        let enable_pyth = env::var("SANDWICH_PYTH")
            .map(|v| v.to_lowercase() != "off" && v != "0")
            .unwrap_or(true);

        let rpc_url = env::var("SANDWICH_RPC_URL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
            match env::var("HELIUS_API_KEY").ok().filter(|s| !s.is_empty()) {
                Some(k) => format!("https://mainnet.helius-rpc.com/?api-key={k}"),
                None => "https://api.mainnet-beta.solana.com".into(),
            }
        });

        Ok(Self {
            ws_url,
            use_helius,
            watched_programs,
            db_url,
            http_bind,
            state_dir,
            enable_pyth,
            rpc_url,
            telegram_bot_token: env::var("TELEGRAM_BOT_TOKEN").ok().filter(|s| !s.is_empty()),
            telegram_chat_id: env::var("TELEGRAM_CHAT_ID").ok().filter(|s| !s.is_empty()),
        })
    }
}
