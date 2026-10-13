use super::*;
use crate::assets::AssetResolver;
use crate::{EngineError, Session, activity::TaskGuard};
use serde_json::{Value, json};
use std::sync::{Arc, mpsc};

struct Pending {
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
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub struct Jobs {
    pending: Option<Pending>,
    pub reports: Vec<Value>,
    next_poll: Option<web_time::Instant>,
    paused: bool,
}
impl Jobs {
    pub fn running(&self) -> bool {
        self.pending.is_some()
    }
}
fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
pub fn start(s: &mut Session, p: &Value) -> crate::Result<Value> {
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
    s.persist()?;
    let state = s.catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first using cloud.syncEnable"))?;
    let lib = s.library.as_mut().ok_or_else(|| error("open a persistent library first"))?;
    let connection = lib
        .cloud_connections()?
        .into_iter()
        .find(|connection| connection.id == state.connection)
        .ok_or_else(|| error("synchronization storage connection is not configured"))?;
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("native asset storage is unavailable"))?;
    let guard = s.activity.start("cloudSync", "Synchronize cloud library", crate::activity::Cancel::Yes);
    let id = guard.id();
    let handle = guard.handle();
    let cancel = guard.cancel_flag().ok_or_else(|| error("synchronization cancellation flag unavailable"))?;
    let worker_cancel = cancel.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("cloud-catalog-sync".into())
        .spawn(move || {
            handle.detail("Reading catalog head, reconciling and publishing durable mutations");
            let result = crate::guard::catch("cloud synchronization worker", || synchronize(&connection, &state, checkpoint, &worker_cancel))
                .map_err(error)
                .and_then(|result| result.map_err(|error| EngineError::Other(error.to_string())));
            let _ = sender.send(result);
        })
        .map_err(|e| error(format!("could not start cloud synchronization: {e}")))?;
    s.cloud_replication_jobs.pending = Some(Pending {
        receiver,
        library: s.library_token(),
        connection_revision: resolver.connection_revision(),
        resolver,
        guard,
        cancel,
        ready: None,
    });
    s.cloud_replication_jobs.paused = false;
    s.cloud_replication_jobs.next_poll = Some(web_time::Instant::now() + std::time::Duration::from_secs(5));
    if !wait {
        return Ok(json!({"task":id,"background":true}));
    }
    loop {
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
        if self.catalog.sync_state().is_none()
            || self.cloud_replication_jobs.running()
            || self.cloud_replication_jobs.paused
            || self.cloud_uploads.running() > 0
            || self.catalog.upload_intents().values().any(|intent| intent.state != lightcraft_catalog::uploads::UploadState::Synchronized)
        {
            return;
        }
        let now = web_time::Instant::now();
        let deadline = self.cloud_replication_jobs.next_poll.get_or_insert(now + std::time::Duration::from_secs(5));
        if now < *deadline {
            return;
        }
        *deadline = now + std::time::Duration::from_secs(5);
        if let Err(error) = start(self, &json!({"wait":false})) {
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
            if !Arc::ptr_eq(&pending.library, &self.library_token()) || pending.connection_revision != pending.resolver.connection_revision() {
                return Err(error("library or storage connection changed; remote result ignored and local pending work retained"));
            }
            let op = crate::cloud_sync::incoming_checkpoint(
                &self.catalog,
                &outcome.state,
                &outcome.acknowledged,
                outcome.cursor.clone(),
                outcome.generation,
            )?;
            let inverse = self.apply_catalog_mode(op, false)?;
            if let Err(error) = self.persist() {
                self.apply_received(inverse)?;
                return Err(error);
            }
            Ok(json!({"generation":outcome.generation,"cursor":outcome.cursor,"acknowledged":outcome.acknowledged.len(),
                "pending":self.catalog.sync_state().map_or(0,|state|state.pending.len())}))
        });
        self.cloud_replication_jobs.paused = result.is_err();
        let report = match result {
            Ok(result) => json!({"task":id,"ok":true,"result":result}),
            Err(error) => json!({"task":id,"ok":false,"error":error.to_string()}),
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
        let outcome =
            Outcome { state: remote.portable(), acknowledged: local.pending.keys().cloned().collect(), cursor: "a".repeat(64), generation: 1 };
        let resolver = Arc::new(AssetResolver::new(std::env::temp_dir().join("lightcraft-sync-owner-unused")));
        let guard = s.activity.start("cloudSync", "test sync", crate::activity::Cancel::Yes);
        let cancel = guard.cancel_flag().unwrap();
        let (_, receiver) = mpsc::sync_channel(1);
        s.cloud_replication_jobs.pending = Some(Pending {
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
}
