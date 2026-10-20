//! Explicit retention for locally recorded multipart sessions, including deleted photos.
//! Receipt files and staged/working bytes are never garbage collection candidates here.

use super::*;
use std::time::SystemTime;

const MAX_FILES: usize = 4096;
const MAX_CANDIDATES: usize = 32;

struct Candidate {
    receipt: Receipt,
    age: Duration,
}

fn scan(root: &Path, connection: &S3Connection, retention: Duration, cancel: &AtomicBool) -> Result<(Vec<Candidate>, usize)> {
    let dir = root.join("uploads");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(error("upload receipt directory is redirected or not a directory; cleanup refused")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(io(e)),
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(io(e)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        if paths.len() >= MAX_FILES {
            return Err(error("too many upload receipts to scan safely; no cleanup was applied"));
        }
        paths.push(entry.map_err(io)?.path());
    }
    paths.sort();
    let now = SystemTime::now();
    let mut candidates = Vec::new();
    let mut remaining = 0;
    for path in paths {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("cleanup cancelled; staged originals and remaining receipts preserved"));
        }
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path).map_err(io)?;
        if !meta.file_type().is_file() || meta.file_type().is_symlink() {
            return Err(error("upload receipt is not a regular file; cleanup refused"));
        }
        let receipt = read_receipt(&path)?.ok_or_else(|| error("upload receipt disappeared during cleanup; retry"))?;
        if path.file_name().is_none_or(|name| name != format!("{}.json", receipt.id).as_str()) {
            return Err(error("upload receipt identity does not match its filename; preserved"));
        }
        if receipt.imported.asset.original.remote.connection != connection.id || receipt.multipart.is_empty() {
            continue;
        }
        let Some(storage) = &receipt.storage else {
            return Err(error(
                "legacy multipart receipt has no storage destination; use explicit abort with its original connection settings before retrying; preserved",
            ));
        };
        if !storage.same_destination(connection) {
            return Err(error("multipart receipt belongs to another storage destination; restore its connection settings; preserved"));
        }
        for blob in blobs(&receipt.imported.asset) {
            connection.validate_blob(blob).map_err(|e| error(e.to_string()))?;
        }
        // A future timestamp cannot establish that a receipt is old enough to retire.
        let age = now.duration_since(meta.modified().map_err(io)?).unwrap_or_default();
        if age < retention {
            continue;
        }
        if candidates.len() < MAX_CANDIDATES {
            candidates.push(Candidate { receipt, age });
        } else {
            remaining += 1;
        }
    }
    Ok((candidates, remaining))
}

fn run(
    root: &Path,
    connection: &S3Connection,
    resolver: &AssetResolver,
    retention: Duration,
    apply: bool,
    cancel: &AtomicBool,
    task: &TaskHandle,
) -> Result<Value> {
    let revision = resolver.connection_revision();
    task.detail("Inspecting idle multipart receipts; originals remain protected");
    resolver.validate_storage().map_err(|e| error(e.to_string()))?;
    let (candidates, remaining) = {
        let _guard = resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
        scan(root, connection, retention, cancel)?
    };
    let preview = candidates
        .iter()
        .map(|candidate| {
            json!({"import": candidate.receipt.id, "idleDays": candidate.age.as_secs() / 86400,
            "multipart": candidate.receipt.multipart.iter().map(|(key, upload)|
                json!({"key": key, "uploadId": upload.id, "savedParts": upload.parts.len()})).collect::<Vec<_>>()})
        })
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    if apply {
        for (index, candidate) in candidates.into_iter().enumerate() {
            if cancel.load(Ordering::Relaxed) || resolver.connection_revision() != revision {
                return Err(error("cleanup cancelled or connection changed; remaining receipts and originals preserved"));
            }
            resolver.validate_storage().map_err(|e| error(e.to_string()))?;
            let path = root.join("uploads").join(format!("{}.json", candidate.receipt.id));
            let mut receipt = {
                let _guard = resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
                read_receipt(&path)?.ok_or_else(|| error("upload receipt disappeared; cleanup stopped"))?
            };
            if receipt != candidate.receipt {
                return Err(error("upload receipt changed after preview; cleanup stopped without discarding its new state"));
            }
            let uploads = receipt.multipart.iter().map(|(key, upload)| (key.clone(), upload.id.clone())).collect::<Vec<_>>();
            let mut aborted = 0;
            for (key, upload_id) in uploads {
                if cancel.load(Ordering::Relaxed) || resolver.connection_revision() != revision {
                    return Err(error("cleanup cancelled or connection changed; remaining receipts and originals preserved"));
                }
                let blob = blobs(&receipt.imported.asset)
                    .find(|blob| blob.remote.key == key)
                    .ok_or_else(|| error("multipart object disappeared from receipt"))?
                    .clone();
                task.detail(&format!("Retiring idle upload for {}", receipt.imported.photo.file_name));
                resolver
                    .background_transfer(&blob, cancel, || connection.abort_multipart(&blob, &upload_id, cancel))
                    .map_err(|e| error(e.to_string()))?;
                // If this atomic write fails, its old ID remains retryable; a confirmed
                // remote 404 is safe to reconcile on the next explicit cleanup.
                receipt.multipart.remove(&key);
                let _guard = resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
                receipt.write(root)?;
                aborted += 1;
            }
            task.progress(index.saturating_add(1) as u64, preview.len() as u64);
            results.push(json!({"import": receipt.id, "aborted": aborted, "originalPreserved": true}));
        }
    }
    Ok(json!({"applied": apply, "candidates": preview, "results": results, "remaining": remaining,
        "originalsPreserved": true, "workingCopiesPreserved": true}))
}

