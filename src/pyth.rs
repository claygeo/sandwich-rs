use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::{info, warn};

/// Pyth SOL/USD price feed ID (mainnet, Hermes).
const SOL_USD_FEED_ID: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const HERMES_URL: &str = "https://hermes.pyth.network/v2/updates/price/latest";

/// Cached SOL/USD spot price. Stored as fixed-point with 6 decimals so it fits
/// in an AtomicU64 and reads are lock-free from the writer hot path.
#[derive(Clone)]
pub struct SolUsdPrice {
    price_e6: Arc<AtomicU64>,
}

impl SolUsdPrice {
    pub fn new() -> Self {
        Self {
            price_e6: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn get(&self) -> Option<Decimal> {
        let v = self.price_e6.load(Ordering::Relaxed);
        if v == 0 {
            None
        } else {
            // Decimal::new(int, scale) means int * 10^-scale. We stored at 6 decimals.
            Some(Decimal::new(v as i64, 6))
        }
    }

    pub async fn run_poller(self) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("build http client")?;
        // Run an immediate fetch then poll every 30s.
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            match fetch_price(&client).await {
                Ok(p) => {
                    let e6 = (p * 1_000_000.0).round().max(0.0) as u64;
                    self.price_e6.store(e6, Ordering::Relaxed);
                    info!(sol_usd = p, "pyth price updated");
                }
                Err(e) => warn!(err = ?e, "pyth fetch failed"),
            }
            interval.tick().await;
        }
    }
}

impl Default for SolUsdPrice {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct HermesResponse {
    parsed: Vec<HermesPriceFeed>,
}

#[derive(Deserialize)]
struct HermesPriceFeed {
    price: HermesPriceData,
}

#[derive(Deserialize)]
struct HermesPriceData {
    price: String,
    expo: i32,
}

async fn fetch_price(client: &reqwest::Client) -> Result<f64> {
    let url = format!("{HERMES_URL}?ids%5B%5D={SOL_USD_FEED_ID}&parsed=true");
    let res: HermesResponse = client
        .get(&url)
        .send()
        .await
        .context("hermes get")?
        .error_for_status()
        .context("hermes status")?
        .json()
        .await
        .context("hermes json")?;
    let feed = res
        .parsed
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("hermes returned no price feed"))?;
    let raw: i64 = feed.price.price.parse().context("parse hermes price")?;
    Ok(raw as f64 * 10f64.powi(feed.price.expo))
}
