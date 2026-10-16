//! Device-local retention for verified downloads. Durable imports/working copies must live
//! outside this directory: only flat SHA-256 blob files are eligible for eviction.

use super::{AssetError, Entry, Result, disk};
use lightcraft_catalog::assets::BlobId;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};

const DEFAULT_BUDGET: u64 = 20 << 30;
const MAX_RECORDS: usize = 100_000;
const MAX_POLICY_BYTES: u64 = 16 << 20;
const POLICY: &str = "retention.json";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OfflineQuality {
    Original,
    Preview,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod managed_tests;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfflinePin {
    pub quality: OfflineQuality,
    /// Derived dependencies; version-3 policy files persist only owner and quality.
    #[serde(default)]
    pub blobs: Vec<BlobId>,
}

/// Library-wide offline copies (Settings → Cache: "Store a copy of all …"). Their blobs are
/// derived from the catalog when the library opens or its assets change, never listed in the
/// policy, so a large library cannot exceed its record limits.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KeepAll {
    pub originals: bool,
    pub previews: bool,
}

/// Version 3 stores live owner/quality intent, with no authoritative saved blob membership.
/// Older snapshot pins are resolved against the catalog before eviction as well.
const POLICY_VERSION: u32 = 3;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    budget_bytes: u64,
    pins: BTreeMap<String, OfflinePin>,
    removed: BTreeSet<BlobId>,
    #[serde(default)]
    keep_all: KeepAll,
}
impl Default for Policy {
    fn default() -> Self {
        Self { version: POLICY_VERSION, budget_bytes: DEFAULT_BUDGET, pins: BTreeMap::new(), removed: BTreeSet::new(), keep_all: KeepAll::default() }
    }
}
impl Policy {
    fn validate(&self) -> Result<()> {
        if !matches!(self.version, 1 | 2 | POLICY_VERSION)
            || self.budget_bytes > (1 << 50)
            || self.pins.len() > MAX_RECORDS
            || self.removed.len() > MAX_RECORDS
        {
            return Err(AssetError::Invalid("unsupported or oversized cache retention policy".into()));
        }
        let mut count = self.removed.len();
        for (owner, pin) in &self.pins {
            if owner.len() > 64
                || !owner.split_once(':').is_some_and(|(kind, id)| matches!(kind, "photo" | "album") && id.parse::<u64>().is_ok_and(|id| id > 0))
            {
                return Err(AssetError::Invalid("invalid offline pin owner".into()));
            }
            count = count.saturating_add(pin.blobs.len());
            if (self.version < 3 && pin.blobs.is_empty()) || count > MAX_RECORDS || pin.blobs.iter().any(|id| !valid_id(id)) {
                return Err(AssetError::Invalid("invalid or oversized offline pins".into()));
            }
        }
        if self.removed.iter().any(|id| !valid_id(id)) {
            return Err(AssetError::Invalid("invalid removed-copy identity".into()));
        }
        Ok(())
    }
}
fn recompute_pins(state: &mut State) {
    state.pinned_ids = state.policy.pins.values().flat_map(|pin| pin.blobs.iter().cloned()).collect();
    state.pinned_ids.extend(state.pending_pin_ids.iter().cloned());
    if state.policy.keep_all != KeepAll::default() {
        state.pinned_ids.extend(state.library_ids.iter().cloned());
    }
}
fn valid_id(id: &BlobId) -> bool {
    id.0.len() == 64 && id.0.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatus {
    pub budget_bytes: u64,
    pub used_bytes: u64,
    pub pinned_bytes: u64,
    pub pins: BTreeMap<String, OfflinePin>,
    pub offline_errors: BTreeMap<String, String>,
    pub resolving_pins: bool,
    pub removed: BTreeSet<BlobId>,
    /// Library-wide offline copies, and how many blobs they currently protect.
    pub keep_all: KeepAll,
    pub keep_all_blobs: usize,
    pub staging_error: Option<String>,
    /// Damaged/future settings are preserved. Eviction and policy writes are disabled.
    pub error: Option<String>,
}

type Entries = Mutex<BTreeMap<BlobId, Arc<Entry>>>;
struct State {
    policy: Policy,
    pinned_ids: BTreeSet<BlobId>,
    /// Previous dependencies remain protected until the catalog mutation is durable.
    pending_pin_ids: BTreeSet<BlobId>,
    unresolved_pins: bool,
    unresolved_library: bool,
    offline_errors: BTreeMap<String, String>,
    /// Blobs protected by [`Policy::keep_all`], supplied by the session from its catalog.
    library_ids: BTreeSet<BlobId>,
    error: Option<String>,
    access: BTreeMap<BlobId, u64>,
    tick: u64,
    used: u64,
    pinned: u64,
    staging_error: Option<String>,
}
pub(super) struct Cache {
    root: PathBuf,
    durable_root: Mutex<Option<PathBuf>>,
    entries: Weak<Entries>,
    state: Mutex<State>,
    maintenance: Mutex<()>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) durable_io: Mutex<()>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) durable_epoch: std::sync::atomic::AtomicU64,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) staging: Mutex<super::managed::Policy>,
    sender: Option<mpsc::SyncSender<()>>,
}
impl Cache {
    pub(super) fn new(root: PathBuf, entries: Weak<Entries>) -> Arc<Self> {
        let loaded = (|| {
            let file = match std::fs::File::open(root.join(POLICY)) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Policy::default()),
                Err(e) => return Err(disk(e)),
            };
            let mut bytes = Vec::new();
            file.take(MAX_POLICY_BYTES + 1).read_to_end(&mut bytes).map_err(disk)?;
            if bytes.len() as u64 > MAX_POLICY_BYTES {
                return Err(AssetError::Invalid("cache retention settings exceed 16 MiB".into()));
            }
            let policy: Policy = serde_json::from_slice(&bytes).map_err(|_| AssetError::Invalid("damaged cache retention settings".into()))?;
            policy.validate()?;
            Ok(policy)
        })();
        let (policy, error) = match loaded {
            Ok(policy) => (policy, None),
            Err(error) => (Policy::default(), Some(error.to_string())),
        };
        #[cfg(not(target_arch = "wasm32"))]
        let (sender, receiver) = mpsc::sync_channel(1);
        let pinned_ids = policy.pins.values().flat_map(|pin| pin.blobs.iter().cloned()).collect();
        let unresolved_pins = !policy.pins.is_empty();
        let unresolved_library = policy.keep_all != KeepAll::default();
        let cache = Arc::new(Self {
            root,
            durable_root: Mutex::new(None),
            entries,
            state: Mutex::new(State {
                policy,
                pinned_ids,
                pending_pin_ids: BTreeSet::new(),
                unresolved_pins,
                unresolved_library,
                offline_errors: BTreeMap::new(),
                library_ids: BTreeSet::new(),
                error,
                access: BTreeMap::new(),
                tick: 0,
                used: 0,
                pinned: 0,
                staging_error: None,
            }),
            maintenance: Mutex::new(()),
            #[cfg(not(target_arch = "wasm32"))]
            durable_io: Mutex::new(()),
            #[cfg(not(target_arch = "wasm32"))]
            durable_epoch: std::sync::atomic::AtomicU64::new(0),
            #[cfg(not(target_arch = "wasm32"))]
            staging: Mutex::new(super::managed::Policy::default()),
            #[cfg(not(target_arch = "wasm32"))]
            sender: Some(sender),
            #[cfg(target_arch = "wasm32")]
            sender: None,
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            let weak = Arc::downgrade(&cache);
            if let Err(error) = std::thread::Builder::new().name("cloud-cache".into()).spawn(move || {
                while receiver.recv().is_ok() {
                    let Some(cache) = weak.upgrade() else { break };
                    if let Err(error) = cache.trim() {
                        log::warn!("cloud cache maintenance: {error}");
                    }
                }
            }) {
                cache.state.lock().unwrap_or_else(PoisonError::into_inner).error = Some(format!("could not start cache maintenance: {error}"));
            }
        }
        cache.schedule_trim();
        cache
    }
    pub(super) fn schedule_trim(&self) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(());
        }
    }
    pub(super) fn touch(&self, id: &BlobId) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.tick = state.tick.saturating_add(1);
        let tick = state.tick;
        if state.access.len() < MAX_RECORDS || state.access.contains_key(id) {
            state.access.insert(id.clone(), tick);
        }
    }
    pub(super) fn removed(&self, id: &BlobId) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.policy.removed.contains(id) && !state.pinned_ids.contains(id)
    }
    pub(super) fn status(&self) -> CacheStatus {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        CacheStatus {
            budget_bytes: state.policy.budget_bytes,
            used_bytes: state.used,
            pinned_bytes: state.pinned,
            pins: state.policy.pins.clone(),
            offline_errors: state.offline_errors.clone(),
            resolving_pins: state.unresolved_pins || state.unresolved_library,
            removed: state.policy.removed.clone(),
            keep_all: state.policy.keep_all,
            keep_all_blobs: if state.policy.keep_all == KeepAll::default() { 0 } else { state.library_ids.len() },
            staging_error: state.staging_error.clone(),
            error: state.error.clone(),
        }
    }
    pub(super) fn usage(&self) -> (u64, u64, u64) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.policy.budget_bytes, state.used, state.pinned)
    }
    pub(super) fn errors(&self) -> (Option<String>, Option<String>) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.error.clone(), state.staging_error.clone())
    }
    pub(super) fn pin(&self, owner: &str) -> Option<OfflinePin> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).policy.pins.get(owner).cloned()
    }
    fn save(&self, state: &mut State, mut policy: Policy) -> Result<()> {
        self.validate_root()?;
        if let Some(error) = &state.error {
            return Err(AssetError::Disk(format!(
                "retention settings preserved; repair {} before changing policy: {error}",
                self.root.join(POLICY).display()
            )));
        }
        policy.version = POLICY_VERSION;
        let mut durable = policy.clone();
        // Reopen resolves owners against the saved catalog before trimming. Blob arrays
        // are runtime dependencies, never an authoritative saved membership snapshot.
        for pin in durable.pins.values_mut() {
            pin.blobs.clear();
        }
        durable.validate()?;
        let bytes = serde_json::to_vec(&durable).map_err(|e| AssetError::Invalid(e.to_string()))?;
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err(AssetError::Invalid("cache retention settings exceed 16 MiB".into()));
        }
        std::fs::create_dir_all(&self.root).map_err(disk)?;
        lightcraft_catalog::safe_file::write_atomic(&self.root.join(POLICY), &bytes).map_err(disk)?;
        state.policy = policy;
        recompute_pins(state);
        Ok(())
    }
    pub(super) fn keep_all(&self) -> KeepAll {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).policy.keep_all
    }
    /// Save the library-wide offline choice with the blobs it protects now. Turning a kind on
    /// forgets earlier explicit local-copy removals of those blobs; turning it off deletes
    /// nothing (the copies become ordinary cache entries, evicted under the size limit).
    pub(super) fn set_keep_all(&self, keep: KeepAll, ids: BTreeSet<BlobId>) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut policy = state.policy.clone();
        policy.version = POLICY_VERSION;
        policy.keep_all = keep;
        policy.removed.retain(|id| !ids.contains(id));
        self.save(&mut state, policy)?;
        state.library_ids = ids;
        state.unresolved_library = false;
        recompute_pins(&mut state);
        drop(state);
        self.schedule_trim();
        Ok(())
    }
    /// Refresh the blobs [`KeepAll`] protects (in memory only: they are derived from the catalog).
    pub(super) fn set_library_ids(&self, ids: BTreeSet<BlobId>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.unresolved_library = false;
        if state.library_ids != ids {
            state.library_ids = ids;
            recompute_pins(&mut state);
        }
    }
    pub(super) fn set_budget(&self, bytes: u64) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut policy = state.policy.clone();
        policy.budget_bytes = bytes;
        self.save(&mut state, policy)?;
        drop(state);
        self.schedule_trim();
        Ok(())
    }
    pub(super) fn set_pins(&self, pins: &[(String, Option<OfflinePin>)]) -> Result<()> {
        if pins.len() > MAX_RECORDS
            || pins.iter().filter_map(|(_, pin)| pin.as_ref()).fold(0usize, |n, pin| n.saturating_add(pin.blobs.len())) > MAX_RECORDS
        {
            return Err(AssetError::Invalid("too many offline pin changes".into()));
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut policy = state.policy.clone();
        for (owner, pin) in pins {
            if let Some(pin) = pin {
                if pin.blobs.iter().any(|id| !valid_id(id)) {
                    return Err(AssetError::Invalid("invalid offline blob identity".into()));
                }
                for id in &pin.blobs {
                    policy.removed.remove(id);
                }
                policy.pins.insert(owner.clone(), pin.clone());
            } else {
                policy.pins.remove(owner);
            }
        }
        self.save(&mut state, policy)?;
        if state.policy.pins.is_empty() {
            state.unresolved_pins = false;
            state.pending_pin_ids.clear();
            state.offline_errors.clear();
            recompute_pins(&mut state);
        }
        drop(state);
        self.schedule_trim();
        Ok(())
    }
    pub(super) fn owners(&self) -> BTreeMap<String, OfflinePin> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .policy
            .pins
            .iter()
            .map(|(owner, pin)| (owner.clone(), OfflinePin { quality: pin.quality, blobs: Vec::new() }))
            .collect()
    }
    /// No disk I/O: the owner/quality policy is durable. Unsaved catalog changes add
    /// protection immediately and retain earlier dependencies through journal failure/retry.
    pub(super) fn resolve_pins(&self, pins: BTreeMap<String, OfflinePin>, errors: BTreeMap<String, String>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let resolved = state.unresolved_pins;
        if !resolved && state.policy.pins == pins && state.offline_errors == errors {
            return;
        }
        if state.policy.pins != pins {
            let previous: Vec<_> = state.policy.pins.values().flat_map(|pin| pin.blobs.iter().cloned()).collect();
            state.pending_pin_ids.extend(previous);
            for (owner, pin) in pins {
                if let Some(current) = state.policy.pins.get_mut(&owner).filter(|current| current.quality == pin.quality) {
                    current.blobs = pin.blobs;
                }
            }
        }
        state.unresolved_pins = false;
        state.offline_errors = errors;
        recompute_pins(&mut state);
        drop(state);
        if resolved {
            self.schedule_trim();
        }
    }
    pub(super) fn confirm_pins(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.pending_pin_ids.is_empty() {
            state.pending_pin_ids.clear();
            recompute_pins(&mut state);
            drop(state);
            self.schedule_trim();
        }
    }
    /// Called under the blob gate; concurrent pin changes are serialized by the policy mutex.
    pub(super) fn remove_local(&self, id: &BlobId) -> Result<()> {
        self.validate_root()?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.unresolved_pins || state.unresolved_library {
            return Err(AssetError::Invalid("offline policy is awaiting its library; open the catalog before removing copies".into()));
        }
        if state.pinned_ids.contains(id) {
            return Err(AssetError::Invalid("offline copy is pinned; unpin all photos/albums using it first".into()));
        }
        let mut policy = state.policy.clone();
        policy.removed.insert(id.clone());
        self.save(&mut state, policy)?;
        match std::fs::remove_file(self.root.join(&id.0)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(disk(e)),
        }
        drop(state);
        self.schedule_trim();
        Ok(())
    }
    pub(super) fn protect_durable_root(&self, root: PathBuf) {
        *self.durable_root.lock().unwrap_or_else(PoisonError::into_inner) = Some(root);
    }

    pub(super) fn validate_root(&self) -> Result<()> {
        if let Some(root) = &*self.durable_root.lock().unwrap_or_else(PoisonError::into_inner) {
            for protected in [root.clone(), root.join("blobs"), root.join("working")] {
                if crate::originals::paths_overlap(&self.root, &protected).map_err(AssetError::Invalid)? {
                    return Err(AssetError::Invalid(
                        "disposable cache overlaps durable managed data; repair the directory mapping before using cloud storage".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn trim(&self) -> Result<()> {
        self.trim_to(None).map(|_| ())
    }

    /// Settings → Cache → Clear cache: evict every eligible verified download now, whatever the
    /// size limit. Offline copies, files in use and durable or unsynchronized data are kept.
    /// Returns the bytes freed.
    pub(super) fn clear(&self) -> Result<u64> {
        self.trim_to(Some(0))
    }

    /// Evict least recently used eligible blobs down to `target` (else the size limit); returns
    /// the bytes freed.
    fn trim_to(&self, target: Option<u64>) -> Result<u64> {
        self.validate_root()?;
        let _maintenance = self.maintenance.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(error) = &self.state.lock().unwrap_or_else(PoisonError::into_inner).error {
            return Err(AssetError::Disk(error.clone()));
        }
        {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.unresolved_pins || state.unresolved_library {
                return Ok(0);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // A bad ownership record prevents staging migration, while verified
            // downloads retain their independent disposable-cache policy.
            let error = self.promote_staging().err().map(|error| error.to_string());
            self.state.lock().unwrap_or_else(PoisonError::into_inner).staging_error = error;
        }
        let directory = match std::fs::read_dir(&self.root) {
            Ok(directory) => directory,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(disk(e)),
        };
        let mut candidates = Vec::new();
        let mut used = 0u64;
        let mut pinned = 0u64;
        for (count, file) in directory.enumerate() {
            if count >= MAX_RECORDS + 1024 {
                return Err(AssetError::Invalid("cache contains too many directory entries to prune safely".into()));
            }
            let file = file.map_err(disk)?;
            let id = BlobId(file.file_name().to_string_lossy().to_string());
            if !valid_id(&id) || !file.file_type().map_err(disk)?.is_file() {
                continue;
            }
            let meta = file.metadata().map_err(disk)?;
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            used = used.saturating_add(meta.len());
            if state.pinned_ids.contains(&id) {
                pinned = pinned.saturating_add(meta.len());
            }
            candidates.push((state.access.get(&id).copied().unwrap_or(0), meta.modified().ok(), id, meta.len()));
        }
        candidates.sort_unstable_by_key(|a| (a.0, a.1));
        let measured = used;
        let Some(entries) = self.entries.upgrade() else { return Ok(0) };
        for (_, _, id, size) in candidates {
            if used <= target.unwrap_or_else(|| self.state.lock().unwrap_or_else(PoisonError::into_inner).policy.budget_bytes) {
                break;
            }
            // Ensure acquisitions of even a not-yet-requested on-disk blob share the same gate.
            let entry = {
                let mut entries = entries.lock().unwrap_or_else(PoisonError::into_inner);
                if entries.len() >= MAX_RECORDS && !entries.contains_key(&id) {
                    continue;
                }
                entries.entry(id.clone()).or_insert_with(|| Arc::new(Entry::default())).clone()
            };
            let _gate = match entry.gate.try_lock() {
                Ok(gate) => gate,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => continue,
            };
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if entry.pins.load(Ordering::Relaxed) != 0 || state.pinned_ids.contains(&id) {
                continue;
            }
            match std::fs::remove_file(self.root.join(&id.0)) {
                Ok(()) => {
                    used = used.saturating_sub(size);
                    *entry.state.lock().unwrap_or_else(PoisonError::into_inner) = super::AssetAvailability::Remote;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => used = used.saturating_sub(size),
                Err(e) => return Err(disk(e)),
            }
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.used = used;
        state.pinned = pinned;
        Ok(measured.saturating_sub(used))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn promote_staging(&self) -> Result<()> {
        self.promote_staging_cancel(&std::sync::atomic::AtomicBool::new(false))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn promote_staging_cancel(&self, cancel: &std::sync::atomic::AtomicBool) -> Result<()> {
        let Some(root) = self.durable_root.lock().unwrap_or_else(PoisonError::into_inner).clone() else {
            return Ok(());
        };
        let epoch = self.durable_epoch.load(Ordering::Acquire);
        let policy = self.staging.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if !policy.imports.values().any(|intent| intent.acknowledged) {
            return Ok(());
        }
        let candidates = super::managed::candidates(&root, &policy)?;
        let blob_directory = root.join("blobs");
        match std::fs::symlink_metadata(&blob_directory) {
            Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err(AssetError::Invalid("staged blob directory is redirected or invalid; preserved".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(disk(e)),
        }
        let _namespace = loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AssetError::Cancelled);
            }
            match self.durable_io.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        };
        if epoch != self.durable_epoch.load(Ordering::Acquire) {
            self.schedule_trim();
            return Ok(());
        }
        let Some(entries) = self.entries.upgrade() else {
            return Ok(());
        };
        for (id, blob) in candidates {
            if cancel.load(Ordering::Relaxed) {
                return Err(AssetError::Cancelled);
            }
            let source = root.join("blobs").join(&id.0);
            let meta = match std::fs::symlink_metadata(&source) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(disk(e)),
            };
            if !meta.file_type().is_file() || meta.file_type().is_symlink() {
                return Err(AssetError::Invalid("staged blob is not a regular file; preserved".into()));
            }
            let entry = {
                let mut entries = entries.lock().unwrap_or_else(PoisonError::into_inner);
                if entries.len() >= MAX_RECORDS && !entries.contains_key(&id) {
                    continue;
                }
                entries.entry(id.clone()).or_insert_with(|| Arc::new(Entry::default())).clone()
            };
            let _gate = match entry.gate.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => continue,
            };
            if entry.pins.load(Ordering::Relaxed) != 0 {
                continue;
            }
            let destination = self.root.join(&id.0);
            std::fs::create_dir_all(&self.root).map_err(disk)?;
            lightcraft_catalog::safe_file::write_atomic_with(&destination, &mut |out| {
                let mut file = std::fs::File::open(&source)?;
                let mut writer = super::VerifiedWriter { out, hash: sha2::Sha256::new(), done: 0, expected: blob.size, cancel };
                std::io::copy(&mut file.by_ref().take(blob.size.saturating_add(1)), &mut writer)?;
                if writer.done != blob.size || format!("{:x}", writer.hash.finalize()) != blob.id.0 {
                    return Err(std::io::Error::other("staged original failed integrity verification; preserved"));
                }
                Ok(())
            })
            .map_err(disk)?;
            if !super::verify_file(&destination, &blob, cancel)? {
                return Err(AssetError::Integrity);
            }
            // No path can be handed out while its gate is held; the verified cache
            // copy is durable before the staged link is removed. Working data and
            // other pending owners were excluded by the ownership scan.
            if epoch != self.durable_epoch.load(Ordering::Acquire) {
                self.schedule_trim();
                return Ok(());
            }
            std::fs::remove_file(&source).map_err(disk)?;
            *entry.state.lock().unwrap_or_else(PoisonError::into_inner) = super::AssetAvailability::Cached;
            self.touch(&id);
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn prepare_removal(&self, cancel: &std::sync::atomic::AtomicBool) -> Result<()> {
        self.validate_root()?;
        let _maintenance = loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AssetError::Cancelled);
            }
            match self.maintenance.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        };
        if let Some(error) = &self.state.lock().unwrap_or_else(PoisonError::into_inner).error {
            return Err(AssetError::Disk(error.clone()));
        }
        self.promote_staging_cancel(cancel)
    }
}
