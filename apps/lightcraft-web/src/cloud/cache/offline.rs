//! Current-library offline intent; origin-wide protection is saved before requesting
//! bytes. Downloads use the same leased admission queue as rendering and export.
use super::{Owner, Policy, storage};
use crate::{
    backend::Backend,
    cloud::memory::{ByteLease, ManagedBytes},
    files::Files,
};
use lightcraft_engine::{
    Session,
    activity::{Cancel, TaskGuard},
    assets::cache::{OfflinePin, OfflineQuality},
    catalog::{AlbumId, Source, assets::Blob},
};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

type Reply<T> = Rc<RefCell<Option<Result<T, String>>>>;
enum Work {
    Load(Reply<Policy>),
    Save(Reply<Policy>),
    Bytes(Reply<()>),
}
struct Job {
    key: (u64, u64),
    owners: BTreeMap<String, Owner>,
    blobs: Vec<Blob>,
    pos: usize,
    lease: Option<ByteLease>,
    guard: TaskGuard,
    saved: bool,
    clear: bool,
    budget: Option<u64>,
}
struct State {
    loaded: bool,
    available: bool,
    owners: BTreeMap<String, OfflinePin>,
    budget: u64,
    budget_change: Option<u64>,
    clear: bool,
    urgent: bool,
    confirm: bool,
    epoch: u64,
    attempted: Option<(u64, u64)>,
    completed: BTreeMap<String, Owner>,
    error: Option<String>,
    errors: BTreeMap<String, String>,
    work: Option<Work>,
    job: Option<Job>,
}
#[derive(Clone)]
pub struct Offline {
    state: Rc<RefCell<State>>,
    backend: Option<Backend>,
    dir: String,
    memory: ManagedBytes,
    files: Files,
}
fn reply<T>() -> Reply<T> {
    Rc::new(RefCell::new(None))
}
impl Offline {
    pub fn new(backend: Option<Backend>, dir: String, memory: ManagedBytes, files: Files) -> Self {
        storage::bind(dir.clone(), files.clone());
        Self {
            backend,
            dir,
            memory,
            files,
            state: Rc::new(RefCell::new(State {
                loaded: false,
                available: false,
                owners: BTreeMap::new(),
                budget: 128 << 20,
                budget_change: None,
                clear: false,
                urgent: false,
                confirm: false,
                epoch: 0,
                attempted: None,
                completed: BTreeMap::new(),
                error: None,
                errors: BTreeMap::new(),
                work: None,
                job: None,
            })),
        }
    }
    pub fn view(&self, s: &Session) -> Value {
        let d = self.state.borrow();
        let mut errors = d.errors.clone();
        if let Some(error) = &d.error {
            errors.insert("storage".into(), error.clone());
        }
        let ready = d.loaded
            && d.available
            && d.attempted == Some((s.catalog.revision, d.epoch))
            && d.job.is_none()
            && errors.is_empty()
            && !self.files.protection().pending
            && storage::certified()
            && self.files.error().is_none();
        let health = storage::status(&self.memory.resident_ids());
        json!({"browser":true,"location":"Browser storage","budgetBytes":d.budget,"usedBytes":health["bytes"],"pins":d.owners,
            "photoCacheBytes":health["bytes"],"offlineBytes":Value::Null,"keepAll":{"originals":d.owners.contains_key("all:originals"),"previews":d.owners.contains_key("all:previews")},
            "offlineReady":ready,"resolvingPins":!ready,"offlineErrors":errors,"error":health["error"],"persistentCache":health,
            "savePending":self.files.is_dirty()||self.files.protection().pending||d.job.as_ref().is_some_and(|job|!job.saved),"background":d.job.is_some()})
    }
    pub fn handle(&self, s: &mut Session, id: &str, p: &Value) -> Option<Result<Value, String>> {
        if !matches!(id, "cloud.cache" | "cloud.storage" | "cloud.pin" | "cloud.unpin" | "cloud.clearCache") {
            return None;
        }
        if matches!(id, "cloud.storage") || id == "cloud.cache" && p.as_object().is_none_or(|p| p.is_empty()) {
            return Some(Ok(self.view(s)));
        }
        let before = {
            let d = self.state.borrow();
            (d.owners.clone(), d.budget, d.budget_change, d.clear, d.urgent)
        };
        let result = (|| {
            let mut d = self.state.borrow_mut();
            if !d.loaded || !d.available {
                return Err("offline cache policy is not available yet; retry after storage opens or repair its reported error".into());
            }
            match id {
                "cloud.cache" => {
                    if let Some(value) = p.get("budgetMb") {
                        let mb = value.as_u64().filter(|v| *v <= 4096).ok_or("browser budgetMb must be between 0 and 4096")?;
                        d.budget_change = Some(mb << 20);
                        d.budget = mb << 20;
                    }
                    for (key, owner, quality) in [
                        ("keepAllOriginals", "all:originals", OfflineQuality::Original),
                        ("keepAllPreviews", "all:previews", OfflineQuality::Preview),
                    ] {
                        if let Some(value) = p.get(key) {
                            if value.as_bool().ok_or("offline retention setting must be true or false")? {
                                d.owners.insert(owner.into(), OfflinePin { quality, blobs: Vec::new() });
                            } else {
                                d.owners.remove(owner);
                            }
                        }
                    }
                }
                "cloud.clearCache" => d.clear = true,
                "cloud.pin" | "cloud.unpin" => {
                    let quality: OfflineQuality = serde_json::from_value(p.get("quality").cloned().unwrap_or(json!("original")))
                        .map_err(|_| "quality must be original or preview")?;
                    let owners = if let Some(owner) = p.get("owner").and_then(Value::as_str).filter(|_| id == "cloud.unpin") {
                        vec![owner.to_owned()]
                    } else if let Some(album) = p.get("albumId").and_then(Value::as_u64) {
                        if s.catalog.album(AlbumId(album)).is_none_or(|a| a.folder) {
                            return Err("choose a regular or smart album".into());
                        }
                        vec![format!("album:{album}")]
                    } else {
                        let ids = s.targets(p);
                        if ids.is_empty() || ids.len() > 1000 {
                            return Err("select between 1 and 1000 cloud photos".into());
                        }
                        for id in &ids {
                            if s.catalog.photo(*id).is_none_or(|p| p.deleted || !matches!(p.source, Source::Managed { .. })) {
                                return Err("select live managed cloud photos".into());
                            }
                        }
                        ids.iter().map(|id| format!("photo:{}", id.0)).collect()
                    };
                    for owner in owners {
                        if id == "cloud.pin" {
                            d.owners.insert(owner, OfflinePin { quality, blobs: Vec::new() });
                        } else {
                            d.owners.remove(&owner);
                        }
                    }
                }
                _ => {}
            }
            d.urgent |= id == "cloud.unpin"
                || id == "cloud.clearCache"
                || id == "cloud.cache"
                    && (p.get("budgetMb").is_some()
                        || p.get("keepAllOriginals") == Some(&json!(false))
                        || p.get("keepAllPreviews") == Some(&json!(false)));
            let bytes = super::encode_intent(&d.owners)?;
            if self.files.get(super::OWNERS_FILE).as_deref() != Some(bytes.as_slice()) {
                self.files.write(super::OWNERS_FILE, &bytes);
            }
            d.epoch = d.epoch.saturating_add(1);
            d.error = None;
            Ok(json!({"background":true,"savePending":true,"offlineReady":false}))
        })();
        if result.is_err() {
            let mut d = self.state.borrow_mut();
            (d.owners, d.budget, d.budget_change, d.clear, d.urgent) = before;
        }
        Some(result)
    }
    pub fn retry(&self) {
        let mut d = self.state.borrow_mut();
        if !d.available {
            d.loaded = false;
        }
        if !d.available {
            d.loaded = false;
        }
        d.epoch = d.epoch.saturating_add(1);
        d.error = None;
    }
    pub fn tick(&self, s: &mut Session, memory: &ManagedBytes, files: &Files, flushing: bool, ctx: &egui::Context) {
        let Some(backend) = self.backend.clone() else { return };
        let mut d = self.state.borrow_mut();
        if let Some(work) = d.work.take() {
            let result = match work {
                Work::Load(ref ready) | Work::Save(ref ready) => ready.borrow_mut().take().map(|result| result.map(Some)),
                Work::Bytes(ref ready) => ready.borrow_mut().take().map(|result| result.map(|_| None)),
            };
            let Some(result) = result else {
                d.work = Some(work);
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            };
            match result {
                Err(error) => {
                    if storage::deferred(&error) {
                        d.attempted = None;
                        d.job = None;
                        ctx.request_repaint_after(std::time::Duration::from_millis(50));
                        return;
                    }
                    d.error = Some(error);
                    if matches!(work, Work::Load(_)) {
                        d.loaded = true;
                        d.available = false;
                    }
                    d.job = None;
                    return;
                }
                Ok(policy) => match work {
                    Work::Load(_) => {
                        if let Some(policy) = policy {
                            d.budget = policy.budget;
                            d.owners = match self.files.get(super::OWNERS_FILE) {
                                Some(bytes) => match super::decode_intent(&bytes) {
                                    Ok(owners) => owners,
                                    Err(error) => {
                                        d.loaded = true;
                                        d.available = false;
                                        d.error = Some(error);
                                        return;
                                    }
                                },
                                None => policy
                                    .libraries
                                    .get(&self.dir)
                                    .into_iter()
                                    .flat_map(|owners| owners.iter())
                                    .map(|(owner, pin)| (owner.clone(), OfflinePin { quality: pin.quality, blobs: Vec::new() }))
                                    .collect(),
                            };
                        }
                        d.loaded = true;
                        d.available = true;
                    }
                    Work::Save(_) => {
                        if let Some(job) = &mut d.job {
                            job.saved = true;
                            let (epoch, clear, budget) = (job.key.1, job.clear, job.budget);
                            if epoch == d.epoch {
                                if clear {
                                    d.clear = false;
                                }
                                if budget.is_some() {
                                    d.budget_change = None;
                                }
                            }
                        }
                    }
                    Work::Bytes(_) => {
                        if let Some(job) = &mut d.job {
                            job.pos = job.pos.saturating_add(1);
                            job.lease = None;
                            job.guard.progress(job.pos as u64, job.blobs.len() as u64);
                        }
                    }
                },
            }
        }
        if !d.loaded {
            if files.protection().pending || flushing {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            let ready = reply();
            let out = ready.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                *out.borrow_mut() = Some(storage::load(&backend).await);
                ctx.request_repaint();
            });
            d.work = Some(Work::Load(ready));
            return;
        }
        if !d.available {
            return;
        }
        if d.job.as_ref().is_some_and(|job| job.key.1 != d.epoch) {
            d.job = None;
            d.attempted = None;
        }
        if let Some(job) = &mut d.job {
            if job.guard.is_cancelled() {
                d.error = Some("offline download cancelled; saved intent retained; reconnect to retry".into());
                d.job = None;
                return;
            }
            let Some(blob) = job.blobs.get(job.pos).cloned() else {
                let completed = job.owners.clone();
                d.completed = completed;
                d.job = None;
                return;
            };
            if job.lease.is_none() {
                match memory.reserve(vec![blob.clone()]) {
                    Ok(lease) => job.lease = Some(lease),
                    Err(error) => {
                        d.error = Some(error);
                        d.job = None;
                        return;
                    }
                }
            }
            let ready = match job.lease.as_ref().map(ByteLease::ready) {
                Some(Ok(ready)) => ready,
                Some(Err(error)) => {
                    d.error = Some(error);
                    d.job = None;
                    return;
                }
                None => false,
            };
            if !ready {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            let bytes = match memory.peek(&blob) {
                Ok(bytes) => bytes,
                Err(error) => {
                    d.error = Some(error);
                    d.job = None;
                    return;
                }
            };
            let ready = reply();
            let out = ready.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                *out.borrow_mut() = Some(storage::ensure(&backend, &blob, &bytes).await);
                ctx.request_repaint();
            });
            d.work = Some(Work::Bytes(ready));
            return;
        }
        let key = (s.catalog.revision, d.epoch);
        if d.attempted == Some(key) && (storage::certified() || d.error.is_some() || !d.errors.is_empty()) {
            return;
        }
        if files.protection().pending || flushing {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        d.attempted = Some(key);
        let dependencies = lightcraft_engine::assets::offline::resolve(&s.catalog, &d.owners);
        d.errors = dependencies.errors;
        if !d.errors.is_empty() {
            return;
        }
        let revision = files.protection().revision;
        let owners: BTreeMap<_, _> = dependencies
            .pins
            .into_iter()
            .map(|(owner, pin)| (owner, Owner { quality: pin.quality, blobs: pin.blobs.into_iter().map(|id| id.0).collect() }))
            .collect();
        if owners == d.completed && d.budget_change.is_none() && !d.clear && !d.confirm && !d.urgent && storage::certified() {
            return;
        }
        let guard = s.activity.start("download", "Save Browser Offline Copies", Cancel::Yes);
        let budget = d.budget_change;
        let clear = d.clear;
        d.urgent = false;
        d.confirm = false;
        let ready = reply();
        let out = ready.clone();
        let dir = self.dir.clone();
        let saved = owners.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            *out.borrow_mut() = Some(storage::update_owners(&backend, &dir, saved, budget, clear, revision).await);
            ctx.request_repaint();
        });
        d.job = Some(Job { key, owners, blobs: dependencies.blobs.into_values().collect(), pos: 0, lease: None, guard, saved: false, clear, budget });
        d.work = Some(Work::Save(ready));
    }
}
