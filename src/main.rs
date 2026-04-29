use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use sandwich_rs::{config, db, detector, http, parser, pyth, slot_resume, telegram, ws};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sandwich_rs=info,sqlx=warn,tower_http=warn".into()),
        )
        .init();

    info!(version = env!("CARGO_PKG_VERSION"), "sandwich-rs starting");

    let cfg = config::Config::from_env()?;
    info!(
        ws_url = %cfg.ws_url,
        helius = cfg.use_helius,
        programs = ?cfg.watched_programs,
        db_configured = cfg.db_url.is_some(),
        http_bind = %cfg.http_bind,
        "config loaded"
    );

    // Pipeline channels (bounded — backpressure on the WS reader).
    let (raw_tx, raw_rx) = mpsc::channel::<Message>(1024);
    let (swap_tx, swap_rx) = mpsc::channel::<parser::Swap>(512);
    let (sandwich_tx, sandwich_rx) = mpsc::channel::<detector::Sandwich>(64);
    let (db_tx, db_rx) = mpsc::channel::<detector::Sandwich>(64);
    let (alert_tx, alert_rx) = mpsc::channel::<detector::Sandwich>(64);
    let (broadcast_tx, _) = broadcast::channel::<detector::Sandwich>(64);

    let pool_state: detector::PoolState = Arc::new(DashMap::new());
    let last_ws_frame = Arc::new(AtomicI64::new(0));
    let started_at_epoch = Utc::now().timestamp();
    let dropped_db = Arc::new(AtomicU64::new(0));
    let dropped_alert = Arc::new(AtomicU64::new(0));
    let dropped_broadcast = Arc::new(AtomicU64::new(0));

    // Slot resume marker (operational visibility into outage gaps).
    let slot_marker = Arc::new(slot_resume::SlotMarker::load_or_default(&cfg.state_dir).await);
    {
        let m = slot_marker.clone();
        tokio::spawn(async move { m.persist_loop().await });
    }

    // Pyth SOL/USD spot for filling sandwich_attempts.profit_usd.
    let sol_price = pyth::SolUsdPrice::new();
    if cfg.enable_pyth {
        let sp = sol_price.clone();
        tokio::spawn(async move {
            if let Err(e) = sp.run_poller().await {
                warn!(err = ?e, "pyth poller exited");
            }
        });
    } else {
        info!("pyth poller disabled (SANDWICH_PYTH=off)");
    }

    // WS reader → stamper → raw_tx
    // The stamper updates `last_ws_frame` so /healthz can reason about freshness.
    let h_ws = {
        let last_ws_frame = last_ws_frame.clone();
        let cfg_ws = cfg.clone();
        tokio::spawn(async move {
            let (probe_tx, mut probe_rx) = mpsc::channel::<Message>(1024);
            let raw_tx_inner = raw_tx;
            tokio::spawn(async move {
                while let Some(msg) = probe_rx.recv().await {
                    last_ws_frame.store(Utc::now().timestamp(), Ordering::Relaxed);
                    if raw_tx_inner.send(msg).await.is_err() {
                        break;
                    }
                }
            });
            ws::run(cfg_ws.ws_url, cfg_ws.watched_programs, cfg_ws.use_helius, probe_tx).await
        })
    };

    let h_parser = {
        let slot_marker = slot_marker.clone();
        // Tee swap_rx through a slot-recording task so the persister sees latest slot.
        let (parsed_swap_tx, parsed_swap_rx) = mpsc::channel::<parser::Swap>(512);
        tokio::spawn(async move {
            let mut rx = parsed_swap_rx;
            while let Some(swap) = rx.recv().await {
                slot_marker.record(swap.slot);
                if swap_tx.send(swap).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(parser::run(raw_rx, parsed_swap_tx))
    };
    let h_detector = tokio::spawn(detector::run(pool_state, swap_rx, sandwich_tx));

    // Fanout: detector → (db, telegram, broadcast).
    // Codex flagged: silent drops here would lose the exact data we ship as the
    // portfolio piece. Each leg gets a bounded try_send + WARN with the signature
    // so we can recover from journald.
    let h_fanout = {
        let broadcast_tx = broadcast_tx.clone();
        let dropped_db = dropped_db.clone();
        let dropped_alert = dropped_alert.clone();
        let dropped_broadcast = dropped_broadcast.clone();
        tokio::spawn(async move {
            let mut rx = sandwich_rx;
            while let Some(s) = rx.recv().await {
                if let Err(e) = db_tx.try_send(s.clone()) {
                    let n = dropped_db.fetch_add(1, Ordering::Relaxed) + 1;
                    warn!(
                        sig = %s.victim.signature,
                        attacker = %s.attacker,
                        kind = ?e,
                        dropped_db_total = n,
                        "DROPPED sandwich (db channel full or closed) — sqlx writer is behind"
                    );
                }
                if let Err(e) = alert_tx.try_send(s.clone()) {
                    let n = dropped_alert.fetch_add(1, Ordering::Relaxed) + 1;
                    warn!(
                        sig = %s.victim.signature,
                        kind = ?e,
                        dropped_alert_total = n,
                        "DROPPED sandwich (alert channel full or closed)"
                    );
                }
                if let Err(e) = broadcast_tx.send(s) {
                    let n = dropped_broadcast.fetch_add(1, Ordering::Relaxed) + 1;
                    warn!(kind = ?e, dropped_broadcast_total = n, "no SSE subscribers (ok)");
                }
            }
            warn!("fanout: input closed");
        })
    };

    let h_telegram = tokio::spawn(telegram::run(cfg.clone(), alert_rx));

    // DB writer + HTTP server: branch on whether DB is configured.
    let h_db: tokio::task::JoinHandle<Result<()>> = if let Some(db_url) = cfg.db_url.clone() {
        match db::connect(&db_url).await {
            Ok(pool) => {
                spawn_stats_refresher(pool.clone());
                spawn_http_server(
                    Some(pool.clone()),
                    last_ws_frame.clone(),
                    broadcast_tx.clone(),
                    cfg.http_bind.clone(),
                    started_at_epoch,
                );
                tokio::spawn(db::run(pool, sol_price.clone(), db_rx))
            }
            Err(e) => {
                warn!(err = ?e, "db connect failed — running without persistence");
                spawn_http_server(
                    None,
                    last_ws_frame.clone(),
                    broadcast_tx.clone(),
                    cfg.http_bind.clone(),
                    started_at_epoch,
                );
                drop(db_rx);
                tokio::spawn(async { Ok(()) })
            }
        }
    } else {
        warn!("SUPABASE_POOLER_URL/SUPABASE_DB_URL not set — running without persistence");
        spawn_http_server(
            None,
            last_ws_frame.clone(),
            broadcast_tx.clone(),
            cfg.http_bind.clone(),
            started_at_epoch,
        );
        drop(db_rx);
        tokio::spawn(async { Ok(()) })
    };

    info!("all tasks spawned");

    tokio::select! {
        r = h_ws       => warn!(?r, "ws task exited"),
        r = h_parser   => warn!(?r, "parser task exited"),
        r = h_detector => warn!(?r, "detector task exited"),
        r = h_fanout   => warn!(?r, "fanout task exited"),
        r = h_db       => warn!(?r, "db task exited"),
        r = h_telegram => warn!(?r, "telegram task exited"),
        _ = tokio::signal::ctrl_c() => info!("shutdown signal received"),
    }

    Ok(())
}

fn spawn_stats_refresher(pool: sqlx::PgPool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = db::refresh_stats(&pool).await {
                warn!(err = ?e, "stats refresh failed");
            }
        }
    });
}

fn spawn_http_server(
    pool: Option<sqlx::PgPool>,
    last_ws_frame: Arc<AtomicI64>,
    broadcaster: broadcast::Sender<detector::Sandwich>,
    bind: String,
    started_at_epoch: i64,
) {
    tokio::spawn(async move {
        let state = http::AppState {
            pool,
            broadcaster,
            last_ws_frame_epoch: last_ws_frame,
            started_at_epoch,
        };
        let app = http::router(state);
        match TcpListener::bind(&bind).await {
            Ok(listener) => {
                info!(%bind, "http server listening");
                if let Err(e) = axum::serve(listener, app).await {
                    warn!(err = ?e, "http serve failed");
                }
            }
            Err(e) => warn!(err = ?e, %bind, "http bind failed"),
        }
    });
}
