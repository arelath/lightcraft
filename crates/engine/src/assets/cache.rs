//! Device-local retention for verified downloads. Durable imports/working copies must live
//! outside this directory: only flat SHA-256 blob files are eligible for eviction.

use super::{AssetError, Entry, Result, disk};
use lightcraft_catalog::assets::BlobId;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfflinePin {
    pub quality: OfflineQuality,
    pub blobs: Vec<BlobId>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    budget_bytes: u64,
    pins: BTreeMap<String, OfflinePin>,
    removed: BTreeSet<BlobId>,
}
impl Default for Policy {
    fn default() -> Self {
        Self { version: 1, budget_bytes: DEFAULT_BUDGET, pins: BTreeMap::new(), removed: BTreeSet::new() }
    }
}
impl Policy {
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.budget_bytes > (1 << 50) || self.pins.len() > MAX_RECORDS || self.removed.len() > MAX_RECORDS {
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
            if pin.blobs.is_empty() || count > MAX_RECORDS || pin.blobs.iter().any(|id| !valid_id(id)) {
                return Err(AssetError::Invalid("invalid or oversized offline pins".into()));
            }
        }
        if self.removed.iter().any(|id| !valid_id(id)) {
            return Err(AssetError::Invalid("invalid removed-copy identity".into()));
        }
        Ok(())
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
    pub removed: BTreeSet<BlobId>,
    /// Damaged/future settings are preserved. Eviction and policy writes are disabled.
    pub error: Option<String>,
}

type Entries = Mutex<BTreeMap<BlobId, Arc<Entry>>>;
struct State {
    policy: Policy,
    pinned_ids: BTreeSet<BlobId>,
    error: Option<String>,
    access: BTreeMap<BlobId, u64>,
    tick: u64,
    used: u64,
    pinned: u64,
}
pub(super) struct Cache {
    root: PathBuf,
    entries: Weak<Entries>,
    state: Mutex<State>,
    maintenance: Mutex<()>,
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
        let cache = Arc::new(Self {
            root,
            entries,
            state: Mutex::new(State { policy, pinned_ids, error, access: BTreeMap::new(), tick: 0, used: 0, pinned: 0 }),
            maintenance: Mutex::new(()),
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
        self.state.lock().unwrap_or_else(PoisonError::into_inner).policy.removed.contains(id)
    }
    pub(super) fn status(&self) -> CacheStatus {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        CacheStatus {
            budget_bytes: state.policy.budget_bytes,
            used_bytes: state.used,
            pinned_bytes: state.pinned,
            pins: state.policy.pins.clone(),
            removed: state.policy.removed.clone(),
            error: state.error.clone(),
        }
    }
    pub(super) fn usage(&self) -> (u64, u64, u64) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.policy.budget_bytes, state.used, state.pinned)
    }
    pub(super) fn pin(&self, owner: &str) -> Option<OfflinePin> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).policy.pins.get(owner).cloned()
    }
    fn save(&self, state: &mut State, policy: Policy) -> Result<()> {
        if let Some(error) = &state.error {
            return Err(AssetError::Disk(format!(
                "retention settings preserved; repair {} before changing policy: {error}",
                self.root.join(POLICY).display()
            )));
        }
        policy.validate()?;
        let bytes = serde_json::to_vec(&policy).map_err(|e| AssetError::Invalid(e.to_string()))?;
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err(AssetError::Invalid("cache retention settings exceed 16 MiB".into()));
        }
        std::fs::create_dir_all(&self.root).map_err(disk)?;
        lightcraft_catalog::safe_file::write_atomic(&self.root.join(POLICY), &bytes).map_err(disk)?;
        state.pinned_ids = policy.pins.values().flat_map(|pin| pin.blobs.iter().cloned()).collect();
        state.policy = policy;
        Ok(())
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
        if pins.len() > MAX_RECORDS {
            return Err(AssetError::Invalid("too many offline pin changes".into()));
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut policy = state.policy.clone();
        for (owner, pin) in pins {
            if let Some(pin) = pin {
                for id in &pin.blobs {
                    policy.removed.remove(id);
                }
                policy.pins.insert(owner.clone(), pin.clone());
            } else {
                policy.pins.remove(owner);
            }
        }
        self.save(&mut state, policy)?;
        drop(state);
        self.schedule_trim();
        Ok(())
    }
    /// Called under the blob gate; concurrent pin changes are serialized by the policy mutex.
    pub(super) fn remove_local(&self, id: &BlobId) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
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
    pub(super) fn trim(&self) -> Result<()> {
        let _maintenance = self.maintenance.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(error) = &self.state.lock().unwrap_or_else(PoisonError::into_inner).error {
            return Err(AssetError::Disk(error.clone()));
        }
        let directory = match std::fs::read_dir(&self.root) {
            Ok(directory) => directory,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
        let Some(entries) = self.entries.upgrade() else { return Ok(()) };
        for (_, _, id, size) in candidates {
            if used <= self.state.lock().unwrap_or_else(PoisonError::into_inner).policy.budget_bytes {
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
        Ok(())
    }
}
