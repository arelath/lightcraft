//! Verified transient bytes for synchronous decoders. Misses queue asynchronous work;
//! eviction here releases only memory, never durable browser originals or pending data.
use super::verify_blob;
use lightcraft_engine::catalog::assets::Blob;
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

const BUDGET: usize = 512 << 20;
#[derive(Default)]
struct Inner {
    bytes: BTreeMap<String, (Arc<[u8]>, u64)>,
    wanted: BTreeMap<String, Blob>,
    loading: BTreeMap<String, Blob>,
    errors: BTreeMap<String, String>,
    clock: u64,
    total: usize,
}
#[derive(Clone, Default)]
pub struct ManagedBytes {
    inner: Arc<Mutex<Inner>>,
    generation: Arc<AtomicU64>,
    wake: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}
impl ManagedBytes {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
        session.media.managed_reader = Some(Arc::new(move |blob| reader.get(blob)));
        session.media.managed_generation = Some(self.counter());
    }
    pub fn get(&self, blob: &Blob) -> Result<Arc<[u8]>, String> {
        blob.validate()?;
        let mut g = self.lock();
        g.clock = g.clock.saturating_add(1);
        let clock = g.clock;
        if let Some((bytes, last)) = g.bytes.get_mut(&blob.id.0) {
            if bytes.len() as u64 != blob.size {
                return Err("cached blob length differs from its immutable description".into());
            }
            *last = clock;
            return Ok(bytes.clone());
        }
        if let Some(error) = g.errors.get(&blob.id.0) {
            return Err(error.clone());
        }
        if !g.loading.contains_key(&blob.id.0) {
            if g.wanted.len() >= 1000 {
                return Err("browser cloud queue is full; retry after current downloads".into());
            }
            g.wanted.entry(blob.id.0.clone()).or_insert_with(|| blob.clone());
        }
        drop(g);
        let wake = self.wake.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(wake) = wake {
            wake();
        }
        Err("cloud bytes are loading asynchronously; connect and retry if offline".into())
    }
    pub fn insert(&self, blob: &Blob, bytes: Arc<[u8]>) -> Result<(), String> {
        verify_blob(blob, &bytes)?;
        let mut g = self.lock();
        g.clock = g.clock.saturating_add(1);
        let clock = g.clock;
        g.loading.remove(&blob.id.0);
        g.wanted.remove(&blob.id.0);
        g.errors.remove(&blob.id.0);
        if let Some((old, _)) = g.bytes.insert(blob.id.0.clone(), (bytes.clone(), clock)) {
            g.total = g.total.saturating_sub(old.len());
        }
        g.total = g.total.saturating_add(bytes.len());
        while g.total > BUDGET && g.bytes.len() > 1 {
            let key = g.bytes.iter().filter(|(key, _)| *key != &blob.id.0).min_by_key(|(_, (_, last))| last).map(|(key, _)| key.clone());
            let Some(key) = key else { break };
            if let Some((old, _)) = g.bytes.remove(&key) {
                g.total = g.total.saturating_sub(old.len());
            }
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    pub fn take_wanted(&self) -> Vec<Blob> {
        let mut g = self.lock();
        let slots = 4usize.saturating_sub(g.loading.len());
        let ids: Vec<_> = g.wanted.keys().take(slots).cloned().collect();
        let mut blobs = Vec::new();
        for id in ids {
            if let Some(blob) = g.wanted.remove(&id) {
                g.loading.insert(id, blob.clone());
                blobs.push(blob);
            }
        }
        blobs
    }
    pub fn failed(&self, blob: &Blob, error: String) {
        let mut g = self.lock();
        g.loading.remove(&blob.id.0);
        g.errors.insert(blob.id.0.clone(), error);
    }
    pub fn retry(&self) {
        self.lock().errors.clear();
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
    pub fn status(&self) -> serde_json::Value {
        let g = self.lock();
        serde_json::json!({"bytes":g.total,"resident":g.bytes.len(),"queued":g.wanted.len(),"loading":g.loading.len(),"errors":g.errors})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn blob() -> Blob {
        Blob {
            id: lightcraft_engine::assets::sha256(b"verified"),
            size: 8,
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
        let blob = blob();
        assert!(memory.get(&blob).is_err());
        assert!(memory.get(&blob).is_err());
        assert_eq!(memory.take_wanted(), vec![blob.clone()]);
        assert!(memory.take_wanted().is_empty());
        assert!(memory.insert(&blob, Arc::from(b"corrupt!".as_slice())).is_err());
        memory.failed(&blob, "offline; retry after connecting".into());
        assert!(memory.get(&blob).unwrap_err().contains("offline"));
        assert!(memory.take_wanted().is_empty());
        memory.retry();
        assert!(memory.get(&blob).is_err());
        assert_eq!(memory.take_wanted(), vec![blob.clone()]);
        memory.insert(&blob, Arc::from(b"verified".as_slice())).unwrap();
        assert_eq!(&*memory.get(&blob).unwrap(), b"verified");
        assert_eq!(memory.generation(), 2);
    }
}
