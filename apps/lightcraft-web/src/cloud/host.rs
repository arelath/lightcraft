//! Browser owner-thread scheduler. Network futures never borrow the app; catalog writes and
//! remote acknowledgement proofs must finish in persistent storage before pending IDs drop.
use super::{
    Connection, Provider,
    durability::{ACK_FILE, AckReceipt},
    memory::{ByteLease, ManagedBytes},
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
fn timestamp_ms() -> Option<u64> {
    let now = js_sys::Date::now();
    if now.is_finite() && (0. ..=8_640_000_000_000_000.).contains(&now) { Some(now as u64) } else { None }
}
enum Stage {
    SaveThenSync { checkpoint: bool, audit: bool },
    Network(Rc<RefCell<Option<Result<Outcome, String>>>>),
    Ack { receipt: AckReceipt, result: Value },
    Received { result: Value },
    Acquire { command: String, params: Value, sources: Vec<(PhotoId, Source)>, lease: ByteLease },
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
    preparing_migration: bool,
    providers: BTreeMap<String, Provider>,
    jobs: Vec<Job>,
    reports: Vec<Value>,
    sync_running: bool,
    paused: bool,
    next_poll: f64,
    last_audit: f64,
    health: lightcraft_engine::cloud_status::PollHealth,
    media_health: lightcraft_engine::cloud_status::ResourceHealth,
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
        let result = self.claim_owner_inner(session);
        session.cloud_writer_error = result.as_ref().err().cloned();
        result
    }
    fn claim_owner_inner(&self, session: &mut Session) -> Result<(), String> {
        let Some(mut state) = session.catalog.sync_state().cloned() else { return Ok(()) };
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
            "cloud.prepareMigration" => Some(self.prepare_migration(s)),
            "cloud.syncStatus" => Some(self.status(s)),
            "cloud.overview" => Some(self.overview(s)),
            "cloud.pause" => Some({
                let result = lightcraft_engine::cloud_status::pause(s, p).map_err(|e| e.to_string());
                for job in &self.data.borrow().jobs {
                    if matches!(job.stage, Stage::SaveThenSync { .. } | Stage::Network(_))
                        && let Some(cancel) = job.guard.cancel_flag()
                    {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                result.and_then(|_| self.overview(s))
            }),
            "cloud.resume" => Some({
                if self.data.borrow().sync_running {
                    return Some(Err("wait for current cloud tasks to stop before resuming".into()));
                }
                lightcraft_engine::cloud_status::resume(s, p).map_err(|e| e.to_string()).and_then(|_| {
                    let mut data = self.data.borrow_mut();
                    data.paused = false;
                    data.preparing_migration = false;
                    data.next_poll = 0.;
                    drop(data);
                    self.overview(s)
                })
            }),
            "cloud.reconnect" => Some({
                self.memory.retry();
                let mut data = self.data.borrow_mut();
                data.paused = data.preparing_migration;
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
        data.paused = data.preparing_migration;
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
            let library = p.get("library").and_then(Value::as_str).map(str::to_owned).map_or_else(random_identity, Ok)?;
            if !protocol.is_head() && self.data.borrow().providers.get(connection).is_none_or(|p| p.connection.bucket_root.is_none()) {
                return Err("append mode requires bucketRoot".into());
            }
            let owner = self.owner.clone().ok_or("persistent browser installation identity unavailable")?;
            let enroll = if p.get("library").and_then(Value::as_str).is_some() {
                lightcraft_engine::cloud_sync::join_mode
            } else {
                lightcraft_engine::cloud_sync::enable_mode
            };
            let op = enroll(&s.catalog, library, random_identity()?, connection.into(), protocol, owner).map_err(|e| e.to_string())?;
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
        status["paused"] = json!(s.cloud_paused || data.preparing_migration);
        status["preparingMigration"] = json!(data.preparing_migration);
        status["migrating"] = json!(data.jobs.iter().any(|job| job.migrating));
        status["reports"] = json!(data.reports);
        status["health"] = data.health.view(
            s.catalog.sync_state().is_some(),
            data.sync_running,
            s.cloud_paused,
            data.preparing_migration || data.jobs.iter().any(|job| job.migrating),
            data.paused,
        );
        status["health"]["secondsSinceLastRemoteScan"] = json!(data.health.age_seconds(timestamp_ms()));
        status["cache"] = self.memory.status();
        status["savePending"] = json!(self.files.is_dirty());
        status["saveError"] = json!(self.files.error());
        Ok(status)
    }
    fn overview(&self, s: &mut Session) -> Result<Value, String> {
        let mut out = lightcraft_engine::cloud_status::overview(s, &Value::Null).map_err(|e| e.to_string())?;
        let data = self.data.borrow();
        out["paused"] = json!(s.cloud_paused || data.preparing_migration);
        out["preparingMigration"] = json!(data.preparing_migration);
        out["migrating"] = json!(data.jobs.iter().any(|j| j.migrating));
        out["canResume"] = json!(!data.sync_running);
        out["health"]["sync"] = data.health.view(
            s.catalog.sync_state().is_some(),
            data.sync_running,
            s.cloud_paused,
            data.preparing_migration || data.jobs.iter().any(|job| job.migrating),
            data.paused,
        );
        out["health"]["sync"]["secondsSinceLastRemoteScan"] = json!(data.health.age_seconds(timestamp_ms()));
        out["health"]["media"] = data.media_health.view();
        if let Some(message) = out["health"]["sync"]["lastError"]["message"].as_str() {
            out["error"] = json!(message);
        } else if let Some(failures) = out["health"]["media"]["failures"].as_object()
            && let Some(message) = failures.values().find_map(|failure| failure["message"].as_str())
        {
            out["error"] = json!(message);
        }
        Ok(out)
    }
    fn start(&self, s: &mut Session, p: &Value) -> Result<Value, String> {
        self.claim_owner(s)?;
        let migrating = p.get("migrate").map(|v| v.as_bool().ok_or("migrate must be true or false")).transpose()?.unwrap_or(false);
        if s.cloud_paused && !migrating {
            return Err("cloud syncing is paused; use cloud.resume first".into());
        }
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
        let guard = s.activity.start("cloudSync", if migrating { "Migrate cloud library" } else { "Synchronize cloud library" }, Cancel::Yes);
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
        data.preparing_migration = false;
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
    fn prepare_migration(&self, s: &Session) -> Result<Value, String> {
        if s.catalog.sync_state().is_none_or(|state| !state.protocol.is_head()) {
            return Err("migration requires an enabled conditional-head library".into());
        }
        let mut data = self.data.borrow_mut();
        if data.jobs.iter().any(|job| job.migrating) {
            return Err("migration is already running".into());
        }
        data.preparing_migration = true;
        data.paused = true;
        for job in &data.jobs {
            if matches!(job.stage, Stage::SaveThenSync { .. } | Stage::Network(_))
                && let Some(cancel) = job.guard.cancel_flag()
            {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok(json!({"paused":true,"running":data.sync_running}))
    }
    fn acquire(&self, s: &mut Session, command: &str, p: &Value) -> Result<Value, String> {
        self.backend.as_ref().ok_or("persistent browser storage is unavailable")?;
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
            let blob = if allow_preview && self.memory.peek(&asset.original).is_err() {
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
        let lease = self.memory.reserve(blobs)?;
        let guard = s.activity.start("download", "Acquire cloud originals", Cancel::Yes);
        let task = guard.id();
        let mut params = p.clone();
        if let Some(object) = params.as_object_mut() {
            object.insert("ids".into(), json!(ids));
        }
        self.data.borrow_mut().jobs.push(Job {
            migrating: false,
            guard,
            owner: s.library_token(),
            stage: Stage::Acquire { command: command.into(), params, sources, lease },
        });
        Ok(json!({"task":task,"background":true}))
    }
    pub fn tick(&self, app: &mut LightcraftApp, flushing: bool, ctx: &egui::Context) {
        let jobs = std::mem::take(&mut self.data.borrow_mut().jobs);
        let mut remaining = Vec::new();
        for mut job in jobs {
            let current_owner = !self.frozen.get() && Arc::ptr_eq(&job.owner, &app.session.library_token());
            let result = if !current_owner {
                Some(Err("library changed; cloud result ignored and pending work retained".into()))
            } else {
                self.advance(&mut job, app, flushing)
            };
            match result {
                None => remaining.push(job),
                Some(result) => {
                    let sync = matches!(&job.stage, Stage::SaveThenSync { .. } | Stage::Network(_) | Stage::Ack { .. } | Stage::Received { .. });
                    let mut data = self.data.borrow_mut();
                    if sync {
                        data.sync_running = false;
                        if current_owner {
                            data.paused = result.is_err() || data.preparing_migration;
                            match &result {
                                Ok(value) if value["refreshRequired"] != true => {
                                    data.health.success(timestamp_ms(), Some(job.guard.id()));
                                }
                                Err(message) if !(app.session.cloud_paused && job.guard.is_cancelled()) => {
                                    data.health.fail(message, Some(job.guard.id()))
                                }
                                _ => {}
                            }
                        }
                        // Leave a retry/explicit-sync window even when a remote scan takes
                        // longer than the polling interval.
                        data.next_poll = js_sys::Date::now() + 5000.;
                    }
                    if current_owner
                        && !job.guard.is_cancelled()
                        && let Stage::Acquire { sources, .. } = &job.stage
                    {
                        for (photo, _) in sources {
                            let resource = format!("download:{}", photo.0);
                            match &result {
                                Ok(_) => data.media_health.success(&resource),
                                Err(message) => data.media_health.fail(resource, message, Some(job.guard.id())),
                            }
                        }
                    }
                    let report = match result {
                        Ok(value) => json!({"task":job.guard.id(),"ok":true,"result":value}),
                        Err(error) => {
                            crate::safety::notice(error.clone());
                            json!({"task":job.guard.id(),"ok":false,"error":error,"paused":app.session.cloud_paused && job.guard.is_cancelled()})
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
            !app.session.cloud_paused
                && !data.sync_running
                && !data.paused
                && js_sys::Date::now() >= data.next_poll
                && app.session.catalog.sync_state().is_some_and(|s| data.providers.contains_key(&s.connection))
        };
        if poll && !self.frozen.get() {
            if let Err(error) = self.start(&mut app.session, &json!({"wait":false,"audit":false})) {
                let mut data = self.data.borrow_mut();
                data.paused = true;
                data.health.fail(&error, None);
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
                    job.guard.handle().detail(if migrating {
                        "Verifying committed history and publishing the migration seed"
                    } else {
                        "Reconciling and publishing saved library changes"
                    });
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
                    if !job.migrating && lightcraft_engine::cloud_sync::unchanged_outcome(&app.session.catalog, &outcome) {
                        let mut value = outcome.report();
                        value["unchanged"] = json!(true);
                        value["protocol"] = json!(app.session.catalog.sync_state().map(|s| s.protocol));
                        value["pending"] = json!(0);
                        return Ok(Some(value));
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
                    let op = lightcraft_engine::cloud_sync::completed_outcome(&next, &outcome, &BTreeSet::new(), 0).map_err(|e| e.to_string())?;
                    let op = if let Some(binding) = binding { Op::Batch { ops: vec![binding, op] } } else { op };
                    app.session.apply_received(op).map_err(|e| e.to_string())?;
                    let received = app.session.catalog.sync_state().ok_or("synchronization identity unavailable")?;
                    let receipt = if outcome.append_progress().is_some() {
                        let progress = received.append.as_ref().ok_or("append publication progress unavailable")?;
                        Some(AckReceipt::new_append(received, outcome.acknowledged(), progress)?)
                    } else if outcome.head_position().is_some() {
                        let position = lightcraft_engine::cloud_outcome::HeadPosition::new(
                            received.cursor.clone().ok_or("received head cursor unavailable")?,
                            received.generation,
                        )?;
                        Some(AckReceipt::new(received, outcome.acknowledged(), position.cursor().to_owned(), position.generation())?)
                    } else {
                        None
                    };
                    app.session.persist().map_err(|e| e.to_string())?;
                    let mut value = outcome.report();
                    value["unchanged"] = json!(false);
                    value["protocol"] = json!(app.session.catalog.sync_state().map(|s| s.protocol));
                    if let Some(receipt) = receipt {
                        self.files.write(ACK_FILE, &serde_json::to_vec(&receipt).map_err(|e| e.to_string())?);
                        job.guard.handle().detail("Saving received changes and remote acknowledgement in browser storage");
                        job.stage = Stage::Ack { receipt, result: value };
                    } else {
                        job.guard.handle().detail("Saving received empty-head state; pending work retained");
                        job.stage = Stage::Received { result: value };
                    }
                    Ok(None)
                }
                Stage::Received { result } => {
                    if let Some(error) = self.files.error() {
                        return Err(format!("browser received-state save failed: {error}; pending work retained"));
                    }
                    if self.files.is_dirty() || flushing {
                        return Ok(None);
                    }
                    result["pending"] = json!(app.session.catalog.sync_state().map_or(0, |state| state.pending.len()));
                    Ok(Some(result.clone()))
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
                Stage::Acquire { command, params, sources, lease } => {
                    if job.guard.is_cancelled() {
                        return Err("cloud acquisition cancelled".into());
                    }
                    if sources.iter().any(|(id, source)| app.session.catalog.photo(*id).is_none_or(|p| &p.source != source)) {
                        return Err("selected source changed; export was not started".into());
                    }
                    if !lease.ready()? {
                        return Ok(None);
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
        for download in self.memory.take_wanted() {
            let blob = download.blob;
            let provider = self.data.borrow().providers.get(&blob.remote.connection).cloned();
            let backend = backend.clone();
            let memory = self.memory.clone();
            let repaint = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let outcome = async {
                    let bytes = match provider {
                        Some(provider) => provider.with_cancel(download.cancel.clone()).blob(&backend, &blob).await?,
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
