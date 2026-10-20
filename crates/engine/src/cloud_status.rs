//! Shared cloud popup data and device-local publication controls.
use crate::{EngineError, Result, Session};
use serde_json::{Value, json};
mod actions;
mod diagnostic;
mod failure;
mod health;
mod polling;
mod presentation;
pub mod provider;
pub use actions::{ActionKind, Actions, DeviceMode, ManualAction};
pub use diagnostic::{Diagnostic, RetryState};
pub use failure::{CloudFailure, FailureKind, Recovery, RetryBudget};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use health::now_ms;
pub use health::{Failure, PollHealth, ResourceHealth};
pub use polling::PollCadence;
pub use presentation::{Presentation, SyncStatus};

pub fn can_upgrade(s: &Session) -> bool {
    let Some(state) = s.catalog.sync_state() else { return false };
    if state.protocol.is_head() {
        return true;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.media.asset_resolver.as_ref().and_then(|resolver| resolver.cloud_provider(&state.connection))
            == Some(crate::cloud_setup::Provider::Backblaze)
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

pub(crate) fn can_resume(s: &Session) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.cloud_uploads.running() == 0 && !s.cloud_replication_jobs.running() && s.cloud_imports.preparing() == 0
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        true
    }
}

pub fn overview(s: &mut Session, _: &Value) -> Result<Value> {
    let summary = s.catalog.sync_overview();
    let tasks: Vec<_> = s.activity.list().into_iter().filter(|t| matches!(t.kind, "upload" | "cloudSync" | "import")).collect();
    let done = tasks.iter().filter(|t| t.unit == crate::activity::Unit::Bytes).fold(0_u64, |n, t| n.saturating_add(t.done.min(t.total)));
    let total = tasks.iter().filter(|t| t.unit == crate::activity::Unit::Bytes).fold(0_u64, |n, t| n.saturating_add(t.total));
    let mut out = json!({"summary":&*summary,"paused":s.cloud_paused,"userPaused":s.cloud_paused,"syncEnabled":s.catalog.sync_state().is_some(),"tasks":tasks,"doneBytes":done,"totalBytes":total,"canResume":tasks.is_empty(),"uploadingFiles":0,"waitingFiles":summary.queued_files.saturating_sub(summary.failed_files),"preparingMigration":false,"migrating":false});
    out["inputLimits"] = crate::input_limits::view();
    #[cfg(not(target_arch = "wasm32"))]
    {
        out["syncEnabled"] = json!(s.catalog.sync_state().is_some() && !s.cloud_transition.local_only());
        out["localOnly"] = json!(s.cloud_transition.local_only());
        if s.cloud_transition.local_only() {
            out["localDestination"] = json!(s.library.as_ref().map(|lib| lib.dir.join("Local Originals").to_string_lossy().into_owned()));
        }
        out["transition"] = crate::cloud_transition::snapshot(s);
        let active = s.cloud_uploads.active_files(&s.catalog) as u64;
        out["uploadingFiles"] = json!(active);
        out["waitingFiles"] = json!(summary.queued_files.saturating_sub(summary.failed_files).saturating_sub(active));
        out["preparingMigration"] = json!(s.cloud_replication_jobs.preparing_migration());
        out["migrating"] = json!(s.cloud_replication_jobs.migrating());
        out["paused"] = json!(s.cloud_paused || s.cloud_replication_jobs.preparing_migration());
        out["canResume"] = json!(can_resume(s));
        out["preparingFiles"] = json!(s.cloud_imports.preparing());
        out["preparingPhotos"] = json!(s.cloud_imports.preparing_photos());
        out["health"]["uploads"]["activity"] = s.cloud_uploads.health();
        out["health"]["imports"]["failed"] = json!(s.cloud_imports.failures);
        if let Some(reason) = s.cloud_imports.failures.first().and_then(|issue| issue["reason"].as_str()) {
            out["error"] = json!(reason);
        }
    }
    let sync = crate::cmd::cloud_sync_status(s, &Value::Null)?;
    out["canUpgrade"] = sync["canUpgrade"].clone();
    out["health"]["sync"] = sync["health"].clone();
    out["health"]["uploads"]["failedImports"] = json!(summary.failed_uploads);
    out["health"]["uploads"]["blockedImports"] = json!(summary.blocked_import_count);
    out["health"]["receipts"] = json!({"pending":summary.pending_receipts,"failed":summary.failed_receipts});
    out["health"]["publicationWaiting"] = json!(summary.pending_changes > 0);
    out["health"]["remoteHistoryWaiting"] = json!(sync["gaps"].as_array().is_some_and(|gaps| !gaps.is_empty()));
    out["conflicts"] = sync["conflicts"].clone();
    out["structuralConflicts"] = sync["structuralConflicts"].clone();
    out["health"]["conflicts"] =
        json!({"fields":sync["conflicts"].as_array().map_or(0,Vec::len),"structural":sync["structuralConflicts"].as_array().map_or(0,Vec::len)});
    if let Some(message) = sync["health"]["lastError"]["message"].as_str() {
        out["error"] = json!(message);
    } else if let Some(reason) = &summary.first_upload_error {
        out["error"] = json!(reason);
    } else if let Some(failures) = out["health"]["uploads"]["activity"]["failures"].as_object()
        && let Some(message) = failures.values().find_map(|failure| failure["message"].as_str())
    {
        out["error"] = json!(message);
    }
    // Receiving continues while the library refuses publication (maintenance, a moved library,
    // a newer writer): local changes then wait here, which the status must say.
    out["publicationBlocked"] = sync["publicationBlocked"].clone();
    out["publicationFailure"] = sync["publicationFailure"].clone();
    out["health"]["publicationBlocked"] = sync["publicationBlocked"].clone();
    out["utilization"] = sync["utilization"].clone();
    out["capacityWarnings"] = sync["capacityWarnings"].clone();
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(status) = crate::cloud_upload::erasure_status(s) {
        if status["complete"] == true {
            out["summary"]["cloudOriginalBytes"] = json!(0);
        }
        out["erasure"] = status;
    }
    Ok(out)
}
pub fn pause(s: &mut Session, _: &Value) -> Result<Value> {
    s.cloud_paused = true;
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.cloud_uploads.pause();
        crate::cloud_replication::pause_publication(s);
    }
    s.save_prefs().map_err(|e| EngineError::Other(format!("Syncing paused in this session; could not save pause: {e}")))?;
    overview(s, &Value::Null)
}
pub fn resume(s: &mut Session, _: &Value) -> Result<Value> {
    #[cfg(not(target_arch = "wasm32"))]
    crate::cloud_transition::ensure_command(s, "cloud.resume")?;
    #[cfg(not(target_arch = "wasm32"))]
    crate::cloud_upload::ensure_not_erased(s)?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        if s.cloud_uploads.running() > 0 || s.cloud_replication_jobs.running() {
            return Err(EngineError::Other("wait for current cloud tasks to stop before resuming".into()));
        }
    }
    let previous = s.cloud_paused;
    s.cloud_paused = false;
    if let Err(e) = s.save_prefs() {
        s.cloud_paused = previous;
        return Err(e);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.cloud_uploads.request_resume(true);
        crate::cloud_import::retry_library(s);
        crate::cloud_replication::resume_publication(s);
    }
    overview(s, &Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryStores;
    use lightcraft_catalog::{MemStore, Op, replication::SyncState};
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn append_upgrade_capability_uses_the_cached_destination_without_credentials_or_probe() {
        use crate::assets::{AssetResolver, s3::S3Connection};
        for (endpoint, supported) in [(Some("https://s3.us-east-005.backblazeb2.com"), true), (Some("https://example.invalid"), false), (None, false)]
        {
            let mut s = Session::new();
            let mut state = SyncState::new("library".into(), "author".into(), "personal".into()).unwrap();
            state.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
            s.catalog.apply(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
            assert!(!can_upgrade(&s));
            let resolver = std::sync::Arc::new(AssetResolver::new(std::env::temp_dir().join("lightcraft-cr43-status-unused")));
            let connection: S3Connection =
                serde_json::from_value(json!({"id":"personal","bucket":"photos","region":"us-east-005","prefix":"library","endpoint":endpoint}))
                    .unwrap();
            resolver.connect_s3(connection).unwrap();
            s.media.asset_resolver = Some(resolver);
            assert_eq!(can_upgrade(&s), supported);
            assert_eq!(crate::cmd::cloud_sync_status(&mut s, &json!({})).unwrap()["canUpgrade"], supported);
            assert_eq!(crate::cloud_replication::prepare_migration(&mut s, &json!({})).is_ok(), supported);
        }
    }
    fn stores(files: MemStore) -> LibraryStores {
        LibraryStores { dir: "cloud-pause-test".into(), catalog: Box::new(MemStore::new()), files: Box::new(files), on_disk: false }
    }
    #[test]
    fn cloud_pause_survives_reopen_and_blocks_manual_and_automatic_publication() {
        let files = MemStore::new();
        let mut s = Session::new();
        s.open_library_in(stores(files.clone()), false).unwrap();
        s.apply_system(Op::SetSyncState { state: Some(Box::new(SyncState::new("library".into(), "device".into(), "personal".into()).unwrap())) })
            .unwrap();
        pause(&mut s, &Value::Null).unwrap();
        assert!(s.execute("cloud.sync", &json!({"wait":false})).unwrap_err().to_string().contains("paused"));
        assert!(crate::cloud_upload::start(&mut s, &json!({"wait":false})).unwrap_err().to_string().contains("paused"));
        s.cloud_uploads.request_resume(true);
        s.resume_cloud_uploads();
        s.resume_cloud_replication();
        assert_eq!(s.cloud_uploads.running(), 0);
        assert!(!s.cloud_replication_jobs.running());
        let mut reopened = Session::new();
        reopened.open_library_in(stores(files), false).unwrap();
        assert!(reopened.cloud_paused);
        resume(&mut reopened, &Value::Null).unwrap();
        assert!(!reopened.cloud_paused);
        s.open_library_in(stores(MemStore::new()), false).unwrap();
        assert!(!s.cloud_paused, "pause belongs to its library/device prefs");
    }
    #[test]
    fn a_failed_pause_save_still_stops_this_session_and_failed_resume_stays_paused() {
        struct NoWrites;
        impl lightcraft_catalog::Store for NoWrites {
            fn read(&mut self, _: &str) -> std::io::Result<Option<Vec<u8>>> {
                Ok(None)
            }
            fn write_atomic(&mut self, _: &str, _: &[u8]) -> std::io::Result<()> {
                Err(std::io::Error::other("full disk"))
            }
            fn append(&mut self, _: &str, _: &[u8]) -> std::io::Result<()> {
                Err(std::io::Error::other("full disk"))
            }
            fn truncate(&mut self, _: &str, _: u64) -> std::io::Result<()> {
                Err(std::io::Error::other("full disk"))
            }
            fn describe(&self) -> String {
                "unwritable preferences".into()
            }
        }
        let mut storage = stores(MemStore::new());
        storage.files = Box::new(NoWrites);
        let mut s = Session::new();
        s.open_library_in(storage, false).unwrap();
        assert!(pause(&mut s, &Value::Null).unwrap_err().to_string().contains("paused in this session"));
        assert!(s.cloud_paused);
        assert!(resume(&mut s, &Value::Null).is_err());
        assert!(s.cloud_paused);
    }
    #[test]
    fn cloud_filter_rejects_unknown_status_without_discarding_existing_constraints() {
        let mut s = Session::with_demo();
        s.execute("library.filter", &json!({"rating":3,"syncStatus":"localOnly"})).unwrap();
        let before = s.filter.clone();
        assert!(s.execute("library.filter", &json!({"syncStatus":"bogus"})).is_err());
        assert_eq!(s.filter, before);
        assert!(crate::filter_chips(&s.filter, &s.catalog).iter().any(|c| c.clear == json!({"syncStatus":null})));
    }
}
