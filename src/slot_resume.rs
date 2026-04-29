use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::fs;
use tracing::{info, warn};

/// Persist the highest slot we've successfully observed to disk.
/// On reconnect after an outage, the operator can compare current Solana slot
/// against the persisted value to know how big the gap is. Helius
/// `transactionSubscribe` doesn't support starting-slot filters, so v1 doesn't
/// auto-replay; this just makes the gap visible for forensic recovery.
pub struct SlotMarker {
    pub last_slot: Arc<AtomicU64>,
    path: PathBuf,
}

impl SlotMarker {
    pub async fn load_or_default(state_dir: &Path) -> Self {
        if let Err(e) = fs::create_dir_all(state_dir).await {
            warn!(err = ?e, dir = %state_dir.display(), "slot_resume: state dir create failed");
        }
        let path = state_dir.join("last_slot");
        let last = match fs::read_to_string(&path).await {
            Ok(s) => s.trim().parse::<u64>().unwrap_or(0),
            Err(_) => 0,
        };
        if last > 0 {
            info!(slot = last, path = %path.display(), "slot_resume: restored from disk");
        } else {
            info!(path = %path.display(), "slot_resume: starting fresh (no prior marker)");
        }
        Self {
            last_slot: Arc::new(AtomicU64::new(last)),
            path,
        }
    }

    pub fn record(&self, slot: u64) {
        if slot == 0 {
            return;
        }
        let mut cur = self.last_slot.load(Ordering::Relaxed);
        while slot > cur {
            match self.last_slot.compare_exchange_weak(
                cur,
                slot,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
    }

    /// Run the persister loop. Writes via tmp+rename for atomicity.
    pub async fn persist_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.tick().await;
        let mut last_written = 0_u64;
        loop {
            tick.tick().await;
            let slot = self.last_slot.load(Ordering::Relaxed);
            if slot == 0 || slot == last_written {
                continue;
            }
            let tmp = self.path.with_extension("tmp");
            if let Err(e) = fs::write(&tmp, slot.to_string()).await {
                warn!(err = ?e, "slot_resume: write tmp failed");
                continue;
            }
            if let Err(e) = fs::rename(&tmp, &self.path).await {
                warn!(err = ?e, "slot_resume: rename failed");
                continue;
            }
            last_written = slot;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_only_advances() {
        let dir = tempfile::tempdir().unwrap();
        let m = SlotMarker::load_or_default(dir.path()).await;
        m.record(100);
        assert_eq!(m.last_slot.load(Ordering::Relaxed), 100);
        m.record(50);
        assert_eq!(m.last_slot.load(Ordering::Relaxed), 100);
        m.record(150);
        assert_eq!(m.last_slot.load(Ordering::Relaxed), 150);
        m.record(0);
        assert_eq!(m.last_slot.load(Ordering::Relaxed), 150);
    }
}
