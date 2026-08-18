use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

/// Durable high-water mark of the highest slot whose swaps were successfully
/// handed downstream.
///
/// # Semantics (read this before trusting the value)
///
/// This is a **handed-off** mark, not a **persisted-to-Postgres** mark. `record`
/// is called once a swap has been accepted by the enricher channel, so the mark
/// never advances past an event that was dropped at that boundary. It can still
/// sit ahead of Postgres if a later stage (enrich, detect, flush) fails, because
/// this is an at-least-once pipeline without end-to-end acknowledgement.
///
/// Concretely: on restart, `last_slot` tells you the newest slot the pipeline
/// *ingested*. It does not promise every event at or below it reached the
/// database. Treat it as a forensic gap marker, not a commit offset.
///
/// Helius `transactionSubscribe` has no starting-slot filter, so there is no
/// auto-replay; the mark exists to make the size of an outage gap visible.
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

        // Three distinct cases, and they must not be collapsed:
        //   - no file            -> genuine first run, 0 is correct
        //   - file, parses       -> restore
        //   - file, unparseable  -> DO NOT silently treat as 0. That turns a
        //     corrupt checkpoint into "we have never seen a slot", which
        //     destroys the only signal this type exists to provide. Preserve the
        //     bytes for forensics and shout about it.
        let last = match fs::read_to_string(&path).await {
            Ok(s) => match s.trim().parse::<u64>() {
                Ok(v) => v,
                Err(e) => {
                    let quarantine = path.with_extension("corrupt");
                    let raw = s.chars().take(64).collect::<String>();
                    match fs::rename(&path, &quarantine).await {
                        Ok(()) => error!(
                            err = ?e,
                            raw = %raw,
                            quarantined = %quarantine.display(),
                            "slot_resume: checkpoint unparseable; moved aside and starting from 0. \
                             The outage-gap signal is LOST for this restart."
                        ),
                        Err(re) => error!(
                            err = ?e,
                            rename_err = ?re,
                            raw = %raw,
                            "slot_resume: checkpoint unparseable AND could not be quarantined; \
                             starting from 0. The outage-gap signal is LOST for this restart."
                        ),
                    }
                    0
                }
            },
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

    /// Advance the in-memory mark. Monotonic: a lower slot is ignored, so
    /// out-of-order arrivals cannot move it backwards.
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

    /// Write the mark durably: tmp -> fsync(tmp) -> rename -> fsync(dir).
    ///
    /// The fsyncs are the whole point. `write` + `rename` alone is atomic with
    /// respect to *other readers* but says nothing about surviving power loss:
    /// the rename can reach disk before the file contents, leaving a
    /// zero-length or torn checkpoint. Syncing the parent directory is what
    /// makes the rename itself durable.
    async fn write_durable(&self, slot: u64) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");

        let mut f = fs::File::create(&tmp).await?;
        f.write_all(slot.to_string().as_bytes()).await?;
        f.sync_all().await?;
        drop(f);

        fs::rename(&tmp, &self.path).await?;

        // fsync the directory so the rename survives a crash. Opening a
        // directory read-only and syncing it is the portable POSIX idiom; on
        // platforms where it is not permitted we degrade rather than fail the
        // whole write, since the contents are already durable.
        if let Some(dir) = self.path.parent() {
            match fs::File::open(dir).await {
                Ok(d) => {
                    if let Err(e) = d.sync_all().await {
                        warn!(err = ?e, "slot_resume: parent dir fsync failed (contents are durable)");
                    }
                }
                Err(e) => {
                    warn!(err = ?e, "slot_resume: parent dir open for fsync failed (contents are durable)");
                }
            }
        }
        Ok(())
    }

    /// Run the persister loop.
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
            match self.write_durable(slot).await {
                Ok(()) => last_written = slot,
                Err(e) => warn!(err = ?e, slot, "slot_resume: durable write failed"),
            }
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

    #[tokio::test]
    async fn durable_write_then_reload_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let m = SlotMarker::load_or_default(dir.path()).await;
        m.record(4242);
        m.write_durable(4242).await.unwrap();

        // No leftover tmp file after a successful write.
        assert!(!dir.path().join("last_slot.tmp").exists());

        let reloaded = SlotMarker::load_or_default(dir.path()).await;
        assert_eq!(reloaded.last_slot.load(Ordering::Relaxed), 4242);
    }

    #[tokio::test]
    async fn corrupt_checkpoint_is_quarantined_not_silently_zeroed() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("last_slot"), b"\x00\x00notanumber")
            .await
            .unwrap();

        let m = SlotMarker::load_or_default(dir.path()).await;

        // Starting from 0 is the only safe value, but the corrupt bytes must be
        // preserved for forensics and the original must be gone so the next
        // write does not clobber evidence.
        assert_eq!(m.last_slot.load(Ordering::Relaxed), 0);
        assert!(
            dir.path().join("last_slot.corrupt").exists(),
            "corrupt checkpoint should be quarantined to last_slot.corrupt"
        );
        assert!(
            !dir.path().join("last_slot").exists(),
            "corrupt checkpoint should be moved aside, not left in place"
        );
    }

    #[tokio::test]
    async fn missing_checkpoint_is_a_clean_fresh_start() {
        let dir = tempfile::tempdir().unwrap();
        let m = SlotMarker::load_or_default(dir.path()).await;
        assert_eq!(m.last_slot.load(Ordering::Relaxed), 0);
        // A genuine first run must NOT create a .corrupt file.
        assert!(!dir.path().join("last_slot.corrupt").exists());
    }

    #[tokio::test]
    async fn whitespace_and_trailing_newline_still_parse() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("last_slot"), b"  900123\n")
            .await
            .unwrap();
        let m = SlotMarker::load_or_default(dir.path()).await;
        assert_eq!(m.last_slot.load(Ordering::Relaxed), 900123);
        assert!(!dir.path().join("last_slot.corrupt").exists());
    }
}
