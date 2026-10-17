//! Explicit whole-library erasure. Distinct from retention: this removes published history.
use super::*;
use crate::assets::s3::{ObjectVersion, PutCondition};
use crate::cloud_control::{Control, State};
pub const PHRASE: &str = "DELETE CLOUD LIBRARY";
fn asset_error(e: crate::assets::AssetError) -> EngineError {
    error(e.to_string())
}
fn owned_key(prefix: &str, library: &str, key: &str) -> bool {
    let Some(tail) = key.strip_prefix(prefix).and_then(|s| s.strip_prefix('/')) else {
        return false;
    };
    if let Some(rest) = tail.strip_prefix("sync-v2/") {
        return rest.split('/').next() == Some(library);
    }
    if retention::media_hash(prefix, key).is_some() {
        return true;
    }
    let hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'));
    if tail.strip_prefix("associated/xmp/").and_then(|s| s.strip_suffix(".xmp")).is_some_and(hash) {
        return true;
    }
    if tail == "catalog/head.json" {
        return true;
    }
    let parts: Vec<_> = tail.split('/').collect();
    match parts.as_slice() {
        ["catalog", kind, file] if matches!(*kind, "changes" | "checkpoints" | "receipts") => file.strip_suffix(".json").is_some_and(hash),
        ["catalog", "epochs", epoch, kind, file]
            if epoch.parse::<u64>().is_ok_and(|n| n >= 2) && matches!(*kind, "changes" | "checkpoints" | "receipts") =>
        {
            file.strip_suffix(".json").is_some_and(hash)
        }
        _ => false,
    }
}
pub fn status(s: &Session) -> Option<Value> {
    let state = s.catalog.sync_state()?;
    let root = s.media.asset_resolver.as_ref()?.durable_root()?;
    let path = root.join(format!("erased-library-{}.json", sha256(state.library.as_bytes()).0));
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        _ => {}
    }
    let complete = (|| -> Option<bool> {
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.file_type().is_file() || meta.len() > 2048 {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path).ok()?.take(2049).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 2048 {
            return None;
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        Some(value["library"] == state.library && value["complete"] == true)
    })()
    .unwrap_or(false);
    Some(json!({"library":state.library,"complete":complete}))
}
pub(crate) fn ensure_not_erased(s: &Session) -> Result<()> {
    let Some(state) = s.catalog.sync_state() else {
        return Ok(());
    };
    let Some(resolver) = s.media.asset_resolver.as_ref() else {
        return Ok(());
    };
    let Some(root) = resolver.durable_root() else {
        return Ok(());
    };
    let path = root.join(format!("erased-library-{}.json", sha256(state.library.as_bytes()).0));
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(error(
            "Deletion of this cloud library has begun. Synchronization and uploads are disabled; review library deletion to finish. Use a new library and storage folder to sync again.",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(e)),
    }
}
struct Target<'a> {
    connection: &'a S3Connection,
    library: &'a str,
}
type DeletionReview = (Value, Vec<ObjectVersion>, Vec<(String, String)>);
fn inventory(connection: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<Vec<ObjectVersion>> {
    let versions = connection.library_versions(cancel).map_err(asset_error)?;
    let root = format!("{}/", connection.prefix);
    for entry in &versions {
        if !owned_key(&connection.prefix, library, &entry.key) {
            return Err(error("storage folder contains another library or unrelated objects; choose a dedicated library folder; nothing erased"));
        }
        if entry.version == "null" && entry.etag.is_none() {
            return Err(error("storage has a null-version delete marker; resolve suspended-versioning markers with the provider before erasing"));
        }
    }
    // A provider must expose the live objects in its version inventory as well.
    let mut request = crate::cloud_append::listing::ListRequest { prefix: root, delimiter: false, continuation: None, start_after: None };
    let live: BTreeSet<_> = versions.iter().filter(|v| v.latest && v.etag.is_some()).map(|v| v.key.as_str()).collect();
    let mut seen = BTreeSet::new();
    let mut listed = BTreeSet::new();
    for _ in 0..256 {
        let page = connection.list_objects_v2(&request, cancel).map_err(asset_error)?;
        if page.keys.iter().any(|k| !live.contains(k.as_str())) {
            return Err(error("storage version listing omitted live objects; nothing erased"));
        }
        listed.extend(page.keys);
        let Some(next) = page.next else {
            if listed.iter().map(String::as_str).collect::<BTreeSet<_>>() != live {
                return Err(error("live and historical storage inventories disagree; nothing erased"));
            }
            return Ok(versions);
        };
        if !seen.insert(next.clone()) {
            return Err(error("repeated storage page; nothing erased"));
        }
        request.continuation = Some(next);
    }
    Err(error("storage listing exceeded its page budget; nothing erased"))
}
fn head(connection: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<Option<(crate::cloud_records::Head, String)>> {
    let key = format!("{}/catalog/head.json", connection.prefix);
    let Some(reply) = connection.get_small(&key, 16 << 10, cancel).map_err(asset_error)? else { return Ok(None) };
    let head = crate::cloud_records::Head::decode(&reply.bytes).map_err(|blocked| error(blocked.to_string()))?;
    if head.library.as_deref() != Some(library) || head.control().map_err(error)?.library != library {
        return Err(error("storage head belongs to another library; nothing erased"));
    }
    if head.control().map_err(error)?.state != State::Deleted {
        head.validate_scope(&connection.prefix, library).map_err(error)?;
    }
    Ok(Some((head, reply.etag.ok_or_else(|| error("storage head has no concurrency token"))?)))
}
fn control(connection: &S3Connection, library: &str, versions: &[ObjectVersion], cancel: &AtomicBool) -> Result<Control> {
    let prefix = crate::cloud_append::control::prefix(&connection.prefix, library).map_err(asset_error)?;
    let keys: BTreeSet<_> = versions.iter().filter(|v| v.latest && v.etag.is_some() && v.key.starts_with(&prefix)).map(|v| v.key.clone()).collect();
    let mut records = Vec::new();
    for key in crate::cloud_append::control::latest(&keys) {
        let bytes = connection
            .get_small(&key, 16 << 10, cancel)
            .map_err(asset_error)?
            .ok_or_else(|| error("library control changed; review deletion again"))?
            .bytes;
        records.push((key, bytes));
    }
    crate::cloud_append::control::effective(&connection.prefix, library, &records).map_err(|b| error(b.to_string()))
}
fn preview(
    connection: &S3Connection,
    library: &str,
    protocol: lightcraft_catalog::replication::SyncProtocol,
    cancel: &AtomicBool,
) -> Result<DeletionReview> {
    let versions = inventory(connection, library, cancel)?;
    let current_head = head(connection, library, cancel)?;
    if protocol.is_head() && current_head.is_none() {
        return Err(error("the selected cloud library has not been created yet; there is no remote library to erase"));
    }
    if !protocol.is_head() && !versions.iter().any(|v| v.key.starts_with(&format!("{}/sync-v2/{library}/", connection.prefix))) {
        return Err(error("the selected cloud library was not found; nothing erased"));
    }
    if let Some((head, _)) = &current_head {
        head.control().map_err(error)?.delete().map_err(|b| error(b.to_string()))?;
    }
    if versions.iter().any(|v| v.key.starts_with(&format!("{}/sync-v2/{library}/", connection.prefix))) {
        control(connection, library, &versions, cancel)?.delete().map_err(|b| error(b.to_string()))?;
    }
    let uploads = connection.library_multipart_uploads(cancel).map_err(asset_error)?;
    if uploads.iter().any(|(key, _)| !owned_key(&connection.prefix, library, key)) {
        return Err(error("multipart inventory contains unrelated data; nothing erased"));
    }
    let binding = json!({"destination":connection.storage_snapshot(),"library":library,"versions":versions,"uploads":uploads});
    let token = sha256(&serde_json::to_vec(&binding).map_err(|e| error(e.to_string()))?).0;
    let bytes = versions.iter().fold(0u64, |n, v| n.saturating_add(v.size));
    Ok((
        json!({"preview":token,"library":library,"bucket":connection.bucket,"prefix":connection.prefix,"versions":versions.len(),"bytes":bytes,"multipartUploads":uploads.len(),"applied":false}),
        versions,
        uploads,
    ))
}
fn erase(
    target: Target<'_>,
    before: &[ObjectVersion],
    uploads: &[(String, String)],
    cancel: &AtomicBool,
    task: &TaskHandle,
    resolver: &AssetResolver,
    revision: u64,
) -> Result<Value> {
    let Target { connection, library } = target;
    let root = resolver.durable_root().ok_or_else(|| error("no durable library storage for deletion intent"))?;
    resolver.validate_storage().map_err(asset_error)?;
    let path = root.join(format!("erased-library-{}.json", sha256(library.as_bytes()).0));
    lightcraft_catalog::safe_file::write_atomic(
        &path,
        &serde_json::to_vec(&json!({"library":library,"deletionStarted":true})).map_err(|e| error(e.to_string()))?,
    )
    .map_err(io)?;
    let mut barriers = BTreeMap::<String, Vec<u8>>::new();
    if let Some((mut head, etag)) = head(connection, library, cancel)? {
        head.control = Some(head.control().map_err(error)?.delete().map_err(|b| error(b.to_string()))?);
        head.version = crate::cloud_records::HEAD_VERSION;
        // Retain no checkpoint or recipe data in the permanent deletion barrier.
        head.checkpoint = None;
        // The terminal head contains no references derived from erased personal history.
        let zero = "0".repeat(64);
        head.generation = 1;
        head.tip = crate::cloud_records::RecordRef {
            key: crate::cloud_records::object_key(&connection.prefix, head.control().map_err(error)?.epoch, "changes", &zero),
            hash: lightcraft_catalog::assets::BlobId(zero),
            size: 1,
        };
        let bytes = serde_json::to_vec(&head).map_err(|e| error(e.to_string()))?;
        let key = format!("{}/catalog/head.json", connection.prefix);
        if connection.get_small(&key, 16 << 10, cancel).map_err(asset_error)?.is_none_or(|r| r.bytes != bytes) {
            connection.put_small(&key, &bytes, PutCondition::Matches(&etag), cancel).map_err(asset_error)?;
        }
        barriers.insert(key, bytes);
    }
    if before.iter().any(|v| v.key.starts_with(&format!("{}/sync-v2/{library}/", connection.prefix))) {
        let control = control(connection, library, before, cancel)?.delete().map_err(|b| error(b.to_string()))?;
        let key = crate::cloud_append::control::key(&connection.prefix, &control).map_err(asset_error)?;
        let bytes = crate::cloud_append::control::encode(&control).map_err(asset_error)?;
        connection.put_immutable_verified(&key, &bytes, cancel).map_err(asset_error)?;
        barriers.insert(key, bytes);
    }
    for (key, id) in uploads {
        if resolver.connection_revision() != revision || cancel.load(Ordering::Relaxed) {
            return Err(error("library deletion interrupted; review again to finish"));
        }
        connection.erase_multipart_upload(key, id, cancel).map_err(asset_error)?;
    }
    if !connection.library_multipart_uploads(cancel).map_err(asset_error)?.is_empty() {
        return Err(error("multipart uploads remain; keep all devices stopped and review deletion again"));
    }
    let planned = inventory(connection, library, cancel)?;
    let reviewed: BTreeMap<_, _> = before.iter().map(|v| ((v.key.as_str(), v.version.as_str()), (v.etag.as_ref(), v.size))).collect();
    if planned
        .iter()
        .any(|v| !barriers.contains_key(&v.key) && reviewed.get(&(v.key.as_str(), v.version.as_str())) != Some(&(v.etag.as_ref(), v.size)))
    {
        return Err(error("storage changed while installing the deletion barrier; no media deleted; keep all devices stopped and review again"));
    }
    for (key, bytes) in &barriers {
        if connection.get_small(key, 16 << 10, cancel).map_err(asset_error)?.is_none_or(|r| r.bytes != *bytes) {
            return Err(error("deletion barrier changed; another device may still be writing; review again"));
        }
    }
    let mut removed = 0u64;
    for object in &planned {
        if barriers.contains_key(&object.key) && object.latest {
            continue;
        }
        if cancel.load(Ordering::Relaxed) || resolver.connection_revision() != revision {
            return Err(error("library erasure interrupted; some data may already be gone; keep every device stopped and review again to finish"));
        }
        resolver.validate_storage().map_err(asset_error)?;
        task.detail("Permanently deleting cloud library objects and historical versions");
        connection.erase_version(object, cancel).map_err(|e| {
            error(format!("library erasure incomplete: {e}; some data may already be gone; keep every device stopped and review again"))
        })?;
        removed = removed.saturating_add(1);
        task.progress(removed, planned.len() as u64);
    }
    for (key, bytes) in &barriers {
        if connection.get_small(key, 16 << 10, cancel).map_err(asset_error)?.is_none_or(|r| r.bytes != *bytes) {
            return Err(error("deletion barrier changed; another writer may still be active; erasure is incomplete"));
        }
    }
    let remaining = inventory(connection, library, cancel)?;
    if remaining.iter().any(|v| !v.latest || !barriers.contains_key(&v.key)) {
        return Err(error("cloud objects remain or a writer is still active; keep devices stopped and review deletion again"));
    }
    if !connection.library_multipart_uploads(cancel).map_err(asset_error)?.is_empty() {
        return Err(error("another upload appeared during deletion; keep all devices stopped and review again"));
    }
    lightcraft_catalog::safe_file::write_atomic(
        &path,
        &serde_json::to_vec(&json!({"library":library,"deletionStarted":true,"complete":true})).map_err(|e| error(e.to_string()))?,
    )
    .map_err(io)?;
    Ok(json!({"applied":true,"library":library,"deletedVersions":removed,"deletionMarkers":barriers.len(),"localCopiesRetained":true}))
}

pub fn start(s: &mut Session, p: &Value) -> Result<Value> {
    let apply = match p.get("apply") {
        None => false,
        Some(Value::Bool(v)) => *v,
        _ => return Err(error("apply must be true or false")),
    };
    let expected = p["preview"].as_str().unwrap_or_default().to_owned();
    if apply && (p["confirmation"].as_str() != Some(PHRASE) || p["allDevicesStopped"] != true || expected.len() != 64) {
        return Err(error("type DELETE CLOUD LIBRARY exactly, confirm all other devices are stopped, and supply the reviewed preview token"));
    }
    let state = s.catalog.sync_state().ok_or_else(|| error("enable or join the cloud library before reviewing deletion"))?;
    let library = state.library.clone();
    let id = state.connection.clone();
    let protocol = state.protocol;
    if p["library"].as_str() != Some(library.as_str()) {
        return Err(error("the selected library changed; review deletion again"));
    }
    if !s.cloud_paused
        || s.cloud_replication_jobs.running()
        || s.cloud_imports.preparing() > 0
        || s.cloud_uploads.running() > 0
        || s.cache_move.is_some()
        || s.interaction.is_some()
        || s.unsaved().is_some()
    {
        return Err(error("pause this device and wait for all transfers, imports and edits to finish before library deletion"));
    }
    let wait = match p.get("wait") {
        None => !crate::assets::is_cached_only(),
        Some(Value::Bool(v)) => *v,
        _ => return Err(error("wait must be true or false")),
    };
    if wait && crate::assets::is_cached_only() {
        return Err(error("desktop library deletion must run in the background"));
    }
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("open a persistent native library first"))?;
    resolver.validate_storage().map_err(asset_error)?;
    let connection = s
        .library
        .as_mut()
        .ok_or_else(|| error("open a library first"))?
        .cloud_connections()?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| error("configure this library's connection first"))?
        .canonicalized();
    if connection.prefix.is_empty() {
        return Err(error("library deletion requires a dedicated nonempty storage folder; bucket-wide deletion is refused"));
    }
    let coordinator = s.cloud_replication_jobs.coordinator.clone();
    let revision = resolver.connection_revision();
    let task = s.activity.start(
        "uploadCleanup",
        if apply { "Delete Entire Cloud Library" } else { "Review Cloud Library Deletion" },
        crate::activity::Cancel::Yes,
    );
    let task_id = task.id();
    let handle = task.handle();
    let cancel = task.cancel_flag().ok_or_else(|| error("library deletion cancellation unavailable"))?;
    let worker_cancel = cancel.clone();
    let worker_resolver = resolver.clone();
    let (sender, receiver) = mpsc::sync_channel(2);
    std::thread::Builder::new()
        .name("cloud-library-erase".into())
        .spawn(move || {
            let result = crate::guard::catch("cloud library erasure", || {
                let _permit = coordinator.acquire(&worker_cancel)?;
                let _storage = worker_resolver.durable_guard(&worker_cancel).map_err(asset_error)?;
                let (review, versions, multipart) = preview(&connection, &library, protocol, &worker_cancel)?;
                if !apply {
                    return Ok(review);
                }
                if review["preview"] != expected {
                    return Err(error("cloud library changed since review; review it again; nothing erased"));
                }
                if worker_resolver.connection_revision() != revision || worker_cancel.load(Ordering::Relaxed) {
                    return Err(error("destination changed or review cancelled; nothing erased"));
                }
                erase(
                    Target { connection: &connection, library: &library },
                    &versions,
                    &multipart,
                    &worker_cancel,
                    &handle,
                    &worker_resolver,
                    revision,
                )
                .map_err(|e| {
                    error(format!("Library deletion incomplete; some data may already be gone. Keep all devices stopped and review again. {e}"))
                })
            })
            .unwrap_or_else(|message| Err(error(message)));
            let _ = sender.send(Event::Finished(result));
        })
        .map_err(io)?;
    s.cloud_uploads.pending.push(Pending {
        receiver,
        library: s.library_token(),
        resolver,
        connection_revision: revision,
        ids: BTreeMap::new(),
        verification_ids: BTreeSet::new(),
        item_outcomes: BTreeMap::new(),
        connections: BTreeSet::from([id.clone()]),
        maintenance: Some(Maintenance::Erase { connection: id, apply }),
        media_verified: BTreeSet::new(),
        guard: task,
        cancel: cancel.clone(),
    });
    if wait { wait_task(s, task_id, cancel, Arc::new(AtomicU64::new(0)), None) } else { Ok(json!({"background":true,"task":task_id})) }
}
