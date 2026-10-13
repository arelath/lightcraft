//! Browser owner-thread scheduler. Network futures never borrow the app; catalog writes and
//! remote acknowledgement proofs must finish in persistent storage before pending IDs drop.
use super::{
    Connection, Provider,
    durability::{ACK_FILE, AckReceipt},
    memory::ManagedBytes,
    protocol::{self, Outcome},
};
use crate::{backend::Backend, files::Files};
use lightcraft_engine::{
    Session,
    activity::{Cancel, TaskGuard},
    catalog::{Op, PhotoId, Source},
};
use lightcraft_ui_egui::{CloudAction, LightcraftApp};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};

pub const CONFIG_FILE: &str = "browser-cloud.json";
enum Stage {
    SaveThenSync { checkpoint: bool, audit: bool },
    Network(Rc<RefCell<Option<Result<Outcome, String>>>>),
    Ack { receipt: AckReceipt, result: Value },
    Acquire { command: String, params: Value, sources: Vec<(PhotoId, Source)>, ready: Rc<RefCell<Option<Result<(), String>>>> },
}
struct Job {
    migrating: bool,
    guard: TaskGuard,
    owner: Arc<()>,
    stage: Stage,
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Some(cancel) = self.guard.cancel_flag() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
#[derive(Default)]
struct Data {
    providers: BTreeMap<String, Provider>,
    jobs: Vec<Job>,
    reports: Vec<Value>,
    sync_running: bool,
    paused: bool,
    next_poll: f64,
    last_audit: f64,
}
#[derive(Clone)]
pub struct Host {
    data: Rc<RefCell<Data>>,
    pub memory: ManagedBytes,
    files: Files,
    backend: Option<Backend>,
    frozen: Rc<Cell<bool>>,
    owner: Option<String>,
}
fn parameter<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing {key}"))
}
pub(crate) fn random_identity() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    web_sys::window()
        .ok_or("browser window unavailable")?
        .crypto()
        .map_err(|_| "secure browser randomness unavailable")?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|_| "secure identity generation failed")?;
    Ok(lightcraft_engine::assets::sha256(&bytes).0)
}
impl Host {
    pub fn new(backend: Option<Backend>, files: Files, frozen: Rc<Cell<bool>>, owner: Option<String>) -> Self {
        let host = Self {
            data: Rc::new(RefCell::new(Data { next_poll: js_sys::Date::now() + 5000., ..Data::default() })),
            memory: ManagedBytes::default(),
            files,
            backend,
            frozen,
            owner,
        };
        if let Some(bytes) = host.files.get(CONFIG_FILE) {
            match serde_json::from_slice::<Vec<Connection>>(&bytes) {
                Ok(configs) if configs.len() <= 64 => {
                    for config in configs {
                        match Provider::new(config, None) {
                            Ok(provider) => {
                                host.data.borrow_mut().providers.insert(provider.connection.id.clone(), provider);
                            }
                            Err(error) => crate::safety::notice(error),
                        }
                    }
                }
                _ => crate::safety::notice("Browser cloud configuration is invalid; saved data retained."),
            }
        }
        host
    }
    pub fn install(&self, session: &mut Session, ctx: &egui::Context) {
        self.memory.install(session);
        let repaint = ctx.clone();
        self.memory.set_waker(Arc::new(move || repaint.request_repaint()));
        if let Some(bytes) = self.files.get(ACK_FILE) {
            let result = (|| -> Result<(), String> {
                let receipt: AckReceipt =
                    serde_json::from_slice(&bytes).map_err(|_| "invalid saved browser acknowledgement; pending work retained")?;
                if let Some(mut state) = session.catalog.sync_state().cloned()
                    && receipt.finish(&mut state)?
                {
                    session.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).map_err(|e| e.to_string())?;
                    session.persist().map_err(|e| e.to_string())?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                crate::safety::notice(error);
            }
        }
        if let Err(error) = self.claim_owner(session) {
            self.data.borrow_mut().paused = true;
            crate::safety::notice(error);
        }
    }
    fn claim_owner(&self, session: &mut Session) -> Result<(), String> {
        let Some(mut state) = session.catalog.sync_state().filter(|s| !s.protocol.is_head()).cloned() else { return Ok(()) };
        if state
            .claim_owner(self.owner.clone().ok_or("persistent browser installation identity unavailable")?, random_identity()?)
            .map_err(|e| e.to_string())?
        {
            session.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).map_err(|e| e.to_string())?;
            session.persist().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn action(&self) -> CloudAction {
        let host = self.clone();
        Box::new(move |session, id, params| host.handle(session, id, params))
    }
    fn handle(&self, s: &mut Session, id: &str, p: &Value) -> Option<Result<Value, String>> {
        match id {
            "cloud.configureBrowser" => Some(self.configure(s, p)),
            "cloud.connections" => Some(Ok(json!({"connections":self.data.borrow().providers.values().map(|v|json!({
                "id":v.connection.id,"bucket":v.connection.storage_origin,"region":v.connection.authorize_endpoint,"prefix":v.connection.prefix,
                "endpoint":v.connection.bucket_root.as_deref().unwrap_or(""),"profile":"","storageOrigin":v.connection.storage_origin,"authorizeEndpoint":v.connection.authorize_endpoint,"bucketRoot":v.connection.bucket_root})).collect::<Vec<_>>()}))),
            "cloud.syncEnable" => Some(self.enable(s, p)),
            "cloud.sync" => Some(self.start(s, p)),
            "cloud.migrateAppend" => Some(self.migrate(s, p)),
            "cloud.syncStatus" => Some(self.status(s)),
            "cloud.reconnect" => Some({
                self.memory.retry();
                self.data.borrow_mut().paused = false;
                Ok(json!({"ready":true}))
            }),
            "cloud.fetch" => Some(self.acquire(s, "cloud.fetch", p)),
            "app.export" if s.targets(p).iter().any(|id| s.catalog.photo(*id).is_some_and(|p| matches!(p.source, Source::Managed { .. }))) => {
                Some(self.acquire(s, id, p))
            }
            _ => None,
        }
    }
    fn configure(&self, s: &mut Session, p: &Value) -> Result<Value, String> {
        if self.backend.is_none() {
            return Err("persistent browser storage is required for cloud support".into());
        }
        let config = Connection {
            id: parameter(p, "id")?.into(),
            prefix: parameter(p, "prefix")?.into(),
            authorize_endpoint: parameter(p, "authorizeEndpoint")?.into(),
            storage_origin: parameter(p, "storageOrigin")?.into(),
            bucket_root: p.get("bucketRoot").and_then(Value::as_str).map(str::to_owned),
        };
        let token = p.get("token").and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
        let mut data = self.data.borrow_mut();
        if data.providers.len() >= 64 && !data.providers.contains_key(&config.id) {
            return Err("browser connection limit reached".into());
        }
        if data.providers.get(&config.id).is_some_and(|old| {
            old.connection.prefix != config.prefix
                || old.connection.storage_origin != config.storage_origin
                || (old.connection.bucket_root.is_some() && old.connection.bucket_root != config.bucket_root)
        }) && s.catalog.sync_state().is_some_and(|state| state.connection == config.id)
        {
            return Err("an enabled library cannot be retargeted; use a separate local library to change its bucket/prefix".into());
        }
        let provider = match data.providers.get(&config.id).filter(|old| old.connection == config && token.is_none()) {
            Some(old) => old.clone(),
            None => Provider::new(config, token)?,
        };
        data.providers.insert(provider.connection.id.clone(), provider);
        let configs: Vec<_> = data.providers.values().map(|v| v.connection.clone()).collect();
        self.files.write(CONFIG_FILE, &serde_json::to_vec(&configs).map_err(|e| e.to_string())?);
        self.memory.retry();
        data.paused = false;
        Ok(json!({"configured":true,"savePending":true,"credentials":"ephemeral authorized URLs"}))
    }
    fn enable(&self, s: &mut Session, p: &Value) -> Result<Value, String> {
        if self.backend.is_none() {
            return Err("open persistent browser storage first".into());
        }
        let protocol: lightcraft_engine::catalog::replication::SyncProtocol =
            serde_json::from_value(p.get("protocol").cloned().unwrap_or(json!("conditional-head-v1")))
                .map_err(|_| "invalid synchronization protocol")?;
        let connection = parameter(p, "connection")?;
        if !self.data.borrow().providers.contains_key(connection) {
            return Err("configure browser cloud storage first".into());
        }
        if let Some(state) = s.catalog.sync_state() {
            if state.connection != connection
                || p.get("library").and_then(Value::as_str).is_some_and(|id| id != state.library)
                || p.get("protocol").is_some_and(|_| state.protocol != protocol)
            {
                return Err("this catalog already has a durable library identity".into());
            }
        } else {
            let library = p.get("library").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned).map_or_else(random_identity, Ok)?;
            if !protocol.is_head() && self.data.borrow().providers.get(connection).is_none_or(|p| p.connection.bucket_root.is_none()) {
                return Err("append mode requires bucketRoot".into());
            }
            let owner =
                if protocol.is_head() { String::new() } else { self.owner.clone().ok_or("persistent browser installation identity unavailable")? };
            let op = lightcraft_engine::cloud_sync::enable_mode(&s.catalog, library, random_identity()?, connection.into(), protocol, owner)
                .map_err(|e| e.to_string())?;
            s.apply_received(op).map_err(|e| e.to_string())?;
            s.persist().map_err(|e| e.to_string())?;
        }
        self.status(s)
    }
    fn status(&self, s: &mut Session) -> Result<Value, String> {
        let mut status = s.execute("cloud.syncStatus", &json!({})).map_err(|e| e.to_string())?;
        let data = self.data.borrow();
        status["transportAvailable"] = json!(self.backend.is_some() && !data.providers.is_empty());
        status["running"] = json!(data.sync_running);
        status["reports"] = json!(data.reports);
        status["cache"] = self.memory.status();
        status["savePending"] = json!(self.files.is_dirty());
        status["saveError"] = json!(self.files.error());
        Ok(status)
    }
    fn start(&self, s: &mut Session, p: &Value) -> Result<Value, String> {
        self.claim_owner(s)?;
        let migrating = p.get("migrate").map(|v| v.as_bool().ok_or("migrate must be true or false")).transpose()?.unwrap_or(false);
        if migrating && p.get("writersStopped").and_then(Value::as_bool) != Some(true) {
            return Err("migration requires writersStopped:true after stopping every old writer".into());
        }
        if self.backend.is_none() || self.frozen.get() {
            return Err("persistent browser storage is unavailable".into());
        }
        let state = s.catalog.sync_state().ok_or("enable synchronization first")?;
        if !self.data.borrow().providers.contains_key(&state.connection) {
            return Err("configure the saved browser connection and reauthorize".into());
        }
        let checkpoint = match p.get("checkpoint") {
            Some(value) => value.as_bool().ok_or("checkpoint must be true or false")?,
            None => false,
        };
        let mut data = self.data.borrow_mut();
        if data.sync_running {
            return Err("cloud synchronization is already running".into());
        }
        if data.jobs.len() >= 8 {
            return Err("browser cloud task queue is full".into());
        }
        s.persist().map_err(|e| e.to_string())?;
        let guard = s.activity.start("cloudSync", "Synchronize cloud library", Cancel::Yes);
        let task = guard.id();
        let audit = p.get("audit").map(|v| v.as_bool().ok_or("audit must be true or false")).transpose()?.unwrap_or(true)
            || data.last_audit == 0.
            || js_sys::Date::now() - data.last_audit >= 300_000.;
        if audit {
            data.last_audit = js_sys::Date::now();
        }
        data.jobs.push(Job { migrating, guard, owner: s.library_token(), stage: Stage::SaveThenSync { checkpoint, audit } });
        data.sync_running = true;
        data.paused = false;
        data.next_poll = js_sys::Date::now() + 5000.;
        self.memory.retry();
        Ok(json!({"task":task,"background":true}))
    }
    fn migrate(&self, s: &mut Session, p: &Value) -> Result<Value, String> {
        if s.catalog.sync_state().is_some_and(|s| !s.protocol.is_head()) {
            return self.status(s);
        }
        let mut params = p.as_object().cloned().ok_or("migration parameters must be an object")?;
        params.insert("migrate".into(), json!(true));
        self.start(s, &Value::Object(params))
    }
    fn acquire(&self, s: &mut Session, command: &str, p: &Value) -> Result<Value, String> {
        let backend = self.backend.clone().ok_or("persistent browser storage is unavailable")?;
        if self.data.borrow().jobs.len() >= 8 {
            return Err("browser cloud task queue is full".into());
        }
        let allow_preview = command == "app.export"
            && lightcraft_engine::export::ExportOptions::from_params(&s.export_params(p).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
                .allow_smart_preview;
        let ids = s.targets(p);
        if ids.is_empty() {
            return Err("select a managed photo first".into());
        }
        let mut sources = Vec::new();
        let mut blobs = Vec::new();
        let mut seen = BTreeSet::new();
        let mut bytes = 0u64;
        for id in &ids {
            let photo = s.catalog.photo(*id).ok_or("selected photo is no longer available")?;
            sources.push((*id, photo.source.clone()));
            let Source::Managed { asset_id } = &photo.source else { continue };
            let asset = s.catalog.asset(asset_id).ok_or("managed asset record is missing")?;
            let blob = if allow_preview && self.memory.get(&asset.original).is_err() {
                asset.smart_preview.as_ref().unwrap_or(&asset.original)
            } else {
                &asset.original
            };
            if seen.insert(blob.id.clone()) {
                bytes = bytes.saturating_add(blob.size);
                blobs.push(blob.clone());
            }
        }
        if bytes > super::MAX_BROWSER_BLOB {
            return Err("this batch exceeds the browser transfer budget; export smaller groups".into());
        }
        let providers = self.data.borrow().providers.clone();
        let memory = self.memory.clone();
        let guard = s.activity.start("download", "Acquire cloud originals", Cancel::Yes);
        let task = guard.id();
        let cancel = guard.cancel_flag().ok_or("cancellation is unavailable")?;
        let ready = Rc::new(RefCell::new(None));
        let result = ready.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = async {
                for blob in blobs {
                    let provider =
                        providers.get(&blob.remote.connection).ok_or("configure the browser connection first")?.with_cancel(cancel.clone());
                    let bytes = provider.blob(&backend, &blob).await?;
                    memory.insert(&blob, Arc::from(bytes))?;
                }
                Ok(())
            }
            .await;
            *result.borrow_mut() = Some(outcome);
            crate::web::request_repaint();
        });
        let mut params = p.clone();
        if let Some(object) = params.as_object_mut() {
            object.insert("ids".into(), json!(ids));
        }
        self.data.borrow_mut().jobs.push(Job {
            migrating: false,
            guard,
            owner: s.library_token(),
            stage: Stage::Acquire { command: command.into(), params, sources, ready },
        });
        Ok(json!({"task":task,"background":true}))
    }
    pub fn tick(&self, app: &mut LightcraftApp, flushing: bool, ctx: &egui::Context) {
        let jobs = std::mem::take(&mut self.data.borrow_mut().jobs);
        let mut remaining = Vec::new();
        for mut job in jobs {
            let result = if self.frozen.get() || !Arc::ptr_eq(&job.owner, &app.session.library_token()) {
                Some(Err("library changed; cloud result ignored and pending work retained".into()))
            } else {
                self.advance(&mut job, app, flushing)
            };
            match result {
                None => remaining.push(job),
                Some(result) => {
                    let sync = matches!(&job.stage, Stage::SaveThenSync { .. } | Stage::Network(_) | Stage::Ack { .. });
                    let mut data = self.data.borrow_mut();
                    if sync {
                        data.sync_running = false;
                        data.paused = result.is_err();
                        // Leave a retry/explicit-sync window even when a remote scan takes
                        // longer than the polling interval.
                        data.next_poll = js_sys::Date::now() + 5000.;
                    }
                    let report = match result {
                        Ok(value) => json!({"task":job.guard.id(),"ok":true,"result":value}),
                        Err(error) => {
                            crate::safety::notice(error.clone());
                            json!({"task":job.guard.id(),"ok":false,"error":error})
                        }
                    };
                    data.reports.push(report);
                    if data.reports.len() > 64 {
                        data.reports.remove(0);
                    }
                }
            }
        }
        self.data.borrow_mut().jobs.extend(remaining);
        let poll = {
            let data = self.data.borrow();
            !data.sync_running
                && !data.paused
                && js_sys::Date::now() >= data.next_poll
                && app.session.catalog.sync_state().is_some_and(|s| data.providers.contains_key(&s.connection))
        };
        if poll && !self.frozen.get() {
            if let Err(error) = self.start(&mut app.session, &json!({"wait":false,"audit":false})) {
                self.data.borrow_mut().paused = true;
                crate::safety::notice(error);
            }
        }
        if self.data.borrow().sync_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }
    fn advance(&self, job: &mut Job, app: &mut LightcraftApp, flushing: bool) -> Option<Result<Value, String>> {
        let result = (|| -> Result<Option<Value>, String> {
            match &mut job.stage {
                Stage::SaveThenSync { checkpoint, audit } => {
                    if job.guard.is_cancelled() {
                        return Err("synchronization cancelled; pending work retained".into());
                    }
                    app.session.persist().map_err(|e| e.to_string())?;
                    if let Some(error) = self.files.error() {
                        return Err(format!("browser save failed: {error}; pending work retained"));
                    }
                    if self.files.is_dirty() || flushing {
                        return Ok(None);
                    }
                    let local = app.session.catalog.sync_state().cloned().ok_or("synchronization identity is unavailable")?;
                    let provider = self.data.borrow().providers.get(&local.connection).cloned().ok_or("browser connection is unavailable")?;
                    let provider = provider.with_cancel(job.guard.cancel_flag().ok_or("cancellation is unavailable")?);
                    let backend = self.backend.clone().ok_or("browser storage is unavailable")?;
                    let checkpoint = *checkpoint;
                    let audit = *audit;
                    let migrating = job.migrating;
                    let ready = Rc::new(RefCell::new(None));
                    let result = ready.clone();
                    job.guard.handle().detail("Reconciling and publishing saved library changes");
                    wasm_bindgen_futures::spawn_local(async move {
                        *result.borrow_mut() = Some(if migrating {
                            super::append::migrate(&provider, &local).await
                        } else {
                            protocol::synchronize(&provider, &backend, &local, checkpoint, audit).await
                        });
                        crate::web::request_repaint();
                    });
                    job.stage = Stage::Network(ready);
                    Ok(None)
                }
                Stage::Network(ready) => {
                    let Some(outcome) = ready.borrow_mut().take() else { return Ok(None) };
                    let outcome = outcome?;
                    if job.guard.is_cancelled() {
                        return Err("synchronization cancelled; pending work retained".into());
                    }
                    if app.session.interaction.is_some() {
                        *ready.borrow_mut() = Some(Ok(outcome));
                        return Ok(None);
                    }
                    let mut binding = None;
                    let mut next = app.session.catalog.clone();
                    if job.migrating {
                        let mut state = next.sync_state().cloned().ok_or("synchronization identity unavailable")?;
                        state.protocol = lightcraft_engine::catalog::replication::SyncProtocol::AppendLog;
                        state.migrated_from_head = true;
                        state.cursor = None;
                        state.generation = 0;
                        state
                            .claim_owner(self.owner.clone().ok_or("persistent installation identity unavailable")?, random_identity()?)
                            .map_err(|e| e.to_string())?;
                        let op = Op::SetSyncState { state: Some(Box::new(state)) };
                        next.apply(op.clone()).map_err(|e| e.to_string())?;
                        binding = Some(op);
                    }
                    let receipt = if let Some(progress) = &outcome.append {
                        AckReceipt::new_append(next.sync_state().ok_or("synchronization identity unavailable")?, &outcome.acknowledged, progress)?
                    } else {
                        AckReceipt::new(
                            app.session.catalog.sync_state().ok_or("synchronization identity is unavailable")?,
                            &outcome.acknowledged,
                            outcome.cursor.clone(),
                            outcome.generation,
                        )?
                    };
                    let op = if let Some(progress) = outcome.append.clone() {
                        lightcraft_engine::cloud_sync::incoming_append(&next, progress, &BTreeSet::new()).map_err(|e| e.to_string())?
                    } else {
                        lightcraft_engine::cloud_sync::incoming_checkpoint(
                            &app.session.catalog,
                            &outcome.state,
                            &BTreeSet::new(),
                            outcome.cursor.clone(),
                            outcome.generation,
                        )
                        .map_err(|e| e.to_string())?
                    };
                    let op = if let Some(binding) = binding { Op::Batch { ops: vec![binding, op] } } else { op };
                    app.session.apply_received(op).map_err(|e| e.to_string())?;
                    app.session.persist().map_err(|e| e.to_string())?;
                    self.files.write(ACK_FILE, &serde_json::to_vec(&receipt).map_err(|e| e.to_string())?);
                    let value = json!({"protocol":app.session.catalog.sync_state().map(|s|s.protocol),"frontiers":outcome.append.as_ref().map(|p|&p.frontiers),"gaps":outcome.append.as_ref().map(|p|&p.gaps),"generation":outcome.generation,"cursor":outcome.cursor,"acknowledged":outcome.acknowledged.len()});
                    job.guard.handle().detail("Saving received changes and remote acknowledgement in browser storage");
                    job.stage = Stage::Ack { receipt, result: value };
                    Ok(None)
                }
                Stage::Ack { receipt, result } => {
                    if let Some(error) = self.files.error() {
                        return Err(format!("browser acknowledgement save failed: {error}; pending work retained"));
                    }
                    if self.files.is_dirty() || flushing {
                        return Ok(None);
                    }
                    let mut state = app.session.catalog.sync_state().cloned().ok_or("synchronization identity is unavailable")?;
                    if !receipt.finish(&mut state)? {
                        return Err("browser acknowledgement is stale; pending work retained".into());
                    }
                    app.session.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).map_err(|e| e.to_string())?;
                    app.session.persist().map_err(|e| e.to_string())?;
                    result["pending"] = json!(app.session.catalog.sync_state().map_or(0, |s| s.pending.len()));
                    Ok(Some(result.clone()))
                }
                Stage::Acquire { command, params, sources, ready } => {
                    let Some(result) = ready.borrow_mut().take() else { return Ok(None) };
                    result?;
                    if job.guard.is_cancelled() {
                        return Err("cloud acquisition cancelled".into());
                    }
                    if sources.iter().any(|(id, source)| app.session.catalog.photo(*id).is_none_or(|p| &p.source != source)) {
                        return Err("selected source changed; export was not started".into());
                    }
                    app.renderer.forget_all();
                    app.session.media.clear_sources();
                    if command == "cloud.fetch" {
                        return Ok(Some(json!({"ready":true})));
                    }
                    let action = app.services.cloud_action.take();
                    let result = app.run(command, params.clone());
                    app.services.cloud_action = action;
                    result.map(Some)
                }
            }
        })();
        match result {
            Ok(value) => value.map(Ok),
            Err(error) => Some(Err(error)),
        }
    }
    pub fn load_wanted(&self, ctx: &egui::Context) {
        let Some(backend) = self.backend.clone() else { return };
        for blob in self.memory.take_wanted() {
            let provider = self.data.borrow().providers.get(&blob.remote.connection).cloned();
            let backend = backend.clone();
            let memory = self.memory.clone();
            let repaint = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let outcome = async {
                    let bytes = match provider {
                        Some(provider) => provider.blob(&backend, &blob).await?,
                        None => {
                            let bytes =
                                backend.read(&super::cache_key(&blob)).await?.ok_or("configure browser cloud storage to download this photo")?;
                            super::verify_blob(&blob, &bytes)?;
                            bytes
                        }
                    };
                    memory.insert(&blob, Arc::from(bytes))
                }
                .await;
                if let Err(error) = outcome {
                    memory.failed(&blob, error);
                }
                repaint.request_repaint();
            });
        }
    }
}
