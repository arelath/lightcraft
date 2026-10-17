//! An adopted replacement cache permits retiring old copies, independently of normal
//! eviction policy. The original resolver owns the lease gates, including leases that
//! outlive library reopen. Every removal still requires a verified replacement.
use super::{AssetAvailability, AssetError, AssetResolver, BlobId, Result, disk, verify_blob_file};
use std::sync::{
    Arc, Mutex, PoisonError, TryLockError,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct AdoptedCache {
    from: Arc<AssetResolver>,
    to: Arc<AssetResolver>,
    fence: Fence,
}

#[derive(Clone)]
struct Fence {
    cancelled: Arc<AtomicBool>,
    deletion: Arc<Mutex<()>>,
}
impl Fence {
    fn active<T>(&self, run: impl FnOnce() -> Result<T>) -> Result<T> {
        let _gate = self.deletion.lock().unwrap_or_else(PoisonError::into_inner);
        if self.cancelled.load(Ordering::Acquire) {
            return Err(AssetError::Cancelled);
        }
        run()
    }
}

/// Owned by the open library and dropped before its process lock. Stopping never
/// waits for a hash read, only an already-started filesystem removal. Once it returns,
/// no old cleanup can remove a root that another move/process may now reuse.
pub(crate) struct CleanupGuard(Fence);
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        self.0.cancelled.store(true, Ordering::Release);
        let _barrier = self.0.deletion.lock().unwrap_or_else(PoisonError::into_inner);
    }
}

impl AdoptedCache {
    /// The owner must first verify that the reopened library selected `to`. A
    /// successful reopen alone is insufficient: retention intent must also match.
    pub(crate) fn adopt(from: Arc<AssetResolver>, to: Arc<AssetResolver>) -> Result<(Self, CleanupGuard)> {
        from.cache.validate_root()?;
        to.cache.validate_root()?;
        if crate::originals::paths_overlap(from.root(), to.root()).map_err(AssetError::Invalid)? {
            return Err(AssetError::Invalid("replacement cache overlaps the old cache".into()));
        }
        let old = from.cache_status();
        let new = to.cache_status();
        if old.error.is_some()
            || new.error.is_some()
            || old.resolving_pins
            || new.resolving_pins
            || old.budget_bytes != new.budget_bytes
            || old.keep_all != new.keep_all
            || old.pins != new.pins
            || old.removed != new.removed
        {
            return Err(AssetError::Invalid(
                "replacement cache did not adopt the old retention settings and offline owners; old copies retained".into(),
            ));
        }
        let fence = Fence { cancelled: Arc::new(AtomicBool::new(false)), deletion: Arc::new(Mutex::new(())) };
        let guard = CleanupGuard(fence.clone());
        from.cache.retire();
        Ok((Self { from, to, fence }, guard))
    }

    /// `false` defers removal while an old byte lease or either acquisition exists.
    /// Nonblocking gates also avoid deadlock between forward/back moves. The destination gate
    /// also prevents eviction between verifying its bytes and deleting the old copy.
    pub(crate) fn remove(&self, id: &BlobId, size: u64) -> Result<bool> {
        if self.fence.cancelled.load(Ordering::Acquire) {
            return Err(AssetError::Cancelled);
        }
        if id.0.len() != 64 || !id.0.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')) {
            return Err(AssetError::Invalid("invalid relocated blob identity".into()));
        }
        self.from.cache.validate_root()?;
        self.to.cache.validate_root()?;
        if crate::originals::paths_overlap(self.from.root(), self.to.root()).map_err(AssetError::Invalid)? {
            return Err(AssetError::Invalid("replacement cache now overlaps the old cache; old copies retained".into()));
        }
        let old = self.from.entry(id)?;
        let new = self.to.entry(id)?;
        let _old_gate = match old.gate.try_lock() {
            Ok(gate) => gate,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Ok(false),
        };
        let _new_gate = match new.gate.try_lock() {
            Ok(gate) => gate,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Ok(false),
        };
        if old.pins.load(Ordering::Relaxed) != 0 {
            return Ok(false);
        }
        let target = self.to.root.join(&id.0);
        if !std::fs::symlink_metadata(&target).map_err(disk)?.file_type().is_file() || !verify_blob_file(&target, id, size, &self.fence.cancelled)? {
            return Err(AssetError::Integrity);
        }
        self.fence.active(|| {
            match std::fs::remove_file(self.from.root.join(&id.0)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(disk(error)),
            }
            *old.state.lock().unwrap_or_else(PoisonError::into_inner) = AssetAvailability::Remote;
            self.from.cache.schedule_trim();
            Ok(true)
        })
    }

    /// Keep old policy when any uncopied/damaged blob or other file remains.
    pub(crate) fn finish(&self) -> Result<()> {
        self.fence.active(|| {
            let only_settings = std::fs::read_dir(&self.from.root)
                .is_ok_and(|entries| entries.into_iter().all(|entry| entry.is_ok_and(|entry| entry.file_name() == "retention.json")));
            if only_settings {
                match std::fs::remove_file(self.from.root.join("retention.json")) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(disk(error)),
                }
                // Only an emptied folder goes.
                let _ = std::fs::remove_dir(&self.from.root);
            }
            Ok(())
        })
    }
}
