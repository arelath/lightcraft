//! Serialized browser cache I/O. The Web Lock spans policy read, eviction and writes
//! across tabs/workers. Metadata saves can consume reserved space; media cannot.
use super::*;
use crate::{
    backend::Backend,
    files::{Files, Protection},
};
use js_sys::{Array, Function, Promise, Reflect};
use lightcraft_engine::catalog::assets::Blob;
use serde_json::{Value, json};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

#[derive(Default)]
struct Health {
    temporary: BTreeSet<String>,
    durable: BTreeSet<String>,
    error: Option<String>,
    quota: Option<u64>,
    usage: Option<u64>,
    bytes: u64,
    entries: usize,
    headroom: u64,
    current: Option<(String, Files)>,
    verified_index: bool,
    owner_revision: Option<u64>,
}
thread_local! { static HEALTH:RefCell<Health>=RefCell::new(Health::default()); }
const TRANSACTION: &str = "cloud/cache-transaction";
const PROTECTION: &str = "cloud/cache-protection-pending";
const ESTABLISHED: &str = "cloud/cache-established";
pub const RESTORE_PENDING: &str = "browser-restore-pending";
const DEFERRED: &str = "Browser catalog save is pending; cache cleanup will retry after it completes.";
pub fn deferred(error: &str) -> bool {
    error == DEFERRED
}
pub fn bind(dir: String, files: Files) {
    HEALTH.with(|h| h.borrow_mut().current = Some((dir, files)));
}
fn current() -> Option<(String, Files)> {
    HEALTH.with(|h| h.borrow().current.clone())
}
fn stable() -> Result<Protection, String> {
    let (_, files) = current().ok_or(DEFERRED)?;
    let protection = files.protection();
    if protection.pending {
        return Err(DEFERRED.into());
    }
    Ok(protection)
}
pub fn certified() -> bool {
    stable().is_ok_and(|p| {
        HEALTH.with(|h| {
            let h = h.borrow();
            h.verified_index && h.owner_revision == Some(p.revision)
        })
    })
}
fn require_certificate() -> Result<(), String> {
    if certified() { Ok(()) } else { Err(DEFERRED.into()) }
}
fn invalidate() {
    HEALTH.with(|h| {
        let mut h = h.borrow_mut();
        h.verified_index = false;
        h.owner_revision = None;
    });
}