pub fn start(s: &mut Session, p: &Value) -> Result<Value> {
    start_impl(s, p, None)
}

pub fn observed(s: &mut Session, p: &Value, observer: &mut dyn FnMut(u64, u64, &str) -> bool) -> Result<Value> {
    s.execute_fn("cloud.cleanupUploads", |s| start_impl(s, p, Some(observer)))
}

fn start_impl(s: &mut Session, p: &Value, observer: Option<&mut dyn FnMut(u64, u64, &str) -> bool>) -> Result<Value> {
    let connection_id =
        p.get("connection").and_then(Value::as_str).filter(|id| identifier(id)).ok_or_else(|| error("specify a valid cleanup connection"))?;
    let retention = p
        .get("olderThanDays")
        .map(|value| value.as_u64().filter(|days| (1..=3650).contains(days)).ok_or_else(|| error("olderThanDays must be between 1 and 3650")))
        .transpose()?
        .unwrap_or(7);
    let boolean = |key: &str, default| {
        p.get(key)
            .map(|value| value.as_bool().ok_or_else(|| error(format!("{key} must be true or false"))))
            .transpose()
            .map(|value| value.unwrap_or(default))
    };
    let apply = boolean("apply", false)?;
    let wait = boolean("wait", !crate::assets::is_cached_only())?;
    if wait && crate::assets::is_cached_only() {
        return Err(error("desktop cleanup runs in the background; omit wait"));
    }
    if s.cloud_uploads.pending.len() >= 4 || s.cloud_uploads.pending.iter().any(|pending| pending.connections.contains(connection_id)) {
        return Err(error("this connection has active upload work; wait or cancel it before previewing or applying cleanup"));
    }
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("open a persistent native library first"))?;
    resolver.validate_storage().map_err(|e| error(e.to_string()))?;
    let root = resolver.durable_root().ok_or_else(|| error("this library has no durable managed storage"))?;
    let connection = s
        .library
        .as_mut()
        .ok_or_else(|| error("open a library first"))?
        .cloud_connections()?
        .into_iter()
        .find(|connection| connection.id == connection_id)
        .ok_or_else(|| error("configure the cleanup connection first"))?;
    let task = s.activity.start(
        "uploadCleanup",
        if apply { "Retire Idle Cloud Uploads" } else { "Preview Idle Cloud Uploads" },
        crate::activity::Cancel::Yes,
    );
    let id = task.id();
    let handle = task.handle();
    let cancel = task.cancel_flag().ok_or_else(|| error("cleanup cancellation unavailable"))?;
    let worker_cancel = cancel.clone();
    let worker_resolver = resolver.clone();
    let (sender, receiver) = mpsc::sync_channel(2);
    std::thread::Builder::new()
        .name("cloud-upload-cleanup".into())
        .spawn(move || {
            let _traffic = crate::cloud_traffic::Scope::operation(crate::cloud_traffic::Purpose::Maintenance, None, None, Some(id));
            let result = crate::guard::catch("cloud upload cleanup", || {
                run(&root, &connection, &worker_resolver, Duration::from_secs(retention * 86400), apply, &worker_cancel, &handle)
            })
            .unwrap_or_else(|message| Err(error(message)));
            let _ = sender.send(Event::Finished(result));
        })
        .map_err(io)?;
    s.cloud_uploads.pending.push(Pending {
        receiver,
        library: s.library_token(),
        connection_revision: resolver.connection_revision(),
        resolver,
        ids: BTreeMap::new(),
        verification_ids: BTreeSet::new(),
        item_outcomes: BTreeMap::new(),
        connections: BTreeSet::from([connection_id.to_owned()]),
        media_verified: BTreeSet::new(),
        maintenance: Some(Maintenance::Cleanup { connection: connection_id.to_owned() }),
        guard: task,
        cancel: cancel.clone(),
    });
    if wait { wait_task(s, id, cancel, Arc::new(AtomicU64::new(0)), observer) } else { Ok(json!({"background": true, "task": id})) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::assets::{AssetId, RemoteLocation};
    use lightcraft_catalog::{Photo, PhotoId};
    use std::sync::atomic::AtomicUsize;
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
        connection: S3Connection,
        receipt: Receipt,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("lightcraft-upload-cleanup-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let connection = S3Connection {
                id: "personal".into(),
                bucket: "photos".into(),
                region: "us-east-1".into(),
                prefix: "library".into(),
                endpoint: Some("http://127.0.0.1:9".into()),
                profile: None,
                keys: Default::default(),
                metadata: Default::default(),
            };
            let blob = |key: &str| Blob {
                id: sha256(key.as_bytes()),
                size: 1,
                remote: RemoteLocation { connection: connection.id.clone(), key: format!("library/{key}"), version: None },
            };
            let asset = Asset {
                associated: Default::default(),
                original: blob("original"),
                smart_preview: Some(blob("preview")),
                thumbnail: Some(blob("thumbnail")),
            };
            let asset_id = AssetId("asset-1".into());
            let receipt = Receipt {
                protocol: Default::default(),
                library: None,
                append_proof: Default::default(),
                publication_intent: Default::default(),
                version: 2,
                id: "import-1".into(),
                imported: Imported {
                    base_asset: None,
                    mutation_id: "import-1".into(),
                    photo_id: "import-1".into(),
                    asset_id: asset_id.clone(),
                    photo: Box::new(Photo::new(PhotoId(1), Source::Managed { asset_id }, "original.png", "PNG", 32, 24, "")),
                    asset: Box::new(asset),
                },
                storage: Some(connection.clone()),
                state: UploadState::SavedLocally,
                verified: BTreeSet::new(),
                multipart: BTreeMap::from([("library/original".into(), Multipart { id: "recorded-upload".into(), parts: BTreeMap::new() })]),
                generation: None,
                error: None,
            };
            receipt.write(&root).unwrap();
            Self { root, connection, receipt }
        }
        fn path(&self) -> PathBuf {
            self.root.join("uploads/import-1.json")
        }
        fn modified(&self, time: SystemTime) {
            std::fs::File::options().write(true).open(self.path()).unwrap().set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn cleanup_age_is_idle_time_and_future_timestamps_never_qualify() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(false);
        let retention = Duration::from_secs(7 * 86400);
        assert!(scan(&fixture.root, &fixture.connection, retention, &cancel).unwrap().0.is_empty());
        fixture.modified(SystemTime::now() - Duration::from_secs(8 * 86400));
        assert_eq!(scan(&fixture.root, &fixture.connection, retention, &cancel).unwrap().0.len(), 1);
        fixture.modified(SystemTime::now() + Duration::from_secs(86400));
        assert!(scan(&fixture.root, &fixture.connection, retention, &cancel).unwrap().0.is_empty());
    }

    #[test]
    fn legacy_future_corrupt_and_misnamed_receipts_are_preserved() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(false);
        let mut legacy = fixture.receipt.clone();
        legacy.version = 1;
        legacy.storage = None;
        legacy.write(&fixture.root).unwrap();
        let saved = std::fs::read(fixture.path()).unwrap();
        assert!(scan(&fixture.root, &fixture.connection, Duration::ZERO, &cancel).is_err());
        assert_eq!(std::fs::read(fixture.path()).unwrap(), saved);
        for bytes in [b"broken".to_vec(), serde_json::to_vec(&json!({"version":99})).unwrap()] {
            std::fs::write(fixture.path(), &bytes).unwrap();
            assert!(scan(&fixture.root, &fixture.connection, Duration::ZERO, &cancel).is_err());
            assert_eq!(std::fs::read(fixture.path()).unwrap(), bytes);
        }
        fixture.receipt.write(&fixture.root).unwrap();
        std::fs::rename(fixture.path(), fixture.root.join("uploads/wrong-id.json")).unwrap();
        assert!(scan(&fixture.root, &fixture.connection, Duration::ZERO, &cancel).is_err());
        assert!(fixture.root.join("uploads/wrong-id.json").is_file());
    }

    #[test]
    fn cleanup_reserves_its_connection_against_manual_and_automatic_uploads() {
        let fixture = Fixture::new();
        let mut session = Session::new().with_fs();
        session.open_library(fixture.root.join("library"), false).unwrap();
        session.execute("cloud.configure", &serde_json::to_value(&fixture.connection).unwrap()).unwrap();
        session.execute("cloud.syncEnable", &json!({"connection": "personal"})).unwrap();
        let asset_id = fixture.receipt.imported.asset_id.clone();
        session
            .apply_system(Op::Batch {
                ops: vec![
                    Op::SetAsset { id: asset_id.clone(), asset: Some(fixture.receipt.imported.asset.clone()) },
                    Op::AddPhoto { photo: fixture.receipt.imported.photo.clone() },
                    Op::SetUploadIntent {
                        id: fixture.receipt.id.clone(),
                        intent: Some(Box::new(UploadIntent {
                            asset_id,
                            photo_id: PhotoId(1),
                            state: UploadState::SavedLocally,
                            verified: BTreeSet::new(),
                            error: None,
                            finalization: None,
                            blocked: None,
                        })),
                    },
                ],
            })
            .unwrap();
        let (sender, receiver) = mpsc::sync_channel(2);
        let guard = session.activity.start("uploadCleanup", "Reserved cleanup", crate::activity::Cancel::Yes);
        let cancel = guard.cancel_flag().unwrap();
        let resolver = session.media.asset_resolver.clone().unwrap();
        session.cloud_uploads.pending.push(Pending {
            receiver,
            library: session.library_token(),
            connection_revision: resolver.connection_revision(),
            resolver,
            ids: BTreeMap::new(),
            verification_ids: BTreeSet::new(),
            item_outcomes: BTreeMap::new(),
            connections: BTreeSet::from([fixture.connection.id.clone()]),
            media_verified: BTreeSet::new(),
            maintenance: Some(Maintenance::Cleanup { connection: fixture.connection.id.clone() }),
            guard,
            cancel,
        });
        let result = session.execute("cloud.upload", &json!({"imports":[fixture.receipt.id],"wait":false}));
        assert!(result.unwrap_err().to_string().contains("cleanup"));
        assert!(
            session
                .execute("cloud.cleanupUploads", &json!({"connection":"personal","wait":false}))
                .unwrap_err()
                .to_string()
                .contains("active upload")
        );
        session.cloud_uploads.request_resume(true);
        session.resume_cloud_uploads();
        assert_eq!(session.cloud_uploads.running(), 1);
        assert!(session.cloud_uploads.attempted.is_empty());
        drop(sender);
        drop(session);
    }

    #[test]
    fn failed_receipt_retirement_keeps_the_old_identity_and_durable_work() {
        let fixture = Fixture::new();
        let saved = std::fs::read(fixture.path()).unwrap();
        std::fs::create_dir_all(fixture.root.join("blobs")).unwrap();
        std::fs::create_dir_all(fixture.root.join("working")).unwrap();
        let original = fixture.root.join("blobs").join(&fixture.receipt.imported.asset.original.id.0);
        let working = fixture.root.join("working/editor.tif");
        std::fs::write(&original, b"durable original").unwrap();
        std::fs::write(&working, b"unsaved editor work").unwrap();
        let mut retired = fixture.receipt.clone();
        retired.multipart.clear();
        let fault = lightcraft_catalog::safe_file::fail_writes_after(0);
        assert!(retired.write(&fixture.root).is_err());
        drop(fault);
        assert_eq!(std::fs::read(fixture.path()).unwrap(), saved);
        assert_eq!(std::fs::read(original).unwrap(), b"durable original");
        assert_eq!(std::fs::read(working).unwrap(), b"unsaved editor work");
        retired.write(&fixture.root).unwrap();
        assert!(read_receipt(&fixture.path()).unwrap().unwrap().multipart.is_empty());
    }
}
