use super::*;
use std::sync::atomic::AtomicU64;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lightcraft-cache-adoption-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn unavailable_or_unadopted_destination_restores_old_selection_without_cleanup() {
    for problem in ["disconnected", "changed-retention", "damaged-retention"] {
        let scratch = Scratch::new();
        let library = scratch.0.join("library");
        let mut session = Session::new().with_fs();
        session.open_library(&library, false).unwrap();
        let cache_dir = session.library.as_ref().unwrap().cache_dir.clone().unwrap();
        let old = scratch.0.join("old-custom");
        std::fs::create_dir_all(&old).unwrap();
        save_pointer(&cache_dir, Some(&Pointer { version: 1, path: old.clone() })).unwrap();
        session.close_library().unwrap();
        session.open_library(&library, false).unwrap();
        assert_eq!(session.media.asset_resolver.as_ref().unwrap().root(), old);
        let bytes = b"an original whose old copy must survive fallback";
        let id = crate::assets::sha256(bytes);
        std::fs::write(old.join(&id.0), bytes).unwrap();
        let target = scratch.0.join("new-cache");
        std::fs::create_dir_all(&target).unwrap();
        let moved = Job {
            from: old.clone(),
            to: target.clone(),
            cache_dir: cache_dir.clone(),
            files: vec![id.0.clone()],
            total: bytes.len() as u64,
            pointer: Some(Pointer { version: 1, path: target.clone() }),
        }
        .run(&AtomicBool::new(false), None)
        .unwrap();
        let copy_root = if problem == "disconnected" {
            let detached = scratch.0.join("detached");
            std::fs::rename(&target, &detached).unwrap();
            detached
        } else {
            let bytes = if problem == "damaged-retention" {
                b"not json".to_vec()
            } else {
                serde_json::to_vec(
                    &json!({"version":3,"budget_bytes":1 << 20,"pins":{},"removed":[],"keep_all":{"originals":false,"previews":false}}),
                )
                .unwrap()
            };
            std::fs::write(target.join(RETENTION), bytes).unwrap();
            target.clone()
        };
        let report = finish(&mut session, moved);
        assert_eq!(report["ok"], false, "{problem}: {report}");
        assert_eq!(report["oldSelectionRestored"], true, "{problem}: {report}");
        assert_eq!(session.media.asset_resolver.as_ref().unwrap().root(), old, "{problem}");
        assert_eq!(std::fs::read(old.join(&id.0)).unwrap(), bytes);
        assert_eq!(std::fs::read(copy_root.join(&id.0)).unwrap(), bytes, "the new copy is preserved too");
        assert_eq!(resolve(&cache_dir).0, old);
        session.close_library().unwrap();
        session.open_library(&library, false).unwrap();
        assert_eq!(session.media.asset_resolver.as_ref().unwrap().root(), old, "rollback is durable");
    }
}

#[test]
fn deferred_cleanup_and_old_maintenance_cannot_delete_a_reused_active_root() {
    use lightcraft_catalog::assets::{Blob, RemoteLocation};
    for reopen in [false, true] {
        let scratch = Scratch::new();
        let library = scratch.0.join("library");
        let mut session = Session::new().with_fs();
        session.open_library(&library, false).unwrap();
        let old = session.media.asset_resolver.clone().unwrap();
        let bytes = b"a lease that outlives two cache moves";
        let blob = Blob {
            id: crate::assets::sha256(bytes),
            size: bytes.len() as u64,
            remote: RemoteLocation { connection: "memory".into(), key: "library/original".into(), version: None },
        };
        std::fs::create_dir_all(old.root()).unwrap();
        std::fs::write(old.root().join(&blob.id.0), bytes).unwrap();
        let old_lease = old.acquire_cached(&blob, &AtomicBool::new(false)).unwrap().unwrap();
        let target = scratch.0.join("elsewhere");
        let result = command(&mut session, &json!({"path":target,"wait":true})).unwrap();
        assert_eq!(result["result"]["ok"], true, "{result}");
        assert_eq!(old_lease.read_bytes().unwrap(), bytes);
        if reopen {
            // Library's cleanup guard drops before its process lock, so another
            // session/process can reuse the root only after deletion is fenced off.
            drop(session);
            session = Session::new().with_fs();
            session.open_library(&library, false).unwrap();
        }
        let result = command(&mut session, &json!({"reset":true,"wait":true})).unwrap();
        assert_eq!(result["result"]["ok"], true, "{result}");
        let current = session.media.asset_resolver.clone().unwrap();
        assert_eq!(current.root(), old.root());
        let current_lease = current.acquire_cached(&blob, &AtomicBool::new(false)).unwrap().unwrap();
        drop(old_lease);
        assert_eq!(old.clear_cache().unwrap(), 0, "retired maintenance cannot evict the current resolver's lease");
        assert!(old.set_cache_budget(0).is_err(), "stale policy cannot overwrite the current cache");
        let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
        while std::time::Instant::now() < until {
            assert_eq!(current_lease.read_bytes().unwrap(), bytes, "{reopen}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(current.root().join(&blob.id.0)).unwrap(), bytes);
    }
}
