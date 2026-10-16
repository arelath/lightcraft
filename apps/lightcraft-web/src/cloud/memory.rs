//! Verified transient bytes. Commands reserve and pin their entire source set;
//! renderer misses share the same byte and concurrency admission limits.
use super::verify_blob;
use lightcraft_engine::catalog::assets::Blob;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const BUDGET: u64 = super::MAX_BROWSER_BLOB;
#[derive(Clone)]
pub struct Download {
    pub blob: Blob,
    pub cancel: Arc<AtomicBool>,
}
#[derive(Default)]
struct Inner {
    bytes: BTreeMap<String, (Arc<[u8]>, u64)>,
    wanted: BTreeMap<String, Blob>,
    loading: BTreeMap<String, Download>,
    pins: BTreeMap<String, (Blob, usize)>,
    renderer: BTreeSet<String>,
    errors: BTreeMap<String, String>,
    clock: u64,
    total: usize,
}
impl Inner {
    // Count each immutable payload once, including reserved future downloads.
    // Decoded pixels and browser/transport copies have separate memory costs.
    fn footprint(&self, extra: Option<&Blob>) -> u64 {
        let mut sizes: BTreeMap<&str, u64> = self.bytes.iter().map(|(id, (bytes, _))| (id.as_str(), bytes.len() as u64)).collect();
        for (blob, _) in self.pins.values() {
            sizes.insert(&blob.id.0, blob.size);
        }
        for d in self.loading.values() {
            sizes.insert(&d.blob.id.0, d.blob.size);
        }
        if let Some(blob) = extra {
            sizes.insert(&blob.id.0, blob.size);
        }
        sizes.values().fold(0u64, |sum, size| sum.saturating_add(*size))
    }
    fn evict_one(&mut self, protected: Option<&str>) -> bool {
        let key = self
            .bytes
            .iter()
            .filter(|(id, _)| !self.pins.contains_key(*id) && !self.loading.contains_key(*id) && Some(id.as_str()) != protected)
            .min_by_key(|(_, (_, last))| last)
            .map(|(id, _)| id.clone());
        let Some(key) = key else { return false };
        if let Some((bytes, _)) = self.bytes.remove(&key) {
            self.total = self.total.saturating_sub(bytes.len());
        }
        true
    }
    fn make_room(&mut self, blob: &Blob, budget: u64) -> bool {
        while self.footprint(Some(blob)) > budget {
            if !self.evict_one(Some(&blob.id.0)) {
                return false;
            }
        }
        true
    }
    fn check_size(&self, blob: &Blob) -> Result<(), String> {
        let size = self
            .bytes
            .get(&blob.id.0)
            .map(|(bytes, _)| bytes.len() as u64)
            .or_else(|| self.pins.get(&blob.id.0).map(|(b, _)| b.size))
            .or_else(|| self.loading.get(&blob.id.0).map(|d| d.blob.size))
            .or_else(|| self.wanted.get(&blob.id.0).map(|b| b.size));
        if size.is_some_and(|size| size != blob.size) {
            return Err("blob length differs from its immutable description".into());
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct ManagedBytes {
    inner: Arc<Mutex<Inner>>,
    generation: Arc<AtomicU64>,
    wake: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
    budget: u64,
}
impl Default for ManagedBytes {
    fn default() -> Self {
        Self { inner: Default::default(), generation: Default::default(), wake: Default::default(), budget: BUDGET }
    }
}
/// Held by the acquisition stage through synchronous command execution. A cancelled
/// transfer retains its reservation until its future settles, even after lease drop.
pub struct ByteLease {
    memory: ManagedBytes,
    blobs: Vec<Blob>,
}
impl ByteLease {
    pub fn ready(&self) -> Result<bool, String> {
        let g = self.memory.lock();
        let mut ready = true;
        for blob in &self.blobs {
            if !g.bytes.contains_key(&blob.id.0) {
                if let Some(error) = g.errors.get(&blob.id.0) {
                    return Err(error.clone());
                }
                ready = false;
            }
        }
        Ok(ready)
    }
}
impl Drop for ByteLease {
    fn drop(&mut self) {
        let mut g = self.memory.lock();
        for blob in &self.blobs {
            if let Some((_, count)) = g.pins.get_mut(&blob.id.0) {
                *count = count.saturating_sub(1);
                if *count != 0 {
                    continue;
                }
                g.pins.remove(&blob.id.0);
            }
            if !g.renderer.contains(&blob.id.0) {
                g.wanted.remove(&blob.id.0);
                if let Some(d) = g.loading.get(&blob.id.0) {
                    d.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        drop(g);
        self.memory.wake();
    }
}
impl ManagedBytes {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn wake(&self) {
        let wake = self.wake.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(wake) = wake {
            wake();
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
    pub fn counter(&self) -> Arc<AtomicU64> {
        self.generation.clone()
    }
    pub fn set_waker(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.wake.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
    }
    pub fn install(&self, session: &mut lightcraft_engine::Session) {
        let reader = self.clone();
        let peek = self.clone();
        session.media.managed_reader = Some(Arc::new(move |blob| reader.get(blob)));
        session.media.managed_peek = Some(Arc::new(move |blob| peek.peek(blob)));
        session.media.managed_generation = Some(self.counter());
    }
    /// Cache-only inspection never schedules an original while choosing a preview.
    pub fn peek(&self, blob: &Blob) -> Result<Arc<[u8]>, String> {
        blob.validate()?;
        let mut g = self.lock();
        g.check_size(blob)?;
        g.clock = g.clock.saturating_add(1);
        let clock = g.clock;
        let (bytes, last) = g.bytes.get_mut(&blob.id.0).ok_or("cloud bytes are not resident")?;
        *last = clock;
        Ok(bytes.clone())
    }
    pub fn get(&self, blob: &Blob) -> Result<Arc<[u8]>, String> {
        if let Ok(bytes) = self.peek(blob) {
            return Ok(bytes);
        }
        blob.validate()?;
        if blob.size > self.budget {
            return Err("this original exceeds the browser transfer budget".into());
        }
        let mut g = self.lock();
        g.check_size(blob)?;
        if let Some(error) = g.errors.get(&blob.id.0) {
            return Err(error.clone());
        }
        if !g.loading.contains_key(&blob.id.0) {
            if g.wanted.len() >= 1000 && !g.wanted.contains_key(&blob.id.0) {
                return Err("browser cloud queue is full; retry after current downloads".into());
            }
            g.wanted.entry(blob.id.0.clone()).or_insert_with(|| blob.clone());
        }
        g.renderer.insert(blob.id.0.clone());
        drop(g);
        self.wake();
        Err("cloud bytes are loading asynchronously; connect and retry if offline".into())
    }
    /// Reserve the complete distinct source set atomically before downloading.
    pub fn reserve(&self, blobs: Vec<Blob>) -> Result<ByteLease, String> {
        let mut g = self.lock();
        let mut unique = BTreeMap::new();
        for blob in blobs {
            blob.validate()?;
            g.check_size(&blob)?;
            if unique.get(&blob.id.0).is_some_and(|old: &Blob| old.size != blob.size) {
                return Err("conflicting immutable blob lengths in acquisition".into());
            }
            unique.insert(blob.id.0.clone(), blob);
        }
        let mut mandatory: BTreeMap<_, _> = g.pins.iter().map(|(id, (blob, _))| (id.clone(), blob.size)).collect();
        for (id, d) in &g.loading {
            mandatory.insert(id.clone(), d.blob.size);
        }
        for (id, blob) in &unique {
            mandatory.insert(id.clone(), blob.size);
        }
        if mandatory.values().fold(0u64, |sum, size| sum.saturating_add(*size)) > self.budget {
            return Err("browser transfer budget is reserved; retry after current work or export smaller groups".into());
        }
        let queued: BTreeSet<_> = g.wanted.keys().chain(unique.keys()).collect();
        if queued.len() > 1000 {
            return Err("browser cloud queue is full; retry after current downloads".into());
        }
        for (id, blob) in &unique {
            let (_, count) = g.pins.entry(id.clone()).or_insert_with(|| (blob.clone(), 0));
            *count = count.saturating_add(1);
            g.errors.remove(id);
            if !g.bytes.contains_key(id) && !g.loading.contains_key(id) {
                g.wanted.insert(id.clone(), blob.clone());
            }
        }
        while g.footprint(None) > self.budget && g.evict_one(None) {}
        drop(g);
        self.wake();
        Ok(ByteLease { memory: self.clone(), blobs: unique.into_values().collect() })
    }
    pub fn insert(&self, blob: &Blob, bytes: Arc<[u8]>) -> Result<(), String> {
        verify_blob(blob, &bytes)?;
        let mut g = self.lock();
        g.check_size(blob)?;
        if !g.make_room(blob, self.budget) {
            return Err("browser transfer budget is reserved by active commands".into());
        }
        g.clock = g.clock.saturating_add(1);
        let clock = g.clock;
        g.loading.remove(&blob.id.0);
        g.wanted.remove(&blob.id.0);
        g.renderer.remove(&blob.id.0);
        g.errors.remove(&blob.id.0);
        if let Some((old, _)) = g.bytes.insert(blob.id.0.clone(), (bytes.clone(), clock)) {
            g.total = g.total.saturating_sub(old.len());
        }
        g.total = g.total.saturating_add(bytes.len());
        self.generation.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    pub fn resident_ids(&self) -> BTreeSet<String> {
        self.lock().bytes.keys().cloned().collect()
    }
    pub fn pending(&self, blob: &Blob) -> bool {
        let g = self.lock();
        g.wanted.contains_key(&blob.id.0) || g.loading.contains_key(&blob.id.0)
    }
    pub fn take_wanted(&self) -> Vec<Download> {
        let mut g = self.lock();
        let mut ids: Vec<_> = g.wanted.keys().cloned().collect();
        ids.sort_by_key(|id| !g.pins.contains_key(id));
        let mut downloads = Vec::new();
        for id in ids {
            if g.loading.len() >= 4 {
                break;
            }
            let Some(blob) = g.wanted.get(&id).cloned() else { continue };
            if !g.make_room(&blob, self.budget) {
                continue;
            }
            g.wanted.remove(&id);
            let download = Download { blob, cancel: Arc::new(AtomicBool::new(false)) };
            g.loading.insert(id, download.clone());
            downloads.push(download);
        }
        downloads
    }
    pub fn failed(&self, blob: &Blob, error: String) {
        let mut g = self.lock();
        let cancelled = g.loading.remove(&blob.id.0).is_some_and(|d| d.cancel.load(Ordering::Relaxed));
        if g.bytes.contains_key(&blob.id.0) {
            return;
        }
        if cancelled {
            if g.pins.contains_key(&blob.id.0) || g.renderer.contains(&blob.id.0) {
                g.wanted.insert(blob.id.0.clone(), blob.clone());
            }
        } else {
            g.errors.insert(blob.id.0.clone(), error);
        }
    }
    pub fn retry(&self) {
        let mut g = self.lock();
        g.errors.clear();
        let retry: Vec<_> = g.pins.values().map(|(blob, _)| blob.clone()).collect();
        for blob in retry {
            if !g.bytes.contains_key(&blob.id.0) && !g.loading.contains_key(&blob.id.0) {
                g.wanted.insert(blob.id.0.clone(), blob);
            }
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
    pub fn status(&self) -> serde_json::Value {
        let g = self.lock();
        serde_json::json!({"bytes":g.total,"resident":g.bytes.len(),"queued":g.wanted.len(),"loading":g.loading.len(),"errors":g.errors,
            "reservedBytes":g.footprint(None),"budgetBytes":self.budget,"pinned":g.pins.len()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn blob(bytes: &[u8]) -> Blob {
        Blob {
            id: lightcraft_engine::assets::sha256(bytes),
            size: bytes.len() as u64,
            remote: lightcraft_engine::catalog::assets::RemoteLocation {
                connection: "browser".into(),
                key: "library/original".into(),
                version: None,
            },
        }
    }
    #[test]
    fn misses_deduplicate_corruption_is_rejected_and_retry_unblocks_async_work() {
        let memory = ManagedBytes::default();
        let blob = blob(b"verified");
        assert!(memory.peek(&blob).is_err());
        assert!(memory.take_wanted().is_empty());
        assert!(memory.get(&blob).is_err());
        assert!(memory.get(&blob).is_err());
        assert_eq!(memory.take_wanted().iter().map(|d| &d.blob).collect::<Vec<_>>(), vec![&blob]);
        assert!(memory.take_wanted().is_empty());
        assert!(memory.insert(&blob, Arc::from(b"corrupt!".as_slice())).is_err());
        memory.failed(&blob, "offline; retry after connecting".into());
        assert!(memory.get(&blob).unwrap_err().contains("offline"));
        assert!(memory.take_wanted().is_empty());
        memory.retry();
        assert!(memory.get(&blob).is_err());
        assert_eq!(memory.take_wanted().len(), 1);
        memory.insert(&blob, Arc::from(b"verified".as_slice())).unwrap();
        assert_eq!(&*memory.get(&blob).unwrap(), b"verified");
        assert_eq!(memory.generation(), 2);
    }
    #[test]
    fn deferred_batch_keeps_a_while_b_waits_and_renderer_c_cannot_consume_its_reservation() {
        let memory = ManagedBytes { budget: 16, ..Default::default() };
        let a = blob(b"aaaaaaaa");
        let b = blob(b"bbbbbbbb");
        let c = blob(b"cccccccc");
        let lease = memory.reserve(vec![a.clone(), b.clone()]).unwrap();
        assert!(!lease.ready().unwrap());
        assert_eq!(memory.take_wanted().len(), 2);
        memory.insert(&a, Arc::from(b"aaaaaaaa".as_slice())).unwrap();
        assert!(memory.get(&c).is_err());
        assert!(memory.take_wanted().is_empty());
        memory.insert(&b, Arc::from(b"bbbbbbbb".as_slice())).unwrap();
        assert!(lease.ready().unwrap());
        assert!(memory.peek(&a).is_ok());
        assert!(memory.peek(&b).is_ok());
        assert!(memory.take_wanted().is_empty());
        assert_eq!(memory.status()["reservedBytes"], 16);
        drop(lease);
        assert_eq!(memory.take_wanted().len(), 1);
        memory.insert(&c, Arc::from(b"cccccccc".as_slice())).unwrap();
        assert!(memory.status()["reservedBytes"].as_u64().unwrap() <= 16);
    }
    #[test]
    fn shared_leases_cancel_only_after_last_release_and_account_until_network_settles() {
        let memory = ManagedBytes { budget: 8, ..Default::default() };
        let a = blob(b"aaaaaaaa");
        let b = blob(b"bbbbbbbb");
        let first = memory.reserve(vec![a.clone(), a.clone()]).unwrap();
        let second = memory.reserve(vec![a.clone()]).unwrap();
        let download = memory.take_wanted().pop().unwrap();
        drop(first);
        assert!(!download.cancel.load(Ordering::Relaxed));
        drop(second);
        assert!(download.cancel.load(Ordering::Relaxed));
        assert!(memory.reserve(vec![b.clone()]).is_err());
        assert_eq!(memory.status()["reservedBytes"], 8);
        let third = memory.reserve(vec![a.clone()]).unwrap();
        memory.failed(&a, "aborted".into());
        assert!(!third.ready().unwrap());
        assert_eq!(memory.take_wanted().len(), 1);
        memory.failed(&a, "offline".into());
        assert!(third.ready().unwrap_err().contains("offline"));
        drop(third);
        assert_eq!(memory.status()["reservedBytes"], 0);
        assert!(memory.reserve(vec![b]).is_ok());
    }
    #[test]
    fn renderer_and_commands_share_four_slots_and_failed_reservations_are_atomic() {
        let memory = ManagedBytes { budget: 8, ..Default::default() };
        let blobs: Vec<_> = (0..8).map(|n| blob(&[n])).collect();
        for blob in &blobs {
            assert!(memory.get(blob).is_err());
        }
        let first = memory.take_wanted();
        assert_eq!(first.len(), 4);
        let lease = memory.reserve(blobs.clone()).unwrap();
        assert!(memory.take_wanted().is_empty());
        assert!(memory.reserve(vec![blob(b"too large")]).is_err());
        assert_eq!(memory.status()["pinned"], 8);
        for download in first {
            let n = blobs.iter().position(|b| b == &download.blob).unwrap();
            memory.insert(&download.blob, Arc::from(vec![n as u8])).unwrap();
        }
        let second = memory.take_wanted();
        assert_eq!(second.len(), 4);
        for download in second {
            let n = blobs.iter().position(|b| b == &download.blob).unwrap();
            memory.insert(&download.blob, Arc::from(vec![n as u8])).unwrap();
        }
        assert!(lease.ready().unwrap());
        assert_eq!(memory.status()["reservedBytes"], 8);
    }
    #[test]
    fn cancellation_releases_queued_work_and_active_retry_retains_its_reservation() {
        let memory = ManagedBytes { budget: 8, ..Default::default() };
        let a = blob(b"aaaaaaaa");
        let queued = memory.reserve(vec![a.clone()]).unwrap();
        drop(queued);
        assert!(memory.take_wanted().is_empty());
        assert_eq!(memory.status()["pinned"], 0);
        let lease = memory.reserve(vec![a.clone()]).unwrap();
        assert_eq!(memory.take_wanted().len(), 1);
        memory.failed(&a, "offline".into());
        assert!(lease.ready().is_err());
        memory.retry();
        assert!(!lease.ready().unwrap());
        assert_eq!(memory.take_wanted().len(), 1);
        assert_eq!(memory.status()["reservedBytes"], 8);
        memory.insert(&a, Arc::from(b"aaaaaaaa".as_slice())).unwrap();
        assert!(lease.ready().unwrap());
    }
}
