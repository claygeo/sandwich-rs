use std::time::Duration;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use sqlx::postgres::{PgPool, PgPoolOptions};
use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::detector::Sandwich;
use crate::pyth::SolUsdPrice;

const BATCH_INTERVAL_MS: u64 = 500;
const BATCH_MAX: usize = 64;

pub async fn connect(db_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(5))
        .connect(db_url)
        .await
        .context("connect supabase pooler")?;
    info!("postgres pool connected");
    Ok(pool)
}

/// Batched writer task. Collects sandwiches from `rx`, flushes every BATCH_INTERVAL_MS
/// or every BATCH_MAX events, whichever comes first. Idempotent via ON CONFLICT.
pub async fn run(
    pool: PgPool,
    sol_price: SolUsdPrice,
    mut rx: mpsc::Receiver<Sandwich>,
) -> Result<()> {
    let mut batch: Vec<Sandwich> = Vec::with_capacity(BATCH_MAX);
    let mut tick = interval(Duration::from_millis(BATCH_INTERVAL_MS));
    tick.tick().await; // skip immediate first tick

    info!("db writer running");
    loop {
        tokio::select! {
            maybe = rx.recv() => {
                match maybe {
                    Some(s) => {
                        batch.push(s);
                        if batch.len() >= BATCH_MAX {
                            flush(&pool, &sol_price, &mut batch).await;
                        }
                    }
                    None => {
                        flush(&pool, &sol_price, &mut batch).await;
                        info!("db writer: input channel closed, exiting");
                        return Ok(());
                    }
                }
            }
            _ = tick.tick() => {
                if !batch.is_empty() {
                    flush(&pool, &sol_price, &mut batch).await;
                }
            }
        }
    }
}

async fn flush(pool: &PgPool, sol_price: &SolUsdPrice, batch: &mut Vec<Sandwich>) {
    if batch.is_empty() {
        return;
    }
    let drained = std::mem::take(batch);
    let count = drained.len();
    let spot = sol_price.get();

    match write_batch(pool, spot.as_ref(), &drained).await {
        Ok(written) => {
            debug!(received = count, written, "flush ok");
        }
        Err(e) => {
            error!(err = ?e, count, "flush failed (events dropped — investigate)");
        }
    }
}

async fn write_batch(
    pool: &PgPool,
    sol_usd: Option<&Decimal>,
    batch: &[Sandwich],
) -> Result<usize> {
    let mut tx = pool.begin().await.context("begin tx")?;
    let mut written = 0_usize;

    for s in batch {
        // Insert each of the three transactions (idempotent via ON CONFLICT).
        for t in [&s.front, &s.victim, &s.back] {
            let raw_logs = serde_json::to_value(&t.raw_logs).unwrap_or(serde_json::Value::Null);
            sqlx::query(
                r#"
                insert into public.transactions (
                    signature, slot, signer, pool, dex, ix_type,
                    amount_in_lamports, amount_out_lamports,
                    token_mint_in, token_mint_out, raw_logs
                ) values ($1,$2,$3,$4,$5,'swap',null,null,null,null,$6)
                on conflict (signature) do nothing
                "#,
            )
            .bind(&t.signature)
            .bind(t.slot as i64)
            .bind(&t.signer)
            .bind(&t.pool)
            .bind(&t.dex)
            .bind(raw_logs)
            .execute(&mut *tx)
            .await
            .context("insert transactions")?;
        }

        let profit_lamports = s.profit_lamports.and_then(|p| i64::try_from(p).ok());
        let profit_sol = s.profit_lamports.and_then(|p| {
            i64::try_from(p)
                .ok()
                .map(|p64| Decimal::from(p64) / Decimal::from(1_000_000_000_i64))
        });
        let profit_usd = match (profit_sol.as_ref(), sol_usd) {
            (Some(sol), Some(usd)) => Some(sol * usd),
            _ => None,
        };

        let result = sqlx::query(
            r#"
            insert into public.sandwich_attempts (
                victim_sig, front_sig, back_sig,
                attacker_signer, victim_signer,
                pool, dex, slot_span,
                profit_lamports, profit_sol, profit_usd,
                confidence
            ) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
            on conflict (front_sig, victim_sig, back_sig) do nothing
            "#,
        )
        .bind(&s.victim.signature)
        .bind(&s.front.signature)
        .bind(&s.back.signature)
        .bind(&s.attacker)
        .bind(&s.victim_signer)
        .bind(&s.pool)
        .bind(&s.dex)
        .bind(s.slot_span)
        .bind(profit_lamports)
        .bind(profit_sol)
        .bind(profit_usd)
        .bind(s.confidence as i16)
        .execute(&mut *tx)
        .await
        .context("insert sandwich_attempts")?;

        written += result.rows_affected() as usize;
    }

    tx.commit().await.context("commit tx")?;
    Ok(written)
}

/// Recompute the singleton stats_24h row. Called on a slow timer (every 60s).
pub async fn refresh_stats(pool: &PgPool) -> Result<()> {
    let r = sqlx::query(
        r#"
        with
          win as (
            select
              count(*)::int as n,
              coalesce(sum(profit_sol), 0)::numeric(20,9) as sol,
              coalesce(sum(profit_usd), 0)::numeric(14,2) as usd,
              count(distinct attacker_signer)::int as attackers,
              count(distinct pool)::int as victim_pools
            from public.sandwich_attempts
            where detected_at > now() - interval '24 hours'
          ),
          prior as (
            select coalesce(sum(profit_sol), 0)::numeric(20,9) as sol_prev
            from public.sandwich_attempts
            where detected_at > now() - interval '48 hours'
              and detected_at <= now() - interval '24 hours'
          )
        update public.stats_24h
        set sandwich_count = win.n,
            total_profit_sol = win.sol,
            total_profit_usd = win.usd,
            unique_attackers = win.attackers,
            unique_victim_pools = win.victim_pools,
            delta_pct = case
                when prior.sol_prev > 0
                  then ((win.sol - prior.sol_prev) / prior.sol_prev * 100)::numeric(6,2)
                else null
              end,
            computed_at = now()
        from win, prior
        where stats_24h.id = 1
        "#,
    )
    .execute(pool)
    .await
    .context("refresh stats_24h")?;

    if r.rows_affected() == 0 {
        warn!("stats_24h row missing — re-seeding");
        sqlx::query("insert into public.stats_24h (id) values (1) on conflict (id) do nothing")
            .execute(pool)
            .await
            .context("seed stats_24h")?;
    }
    Ok(())
}
