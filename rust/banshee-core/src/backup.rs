// Backup conventions (docs/data-safety.md): a backup is not a backup until
// it has been restored somewhere else and read. `backup_verified` therefore
// copies AND re-opens the copy out-of-place, validating schema and row count
// before reporting success.

use std::path::Path;

use rusqlite::Connection;

use crate::{CoreError, Result, SampleStore, schema};

/// Snapshot the store to `dest`, then verify the snapshot by opening it
/// out-of-place and comparing row counts. Returns the verified row count.
///
/// **Counts `samples`**, the table that grows every 15 seconds and is therefore
/// the one whose loss would be noticed. The rollup, census and alert tables ride
/// along in the same file — a SQLite backup is whole-file — but a single
/// discriminating count is enough to catch a truncated or half-written copy, and
/// summing five counts would only make a mismatch harder to read.
pub fn backup_verified(store: &SampleStore, dest: &Path) -> Result<usize> {
    if dest.exists() {
        return Err(CoreError::InvalidInput(format!(
            "backup destination already exists: {}",
            dest.display()
        )));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let src = store.connection();
    let mut dst = Connection::open(dest)?;
    let backup = rusqlite::backup::Backup::new(src, &mut dst)?;
    backup.run_to_completion(5, std::time::Duration::from_millis(50), None)?;
    drop(backup);
    drop(dst);

    // Out-of-place verification: fresh connection, schema check, row count.
    let verify = Connection::open(dest)?;
    schema::validate_or_init(&verify)?;
    let backed_up: usize = verify.query_row("SELECT COUNT(*) FROM samples", [], |r| r.get(0))?;
    let original: usize = src.query_row("SELECT COUNT(*) FROM samples", [], |r| r.get(0))?;
    if backed_up != original {
        return Err(CoreError::Schema(format!(
            "backup row count {backed_up} != source {original}; backup at {} is \
             not trustworthy",
            dest.display()
        )));
    }
    Ok(backed_up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::Sample;
    use crate::sys::{SysReading, VolumeReading};
    use chrono::Utc;

    fn reading() -> SysReading {
        SysReading {
            ncpu: 8,
            mem_total_bytes: 25_769_803_776,
            page_size: 16384,
            boot_time_secs: 1_788_100_000,
            load_1m: 1.0,
            load_5m: 1.0,
            load_15m: 1.0,
            swap_total_bytes: 8_589_934_592,
            swap_used_bytes: 1_000_000_000,
            pages_free: 400_000,
            pages_active: 200_000,
            pages_inactive: 200_000,
            pages_wired: 100_000,
            pages_speculative: 0,
            pages_compressor: 10_000,
            pages_purgeable: 0,
            pages_external: 0,
            swapins: 1_000,
            swapouts: 2_000,
            memory_pressure_level: 1,
            thermal_pressure_level: Some(0),
            cpu_speed_limit_percent: None,
            cpu_ticks_total: Some(80_000_000),
            cpu_ticks_idle: Some(60_000_000),
            kernel_free_percent: Some(50),
            jetsam_kills: Some(0),
        }
    }

    fn seed(store: &SampleStore, count: i64) {
        let volumes = [VolumeReading {
            mount_point: "/System/Volumes/Data".into(),
            total_bytes: 494_384_795_648,
            avail_bytes: 100_000_000_000,
        }];
        for i in 0..count {
            let at = Utc::now() - chrono::Duration::seconds(15 * i);
            store
                .insert(&Sample::from_reading(at, &reading(), &volumes))
                .unwrap();
        }
    }

    #[test]
    fn backup_copies_all_rows_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = SampleStore::open(&dir.path().join("live.db")).unwrap();
        seed(&store, 2);

        let dest = dir.path().join("backups/snap.db");
        let count = backup_verified(&store, &dest).unwrap();
        assert_eq!(count, 2);

        // The backup is independently openable and complete — including the
        // child rows, which is what the FK cascade and the retention sweep
        // depend on being there.
        let restored = SampleStore::open(&dest).unwrap();
        assert_eq!(restored.count_samples().unwrap(), 2);
        let samples = restored.recent_samples(10).unwrap();
        assert_eq!(samples.len(), 2);
        assert!(
            samples.iter().all(|s| s.volumes.len() == 1),
            "volume rows must survive the copy, not just the parents"
        );
    }

    #[test]
    fn refuses_to_overwrite_existing_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = SampleStore::open(&dir.path().join("live.db")).unwrap();
        let dest = dir.path().join("snap.db");
        std::fs::write(&dest, b"precious").unwrap();
        let err = backup_verified(&store, &dest).unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)));
        // And the existing file is untouched.
        assert_eq!(std::fs::read(&dest).unwrap(), b"precious");
    }

    #[test]
    fn backup_of_empty_store_verifies_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let store = SampleStore::open(&dir.path().join("live.db")).unwrap();
        let count = backup_verified(&store, &dir.path().join("snap.db")).unwrap();
        assert_eq!(count, 0);
    }
}
