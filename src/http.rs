use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use futures_util::stream::Stream;
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{postgres::PgRow, PgPool, Row};
use tokio::sync::broadcast;
use tracing::warn;

use crate::detector::Sandwich;

#[derive(Clone)]
pub struct AppState {
    pub pool: Option<PgPool>,
    pub broadcaster: broadcast::Sender<Sandwich>,
    pub last_ws_frame_epoch: Arc<AtomicI64>,
    pub started_at_epoch: i64,
}

/// During the first STARTUP_GRACE_SECS after process start, /healthz returns 200 even
/// before the WS reader has produced its first frame. Without this, every restart
/// produces a 60s false-positive alert in any uptime monitor.
const STARTUP_GRACE_SECS: i64 = 30;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/events", get(events))
        .route("/stats", get(stats))
        .with_state(state)
}

#[derive(Serialize)]
struct HealthResp {
    healthy: bool,
    last_ws_frame_age_secs: i64,
    uptime_secs: i64,
    db_connected: bool,
    in_startup_grace: bool,
}

async fn healthz(State(s): State<AppState>) -> (StatusCode, Json<HealthResp>) {
    let now = Utc::now().timestamp();
    let last = s.last_ws_frame_epoch.load(Ordering::Relaxed);
    let uptime = (now - s.started_at_epoch).max(0);
    let in_grace = uptime < STARTUP_GRACE_SECS;
    let age = if last == 0 { i64::MAX } else { now - last };
    let frame_fresh = (0..60).contains(&age);
    let healthy = frame_fresh || in_grace;
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(HealthResp {
            healthy,
            last_ws_frame_age_secs: age,
            uptime_secs: uptime,
            db_connected: s.pool.is_some(),
            in_startup_grace: in_grace,
        }),
    )
}

async fn events(
    State(s): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = s.broadcaster.subscribe();
    let pool = s.pool.clone();
    let stream = async_stream::stream! {
        if let Some(pool) = pool {
            match recent_sandwiches(&pool, 50).await {
                Ok(rows) => {
                    for r in rows {
                        if let Ok(json) = serde_json::to_string(&r) {
                            yield Ok(Event::default().event("init").data(json));
                        }
                    }
                }
                Err(e) => {
                    warn!(err = ?e, "failed to seed /events with recent rows");
                }
            }
        }
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(s) => {
                    if let Ok(json) = serde_json::to_string(&s) {
                        yield Ok(Event::default().event("sandwich").data(json));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    yield Ok(Event::default().event("lag").data(n.to_string()));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Serialize)]
struct RecentRow {
    id: String,
    front_sig: String,
    victim_sig: String,
    back_sig: String,
    attacker_signer: String,
    victim_signer: String,
    pool: String,
    dex: String,
    slot_span: i32,
    profit_sol: Option<Decimal>,
    profit_usd: Option<Decimal>,
    confidence: i16,
    detected_at: DateTime<Utc>,
}

async fn recent_sandwiches(pool: &PgPool, limit: i64) -> sqlx::Result<Vec<RecentRow>> {
    let rows = sqlx::query(
        r#"
        select id::text as id, front_sig, victim_sig, back_sig,
               attacker_signer, victim_signer, pool, dex,
               slot_span, profit_sol, profit_usd, confidence, detected_at
        from public.sandwich_attempts
        order by detected_at desc
        limit $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(map_row).collect())
}

fn map_row(r: PgRow) -> RecentRow {
    RecentRow {
        id: r.try_get("id").unwrap_or_default(),
        front_sig: r.try_get("front_sig").unwrap_or_default(),
        victim_sig: r.try_get("victim_sig").unwrap_or_default(),
        back_sig: r.try_get("back_sig").unwrap_or_default(),
        attacker_signer: r.try_get("attacker_signer").unwrap_or_default(),
        victim_signer: r.try_get("victim_signer").unwrap_or_default(),
        pool: r.try_get("pool").unwrap_or_default(),
        dex: r.try_get("dex").unwrap_or_default(),
        slot_span: r.try_get("slot_span").unwrap_or_default(),
        profit_sol: r.try_get("profit_sol").ok(),
        profit_usd: r.try_get("profit_usd").ok(),
        confidence: r.try_get("confidence").unwrap_or_default(),
        detected_at: r.try_get("detected_at").unwrap_or_else(|_| Utc::now()),
    }
}

#[derive(Serialize)]
struct StatsResp {
    sandwich_count: i32,
    total_profit_sol: Decimal,
    total_profit_usd: Decimal,
    unique_attackers: i32,
    unique_victim_pools: i32,
    delta_pct: Option<Decimal>,
    computed_at: DateTime<Utc>,
}

async fn stats(State(s): State<AppState>) -> Result<Json<StatsResp>, StatusCode> {
    let pool = s.pool.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let row = sqlx::query(
        r#"
        select sandwich_count, total_profit_sol, total_profit_usd,
               unique_attackers, unique_victim_pools, delta_pct, computed_at
        from public.stats_24h where id = 1
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| {
        warn!(err = ?e, "stats query failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(StatsResp {
        sandwich_count: row.try_get("sandwich_count").unwrap_or_default(),
        total_profit_sol: row.try_get("total_profit_sol").unwrap_or_default(),
        total_profit_usd: row.try_get("total_profit_usd").unwrap_or_default(),
        unique_attackers: row.try_get("unique_attackers").unwrap_or_default(),
        unique_victim_pools: row.try_get("unique_victim_pools").unwrap_or_default(),
        delta_pct: row.try_get("delta_pct").ok(),
        computed_at: row.try_get("computed_at").unwrap_or_else(|_| Utc::now()),
    }))
}
