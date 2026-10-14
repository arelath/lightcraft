//! Shared cloud popup data and device-local publication controls.
use crate::{EngineError, Result, Session};
use serde_json::{Value, json};
mod health;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use health::now_ms;
pub use health::{PollHealth, ResourceHealth};

pub fn overview(s: &mut Session, _: &Value) -> Result<Value> {
    let summary = s.catalog.sync_overview();
    let tasks: Vec<_> = s.activity.list().into_iter().filter(|t| matches!(t.kind, "upload" | "cloudSync")).collect();
    let done = tasks.iter().filter(|t| t.unit == crate::activity::Unit::Bytes).fold(0_u64, |n, t| n.saturating_add(t.done.min(t.total)));
    let total = tasks.iter().filter(|t| t.unit == crate::activity::Unit::Bytes).fold(0_u64, |n, t| n.saturating_add(t.total));
    let mut out = json!({"summary":&*summary,"paused":s.cloud_paused,"userPaused":s.cloud_paused,"syncEnabled":s.catalog.sync_state().is_some(),"tasks":tasks,"doneBytes":done,"totalBytes":total,"canResume":tasks.is_empty(),"uploadingFiles":0,"waitingFiles":summary.queued_files.saturating_sub(summary.failed_files),"preparingMigration":false,"migrating":false});
    #[cfg(not(target_arch = "wasm32"))]
    {
        let active = s.cloud_uploads.active_files(&s.catalog) as u64;
        out["uploadingFiles"] = json!(active);
        out["waitingFiles"] = json!(summary.queued_files.saturating_sub(summary.failed_files).saturating_sub(active));
        out["preparingMigration"] = json!(s.cloud_replication_jobs.preparing_migration());
        out["migrating"] = json!(s.cloud_replication_jobs.migrating());
        out["paused"] = json!(s.cloud_paused || s.cloud_replication_jobs.preparing_migration());
        out["canResume"] = json!(s.cloud_uploads.running() == 0 && !s.cloud_replication_jobs.running());
        out["health"]["uploads"]["activity"] = s.cloud_uploads.health();
    }
    let sync = crate::cmd::cloud_sync_status(s, &Value::Null)?;
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
    out["utilization"] = sync["utilization"].clone();
    out["capacityWarnings"] = sync["capacityWarnings"].clone();
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
        crate::cloud_replication::resume_publication(s);
    }
    overview(s, &Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryStores;
    use lightcraft_catalog::{MemStore, Op, replication::SyncState};
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
