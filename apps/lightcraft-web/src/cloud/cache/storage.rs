//! Serialized browser cache I/O. The Web Lock spans policy read, eviction and writes
//! across tabs/workers. Metadata saves can consume reserved space; media cannot.
use super::*;
use crate::backend::Backend;
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
}
thread_local! { static HEALTH:RefCell<Health>=RefCell::new(Health::default()); }
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
        h.headroom = h.headroom.max(policy.headroom);
    });
}
struct Lock(Function);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.call0(&JsValue::UNDEFINED);
    }
}
async fn lock() -> Result<Lock, String> {
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
    let requested = request.call2(&locks, &"lightcraft-managed-cache-v1".into(), &callback).map_err(|_| "could not acquire browser cache lock")?;
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
    let policy = match backend.read_limited(POLICY_KEY, POLICY_LIMIT).await? {
        Some(bytes) => Policy::decode(&bytes)?,
        None => Policy::default(),
    };
    note(&policy);
    Ok(policy)
}
async fn save(backend: &Backend, policy: &Policy) -> Result<(), String> {
    let bytes = serde_json::to_vec(policy).map_err(|e| e.to_string())?;
    Policy::decode(&bytes)?;
    backend.write(POLICY_KEY, &bytes).await?;
    note(policy);
    Ok(())
}
async fn evict(backend: &Backend, policy: &mut Policy, gone: Vec<String>) -> Result<u64, String> {
    let mut freed = 0u64;
    for id in gone {
        backend.remove(&format!("cloud/blobs/{id}")).await?;
        if let Some(entry) = policy.entries.remove(&id) {
            freed = freed.saturating_add(entry.size);
        }
    }
    save(backend, policy).await?;
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
    let mut policy = load(backend).await?;
    policy.headroom = policy.headroom.max(required());
    let protected = policy.protected();
    let pinned = policy.entries.iter().filter(|(id, _)| protected.contains(id.as_str())).fold(0u64, |n, (_, e)| n.saturating_add(e.size));
    let extra = if protected.contains(blob.id.0.as_str()) && !policy.entries.contains_key(&blob.id.0) { blob.size } else { 0 };
    let budget = policy.budget.max(pinned.saturating_add(extra));
    let reserve = policy.headroom;
    room(backend, &mut policy, blob.size, reserve, budget, Some(&blob.id.0)).await?;
    let key = super::super::cache_key(blob);
    if let Err(error) = write_blob(backend, &key, bytes).await {
        if !crate::backend::quota_error(&error) {
            return Err(error);
        }
        reclaim(backend, &mut policy, Some(&blob.id.0)).await?;
        write_blob(backend, &key, bytes)
            .await
            .map_err(|e| format!("browser cache save failed: {e}; unpin offline copies or free site storage, then retry"))?;
    }
    let used = js_sys::Date::now().max(0.) as u64;
    policy.entries.insert(blob.id.0.clone(), Entry { size: blob.size, used });
    save(backend, &policy).await?;
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
    let mut policy = load(backend).await?;
    policy.entries.insert(blob.id.0.clone(), Entry { size: blob.size, used: js_sys::Date::now().max(0.) as u64 });
    save(backend, &policy).await
}
/// Called only by the catalog flush path. A failed save retains the dirty queue;
/// even a missing estimate/policy never authorizes deleting protected data.
pub async fn critical(backend: &Backend, path: &str, offset: Option<usize>, bytes: &[u8]) -> Result<(), String> {
    let _lock = lock().await.ok();
    if _lock.is_some()
        && let Ok(mut policy) = load(backend).await
    {
        let size = offset.unwrap_or(0).saturating_add(bytes.len()) as u64;
        let increased = policy.headroom.max(required()).max(size.saturating_mul(2).saturating_add(HEADROOM));
        if increased != policy.headroom {
            policy.headroom = increased;
            let _ = save(backend, &policy).await;
        }
        let _ = room(backend, &mut policy, size, 0, u64::MAX, None).await;
    }
    let result = backend.write_at(path, offset, bytes).await;
    if result.as_ref().is_err_and(|error| crate::backend::quota_error(error))
        && _lock.is_some()
        && let Ok(mut policy) = load(backend).await
    {
        let _ = reclaim(backend, &mut policy, None).await;
        return backend.write_at(path, offset, bytes).await;
    }
    result
}
async fn reclaim(backend: &Backend, policy: &mut Policy, keep: Option<&str>) -> Result<(), String> {
    let protected = policy.protected();
    let gone = policy.entries.keys().filter(|id| Some(id.as_str()) != keep && !protected.contains(id.as_str())).cloned().collect();
    evict(backend, policy, gone).await?;
    Ok(())
}
pub async fn auxiliary(backend: &Backend, path: &str, bytes: &[u8]) -> Result<(), String> {
    let _lock = lock().await?;
    let mut policy = load(backend).await?;
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
    conservative: bool,
) -> Result<Policy, String> {
    let _lock = lock().await?;
    let mut policy = load(backend).await?;
    policy.headroom = policy.headroom.max(required());
    let mut owners = owners;
    if conservative && let Some(previous) = policy.libraries.get(dir) {
        // Explicit unpins may free space before unrelated catalog writes finish.
        // Every retained owner keeps both the last durable and current targets.
        for (owner, pin) in &mut owners {
            if let Some(old) = previous.get(owner) {
                pin.blobs.extend(old.blobs.iter().cloned());
            }
        }
    }
    policy.libraries.insert(dir.into(), owners);
    if let Some(budget) = budget {
        policy.budget = budget;
    }
    // Protection is durable before any cache deletion or new download.
    save(backend, &policy).await?;
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
