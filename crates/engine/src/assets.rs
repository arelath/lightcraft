//! Asset acquisition is independent of decoding and catalog persistence.
//!
//! Native acquisitions run on render/transfer workers. Only verified immutable bytes are
//! published in the original cache. An acquisition lease prevents eviction during use.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

thread_local! {
    static CACHED_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Native UI dispatch uses this scope: acquisitions may verify local bytes but never wait
/// for a transfer or start network I/O. Render and transfer workers have independent scopes.
#[must_use]
pub struct CachedOnly {
    previous: bool,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl CachedOnly {
    pub fn enter(enabled: bool) -> Self {
        let previous = CACHED_ONLY.with(|flag| flag.replace(flag.get() || enabled));
        Self { previous, _thread: std::marker::PhantomData }
    }
}

impl Drop for CachedOnly {
    fn drop(&mut self) {
        CACHED_ONLY.with(|flag| flag.set(self.previous));
    }
}

fn pending_source() -> AssetError {
    AssetError::Pending
}

use lightcraft_catalog::assets::{Asset, Blob, BlobId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetError {
    #[error("source is not ready on this device; download it with cloud.fetch and retry")]
    Pending,
    #[error("invalid asset: {0}")]
    Invalid(String),
    #[error("cloud object is missing")]
    Missing,
    #[error("storage credentials were rejected; reconnect the storage connection")]
    Unauthorized,
    #[error("storage is temporarily unavailable: {0}")]
    Network(String),
    #[error("download failed integrity verification")]
    Integrity,
    #[error("asset cache: {0}")]
    Disk(String),
    #[error("asset transfer cancelled")]
    Cancelled,
    #[error("cloud storage is not configured: {0}")]
    Unconfigured(String),
    #[error("this storage operation is unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, AssetError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceRequirement {
    PreviewAllowed,
    OriginalRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum AssetAvailability {
    Remote,
    Downloading { done: u64, total: u64 },
    Cached,
    Missing,
    Failed { error: String },
}

/// Transport writes bytes only. The resolver owns their size/hash checks and publication.
pub trait BlobStore: Send + Sync {
    fn download(&self, blob: &Blob, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()>;
}

/// Catalog synchronization deliberately does not implement the local catalog `Store` trait.
pub trait CatalogSync: Send + Sync {
    fn read_head(&self, library: &str) -> Result<Option<(Vec<u8>, String)>>;
    fn publish_head(&self, library: &str, bytes: &[u8], expected_etag: Option<&str>) -> Result<String>;
}

pub trait TransferManager: Send + Sync {
    fn acquire(&self, blob: &Blob, cancel: &AtomicBool) -> Result<AssetLease>;
    fn availability(&self, blob: &BlobId) -> AssetAvailability;
}

struct Entry {
    gate: Mutex<()>,
    state: Mutex<AssetAvailability>,
    pins: AtomicUsize,
}

impl Default for Entry {
    fn default() -> Self {
        Self { gate: Mutex::new(()), state: Mutex::new(AssetAvailability::Remote), pins: AtomicUsize::new(0) }
    }
}

fn lock_entry(entry: &Entry) -> Result<MutexGuard<'_, ()>> {
    if CACHED_ONLY.with(std::cell::Cell::get) {
        return match entry.gate.try_lock() {
            Ok(gate) => Ok(gate),
            Err(TryLockError::Poisoned(error)) => Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) => Err(pending_source()),
        };
    }
    Ok(entry.gate.lock().unwrap_or_else(PoisonError::into_inner))
}

/// A verified local source. Keep this alive while decoding or handing the path to another app.
pub struct AssetLease {
    path: PathBuf,
    entry: Arc<Entry>,
}

impl AssetLease {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn read_bytes(&self) -> Result<Vec<u8>> {
        let file = std::fs::File::open(&self.path).map_err(disk)?;
        let size = file.metadata().map_err(disk)?.len();
        const MAX_DECODE_BYTES: u64 = 512 << 20;
        if size > MAX_DECODE_BYTES {
            return Err(AssetError::Unsupported("original exceeds the 512 MiB decode limit".into()));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(usize::try_from(size).map_err(|_| AssetError::Invalid("original is too large".into()))?)
            .map_err(|_| AssetError::Disk("not enough memory for original".into()))?;
        file.take(size.saturating_add(1)).read_to_end(&mut bytes).map_err(disk)?;
        if bytes.len() as u64 != size {
            return Err(AssetError::Integrity);
        }
        Ok(bytes)
    }
}

impl Drop for AssetLease {
    fn drop(&mut self) {
        self.entry.pins.fetch_sub(1, Ordering::Relaxed);
    }
}

fn disk(e: std::io::Error) -> AssetError {
    AssetError::Disk(e.to_string())
}

pub fn sha256(bytes: &[u8]) -> BlobId {
    BlobId(format!("{:x}", Sha256::digest(bytes)))
}

/// Device-local cache and a collection of configured transports. No local path is catalogued.
pub struct AssetResolver {
    root: PathBuf,
    stores: Mutex<BTreeMap<String, Arc<dyn BlobStore>>>,
    entries: Mutex<BTreeMap<BlobId, Arc<Entry>>>,
    generation: AtomicU64,
    wake: Mutex<Option<crate::availability::Waker>>,
}

impl AssetResolver {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            stores: Mutex::new(BTreeMap::new()),
            entries: Mutex::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
            wake: Mutex::new(None),
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
    pub fn set_waker(&self, wake: crate::availability::Waker) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
    }
    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        let wake = self.wake.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(wake) = wake {
            wake();
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn connect(&self, id: String, store: Arc<dyn BlobStore>) -> Result<()> {
        if !lightcraft_catalog::assets::identifier(&id) {
            return Err(AssetError::Invalid("invalid connection identity".into()));
        }
        self.stores.lock().unwrap_or_else(PoisonError::into_inner).insert(id, store);
        self.changed();
        Ok(())
    }
    fn entry(&self, id: &BlobId) -> Result<Arc<Entry>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if !entries.contains_key(id) && entries.len() >= 100_000 {
            return Err(AssetError::Invalid("too many active asset requests".into()));
        }
        Ok(entries.entry(id.clone()).or_insert_with(|| Arc::new(Entry::default())).clone())
    }
    pub fn acquire_asset(&self, asset: &Asset, requirement: SourceRequirement, cancel: &AtomicBool) -> Result<(AssetLease, bool)> {
        asset.validate().map_err(AssetError::Invalid)?;
        if requirement == SourceRequirement::PreviewAllowed
            && let Some(lease) = self.acquire_cached(&asset.original, cancel)?
        {
            return Ok((lease, false));
        }
        if requirement == SourceRequirement::PreviewAllowed
            && let Some(preview) = &asset.smart_preview
        {
            if let Ok(lease) = self.acquire(preview, cancel) {
                return Ok((lease, true));
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(AssetError::Cancelled);
            }
        }
        self.acquire(&asset.original, cancel).map(|lease| (lease, false))
    }

    /// Verify and lease local bytes only; synchronous decoder callbacks never start transfers.
    pub fn acquire_cached(&self, blob: &Blob, cancel: &AtomicBool) -> Result<Option<AssetLease>> {
        blob.validate().map_err(AssetError::Invalid)?;
        let entry = self.entry(&blob.id)?;
        let gate = lock_entry(&entry)?;
        let path = self.root.join(&blob.id.0);
        if !verify_file(&path, blob, cancel)? {
            return Ok(None);
        }
        let mut state = entry.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = *state != AssetAvailability::Cached;
        *state = AssetAvailability::Cached;
        drop(state);
        entry.pins.fetch_add(1, Ordering::Relaxed);
        let lease = AssetLease { path, entry: entry.clone() };
        drop(gate);
        if changed {
            self.changed();
        }
        Ok(Some(lease))
    }
    /// Eviction is serialized against acquisition and prohibited while a lease exists.
    pub fn evict(&self, blob: &Blob) -> Result<()> {
        blob.validate().map_err(AssetError::Invalid)?;
        let entry = self.entry(&blob.id)?;
        let _gate = lock_entry(&entry)?;
        if entry.pins.load(Ordering::Relaxed) != 0 {
            return Err(AssetError::Invalid("original is in use".into()));
        }
        match std::fs::remove_file(self.root.join(&blob.id.0)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(disk(e)),
        }
        *entry.state.lock().unwrap_or_else(PoisonError::into_inner) = AssetAvailability::Remote;
        drop(_gate);
        self.changed();
        Ok(())
    }
}

fn verify_file(path: &Path, blob: &Blob, cancel: &AtomicBool) -> Result<bool> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(disk(e)),
    };
    if file.metadata().map_err(disk)?.len() != blob.size {
        return Ok(false);
    }
    let mut hash = Sha256::new();
    let mut buf = [0u8; 64 << 10];
    let mut size = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        let n = file.read(&mut buf).map_err(disk)?;
        if n == 0 {
            break;
        }
        size = size.saturating_add(n as u64);
        if size > blob.size {
            return Ok(false);
        }
        hash.update(buf.get(..n).ok_or(AssetError::Integrity)?);
    }
    Ok(size == blob.size && format!("{:x}", hash.finalize()) == blob.id.0)
}