/// A failed blob/index save, deletion or restore always leaves a durable recovery marker.
pub struct Transaction {
    _lock: Option<Lock>,
    complete: bool,
}
impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.complete {
            invalidate();
        }
    }
}
async fn begin(backend: &Backend) -> Result<Transaction, String> {
    backend.write(TRANSACTION, b"1").await?;
    Ok(Transaction { _lock: None, complete: false })
}
impl Transaction {
    async fn finish(mut self, backend: &Backend, policy: &Policy) -> Result<(), String> {
        save(backend, policy).await?;
        backend.remove(TRANSACTION).await?;
        self.complete = true;
        HEALTH.with(|h| h.borrow_mut().verified_index = true);
        Ok(())
    }
}
/// Backup restore writes directly to storage. Its whole transaction shares the cache lock.
pub async fn restoring(backend: &Backend) -> Result<Transaction, String> {
    let lock = lock().await?;
    let mut transaction = begin(backend).await?;
    transaction._lock = Some(lock);
    invalidate();
    Ok(transaction)
}
pub fn status(resident: &BTreeSet<String>) -> Value {
    HEALTH.with(|h| {let h=h.borrow();json!({"temporaryBlobs":resident.iter().filter(|id|!h.durable.contains(*id)||h.temporary.contains(*id)).count(),"error":h.error,"quotaBytes":h.quota,"usageBytes":h.usage,"reservedHeadroomBytes":h.headroom.max(HEADROOM),"bytes":h.bytes,"entries":h.entries})})
}
pub fn temporary(blob: &Blob, error: String) {
    HEALTH.with(|h| {
        let mut h = h.borrow_mut();
        if h.temporary.len() < 100_000 {
            h.temporary.insert(blob.id.0.clone());
        }
        h.error = Some(error);
    });
}
pub fn reserve_catalog(bytes: u64) {
    HEALTH.with(|h| {
        let mut h = h.borrow_mut();
        h.headroom = h.headroom.max(bytes).max(HEADROOM);
    });
}
fn required() -> u64 {
    HEALTH.with(|h| h.borrow().headroom.max(HEADROOM))
}
fn note(policy: &Policy) {
    HEALTH.with(|h| {
        let mut h = h.borrow_mut();
        h.bytes = policy.bytes();
        h.entries = policy.entries.len();
        h.durable = policy.entries.keys().cloned().collect();
        h.temporary.retain(|id| !policy.entries.contains_key(id));
        if h.temporary.is_empty() {
            h.error = None;
        }
        h.headroom = h.headroom.max(policy.headroom);
    });
}
pub(crate) struct Lock(Function);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.call0(&JsValue::UNDEFINED);
    }
}
async fn lock() -> Result<Lock, String> {
    named_lock("lightcraft-managed-cache-v1").await
}
pub(crate) async fn verification_lock() -> Result<Lock, String> {
    named_lock("lightcraft-remote-verification-v1").await
}
async fn named_lock(name: &str) -> Result<Lock, String> {
    let navigator = Reflect::get(&js_sys::global(), &"navigator".into()).map_err(|_| "browser storage coordination unavailable")?;
    let locks = Reflect::get(&navigator, &"locks".into()).map_err(|_| "Web Locks unavailable; cache retained")?;
    let request = Reflect::get(&locks, &"request".into())
        .ok()
        .and_then(|v| v.dyn_into::<Function>().ok())
        .ok_or("Web Locks unavailable; persistent cache writes disabled")?;
    let mut release = None;
    let released = Promise::new(&mut |resolve, _| release = Some(resolve));
    let mut grant = None;
    let granted = Promise::new(&mut |resolve, _| grant = Some(resolve));
    let grant = grant.ok_or("could not coordinate browser cache")?;
    let callback = Closure::once_into_js(move |_: JsValue| {
        let _ = grant.call0(&JsValue::UNDEFINED);
        released
    });
    let requested = request.call2(&locks, &name.into(), &callback).map_err(|_| "could not acquire browser storage lock")?;
    let race = Array::new();
    race.push(&granted);
    race.push(&requested);
    JsFuture::from(Promise::race(&race)).await.map_err(|_| "browser cache coordination failed")?;
    Ok(Lock(release.ok_or("browser cache release unavailable")?))
}
async fn estimate() -> Option<(u64, u64)> {
    let navigator = Reflect::get(&js_sys::global(), &"navigator".into()).ok()?;
    let storage = Reflect::get(&navigator, &"storage".into()).ok()?;
    let method = Reflect::get(&storage, &"estimate".into()).ok()?.dyn_into::<Function>().ok()?;
    let estimate = JsFuture::from(method.call0(&storage).ok()?.dyn_into::<Promise>().ok()?).await.ok()?;
    let number = |key: &str| {
        Reflect::get(&estimate, &key.into()).ok()?.as_f64().filter(|n| n.is_finite() && *n >= 0. && *n <= u64::MAX as f64).map(|n| n as u64)
    };
    let quota = number("quota")?;
    let usage = number("usage")?;
    HEALTH.with(|h| {
        let mut h = h.borrow_mut();
        h.quota = Some(quota);
        h.usage = Some(usage);
    });
    Some((quota, usage))
}
pub async fn load(backend: &Backend) -> Result<Policy, String> {
    let _lock = lock().await?;
    load_locked(backend).await
}
async fn load_locked(backend: &Backend) -> Result<Policy, String> {
    let mut policy = match backend.read_limited(POLICY_KEY, POLICY_LIMIT).await? {
        Some(bytes) => Policy::decode(&bytes)?,
        None => {
            if backend.exists(ESTABLISHED).await? {
                return Err("Browser cache policy is missing; offline copies retained for repair.".into());
            }
            Policy::default()
        }
    };
    let interrupted = backend.read_limited(TRANSACTION, 64).await?.is_some();
    if interrupted || !HEALTH.with(|h| h.borrow().verified_index) {
        let protection = stable()?;
        audit(backend, &mut policy).await?;
        if stable()? != protection {
            return Err(DEFERRED.into());
        }
        let transaction = begin(backend).await?;
        transaction.finish(backend, &policy).await?;
        backend.remove(PROTECTION).await?;
        if stable()? != protection {
            return Err(DEFERRED.into());
        }
        HEALTH.with(|h| h.borrow_mut().owner_revision = Some(protection.revision));
    }
    note(&policy);
    Ok(policy)
}
async fn audit(backend: &Backend, policy: &mut Policy) -> Result<(), String> {
    use super::recovery::*;
    let mut verified = BTreeMap::new();
    let mut total = 0u64;
    for name in backend.names_bounded("cloud/blobs", MAX_FILES).await? {
        if !hash(&name) {
            return Err("Unknown browser cache file; copies retained for repair.".into());
        }
        let Some((size, digest)) = backend.file_digest(&format!("cloud/blobs/{name}"), super::super::MAX_BROWSER_BLOB).await? else {
            return Err("Browser cache changed during recovery; retry.".into());
        };
        total = total.saturating_add(size);
        if total > MAX_AUDIT_BYTES {
            return Err("Browser cache exceeds the bounded recovery limit; copies retained.".into());
        }
        let (id, size) = verified_entry(&name, size, &digest)?;
        verified.insert(id, size);
    }
    entries(policy, verified)?;
    note(policy);
    let mut dirs: BTreeSet<_> = backend.names_bounded("", 200_000).await?.into_iter().filter(|dir| crate::backup::valid_library_dir(dir)).collect();
    dirs.extend(policy.libraries.keys().cloned());
    if dirs.len() > MAX_LIBRARIES {
        return Err("Too many browser libraries to recover safely; copies retained.".into());
    }
    let mut libraries = BTreeMap::new();
    let mut metadata = 0u64;
    for dir in dirs {
        let legacy = policy.libraries.get(&dir);
        if legacy.is_none() && backend.read_limited(&format!("{dir}/{RESTORE_PENDING}"), 64).await?.is_some() {
            continue;
        }
        let intent = backend.read_limited(&format!("{dir}/{OWNERS_FILE}"), POLICY_LIMIT).await?;
        if intent.is_none() && legacy.is_none_or(BTreeMap::is_empty) {
            continue;
        }
        let snapshot = backend.read_limited(&format!("{dir}/catalog.snap"), MAX_CATALOG_FILE).await?;
        let journal = backend.read_limited(&format!("{dir}/catalog.log"), MAX_CATALOG_FILE).await?;
        metadata = metadata
            .saturating_add(snapshot.as_ref().map_or(0, |b| b.len() as u64))
            .saturating_add(journal.as_ref().map_or(0, |b| b.len() as u64))
            .saturating_add(intent.as_ref().map_or(0, |b| b.len() as u64));
        if metadata > MAX_METADATA {
            return Err("Browser offline catalogs exceed the bounded recovery limit; copies retained.".into());
        }
        if let Some(owners) = owners(&dir, snapshot, journal, intent, legacy)? {
            libraries.insert(dir, owners);
        }
    }
    policy.libraries = libraries;
    Ok(())
}
async fn save(backend: &Backend, policy: &Policy) -> Result<(), String> {
    let bytes = serde_json::to_vec(policy).map_err(|e| e.to_string())?;
    Policy::decode(&bytes)?;
    backend.write(POLICY_KEY, &bytes).await?;
    backend.write(ESTABLISHED, b"1").await?;
    note(policy);
    Ok(())
}
async fn evict(backend: &Backend, policy: &mut Policy, gone: Vec<String>) -> Result<u64, String> {
    require_certificate()?;
    let transaction = begin(backend).await?;
    let mut freed = 0u64;
    for id in gone {
        require_certificate()?;
        backend.remove(&format!("cloud/blobs/{id}")).await?;
        if let Some(entry) = policy.entries.remove(&id) {
            freed = freed.saturating_add(entry.size);
        }
    }
    transaction.finish(backend, policy).await?;
    Ok(freed)
}
async fn room(backend: &Backend, policy: &mut Policy, incoming: u64, reserve: u64, budget: u64, keep: Option<&str>) -> Result<(), String> {
    let (quota, usage) = estimate().await.ok_or("browser storage estimate unavailable; viewing remains temporary")?;
    // Writable streams / IDB replacement may need a second copy before commit.
    let gone = policy.evictions(incoming, quota.saturating_sub(usage).saturating_sub(incoming), reserve, budget, keep)?;
    if !gone.is_empty() {
        evict(backend, policy, gone).await?;
    }
    Ok(())
}
pub async fn store(backend: &Backend, blob: &Blob, bytes: &[u8]) -> Result<(), String> {
    super::super::verify_blob(blob, bytes)?;
    let _lock = lock().await?;
    let mut policy = load_locked(backend).await?;
    policy.headroom = policy.headroom.max(required());
    let protected = policy.protected();
    let pinned = policy.entries.iter().filter(|(id, _)| protected.contains(id.as_str())).fold(0u64, |n, (_, e)| n.saturating_add(e.size));
    let extra = if protected.contains(blob.id.0.as_str()) && !policy.entries.contains_key(&blob.id.0) { blob.size } else { 0 };
    let budget = policy.budget.max(pinned.saturating_add(extra));
    let reserve = policy.headroom;
    room(backend, &mut policy, blob.size, reserve, budget, Some(&blob.id.0)).await?;
    let transaction = begin(backend).await?;
    let key = super::super::cache_key(blob);
    write_blob(backend, &key, bytes)
        .await
        .map_err(|e| format!("Browser cache save failed: {e}; unpin offline copies or free site storage, then retry"))?;
    let used = js_sys::Date::now().max(0.) as u64;
    policy.entries.insert(blob.id.0.clone(), Entry { size: blob.size, used });
    transaction.finish(backend, &policy).await?;
    HEALTH.with(|h| {
        let mut h = h.borrow_mut();
        h.temporary.remove(&blob.id.0);
        if h.temporary.is_empty() {
            h.error = None;
        }
    });
    Ok(())
}
async fn write_blob(backend: &Backend, key: &str, bytes: &[u8]) -> Result<(), String> {
    let result = backend.write(key, bytes).await;
    if result.is_err() && matches!(backend.read_limited(key,0).await,Ok(Some(empty)) if empty.is_empty()) {
        // OPFS can create an empty directory entry before createWritable fails.
        // It is not a verified copy; never leave it masquerading as one in backup.
        let _ = backend.remove(key).await;
    }
    result
}
pub async fn touch(backend: &Backend, blob: &Blob) -> Result<(), String> {
    let _lock = lock().await?;
    let mut policy = load_locked(backend).await?;
    if let Some(entry) = policy.entries.get_mut(&blob.id.0) {
        if entry.size != blob.size || !backend.exists(&super::super::cache_key(blob)).await? {
            return Err("Cached copy changed; retry its download.".into());
        }
        entry.used = js_sys::Date::now().max(0.) as u64;
        begin(backend).await?.finish(backend, &policy).await?;
    }
    Ok(())
}
/// Catalog writes never reclaim against a potentially stale offline selection. The
/// marker precedes every protected write under the same lock used by deletion.
pub async fn critical(backend: &Backend, path: &str, offset: Option<usize>, bytes: &[u8]) -> Result<(), String> {
    let _lock = lock().await?;
    if path.rsplit('/').next().is_some_and(crate::files::protects_cache) {
        backend.write(PROTECTION, b"1").await?;
        HEALTH.with(|h| h.borrow_mut().owner_revision = None);
    }
    backend.write_at(path, offset, bytes).await
}
pub async fn auxiliary(backend: &Backend, path: &str, bytes: &[u8]) -> Result<(), String> {
    let _lock = lock().await?;
    // Render workers do not own a catalog mirror and cannot certify offline owners.
    // They may use available headroom but never reclaim managed copies.
    if current().is_none() {
        let (quota, usage) = estimate().await.ok_or("Browser storage estimate unavailable")?;
        if quota.saturating_sub(usage) < (bytes.len() as u64).saturating_mul(2).saturating_add(required()) {
            return Err("Browser catalog headroom reserved; thumbnail remains temporary.".into());
        }
        return backend.write(path, bytes).await;
    }
    let mut policy = load_locked(backend).await?;
    let reserve = policy.headroom.max(required());
    room(backend, &mut policy, bytes.len() as u64, reserve, u64::MAX, None).await?;
    backend.write(path, bytes).await
}
pub async fn update_owners(
    backend: &Backend,
    dir: &str,
    owners: BTreeMap<String, Owner>,
    budget: Option<u64>,
    clear: bool,
    revision: u64,
) -> Result<Policy, String> {
    let _lock = lock().await?;
    let mut policy = load_locked(backend).await?;
    let protection = stable()?;
    let (current_dir, files) = current().ok_or(DEFERRED)?;
    if current_dir != dir || protection.revision != revision {
        return Err(DEFERRED.into());
    }
    let intent = match files.get(OWNERS_FILE) {
        Some(bytes) => super::decode_intent(&bytes)?.into_iter().map(|(id, pin)| (id, pin.quality)).collect::<BTreeMap<_, _>>(),
        None => policy.libraries.get(dir).into_iter().flat_map(|owners| owners.iter()).map(|(id, pin)| (id.clone(), pin.quality)).collect(),
    };
    if intent != owners.iter().map(|(id, pin)| (id.clone(), pin.quality)).collect() {
        return Err(DEFERRED.into());
    }
    let changed = policy.libraries.get(dir) != Some(&owners) || budget.is_some_and(|b| b != policy.budget) || policy.headroom < required();
    policy.headroom = policy.headroom.max(required());
    policy.libraries.insert(dir.into(), owners);
    if let Some(budget) = budget {
        policy.budget = budget;
    }
    if changed {
        begin(backend).await?.finish(backend, &policy).await?;
    }
    if stable()? != protection {
        return Err(DEFERRED.into());
    }
    backend.remove(PROTECTION).await?;
    if stable()? != protection {
        return Err(DEFERRED.into());
    }
    HEALTH.with(|h| h.borrow_mut().owner_revision = Some(revision));
    let protected = policy.protected();
    let pinned = policy.entries.iter().filter(|(id, _)| protected.contains(id.as_str())).fold(0u64, |n, (_, e)| n.saturating_add(e.size));
    let gone = if clear {
        policy.entries.keys().filter(|id| !protected.contains(id.as_str())).cloned().collect()
    } else {
        policy.evictions(0, u64::MAX, 0, policy.budget.max(pinned), None)?
    };
    if !gone.is_empty() {
        evict(backend, &mut policy, gone).await?;
    }
    Ok(policy)
}

pub async fn ensure(backend: &Backend, blob: &Blob, bytes: &[u8]) -> Result<(), String> {
    let is_temporary = HEALTH.with(|h| h.borrow().temporary.contains(&blob.id.0));
    if !is_temporary
        && load(backend).await?.entries.get(&blob.id.0).is_some_and(|entry| entry.size == blob.size)
        && backend.exists(&super::super::cache_key(blob)).await?
    {
        return Ok(());
    }
    let result = store(backend, blob, bytes).await;
    if let Err(error) = &result {
        temporary(blob, error.clone());
    }
    result
}

pub async fn read_verified(backend: &Backend, blob: &Blob) -> Result<Option<Vec<u8>>, String> {
    let _lock = lock().await?;
    let Some(bytes) = backend.read_limited(&super::super::cache_key(blob), blob.size).await? else { return Ok(None) };
    super::super::verify_blob(blob, &bytes)?;
    Ok(Some(bytes))
}
