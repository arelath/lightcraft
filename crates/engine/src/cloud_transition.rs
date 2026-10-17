//! Device-local cloud mode changes. The catalog keeps causal identities even while
//! disconnected; durable originals live outside both cache and upload staging.
use crate::{EngineError, Result, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transition {
    pub stage: String,
    pub destination: String,
    pub error: Option<String>,
    pub done_files: u64,
    pub total_files: u64,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current_file: String,
    #[serde(default)]
    pub current_blob: String,
    pub blocking: bool,
    pub local_only: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Mode {
    version: u32,
    pub local_only: bool,
    transition: Option<Transition>,
}
impl Default for Mode {
    fn default() -> Self {
        Self { version: 1, local_only: false, transition: None }
    }
}
#[derive(Default)]
pub struct Controller {
    mode: Mode,
    job: Option<Job>,
    record_error: Option<String>,
}
struct Job {
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Transition>>,
    receiver: mpsc::Receiver<Result<crate::cloud_outcome::Outcome>>,
    library: Arc<()>,
    connection_revision: u64,
    catalog_revision: u64,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
impl Controller {
    pub(crate) fn running(&self) -> bool {
        self.job.is_some()
    }
    pub(crate) fn stop_for_close(&self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn local_only(&self) -> bool {
        self.mode.local_only
    }
    pub fn blocking(&self) -> bool {
        self.mode.transition.as_ref().is_some_and(|t| t.blocking)
    }
    pub fn view(&self) -> Value {
        let mut t =
            self.job.as_ref().map(|j| j.progress.lock().unwrap_or_else(PoisonError::into_inner).clone()).or_else(|| self.mode.transition.clone());
        if let Some(t) = &mut t {
            t.local_only = self.mode.local_only;
        }
        json!(t)
    }
    pub(crate) fn mode(&self) -> &Mode {
        &self.mode
    }
    pub(crate) fn restore(&mut self, mode: Mode) -> Result<()> {
        if mode.version != 1 {
            return Err(error("unsupported cloud mode record; update LightCraft before opening this library"));
        }
        self.mode = mode;
        self.job = None;
        if let Some(t) = self.mode.transition.as_mut().filter(|t| t.blocking) {
            t.stage = "failed".into();
            t.error = Some("Local download was interrupted. Retry to verify and reuse completed files, or cancel to keep cloud sync enabled.".into());
        }
        Ok(())
    }
}
fn destination(s: &Session) -> Result<PathBuf> {
    let lib = s
        .library
        .as_ref()
        .filter(|l| l.on_disk)
        .ok_or_else(|| error("turning cloud sync off requires a desktop library on durable local storage"))?;
    Ok(lib.dir.join("Local Originals"))
}
pub fn status(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(snapshot(s))
}
pub(crate) fn snapshot(s: &Session) -> Value {
    let mut value = s.cloud_transition.view();
    if let Some(blob) = value["currentBlob"].as_str().filter(|v| !v.is_empty())
        && let Some(resolver) = &s.media.asset_resolver
        && let crate::assets::AssetAvailability::Downloading { done, total } =
            crate::assets::TransferManager::availability(&**resolver, &lightcraft_catalog::assets::BlobId(blob.into()))
    {
        value["currentDownloadBytes"] = json!(done);
        value["currentDownloadTotal"] = json!(total);
    }
    value
}
pub fn preview(s: &mut Session, _: &Value) -> Result<Value> {
    if s.catalog.sync_state().is_none() {
        return Err(error("cloud sync is not enabled"));
    }
    let blobs = crate::cloud_local_transfer::plan(&s.catalog)?;
    Ok(json!({"destination":destination(s)?,"totalFiles":blobs.len(),
        "totalBytes":blobs.iter().fold(0u64,|n,b|n.saturating_add(b.size)),"estimate":true,
        "scope":"All live library originals, retained conflict originals and supporting data observed in the final verified cloud scan; cloud history and unrelated bucket files are not downloaded."}))
}
pub(crate) fn ensure_command(s: &Session, id: &str) -> Result<()> {
    if s.cloud_transition.blocking()
        && !matches!(
            id,
            "cloud.syncDisable"
                | "cloud.syncDisablePreview"
                | "cloud.transitionStatus"
                | "cloud.overview"
                | "cloud.syncStatus"
                | "cloud.uploadStatus"
                | "cloud.importStatus"
                | "activity.list"
                | "catalog.query"
                | "albums.list"
        )
    {
        return Err(error("Finishing local library download. Wait, retry or cancel in Turn off cloud sync before editing or switching libraries."));
    }
    if s.cloud_transition.local_only()
        && id.starts_with("cloud.")
        && !matches!(
            id,
            "cloud.syncEnable"
                | "cloud.syncDisable"
                | "cloud.syncDisablePreview"
                | "cloud.transitionStatus"
                | "cloud.overview"
                | "cloud.syncStatus"
                | "cloud.uploadStatus"
                | "cloud.importStatus"
                | "cloud.connections"
                | "cloud.cacheStatus"
                | "cloud.cacheSettings"
                | "cloud.offlineStatus"
        )
    {
        return Err(error("Cloud sync is off on this device. Turn on cloud sync explicitly before reconnecting, uploading or resuming."));
    }
    Ok(())
}
/// Only called after validating the same durable connection/library/protocol.
pub(crate) fn enable(s: &mut Session) -> Result<()> {
    if let Some(reason) = &s.cloud_transition.record_error {
        return Err(error(reason.clone()));
    }
    if s.cloud_transition.blocking() {
        return Err(error("finish or cancel the local download before turning sync on"));
    }
    if !s.cloud_transition.local_only() {
        return Ok(());
    }
    let previous = s.cloud_transition.mode.clone();
    let previous_pause = s.cloud_paused;
    s.cloud_paused = false;
    if let Err(e) = s.save_prefs() {
        s.cloud_paused = previous_pause;
        return Err(e);
    }
    s.cloud_transition.mode = Mode::default();
    if let Err(e) = s.save_cloud_mode() {
        s.cloud_transition.mode = previous;
        s.cloud_paused = previous_pause;
        let _ = s.save_prefs();
        return Err(e);
    }
    if let Some(resolver) = &s.media.asset_resolver {
        resolver.set_local_only(false);
    }
    crate::cloud_import::retry_library(s);
    s.cloud_uploads.request_resume(true);
    crate::cloud_replication::resume_publication(s);
    Ok(())
}
pub fn disable(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(reason) = &s.cloud_transition.record_error {
        return Err(error(reason.clone()));
    }
    for key in ["confirm", "cancel", "wait"] {
        if p.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(error(format!("{key} must be true or false")));
        }
    }
    if p["wait"] == true && crate::assets::is_cached_only() {
        return Err(error("desktop conversion runs in the background; use wait:false"));
    }
    if p["cancel"] == true {
        if let Some(job) = &s.cloud_transition.job {
            job.cancel.store(true, Ordering::Relaxed);
            return Ok(s.cloud_transition.view());
        }
        let previous = s.cloud_transition.mode.clone();
        s.cloud_transition.mode.transition = None;
        if let Err(e) = s.save_cloud_mode() {
            s.cloud_transition.mode = previous;
            return Err(e);
        }
        // Explicit cancel returns to the previous pause state; completed copies remain.
        if !s.cloud_paused && !s.cloud_transition.local_only() {
            s.cloud_uploads.request_resume(true);
            crate::cloud_replication::resume_publication(s);
        }
        return Ok(s.cloud_transition.view());
    }
    if p["confirm"] != true {
        return Err(error("review cloud.syncDisablePreview, then confirm:true to download and verify the local library before turning sync off"));
    }
    if s.cloud_transition.local_only() {
        return Ok(s.cloud_transition.view());
    }
    if s.catalog.sync_state().is_none() {
        return Err(error("cloud sync is not enabled"));
    }
    if s.cloud_transition.job.is_some() {
        return Ok(s.cloud_transition.view());
    }
    if s.cloud_imports.running() {
        return Err(error("finish or cancel current photo preparation before turning sync off"));
    }
    if s.cloud_uploads.reclaiming()
        || s.cache_move.is_some()
        || s.cloud_replication_jobs.migrating()
        || s.cloud_replication_jobs.preparing_migration()
        || s.interaction.is_some()
    {
        return Err(error("finish current maintenance or editing before turning cloud sync off"));
    }
    s.persist()?;
    let previous = s.cloud_transition.mode.clone();
    s.cloud_transition.mode.transition = Some(Transition {
        stage: "draining".into(),
        destination: destination(s)?.to_string_lossy().into_owned(),
        blocking: true,
        ..Default::default()
    });
    if let Err(e) = s.save_cloud_mode() {
        s.cloud_transition.mode = previous;
        return Err(e);
    }
    s.cloud_uploads.pause();
    crate::cloud_replication::pause_publication(s);
    s.poll_cloud_transition();
    if p["wait"] == true {
        while s.cloud_transition.blocking() && s.cloud_transition.mode.transition.as_ref().is_some_and(|t| t.stage != "failed") {
            s.poll_cloud_imports();
            s.poll_cloud_uploads();
            s.poll_cloud_replication();
            s.poll_cloud_transition();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    Ok(s.cloud_transition.view())
}
fn copy_library(
    mut catalog: lightcraft_catalog::Catalog,
    connection: crate::assets::s3::S3Connection,
    resolver: Arc<crate::assets::AssetResolver>,
    destination: PathBuf,
    cancel: &AtomicBool,
    progress: &Mutex<Transition>,
) -> Result<crate::cloud_outcome::Outcome> {
    let mut previous = None;
    for pass in 0..8 {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("Local download cancelled. Verified copies are retained; cloud sync is still enabled."));
        }
        {
            let mut p = progress.lock().unwrap_or_else(PoisonError::into_inner);
            p.stage = if pass == 0 { "reading" } else { "checking" }.into();
            p.current_file.clear();
        }
        let state = catalog.sync_state().ok_or_else(|| error("cloud identity is unavailable"))?;
        let outcome = crate::cloud_replication::receive_only(&connection, state, cancel).map_err(|e| error(e.to_string()))?;
        if outcome.append_progress().is_some_and(|p| !p.gaps.is_empty()) {
            return Err(error("Cloud history has missing dependencies. Reconnect and synchronize before retrying."));
        }
        let fingerprint = crate::assets::sha256(&serde_json::to_vec(outcome.state()).map_err(|e| error(e.to_string()))?);
        let op = crate::cloud_sync::completed_outcome(&catalog, &outcome, outcome.acknowledged(), state.generation)?;
        catalog.apply(op)?;
        let blobs = crate::cloud_local_transfer::plan(&catalog)?;
        if previous.as_ref() == Some(&fingerprint) {
            crate::cloud_local_transfer::verify_local_files(&catalog, cancel)?;
            // Read-back every complete local reference immediately before owner commit.
            for blob in &blobs {
                if !crate::cloud_local_transfer::verify(&destination, blob, cancel)? {
                    return Err(error("A local file changed during verification. Retry to recover it; sync remains enabled."));
                }
            }
            return Ok(outcome);
        }
        progress.lock().unwrap_or_else(PoisonError::into_inner).stage = "downloading".into();
        let mut names = std::collections::BTreeMap::new();
        for photo in catalog.photos() {
            let lightcraft_catalog::Source::Managed { asset_id } = &photo.source else { continue };
            let Some(asset) = catalog.asset(asset_id) else { continue };
            names.insert(asset.original.id.clone(), photo.file_name.clone());
            for blob in asset.associated.values() {
                names.insert(blob.id.clone(), format!("{} (sidecar)", photo.file_name));
            }
            if let Some(blob) = &asset.thumbnail {
                names.insert(blob.id.clone(), format!("{} (thumbnail)", photo.file_name));
            }
            if let Some(blob) = &asset.smart_preview {
                names.insert(blob.id.clone(), format!("{} (preview)", photo.file_name));
            }
        }
        crate::cloud_local_transfer::materialize(resolver.clone(), &destination, &blobs, cancel, |v| {
            let mut p = progress.lock().unwrap_or_else(PoisonError::into_inner);
            p.done_files = v.files;
            p.total_files = v.total_files;
            p.done_bytes = v.bytes;
            p.total_bytes = v.total_bytes;
            if let Some(blob) = blobs.iter().find(|b| b.remote.key == v.current_file) {
                p.current_blob = blob.id.0.clone();
                p.current_file = names.get(&blob.id).cloned().unwrap_or_else(|| "Retained photo data".into());
            } else {
                p.current_file = v.current_file;
                p.current_blob.clear();
            }
        })?;
        previous = Some(fingerprint);
    }
    Err(error("Cloud library kept changing during download. Verified copies are retained. Retry when other devices are less active."))
}
impl Session {
    pub(crate) fn save_cloud_mode(&self) -> Result<()> {
        let Some(lib) = self.library.as_ref().filter(|l| l.on_disk) else {
            return Err(error("cloud mode changes require a durable desktop library"));
        };
        let bytes = serde_json::to_vec(self.cloud_transition.mode()).map_err(|e| error(e.to_string()))?;
        lightcraft_catalog::safe_file::write_atomic(&lib.dir.join("cloud-mode.json"), &bytes)
            .map_err(|e| error(format!("Could not save cloud mode: {e}; sync remains enabled")))
    }
    pub(crate) fn load_cloud_mode(&mut self) -> Result<()> {
        use std::io::Read;
        self.cloud_transition = Controller::default();
        let Some(lib) = self.library.as_ref().filter(|l| l.on_disk) else { return Ok(()) };
        let destination = lib.dir.join("Local Originals");
        if let Some(resolver) = &self.media.asset_resolver {
            resolver.set_permanent_root(destination);
        }
        let path = lib.dir.join("cloud-mode.json");
        let mode = (|| {
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Mode::default()),
                Err(e) => return Err(error(e.to_string())),
            };
            let mut bytes = Vec::new();
            file.take(65_537).read_to_end(&mut bytes).map_err(|e| error(e.to_string()))?;
            if bytes.len() > 65_536 {
                return Err(error("cloud mode record exceeds its size limit"));
            }
            serde_json::from_slice::<Mode>(&bytes)
                .map_err(|e| error(format!("invalid cloud mode record: {e}; restore cloud-mode.json before reconnecting")))
        })();
        match mode.and_then(|m| self.cloud_transition.restore(m)) {
            Ok(()) => {}
            Err(e) => {
                // An unreadable mode file must never silently reconnect the library.
                self.cloud_transition.mode.local_only = true;
                self.cloud_transition.record_error = Some(e.to_string());
                if let Some(resolver) = &self.media.asset_resolver {
                    resolver.set_local_only(true);
                }
                return Err(e);
            }
        }
        if let Some(resolver) = &self.media.asset_resolver {
            resolver.set_local_only(self.cloud_transition.local_only());
        }
        Ok(())
    }
    /// Hosts pump this before normal cloud schedulers; command dispatch pumps it too.
    pub fn poll_cloud_transition(&mut self) {
        let Some(current) = self.cloud_transition.mode.transition.as_ref() else { return };
        if !current.blocking || current.stage == "failed" {
            return;
        }
        if let Some(job) = self.cloud_transition.job.take() {
            let result = match job.receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => {
                    self.cloud_transition.job = Some(job);
                    return;
                }
                Err(_) => Err(error("Local download worker stopped. Retry to reuse verified files.")),
            };
            let mut completed = job.progress.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let canceled = job.cancel.load(Ordering::Relaxed);
            if canceled {
                self.cloud_transition.mode.transition = None;
                if let Err(e) = self.save_cloud_mode() {
                    completed.stage = "failed".into();
                    completed.error = Some(e.to_string());
                    completed.blocking = true;
                    self.cloud_transition.mode.transition = Some(completed);
                } else if !self.cloud_paused {
                    self.cloud_uploads.request_resume(true);
                    crate::cloud_replication::resume_publication(self);
                }
                return;
            }
            let result = result.and_then(|outcome| {
                if canceled
                    || self.catalog.revision != job.catalog_revision
                    || !Arc::ptr_eq(&job.library, &self.library_token())
                    || self.media.asset_resolver.as_ref().is_none_or(|r| r.connection_revision() != job.connection_revision)
                {
                    return Err(error("Local download cancelled or library binding changed; sync remains enabled."));
                }
                let generation = self.catalog.sync_state().map_or(0, |s| s.generation);
                let op = crate::cloud_sync::completed_outcome(&self.catalog, &outcome, outcome.acknowledged(), generation)?;
                self.apply_received(op)?;
                self.persist()?;
                // Catalog commit precedes the single atomic mode record write.
                self.cloud_transition.mode.local_only = true;
                completed.stage = "complete".into();
                completed.blocking = false;
                completed.local_only = true;
                completed.error = None;
                self.cloud_transition.mode.transition = Some(completed.clone());
                if let Err(e) = self.save_cloud_mode() {
                    self.cloud_transition.mode.local_only = false;
                    return Err(e);
                }
                if let Some(resolver) = &self.media.asset_resolver {
                    resolver.set_local_only(true);
                }
                Ok(())
            });
            if let Err(e) = result {
                completed.stage = "failed".into();
                completed.error = Some(format!("{e} Sync remains enabled. Retry or cancel; completed local files are retained."));
                completed.blocking = true;
                self.cloud_transition.mode.transition = Some(completed);
                if let Err(save) = self.save_cloud_mode() {
                    log::error!("cloud transition error could not be saved: {save}");
                }
            }
            return;
        }
        if self.cloud_uploads.running() > 0 || self.cloud_replication_jobs.running() || self.cloud_imports.preparing() > 0 {
            return;
        }
        let result: Result<()> = (|| {
            self.persist()?;
            let resolver = self.media.asset_resolver.clone().ok_or_else(|| error("persistent original storage unavailable"))?;
            let connection_revision = resolver.connection_revision();
            let state = self.catalog.sync_state().ok_or_else(|| error("cloud identity is unavailable"))?;
            let id = state.connection.clone();
            let connection = self
                .library
                .as_mut()
                .ok_or_else(|| error("open a library first"))?
                .cloud_connections()?
                .into_iter()
                .find(|c| c.id == id)
                .ok_or_else(|| error("Reconnect the configured cloud connection, then retry."))?;
            let destination = destination(self)?;
            let cancel = Arc::new(AtomicBool::new(false));
            let progress = Arc::new(Mutex::new(self.cloud_transition.mode.transition.clone().ok_or_else(|| error("transition unavailable"))?));
            let (sender, receiver) = mpsc::channel();
            let (worker_cancel, worker_progress, catalog) = (cancel.clone(), progress.clone(), self.catalog.clone());
            std::thread::Builder::new()
                .name("lc-local-library".into())
                .spawn(move || {
                    let result = crate::guard::catch("local library download", || {
                        copy_library(catalog, connection, resolver, destination, &worker_cancel, &worker_progress)
                    })
                    .unwrap_or_else(|e| Err(error(e)));
                    let _ = sender.send(result);
                })
                .map_err(|e| error(e.to_string()))?;
            self.cloud_transition.job =
                Some(Job { cancel, progress, receiver, library: self.library_token(), connection_revision, catalog_revision: self.catalog.revision });
            Ok(())
        })();
        if let Err(e) = result {
            if let Some(t) = self.cloud_transition.mode.transition.as_mut() {
                t.stage = "failed".into();
                t.error = Some(e.to_string());
            }
            let _ = self.save_cloud_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{Op, replication::SyncState};
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("lc-cloud-mode-{}", crate::cloud_owner::random().unwrap()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn open(&self) -> Session {
            let mut s = Session::new().with_fs();
            s.open_library(&self.0, false).unwrap();
            s
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn bind(s: &mut Session) {
        let state = SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
        s.apply_system(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
        s.persist().unwrap();
    }
    #[test]
    fn interrupted_conversion_reopens_blocked_and_cancel_restores_cloud_mode() {
        let scratch = Scratch::new();
        {
            let mut s = scratch.open();
            bind(&mut s);
            s.cloud_transition.mode.transition = Some(Transition { stage: "downloading".into(), blocking: true, ..Default::default() });
            s.save_cloud_mode().unwrap();
        }
        let mut s = scratch.open();
        assert!(s.cloud_transition.blocking());
        assert_eq!(s.cloud_transition.view()["stage"], "failed");
        assert!(s.execute("library.preferences", &json!({})).unwrap_err().to_string().contains("local library download"));
        assert!(s.open_library(scratch.0.join("another"), false).is_err());
        assert!(s.commit("edit", Op::SetLabelName { label: lightcraft_catalog::ColorLabel::Red, name: Some("blocked".into()) }).is_err());
        disable(&mut s, &json!({"cancel":true})).unwrap();
        assert!(!s.cloud_transition.blocking());
        assert!(!s.cloud_transition.local_only());
        assert!(s.catalog.sync_state().is_some());
    }
    #[test]
    fn one_cancel_drains_worker_without_committing_local_only() {
        let scratch = Scratch::new();
        let mut s = scratch.open();
        bind(&mut s);
        let transition = Transition { stage: "downloading".into(), blocking: true, ..Default::default() };
        s.cloud_transition.mode.transition = Some(transition.clone());
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        s.cloud_transition.job = Some(Job {
            cancel: cancel.clone(),
            progress: Arc::new(Mutex::new(transition)),
            receiver,
            library: s.library_token(),
            connection_revision: 0,
            catalog_revision: s.catalog.revision,
        });
        disable(&mut s, &json!({"cancel":true})).unwrap();
        assert!(cancel.load(Ordering::Relaxed));
        sender.send(Err(error("cancelled"))).unwrap();
        s.poll_cloud_transition();
        assert!(!s.cloud_transition.blocking());
        assert!(!s.cloud_transition.local_only());
        let record: Mode = serde_json::from_slice(&std::fs::read(scratch.0.join("cloud-mode.json")).unwrap()).unwrap();
        assert!(record.transition.is_none());
        assert!(!record.local_only);
    }
    #[test]
    fn local_only_survives_restart_and_rejects_upload_resume_and_reconnect() {
        let scratch = Scratch::new();
        {
            let mut s = scratch.open();
            bind(&mut s);
            s.cloud_transition.mode.local_only = true;
            s.save_cloud_mode().unwrap();
        }
        let mut s = scratch.open();
        for id in ["cloud.resume", "cloud.sync", "cloud.upload", "cloud.reconnect", "cloud.configure"] {
            assert!(s.execute(id, &json!({})).is_err(), "{id}");
        }
        s.resume_cloud_uploads();
        s.resume_cloud_replication();
        assert_eq!(s.cloud_uploads.running(), 0);
        assert!(!s.cloud_replication_jobs.running());
        assert_eq!(s.execute("cloud.overview", &json!({})).unwrap()["syncEnabled"], false);
        assert_eq!(s.execute("cloud.syncStatus", &json!({})).unwrap()["enabled"], false);
        assert!(s.catalog.sync_state().is_some());
    }
    #[test]
    fn failed_mode_write_retains_previous_mode_and_history() {
        let scratch = Scratch::new();
        let mut s = scratch.open();
        bind(&mut s);
        let before = s.catalog.to_snapshot();
        let fault = lightcraft_catalog::safe_file::fail_writes_after(0);
        assert!(disable(&mut s, &json!({"confirm":true})).is_err());
        drop(fault);
        assert!(!s.cloud_transition.blocking());
        assert!(!s.cloud_transition.local_only());
        assert_eq!(before, s.catalog.to_snapshot());
    }
    #[test]
    fn final_mode_save_failure_cannot_report_success_or_enable_local_only() {
        let scratch = Scratch::new();
        let mut s = scratch.open();
        bind(&mut s);
        let transition = Transition { stage: "checking".into(), blocking: true, ..Default::default() };
        s.cloud_transition.mode.transition = Some(transition.clone());
        s.save_cloud_mode().unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(crate::cloud_outcome::Outcome::empty(s.catalog.sync_state().unwrap().portable()))).unwrap();
        s.cloud_transition.job = Some(Job {
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(transition)),
            receiver,
            library: s.library_token(),
            connection_revision: s.media.asset_resolver.as_ref().unwrap().connection_revision(),
            catalog_revision: s.catalog.revision,
        });
        let fault = lightcraft_catalog::safe_file::fail_writes_after(0);
        s.poll_cloud_transition();
        drop(fault);
        assert!(!s.cloud_transition.local_only());
        assert!(s.cloud_transition.blocking());
        assert_eq!(s.cloud_transition.view()["stage"], "failed");
        assert!(s.catalog.sync_state().is_some());
    }
    #[test]
    fn unreadable_mode_record_never_silently_reconnects() {
        let scratch = Scratch::new();
        {
            let mut s = scratch.open();
            bind(&mut s);
        }
        std::fs::write(scratch.0.join("cloud-mode.json"), b"{future-invalid}").unwrap();
        let mut s = Session::new().with_fs();
        assert!(s.open_library(&scratch.0, false).is_err());
        assert!(s.cloud_transition.local_only());
        assert!(s.execute("cloud.syncEnable", &json!({})).is_err());
        assert_eq!(std::fs::read(scratch.0.join("cloud-mode.json")).unwrap(), b"{future-invalid}");
        s.resume_cloud_uploads();
        s.resume_cloud_replication();
        assert_eq!(s.cloud_uploads.running(), 0);
        assert!(!s.cloud_replication_jobs.running());
    }
    #[test]
    fn desktop_wait_argument_fails_before_persisting_a_transition() {
        let scratch = Scratch::new();
        let mut s = scratch.open();
        bind(&mut s);
        let _scope = crate::assets::CachedOnly::enter(true);
        assert!(disable(&mut s, &json!({"confirm":true,"wait":true})).is_err());
        assert!(!s.cloud_transition.blocking());
        assert!(!scratch.0.join("cloud-mode.json").exists());
    }
}
