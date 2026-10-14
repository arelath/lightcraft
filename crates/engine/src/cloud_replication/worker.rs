use super::*;
use crate::assets::AssetResolver;
use crate::{EngineError, Session, activity::TaskGuard};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError, mpsc};

/// One metadata publisher per library, held through durable owner acknowledgement.
/// Media transfer and receipt I/O use separate locks and continue independently.
#[derive(Default)]
pub(crate) struct Coordinator(Mutex<()>);
impl Coordinator {
    pub(crate) fn acquire(&self, cancel: &AtomicBool) -> crate::Result<MutexGuard<'_, ()>> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(error("metadata publication cancelled; pending work retained"));
            }
            match self.0.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(e)) => return Ok(e.into_inner()),
                Err(TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }
}

struct Pending {
    owner_done: Option<mpsc::SyncSender<()>>,
    started_generation: u64,
    migration_owner: Option<String>,
    receiver: mpsc::Receiver<crate::Result<Outcome>>,
    library: Arc<()>,
    resolver: Arc<AssetResolver>,
    connection_revision: u64,
    guard: TaskGuard,
    cancel: Arc<AtomicBool>,
    ready: Option<crate::Result<Outcome>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(done) = self.owner_done.take() {
            let _ = done.send(());
        }
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub struct Jobs {
    pub(crate) coordinator: Arc<Coordinator>,
    preparing_migration: bool,
    pending: Option<Pending>,
    pub reports: Vec<Value>,
    next_poll: Option<web_time::Instant>,
    paused: bool,
    last_audit: Option<web_time::Instant>,
}
impl Jobs {
    pub fn preparing_migration(&self) -> bool {
        self.preparing_migration
    }
    pub fn migrating(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| p.migration_owner.is_some())
    }
    pub fn running(&self) -> bool {
        self.pending.is_some()
    }
}
fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
pub fn pause_publication(s: &mut Session) {
    s.cloud_replication_jobs.paused = true;
    if let Some(job) = &s.cloud_replication_jobs.pending {
        job.cancel.store(true, Ordering::Relaxed);
    }
}
pub fn resume_publication(s: &mut Session) {
    s.cloud_replication_jobs.preparing_migration = false;
    s.cloud_replication_jobs.paused = false;
    s.cloud_replication_jobs.next_poll = Some(web_time::Instant::now());
}
pub(crate) fn request_poll(s: &mut Session) {
    s.cloud_replication_jobs.next_poll = Some(web_time::Instant::now());
}
pub fn start(s: &mut Session, p: &Value) -> crate::Result<Value> {
    start_impl(s, p, false)
}
pub fn prepare_migration(s: &mut Session, _: &Value) -> crate::Result<Value> {
    if s.cloud_replication_jobs.migrating() {
        return Err(error("migration is already running"));
    }
    if s.catalog.sync_state().is_none_or(|state| !state.protocol.is_head()) {
        return Err(error("migration requires an enabled conditional-head library"));
    }
    s.cloud_replication_jobs.preparing_migration = true;
    s.cloud_replication_jobs.paused = true;
    if let Some(pending) = &s.cloud_replication_jobs.pending {
        pending.cancel.store(true, Ordering::Relaxed);
    }
    Ok(json!({"paused":true,"running":s.cloud_replication_jobs.running()}))
}
pub fn start_migration(s: &mut Session, p: &Value) -> crate::Result<Value> {
    if p.get("writersStopped").and_then(Value::as_bool) != Some(true) {
        return Err(error("migration requires writersStopped:true after stopping and upgrading every other device"));
    }
    if s.catalog.sync_state().is_some_and(|state| !state.protocol.is_head()) {
        return s.execute("cloud.syncStatus", p);
    }
    if s.cloud_uploads.running() > 0 {
        return Err(error("finish or cancel uploads before migration"));
    }
    start_impl(s, p, true)
}
fn start_impl(s: &mut Session, p: &Value, migrating: bool) -> crate::Result<Value> {
    if s.cloud_paused && !migrating {
        return Err(error("cloud syncing is paused; use cloud.resume first"));
    }
    s.claim_cloud_owner()?;
    if s.cloud_replication_jobs.running() {
        return Err(error("cloud synchronization is already running"));
    }
    let wait = match p.get("wait") {
        Some(value) => value.as_bool().ok_or_else(|| error("wait must be true or false"))?,
        None => !crate::assets::is_cached_only(),
    };
    if wait && crate::assets::is_cached_only() {
        return Err(error("desktop synchronization runs in the background; inspect activity and cloud.syncStatus"));
    }
    let checkpoint = match p.get("checkpoint") {
        Some(value) => value.as_bool().ok_or_else(|| error("checkpoint must be true or false"))?,
        None => false,
    };
    let requested_audit = p.get("audit").map(|v| v.as_bool().ok_or_else(|| error("audit must be true or false"))).transpose()?.unwrap_or(true);
    let now = web_time::Instant::now();
    let audit = requested_audit || s.cloud_replication_jobs.last_audit.is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_secs(300));
    if audit {
        s.cloud_replication_jobs.last_audit = Some(now);
    }
    s.persist()?;
    let state = s.catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first using cloud.syncEnable"))?;
    let lib = s.library.as_mut().ok_or_else(|| error("open a persistent library first"))?;
    let connection = lib
        .cloud_connections()?
        .into_iter()
        .find(|connection| connection.id == state.connection)
        .ok_or_else(|| error("synchronization storage connection is not configured"))?;
    let migration_owner = if migrating { Some(crate::cloud_append::owner::identity(&lib.dir)?) } else { None };
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("native asset storage is unavailable"))?;
    let guard =
        s.activity.start("cloudSync", if migrating { "Migrate cloud library" } else { "Synchronize cloud library" }, crate::activity::Cancel::Yes);
    let id = guard.id();
    let handle = guard.handle();
    let cancel = guard.cancel_flag().ok_or_else(|| error("synchronization cancellation flag unavailable"))?;
    let worker_cancel = cancel.clone();
    let started_generation = state.generation;
    let (sender, receiver) = mpsc::sync_channel(1);
    let (owner_done, applied) = mpsc::sync_channel(1);
    let coordinator = s.cloud_replication_jobs.coordinator.clone();
    std::thread::Builder::new()
        .name("cloud-catalog-sync".into())
        .spawn(move || {
            let _permit = match coordinator.acquire(&worker_cancel) {
                Ok(permit) => permit,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            };
            handle.detail(if migrating {
                "Verifying committed history and publishing the migration seed"
            } else {
                "Reconciling and publishing saved library changes"
            });
            let result = crate::guard::catch("cloud synchronization worker", || {
                if migrating {
                    crate::cloud_append::migration::seed(&connection, &state, &worker_cancel).map_err(|e| AssetError::Invalid(e.to_string()))
                } else if !state.protocol.is_head() {
                    crate::cloud_append::native::synchronize_with_audit(&connection, &state, checkpoint, audit, &worker_cancel)
                } else {
                    synchronize(&connection, &state, checkpoint, &worker_cancel)
                }
            })
            .map_err(error)
            .and_then(|result| result.map_err(|error| EngineError::Other(error.to_string())));
            if sender.send(result).is_ok() {
                loop {
                    match applied.recv_timeout(std::time::Duration::from_millis(20)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(_) if worker_cancel.load(Ordering::Relaxed) => break,
                        Err(_) => {}
                    }
                }
            }
        })
        .map_err(|e| error(format!("could not start cloud synchronization: {e}")))?;
    s.cloud_replication_jobs.pending = Some(Pending {
        owner_done: Some(owner_done),
        started_generation,
        migration_owner,
        receiver,
        library: s.library_token(),
        connection_revision: resolver.connection_revision(),
        resolver,
        guard,
        cancel,
        ready: None,
    });
    s.cloud_replication_jobs.paused = false;
    s.cloud_replication_jobs.preparing_migration = false;
    s.cloud_replication_jobs.next_poll = Some(web_time::Instant::now() + std::time::Duration::from_secs(5));
    if !wait {
        return Ok(json!({"task":id,"background":true}));
    }
    loop {
        s.poll_cloud_uploads();
        s.poll_cloud_replication();
        if let Some(report) = s.cloud_replication_jobs.reports.iter().find(|report| report.get("task").and_then(Value::as_u64) == Some(id)) {
            if report.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(report.get("result").cloned().unwrap_or(Value::Null));
            }
            return Err(error(report.get("error").and_then(Value::as_str).unwrap_or("cloud synchronization failed")));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
impl Session {
    pub fn resume_cloud_replication(&mut self) {
        if self.cloud_paused
            || self.cloud_replication_jobs.preparing_migration()
            || self.catalog.sync_state().is_none()
            || self.cloud_replication_jobs.running()
            || self.cloud_replication_jobs.paused
        {
            return;
        }
        let now = web_time::Instant::now();
        let deadline = self.cloud_replication_jobs.next_poll.get_or_insert(now + std::time::Duration::from_secs(5));
        if now < *deadline {
            return;
        }
        *deadline = now + std::time::Duration::from_secs(5);
        if let Err(error) = start(self, &json!({"wait":false,"audit":false})) {
            self.cloud_replication_jobs.paused = true;
            self.cloud_replication_jobs.reports.push(json!({"ok":false,"error":error.to_string()}));
        }
    }
    pub fn poll_cloud_replication(&mut self) {
        let Some(mut pending) = self.cloud_replication_jobs.pending.take() else {
            return;
        };
        let result = if let Some(result) = pending.ready.take() {
            result
        } else {
            match pending.receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => {
                    self.cloud_replication_jobs.pending = Some(pending);
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => Err(error("cloud synchronization worker stopped; durable work retained")),
            }
        };
        if self.interaction.is_some() {
            pending.ready = Some(result);
            self.cloud_replication_jobs.pending = Some(pending);
            return;
        }
        let id = pending.guard.id();
        let result = result.and_then(|outcome| {
            if pending.guard.is_cancelled() { return Err(error("synchronization cancelled; pending work and prior protocol retained")); }
            if !Arc::ptr_eq(&pending.library, &self.library_token()) || pending.connection_revision != pending.resolver.connection_revision() {
                return Err(error("library or storage connection changed; remote result ignored and local pending work retained"));
            }
            if pending.migration_owner.is_none() && outcome.append.is_none()
                && crate::cloud_sync::unchanged_head(&self.catalog, &outcome.state, &outcome.acknowledged, &outcome.cursor, outcome.generation) {
                // Retry any existing unsaved local work, but create no new catalog
                // operation or journal entry for an unchanged synchronization poll.
                if !self.pending_log.is_empty() { self.persist()?; }
                return Ok(json!({"unchanged":true,"migrated":false,"legacyHistoryRetained":true,"protocol":self.catalog.sync_state().map(|s|s.protocol),
                    "generation":outcome.generation,"cursor":outcome.cursor,"acknowledged":0,"pending":0}));
            }
            let mut migrated = None;
            let binding = if let Some(owner) = &pending.migration_owner {
                let mut next = self.catalog.clone();
                let mut state = next.sync_state().cloned().ok_or_else(|| error("synchronization identity unavailable"))?;
                if !state.protocol.is_head() { return Err(error("protocol changed during migration; result retained remotely")); }
                state.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
                state.migrated_from_head = true;
                state.cursor = None;
                state.generation = 0;
                state.claim_owner(owner.clone(), crate::cloud_append::owner::random()?)?;
                let op = lightcraft_catalog::Op::SetSyncState { state: Some(Box::new(state)) };
                next.apply(op.clone())?;
                migrated = Some(next);
                Some(op)
            } else { None };
            let reconciled = if let Some(progress)=outcome.append.clone() {crate::cloud_sync::incoming_append(migrated.as_ref().unwrap_or(&self.catalog),progress,&outcome.acknowledged)}else{crate::cloud_sync::completed_checkpoint(
                &self.catalog,
                &outcome.state,
                &outcome.acknowledged,
                outcome.cursor.clone(),
                outcome.generation,
                pending.started_generation,
            )};
            let op = match reconciled {
                Err(error) if pending.migration_owner.is_none() && crate::cloud_sync::obsolete_completion_needs_proof(&self.catalog, &outcome, &error) => {
                    return Ok(json!({"superseded":true,"refreshRequired":true,"pending":self.catalog.sync_state().map_or(0,|state|state.pending.len())}));
                }
                result => result?,
            };
            let op = if let Some(binding) = binding { lightcraft_catalog::Op::Batch { ops: vec![binding, op] } } else { op };
            let inverse = self.apply_catalog_mode(op, false)?;
            if let Err(error) = self.persist() {
                self.apply_received(inverse)?;
                return Err(error);
            }
            Ok(json!({"unchanged":false,"migrated":pending.migration_owner.is_some(),"legacyHistoryRetained":true,"protocol":self.catalog.sync_state().map(|s|s.protocol),"frontiers":outcome.append.as_ref().map(|p|&p.frontiers),"gaps":outcome.append.as_ref().map(|p|&p.gaps),"generation":outcome.generation,"cursor":outcome.cursor,"acknowledged":outcome.acknowledged.len(),
                "pending":self.catalog.sync_state().map_or(0,|state|state.pending.len())}))
        });
        self.cloud_replication_jobs.paused = result.is_err() || self.cloud_replication_jobs.preparing_migration;
        self.cloud_replication_jobs.next_poll = Some(web_time::Instant::now() + std::time::Duration::from_secs(5));
        if result.as_ref().is_ok_and(|value| value.get("refreshRequired").and_then(Value::as_bool) == Some(true)) {
            request_poll(self);
        }
        let report = match result {
            Ok(result) => json!({"task":id,"ok":true,"result":result}),
            Err(error) => {
                json!({"task":id,"ok":false,"error":error.to_string(),"paused":self.cloud_paused && pending.cancel.load(Ordering::Relaxed)})
            }
        };
        self.cloud_replication_jobs.reports.push(report);
        if self.cloud_replication_jobs.reports.len() > 64 {
            self.cloud_replication_jobs.reports.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryStores;
    use lightcraft_catalog::{Journal, MemStore, Op, Photo, PhotoId, Source, Store};
    #[derive(Clone)]
    struct Failing {
        files: MemStore,
        fail: Arc<AtomicBool>,
    }
    impl Store for Failing {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.files.read(name)
        }
        fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.files.write_atomic(name, bytes)
        }
        fn append(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("simulated full disk"));
            }
            self.files.append(name, bytes)
        }
        fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
            self.files.truncate(name, len)
        }
        fn describe(&self) -> String {
            "ack-failure".into()
        }
    }
    fn session() -> (Session, Failing) {
        let files = Failing { files: MemStore::new(), fail: Arc::new(AtomicBool::new(false)) };
        let mut s = Session::new();
        s.open_library_in(
            LibraryStores { dir: "sync-owner".into(), catalog: Box::new(files.clone()), files: Box::new(MemStore::new()), on_disk: false },
            false,
        )
        .unwrap();
        s.apply_system(Op::AddPhoto {
            photo: Box::new(Photo::new(PhotoId(1), Source::File { path: "local.jpg".into() }, "local.jpg", "JPEG", 1, 1, "")),
        })
        .unwrap();
        s.apply_system(crate::cloud_sync::enable(&s.catalog, "library".into(), "device".into(), "personal".into()).unwrap()).unwrap();
        s.persist().unwrap();
        s.selection = crate::Selection::single(PhotoId(1));
        (s, files)
    }
    fn ready(s: &mut Session) -> Arc<AssetResolver> {
        let local = s.catalog.sync_state().unwrap();
        let mut remote = SyncState::new("library".into(), "other".into(), "personal".into()).unwrap();
        remote.merge(&local.portable()).unwrap();
        remote
            .enqueue(vec![lightcraft_catalog::replication::Change {
                entity: "library:library".into(),
                field: "label.Green".into(),
                base: None,
                value: json!("Remote green"),
            }])
            .unwrap();
        let outcome = Outcome {
            append: None,
            state: remote.portable(),
            acknowledged: local.pending.keys().cloned().collect(),
            cursor: "a".repeat(64),
            generation: 1,
        };
        let resolver = Arc::new(AssetResolver::new(std::env::temp_dir().join("lightcraft-sync-owner-unused")));
        let guard = s.activity.start("cloudSync", "test sync", crate::activity::Cancel::Yes);
        let cancel = guard.cancel_flag().unwrap();
        let (_, receiver) = mpsc::sync_channel(1);
        s.cloud_replication_jobs.pending = Some(Pending {
            owner_done: None,
            started_generation: local.generation,
            migration_owner: None,
            receiver,
            library: s.library_token(),
            resolver: resolver.clone(),
            connection_revision: resolver.connection_revision(),
            guard,
            cancel,
            ready: Some(Ok(outcome)),
        });
        resolver
    }
    #[test]
    fn failed_incoming_ack_save_retains_outbox_and_restart_replays_publication_safely() {
        let (mut s, files) = session();
        let before = s.catalog.sync_state().unwrap().clone();
        ready(&mut s);
        files.fail.store(true, Ordering::Relaxed);
        s.poll_cloud_replication();
        assert_eq!(s.catalog.sync_state(), Some(&before));
        assert_eq!(s.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Green), None);
        let (_, loaded, _) = Journal::open(Box::new(files.files.clone())).unwrap();
        assert_eq!(loaded.sync_state(), Some(&before));
        files.fail.store(false, Ordering::Relaxed);
        s.persist().unwrap();
        ready(&mut s);
        s.poll_cloud_replication();
        assert!(s.catalog.sync_state().unwrap().pending.is_empty());
        assert_eq!(s.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Green), Some("Remote green"));
        let (_, loaded, _) = Journal::open(Box::new(files.files)).unwrap();
        assert_eq!(loaded.sync_state(), s.catalog.sync_state());
    }
    #[test]
    fn incoming_result_waits_for_slider_group_and_rejects_stale_library_owner() {
        let (mut s, _) = session();
        ready(&mut s);
        s.begin_interaction("slider").unwrap();
        s.poll_cloud_replication();
        assert!(s.cloud_replication_jobs.running());
        assert_eq!(s.catalog.sync_state().unwrap().generation, 0);
        s.end_interaction().unwrap();
        s.poll_cloud_replication();
        assert_eq!(s.catalog.sync_state().unwrap().generation, 1);
        ready(&mut s);
        s.cloud_replication_jobs.pending.as_mut().unwrap().library = Arc::new(());
        let before = s.catalog.to_snapshot();
        s.poll_cloud_replication();
        assert_eq!(s.catalog.to_snapshot(), before);
        assert_eq!(s.cloud_replication_jobs.reports.last().unwrap()["ok"], false);
    }
    fn ready_migration(s: &mut Session) {
        ready(s);
        let pending = s.cloud_replication_jobs.pending.as_mut().unwrap();
        pending.migration_owner = Some("migration-owner".into());
        let outcome = pending.ready.as_mut().unwrap().as_mut().unwrap();
        outcome.acknowledged.clear();
        outcome.append = Some(lightcraft_catalog::replication::AppendProgress {
            version: 1,
            manifest: crate::assets::sha256(&crate::cloud_append::manifest("library").unwrap()).0,
            frontiers: lightcraft_catalog::replication::publication_prefixes(&outcome.state).unwrap(),
            remote: outcome.state.clone(),
            gaps: Default::default(),
        });
        outcome.cursor.clear();
        outcome.generation = 0;
    }
    fn ready_same_head(s: &mut Session, generation: u64) {
        ready(s);
        let local = s.catalog.sync_state().unwrap();
        let same =
            Outcome { append: None, state: local.portable(), acknowledged: BTreeSet::new(), cursor: local.cursor.clone().unwrap(), generation };
        s.cloud_replication_jobs.pending.as_mut().unwrap().ready = Some(Ok(same));
    }
    #[test]
    fn unchanged_head_poll_creates_no_operation_journal_write_or_cache_invalidation() {
        let (mut s, files) = session();
        ready(&mut s);
        s.poll_cloud_replication();
        s.execute("cloud.syncStatus", &json!({})).unwrap();
        s.cloud_conflicts_cache.as_mut().unwrap().2["reuseMarker"] = json!(true);
        let before = s.catalog.to_snapshot();
        let revision = s.catalog.revision;
        let journal = files.files.get(lightcraft_catalog::journal::LOG);
        files.fail.store(true, Ordering::Relaxed);
        for _ in 0..3 {
            ready_same_head(&mut s, 1);
            s.poll_cloud_replication();
            let report = s.cloud_replication_jobs.reports.last().unwrap();
            assert_eq!(report["ok"], true, "{report}");
            assert_eq!(report["result"]["unchanged"], true);
        }
        assert_eq!(s.catalog.to_snapshot(), before);
        assert_eq!(s.catalog.revision, revision);
        assert_eq!(files.files.get(lightcraft_catalog::journal::LOG), journal);
        assert!(s.pending_log.is_empty());
        s.execute("cloud.syncStatus", &json!({})).unwrap();
        assert_eq!(s.cloud_conflicts_cache.as_ref().unwrap().2["reuseMarker"], true);
    }
    #[test]
    fn head_ack_and_generation_progress_still_save_and_unsaved_local_changes_are_retried() {
        let (mut s, files) = session();
        let initial = files.files.get(lightcraft_catalog::journal::LOG);
        ready(&mut s);
        let local = s.catalog.sync_state().unwrap().portable();
        s.cloud_replication_jobs.pending.as_mut().unwrap().ready.as_mut().unwrap().as_mut().unwrap().state = local;
        s.poll_cloud_replication(); // Exact ACK, even though portable field values agree.
        assert!(s.catalog.sync_state().unwrap().pending.is_empty());
        assert_eq!(s.cloud_replication_jobs.reports.last().unwrap()["result"]["unchanged"], false);
        assert_ne!(files.files.get(lightcraft_catalog::journal::LOG), initial);
        let revision = s.catalog.revision;
        ready_same_head(&mut s, 2);
        s.poll_cloud_replication();
        assert_eq!(s.catalog.sync_state().unwrap().generation, 2);
        assert!(s.catalog.revision > revision);
        assert_eq!(s.cloud_replication_jobs.reports.last().unwrap()["result"]["unchanged"], false);
        s.apply_system(Op::SetRating { id: PhotoId(1), rating: 5 }).unwrap();
        assert!(!s.pending_log.is_empty());
        let revision = s.catalog.revision;
        ready_same_head(&mut s, 2);
        s.poll_cloud_replication();
        assert_eq!(s.catalog.revision, revision);
        assert!(s.pending_log.is_empty());
        assert_eq!(s.cloud_replication_jobs.reports.last().unwrap()["result"]["unchanged"], true);
        let (_, restored, _) = Journal::open(Box::new(files.files)).unwrap();
        assert_eq!(restored.photo(PhotoId(1)).unwrap().rating, 5);
    }
    #[test]
    fn publication_permit_is_held_until_owner_save_even_during_interaction() {
        let (mut s, _) = session();
        ready(&mut s);
        let coordinator = s.cloud_replication_jobs.coordinator.clone();
        let outcome = s.cloud_replication_jobs.pending.as_mut().unwrap().ready.take().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let (owner_done, applied) = mpsc::sync_channel(1);
        let (held, holding) = mpsc::sync_channel(1);
        let worker_gate = coordinator.clone();
        let worker = std::thread::spawn(move || {
            let _permit = worker_gate.acquire(&AtomicBool::new(false)).unwrap();
            sender.send(outcome).unwrap();
            held.send(()).unwrap();
            applied.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        });
        let pending = s.cloud_replication_jobs.pending.as_mut().unwrap();
        pending.receiver = receiver;
        pending.owner_done = Some(owner_done);
        holding.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        s.begin_interaction("slider").unwrap();
        s.poll_cloud_replication();
        assert!(coordinator.0.try_lock().is_err());
        s.end_interaction().unwrap();
        s.poll_cloud_replication();
        worker.join().unwrap();
        assert!(coordinator.0.try_lock().is_ok());
        assert_eq!(s.catalog.sync_state().unwrap().generation, 1);
        let _permit = coordinator.acquire(&AtomicBool::new(false)).unwrap();
        assert!(coordinator.acquire(&AtomicBool::new(true)).is_err());
    }
    #[test]
    fn older_completion_without_new_archive_witnesses_refreshes_without_pausing() {
        let (mut s, _) = session();
        ready(&mut s);
        let old = s.cloud_replication_jobs.pending.as_mut().unwrap().ready.take().unwrap().unwrap();
        let mut newer = SyncState::restore_durable(&old.state, "other".into(), "personal".into()).unwrap();
        newer
            .enqueue(vec![lightcraft_catalog::replication::Change {
                entity: "library:library".into(),
                field: "label.Green".into(),
                base: None,
                value: json!("Newer green"),
            }])
            .unwrap();
        newer.pending.clear();
        let plan = newer.archive_plan().unwrap();
        newer.compact_receipts(&plan).unwrap();
        let mut portable = newer.portable();
        portable.proofs = plan.objects;
        let op = crate::cloud_sync::incoming_checkpoint(&s.catalog, &portable, &old.acknowledged, "b".repeat(64), 11).unwrap();
        s.apply_received(op).unwrap();
        s.persist().unwrap();
        let before = s.catalog.to_snapshot();
        s.cloud_replication_jobs.pending.as_mut().unwrap().ready = Some(Ok(Outcome { generation: 10, ..old }));
        s.poll_cloud_replication();
        assert!(!s.cloud_replication_jobs.paused);
        assert_eq!(s.catalog.to_snapshot(), before);
        assert_eq!(s.cloud_replication_jobs.reports.last().unwrap()["result"]["refreshRequired"], true);
        assert!(s.cloud_replication_jobs.next_poll.unwrap() <= web_time::Instant::now());
    }
    #[test]
    fn older_completion_retains_newer_head_without_pausing_but_rollback_and_forks_fail() {
        let (mut s, _) = session();
        ready(&mut s);
        let old = s.cloud_replication_jobs.pending.as_mut().unwrap().ready.take().unwrap().unwrap();
        let old_state = old.state.clone();
        let old_ids = old.acknowledged.clone();
        s.cloud_replication_jobs.pending.as_mut().unwrap().ready = Some(Ok(Outcome { generation: 11, cursor: "b".repeat(64), ..old }));
        s.poll_cloud_replication();
        ready(&mut s);
        s.cloud_replication_jobs.pending.as_mut().unwrap().started_generation = 0;
        s.cloud_replication_jobs.pending.as_mut().unwrap().ready =
            Some(Ok(Outcome { append: None, state: old_state, acknowledged: old_ids, cursor: "a".repeat(64), generation: 10 }));
        s.poll_cloud_replication();
        assert!(!s.cloud_replication_jobs.paused);
        assert_eq!(s.cloud_replication_jobs.reports.last().unwrap()["ok"], true);
        assert_eq!(s.catalog.sync_state().unwrap().generation, 11);
        assert_eq!(s.catalog.sync_state().unwrap().cursor.as_deref(), Some("b".repeat(64).as_str()));
        assert_eq!(s.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Green), Some("Remote green"));
        let before = s.catalog.to_snapshot();
        ready_same_head(&mut s, 10); // Started after the owner acknowledged 11: actual rollback.
        s.poll_cloud_replication();
        assert!(s.cloud_replication_jobs.paused);
        assert_eq!(s.catalog.to_snapshot(), before);
        ready_same_head(&mut s, 11);
        s.cloud_replication_jobs.pending.as_mut().unwrap().ready.as_mut().unwrap().as_mut().unwrap().cursor = "c".repeat(64);
        s.poll_cloud_replication();
        assert!(s.cloud_replication_jobs.paused);
        assert_eq!(s.catalog.to_snapshot(), before);
    }
    #[test]
    fn migration_owner_retains_late_edits_and_rolls_back_mode_on_failed_save() {
        let (mut s, files) = session();
        ready_migration(&mut s);
        let mut late = s.catalog.sync_state().unwrap().clone();
        late.enqueue(vec![lightcraft_catalog::replication::Change {
            entity: "library:library".into(),
            field: "label.Blue".into(),
            base: None,
            value: json!("Late blue"),
        }])
        .unwrap();
        s.apply_system(Op::SetSyncState { state: Some(Box::new(late)) }).unwrap();
        s.persist().unwrap();
        let before = s.catalog.sync_state().unwrap().clone();
        assert!(crate::cloud_upload::start(&mut s, &json!({})).unwrap_err().to_string().contains("paused for migration"));
        files.fail.store(true, Ordering::Relaxed);
        s.poll_cloud_replication();
        assert_eq!(s.catalog.sync_state(), Some(&before));
        let (_, loaded, _) = Journal::open(Box::new(files.files.clone())).unwrap();
        assert_eq!(loaded.sync_state(), Some(&before));
        files.fail.store(false, Ordering::Relaxed);
        s.persist().unwrap();
        ready_migration(&mut s);
        // Simulate another edit after the worker captured its seed.
        let mut newest = s.catalog.sync_state().unwrap().clone();
        let m = newest
            .enqueue(vec![lightcraft_catalog::replication::Change {
                entity: "library:library".into(),
                field: "label.Red".into(),
                base: None,
                value: json!("Newest red"),
            }])
            .unwrap()
            .unwrap();
        let pending = newest.pending.clone();
        s.apply_system(Op::SetSyncState { state: Some(Box::new(newest)) }).unwrap();
        s.poll_cloud_replication();
        let state = s.catalog.sync_state().unwrap();
        assert!(!state.protocol.is_head());
        assert_eq!(state.pending, pending);
        assert!(state.previous_authors.contains(&before.device));
        assert!(!state.append.as_ref().unwrap().remote.received.contains_key(&m.revision.id()));
        assert_eq!(s.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Red), Some("Newest red"));
        let (_, loaded, _) = Journal::open(Box::new(files.files)).unwrap();
        assert_eq!(loaded.sync_state(), s.catalog.sync_state());
    }
    #[test]
    fn cancelled_or_stale_migration_never_switches_the_local_protocol() {
        for reason in ["cancelled", "library", "connection"] {
            let (mut s, files) = session();
            ready_migration(&mut s);
            let before = s.catalog.to_snapshot();
            let pending = s.cloud_replication_jobs.pending.as_mut().unwrap();
            match reason {
                "cancelled" => pending.cancel.store(true, Ordering::Relaxed),
                "library" => pending.library = Arc::new(()),
                _ => pending.connection_revision = pending.connection_revision.saturating_add(1),
            }
            s.poll_cloud_replication();
            assert_eq!(s.catalog.to_snapshot(), before, "{reason}");
            assert_eq!(s.cloud_replication_jobs.reports.last().unwrap()["ok"], false);
            let (_, loaded, _) = Journal::open(Box::new(files.files)).unwrap();
            assert_eq!(loaded.sync_state(), s.catalog.sync_state());
        }
    }
    #[test]
    fn preparing_migration_cancels_publication_and_stays_paused_after_completion() {
        let (mut s, _) = session();
        ready(&mut s);
        let before = s.catalog.to_snapshot();
        prepare_migration(&mut s, &json!({})).unwrap();
        s.poll_cloud_replication();
        assert_eq!(s.catalog.to_snapshot(), before);
        assert!(s.cloud_replication_jobs.preparing_migration());
        assert!(s.cloud_replication_jobs.paused);
        assert!(!s.cloud_replication_jobs.running());
        assert!(start_migration(&mut s, &json!({"writersStopped":false})).is_err());
        let reports = s.cloud_replication_jobs.reports.len();
        s.cloud_replication_jobs.next_poll = Some(web_time::Instant::now());
        s.resume_cloud_replication();
        assert_eq!(s.cloud_replication_jobs.reports.len(), reports);
    }
    #[test]
    fn replacing_a_library_clears_migration_preparation_and_cancels_its_worker() {
        let (mut s, _) = session();
        ready(&mut s);
        let cancel = s.cloud_replication_jobs.pending.as_ref().unwrap().cancel.clone();
        prepare_migration(&mut s, &json!({})).unwrap();
        s.open_library_in(
            LibraryStores {
                dir: "migration-other-library".into(),
                catalog: Box::new(MemStore::new()),
                files: Box::new(MemStore::new()),
                on_disk: false,
            },
            false,
        )
        .unwrap();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(!s.cloud_replication_jobs.preparing_migration());
        assert!(!s.cloud_replication_jobs.running());
        assert!(!s.cloud_replication_jobs.paused);
    }
    #[test]
    fn automatic_receive_is_scheduled_despite_failed_or_orphaned_other_connection_uploads() {
        let (mut s, _) = session();
        let asset = lightcraft_catalog::assets::Asset {
            original: lightcraft_catalog::assets::Blob {
                id: lightcraft_catalog::assets::BlobId("a".repeat(64)),
                size: 1,
                remote: lightcraft_catalog::assets::RemoteLocation { connection: "unrelated".into(), key: "originals/a".into(), version: None },
            },
            smart_preview: None,
            thumbnail: None,
            associated: Default::default(),
        };
        s.apply_system(Op::SetAsset { id: lightcraft_catalog::assets::AssetId("orphan".into()), asset: Some(Box::new(asset)) }).unwrap();
        s.apply_system(Op::SetUploadIntent {
            id: "orphan".into(),
            intent: Some(Box::new(lightcraft_catalog::uploads::UploadIntent {
                asset_id: lightcraft_catalog::assets::AssetId("orphan".into()),
                photo_id: PhotoId(99),
                state: lightcraft_catalog::uploads::UploadState::Failed,
                verified: Default::default(),
                error: Some("offline".into()),
                finalization: None,
                blocked: None,
            })),
        })
        .unwrap();
        s.cloud_replication_jobs.next_poll = Some(web_time::Instant::now() - std::time::Duration::from_secs(1));
        s.resume_cloud_replication();
        // This memory-backed test has no connection. Reaching that error proves scheduling
        // progressed past the unrelated upload, instead of silently waiting forever.
        assert_eq!(s.cloud_replication_jobs.reports.len(), 1);
        assert!(s.cloud_replication_jobs.reports[0]["error"].as_str().unwrap().contains("connection"));
    }

    #[test]
    fn slow_sync_completion_leaves_a_window_for_explicit_retry_before_auto_polling() {
        let (mut s, _) = session();
        ready(&mut s);
        s.cloud_replication_jobs.next_poll = Some(web_time::Instant::now() - std::time::Duration::from_secs(1));
        s.poll_cloud_replication();
        assert!(!s.cloud_replication_jobs.running());
        let reports = s.cloud_replication_jobs.reports.len();
        s.resume_cloud_replication();
        assert!(!s.cloud_replication_jobs.running());
        assert!(!s.cloud_replication_jobs.paused);
        assert_eq!(s.cloud_replication_jobs.reports.len(), reports);
        assert!(s.cloud_replication_jobs.next_poll.unwrap() > web_time::Instant::now());
    }
    #[test]
    fn append_publication_proof_incoming_fields_and_outbox_rollback_together_on_full_disk() {
        let (mut s, files) = session();
        let mut state = s.catalog.sync_state().unwrap().clone();
        state.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
        s.apply_system(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
        s.persist().unwrap();
        let before = s.catalog.sync_state().unwrap().clone();
        let prepare = |s: &mut Session| {
            ready(s);
            let outcome = s.cloud_replication_jobs.pending.as_mut().unwrap().ready.as_mut().unwrap().as_mut().unwrap();
            let p = lightcraft_catalog::replication::AppendProgress {
                version: 1,
                manifest: crate::assets::sha256(&crate::cloud_append::manifest("library").unwrap()).0,
                frontiers: lightcraft_catalog::replication::publication_prefixes(&outcome.state).unwrap(),
                remote: outcome.state.clone(),
                gaps: Default::default(),
            };
            outcome.append = Some(p);
            outcome.cursor.clear();
            outcome.generation = 0;
        };
        prepare(&mut s);
        files.fail.store(true, Ordering::Relaxed);
        s.poll_cloud_replication();
        assert_eq!(s.catalog.sync_state(), Some(&before));
        assert_eq!(s.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Green), None);
        let (_, loaded, _) = Journal::open(Box::new(files.files.clone())).unwrap();
        assert_eq!(loaded.sync_state(), Some(&before));
        files.fail.store(false, Ordering::Relaxed);
        s.persist().unwrap();
        prepare(&mut s);
        s.poll_cloud_replication();
        assert!(s.catalog.sync_state().unwrap().pending.is_empty());
        assert!(s.catalog.sync_state().unwrap().append.is_some());
        let (_, loaded, _) = Journal::open(Box::new(files.files)).unwrap();
        assert_eq!(loaded.sync_state(), s.catalog.sync_state());
    }
}