struct VerifiedWriter<'a> {
    out: &'a mut dyn Write,
    hash: Sha256,
    done: u64,
    expected: u64,
    cancel: &'a AtomicBool,
}
impl Write for VerifiedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("transfer cancelled"));
        }
        if self.done.saturating_add(bytes.len() as u64) > self.expected {
            return Err(std::io::Error::other("object exceeded its declared size"));
        }
        let n = self.out.write(bytes)?;
        self.hash.update(bytes.get(..n).ok_or_else(|| std::io::Error::other("invalid write count"))?);
        self.done = self.done.saturating_add(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl TransferManager for AssetResolver {
    fn acquire(&self, blob: &Blob, cancel: &AtomicBool) -> Result<AssetLease> {
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        blob.validate().map_err(AssetError::Invalid)?;
        let entry = self.entry(&blob.id)?;
        let _gate = lock_entry(&entry)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        let path = self.root.join(&blob.id.0);
        let mut availability_changed = false;
        let result = (|| {
            if !verify_file(&path, blob, cancel)? {
                if CACHED_ONLY.with(std::cell::Cell::get) {
                    return Err(pending_source());
                }
                let store = self
                    .stores
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&blob.remote.connection)
                    .cloned()
                    .ok_or_else(|| AssetError::Unconfigured(blob.remote.connection.clone()))?;
                std::fs::create_dir_all(&self.root).map_err(disk)?;
                *entry.state.lock().unwrap_or_else(PoisonError::into_inner) = AssetAvailability::Downloading { done: 0, total: blob.size };
                let mut transfer_error = None;
                let result = lightcraft_catalog::safe_file::write_atomic_with(&path, &mut |out| {
                    let mut writer = VerifiedWriter { out, hash: Sha256::new(), done: 0, expected: blob.size, cancel };
                    if let Err(e) = store.download(blob, &mut writer, cancel, &mut |done| {
                        *entry.state.lock().unwrap_or_else(PoisonError::into_inner) = AssetAvailability::Downloading { done, total: blob.size };
                    }) {
                        transfer_error = Some(e);
                        return Err(std::io::Error::other("object transfer failed"));
                    }
                    if cancel.load(Ordering::Relaxed) || writer.done != blob.size || format!("{:x}", writer.hash.finalize()) != blob.id.0 {
                        transfer_error = Some(if cancel.load(Ordering::Relaxed) { AssetError::Cancelled } else { AssetError::Integrity });
                        return Err(std::io::Error::other("object verification failed"));
                    }
                    Ok(())
                });
                if cancel.load(Ordering::Relaxed) {
                    return Err(AssetError::Cancelled);
                }
                if let Some(e) = transfer_error {
                    return Err(e);
                }
                result.map_err(disk)?;
            }
            let mut state = entry.state.lock().unwrap_or_else(PoisonError::into_inner);
            availability_changed = *state != AssetAvailability::Cached;
            *state = AssetAvailability::Cached;
            drop(state);
            entry.pins.fetch_add(1, Ordering::Relaxed);
            Ok(AssetLease { path, entry: entry.clone() })
        })();
        if let Err(error) = &result {
            *entry.state.lock().unwrap_or_else(PoisonError::into_inner) = match error {
                AssetError::Pending => AssetAvailability::Remote,
                AssetError::Missing => AssetAvailability::Missing,
                _ => AssetAvailability::Failed { error: error.to_string() },
            };
        }
        drop(_gate);
        if availability_changed {
            self.changed();
        }
        result
    }
    fn availability(&self, blob: &BlobId) -> AssetAvailability {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(blob)
            .map(|e| e.state.lock().unwrap_or_else(PoisonError::into_inner).clone())
            .unwrap_or(AssetAvailability::Remote)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub mod s3;

impl crate::Session {
    /// Detached reader for commands/jobs that require original bytes. Acquires only on use,
    /// so preparing an export on the UI thread does not perform any network I/O.
    pub fn original_reader_for(&self, sources: &[lightcraft_catalog::Source]) -> crate::merge::ByteReader {
        let resolver = self.media.asset_resolver.clone();
        let assets: BTreeMap<_, _> = sources
            .iter()
            .filter_map(|source| {
                let lightcraft_catalog::Source::Managed { asset_id } = source else { return None };
                self.catalog.asset(asset_id).map(|asset| (asset_id.clone(), asset.original.clone()))
            })
            .collect();
        let local = self.media.file_bytes.clone();
        Arc::new(move |path| {
            if let Some(id) = path.strip_prefix("lcasset:\0") {
                let blob = assets.get(&lightcraft_catalog::assets::AssetId(id.into())).ok_or("managed asset record is missing")?;
                let resolver = resolver.as_ref().ok_or("cloud storage is not configured on this device")?;
                let lease = resolver.acquire(blob, &AtomicBool::new(false)).map_err(|e| e.to_string())?;
                return lease.read_bytes().map_err(|e| e.to_string());
            }
            match &local {
                Some(read) => read(path),
                None => std::fs::read(path).map_err(|e| format!("{path}: {e}")),
            }
        })
    }
}

pub fn source_path(source: &lightcraft_catalog::Source) -> std::result::Result<String, String> {
    match source {
        lightcraft_catalog::Source::File { path } => Ok(path.clone()),
        // NUL cannot occur in a filesystem path, so a valid local filename can never collide
        // with the private original-reader handle.
        lightcraft_catalog::Source::Managed { asset_id } => Ok(format!("lcasset:\0{}", asset_id.0)),
        lightcraft_catalog::Source::Demo { .. } => Err("generated demo photos have no original file".into()),
    }
}

#[cfg(test)]
mod tests;
