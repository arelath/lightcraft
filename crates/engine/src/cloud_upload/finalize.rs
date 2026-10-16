//! Local receipt completion consumes durable shared ACKs; it never transfers media.
use super::*;

const RETRY: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(super) struct Ready {
    pub(super) id: String,
    pub(super) intent: UploadIntent,
    global: String,
    asset: Asset,
    connection: S3Connection,
    proof: UploadFinalization,
}

fn ready(s: &mut Session, ids: &BTreeSet<String>) -> Result<Vec<Ready>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > 32 {
        return Err(error(
            "finish queued receipts before removing more than 32 imports at once; resume local finalization or select a smaller batch; staged data retained",
        ));
    }
    let connections = s.library.as_mut().ok_or_else(|| error("open a persistent library to finish upload receipts"))?.cloud_connections()?;
    ids.iter()
        .map(|id| {
            let intent = s.catalog.upload_intents().get(id).ok_or_else(|| error("upload intent disappeared; staged data retained"))?;
            let proof = intent.finalization.clone().ok_or_else(|| error("upload receipt has no durable finalization proof"))?;
            let asset = s.catalog.asset(&intent.asset_id).ok_or_else(|| error("upload asset disappeared; staged data retained"))?.clone();
            intent.validate(&asset).map_err(error)?;
            let global = s
                .catalog
                .cloud_identity(&format!("photo:{}", intent.photo_id.0))
                .ok_or_else(|| error("upload photo identity is unavailable; staged data retained"))?
                .to_owned();
            let connection = connections
                .iter()
                .find(|connection| connection.id == asset.original.remote.connection)
                .ok_or_else(|| error("restore the upload's connection settings to finish its local receipt"))?
                .clone();
            Ok(Ready { id: id.clone(), intent: intent.clone(), global, asset, connection, proof })
        })
        .collect()
}

fn write(root: &Path, item: &Ready) -> Result<()> {
    write_with(root, item, |receipt, root| receipt.write(root))
}

fn write_with(root: &Path, item: &Ready, save: impl FnOnce(&Receipt, &Path) -> Result<()>) -> Result<()> {
    let dir = root.join("uploads");
    let meta = std::fs::symlink_metadata(&dir).map_err(io)?;
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        return Err(error("upload receipt directory is redirected or invalid; staged data retained"));
    }
    let path = dir.join(format!("{}.json", item.id));
    let meta = std::fs::symlink_metadata(&path).map_err(io)?;
    if !meta.file_type().is_file() || meta.file_type().is_symlink() {
        return Err(error("upload receipt is not a regular file; staged data retained"));
    }
    let mut receipt = read_receipt(&path)?.ok_or_else(|| error("upload receipt disappeared; staged data retained"))?;
    if receipt.id != item.id
        || receipt.imported.asset_id != item.intent.asset_id
        || *receipt.imported.asset != item.asset
        || receipt.imported.photo.id != item.intent.photo_id
        || receipt.imported.photo_id != item.global
        || receipt.protocol != item.proof.protocol
        || receipt.library.as_deref() != Some(item.proof.library.as_str())
        || receipt.storage.as_ref().is_none_or(|storage| !storage.same_destination(&item.connection))
        || !receipt.multipart.is_empty()
    {
        return Err(error(
            "upload receipt binding differs from its durable publication proof; restore the original receipt and connection; staged data retained",
        ));
    }
    for (id, digest) in &item.proof.proofs {
        if receipt.publication_intent.get(id).is_some_and(|expected| expected != digest)
            || receipt.append_proof.get(id).is_some_and(|expected| expected != digest)
        {
            return Err(error("upload receipt mutation proof differs; staged data retained"));
        }
    }
    let previous = receipt.clone();
    receipt.append_proof.extend(item.proof.proofs.clone());
    if let Some(generation) = item.proof.generation {
        receipt.generation = Some(receipt.generation.unwrap_or(0).max(generation));
    }
    receipt.verified = item.intent.verified.clone();
    receipt.state = UploadState::Synchronized;
    receipt.error = None;
    if receipt != previous {
        save(&receipt, root)?;
    }
    Ok(())
}

pub(super) fn run(items: &[Ready], resolver: &AssetResolver, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) -> Result<()> {
    let root = resolver.durable_root().ok_or_else(|| error("this library has no durable managed storage"))?;
    for item in items {
        {
            let _guard = resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
            resolver.validate_storage().map_err(|e| error(e.to_string()))?;
            write(&root, item)?;
        }
        // Never hold the staging lock while awaiting the owner's journal write.
        acknowledge(&item.id, item.proof.clone(), events, cancel)?;
    }
    Ok(())
}

pub(super) fn acknowledge(id: &str, proof: UploadFinalization, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) -> Result<()> {
    let (reply, response) = mpsc::sync_channel(1);
    events.send(Event::Finalized { id: id.into(), proof, reply }).map_err(|_| error("receipt owner closed; durable proof retained"))?;
    loop {
        match response.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => {
                result?;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(error("receipt owner closed; durable proof retained")),
            Err(_) if cancel.load(Ordering::Relaxed) => return Err(error("receipt finalization cancelled; durable proof retained")),
            Err(_) => {}
        }
    }
    Ok(())
}

pub(super) fn for_removal(s: &mut Session, blobs: &[Blob]) -> Result<Vec<Ready>> {
    let keys: BTreeSet<_> = blobs.iter().map(|blob| &blob.id).collect();
    let ids = s
        .catalog
        .upload_intents()
        .iter()
        .filter(|(_, intent)| {
            intent.finalization.is_some() && s.catalog.asset(&intent.asset_id).is_some_and(|asset| asset.blobs().any(|blob| keys.contains(&blob.id)))
        })
        .map(|(id, _)| id.clone())
        .collect();
    ready(s, &ids)
}

pub(super) fn schedule(s: &mut Session) {
    if s.cloud_uploads.pending.len() >= 4 || s.library.is_none() || s.media.asset_resolver.is_none() {
        return;
    }
    let now = web_time::Instant::now();
    if s.cloud_uploads.finalization_revision != Some(s.catalog.revision) {
        s.cloud_uploads.finalization_candidates =
            s.catalog.upload_intents().iter().filter(|(_, intent)| intent.finalization.is_some()).map(|(id, _)| id.clone()).collect();
        s.cloud_uploads.finalization_revision = Some(s.catalog.revision);
        s.cloud_uploads.finalization_retry.retain(|id, _| s.cloud_uploads.finalization_candidates.contains(id));
    }
    let Some(id) = s
        .cloud_uploads
        .finalization_candidates
        .iter()
        .find(|id| {
            s.catalog.upload_intents().get(*id).is_some_and(|intent| intent.finalization.is_some())
                && !s.cloud_uploads.pending.iter().any(|pending| pending.ids.contains_key(*id))
                && s.cloud_uploads.finalization_retry.get(*id).is_none_or(|deadline| now >= *deadline)
        })
        .cloned()
    else {
        return;
    };
    s.cloud_uploads.finalization_retry.insert(id.clone(), now + RETRY);
    if let Err(reason) = start(s, &id) {
        record_error(s, &id, &reason.to_string());
    }
}

fn start(s: &mut Session, id: &str) -> Result<()> {
    s.persist()?; // A queued proof must be durable before any sidecar write.
    let items = ready(s, &BTreeSet::from([id.to_owned()]))?;
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("open a persistent native library first"))?;
    let task = s.activity.start("upload", "Finalize Uploaded Receipt", crate::activity::Cancel::Yes);
    let cancel = task.cancel_flag().ok_or_else(|| error("receipt cancellation unavailable"))?;
    let (sender, receiver) = mpsc::sync_channel(2);
    let ids = items.iter().map(|item| (item.id.clone(), item.intent.asset_id.clone())).collect();
    let worker_resolver = resolver.clone();
    let worker_cancel = cancel.clone();
    std::thread::Builder::new()
        .name("cloud-receipt".into())
        .spawn(move || {
            let result = crate::guard::catch("cloud receipt finalization", || {
                run(&items, &worker_resolver, &sender, &worker_cancel)?;
                Ok(json!({"receiptFinalized":true,"imports":items.iter().map(|item| &item.id).collect::<Vec<_>>(),"transferredBytes":0}))
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
        ids,
        verification_ids: BTreeSet::new(),
        item_outcomes: BTreeMap::new(),
        connections: BTreeSet::new(),
        media_verified: BTreeSet::new(),
        maintenance: Some(Maintenance::Receipt),
        guard: task,
        cancel,
    });
    Ok(())
}

pub(super) fn record_error(s: &mut Session, id: &str, message: &str) {
    let Some(previous) = s.catalog.upload_intents().get(id).filter(|intent| intent.finalization.is_some()).cloned() else { return };
    let mut intent = previous;
    let mut message =
        format!("Local receipt finalization: {message}. Staged data retained; retry after restoring local storage or receipt settings.")
            .replace('\0', "�");
    if message.len() > 4096 {
        let mut end = 4096;
        while !message.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        message.truncate(end);
    }
    if intent.error.as_deref() == Some(message.as_str()) {
        return;
    }
    intent.error = Some(message);
    let result = s.apply_system(Op::SetUploadIntent { id: id.into(), intent: Some(Box::new(intent)) }).and_then(|_| s.persist());
    if let Err(reason) = result {
        log::warn!("receipt finalization error could not be saved: {reason}; proof remains queued");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryStores;
    use lightcraft_catalog::replication::SyncProtocol;
    use lightcraft_catalog::{FsStore, Store};

    struct FailingJournal {
        inner: FsStore,
        fail: Arc<AtomicBool>,
    }
    impl Store for FailingJournal {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.inner.read(name)
        }
        fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.inner.write_atomic(name, bytes)
        }
        fn append(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("injected full disk during receipt acknowledgement"));
            }
            self.inner.append(name, bytes)
        }
        fn truncate(&mut self, name: &str, length: u64) -> std::io::Result<()> {
            self.inner.truncate(name, length)
        }
        fn describe(&self) -> String {
            "receipt journal fault".into()
        }
    }

    fn fixture(protocol: SyncProtocol) -> (Session, PathBuf, Ready, Receipt) {
        let root = std::env::temp_dir().join(format!(
            "lightcraft-receipt-finalize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 100, 255]);
        let source = root.join("source.png");
        std::fs::write(&source, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap())
            .unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(root.join("library"), false).unwrap();
        s.execute(
            "cloud.configure",
            &json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9","profile":"disabled"}),
        )
        .unwrap();
        s.execute("cloud.syncEnable", &json!({"connection":"personal","protocol":protocol})).unwrap();
        s.execute("cloud.import", &json!({"connection":"personal","paths":[source]})).unwrap();
        let id = s.catalog.upload_intents().keys().next().unwrap().clone();
        let mut intent = s.catalog.upload_intents()[&id].clone();
        let asset = s.catalog.asset(&intent.asset_id).unwrap().clone();
        intent.state = UploadState::ObjectsVerified;
        intent.verified = asset.blobs().map(|blob| blob.remote.key.clone()).collect();
        s.apply_system(Op::SetUploadIntent { id: id.clone(), intent: Some(Box::new(intent.clone())) }).unwrap();
        let state = s.catalog.sync_state().unwrap().clone();
        let ids = state.pending.keys().cloned().collect();
        let proofs = state
            .pending
            .iter()
            .filter(|(_, mutation)| {
                mutation.changes.iter().any(|change| {
                    change.field == "content" && change.value.get("assetId").and_then(Value::as_str) == Some(intent.asset_id.0.as_str())
                })
            })
            .map(|(id, mutation)| (id.clone(), mutation.digest().unwrap()))
            .collect();
        let proof = UploadFinalization { version: 1, protocol, library: state.library.clone(), generation: protocol.is_head().then_some(11), proofs };
        let receipt = Receipt {
            version: if protocol.is_head() { 4 } else { 3 },
            protocol,
            library: Some(state.library.clone()),
            append_proof: BTreeMap::new(),
            publication_intent: proof.proofs.clone(),
            id: id.clone(),
            imported: Imported {
                mutation_id: id.clone(),
                photo_id: s.catalog.cloud_identity(&format!("photo:{}", intent.photo_id.0)).unwrap().into(),
                asset_id: intent.asset_id.clone(),
                photo: Box::new(s.catalog.photo(intent.photo_id).unwrap().as_ref().clone()),
                asset: Box::new(asset.clone()),
                base_asset: None,
            },
            storage: Some(s.library.as_mut().unwrap().cloud_connections().unwrap()[0].storage_snapshot()),
            state: UploadState::ObjectsVerified,
            verified: intent.verified.clone(),
            multipart: BTreeMap::new(),
            generation: None,
            error: None,
        };
        receipt.write(&root.join("library/Managed")).unwrap();
        let op = if protocol.is_head() {
            crate::cloud_sync::incoming_checkpoint(&s.catalog, &state.portable(), &ids, "a".repeat(64), 11).unwrap()
        } else {
            let remote = state.portable();
            let progress = lightcraft_catalog::replication::AppendProgress {
                version: 1,
                manifest: "a".repeat(64),
                frontiers: lightcraft_catalog::replication::publication_prefixes(&remote).unwrap(),
                remote,
                scan: Default::default(),
                gaps: BTreeSet::new(),
            };
            crate::cloud_sync::incoming_append(&s.catalog, progress, &ids).unwrap()
        };
        s.apply_received(op).unwrap();
        s.persist().unwrap();
        assert_eq!(s.catalog.upload_intents()[&id].finalization.as_ref(), Some(&proof));
        let items = ready(&mut s, &BTreeSet::from([id])).unwrap();
        (s, root, items.into_iter().next().unwrap(), receipt)
    }

    fn drain(s: &mut Session) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            s.poll_cloud_uploads();
            if s.cloud_uploads.pending.is_empty() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "receipt worker deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn cleanup(root: PathBuf) {
        let resolved = std::fs::canonicalize(&root).unwrap();
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        assert!(resolved.starts_with(temp));
        assert!(root.file_name().unwrap().to_string_lossy().starts_with("lightcraft-receipt-finalize-"));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::remove_dir_all(&root) {
                Ok(()) => break,
                Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => break,
                Err(reason) => {
                    assert!(std::time::Instant::now() < deadline, "cache worker cleanup: {reason}");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    #[test]
    fn queued_shared_proof_survives_local_failure_restart_and_pause_without_media_transfer() {
        for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
            let (mut s, root, item, receipt) = fixture(protocol);
            let managed = root.join("library/Managed");
            let path = managed.join("uploads").join(format!("{}.json", item.id));
            let bytes = std::fs::read(&path).unwrap();
            let resolver = s.media.asset_resolver.clone().unwrap();
            resolver.prepare_cache_removal(&AtomicBool::new(false)).unwrap();
            drop(resolver);
            assert!(managed.join("blobs").join(&item.asset.original.id.0).is_file(), "queued proof is not disposable staging");
            assert!(write_with(&managed, &item, |_, _| Err(error("injected full disk during receipt write"))).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(s.catalog.upload_intents()[&item.id].finalization.as_ref(), Some(&item.proof));
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            s.cloud_paused = true;
            drain(&mut s);
            assert!(s.catalog.upload_intents()[&item.id].error.as_deref().unwrap().contains("Local receipt finalization"));
            assert_eq!(s.catalog.upload_intents()[&item.id].finalization.as_ref(), Some(&item.proof));
            assert_eq!(s.catalog.upload_intents()[&item.id].state, UploadState::Synchronized);
            s.close_library().unwrap();
            drop(s);
            std::fs::remove_dir(&path).unwrap();
            receipt.write(&managed).unwrap();
            let mut s = Session::new().with_fs();
            s.open_library(root.join("library"), false).unwrap();
            s.cloud_paused = true;
            drain(&mut s);
            assert!(s.catalog.upload_intents()[&item.id].finalization.is_none());
            assert!(s.catalog.upload_intents()[&item.id].error.is_none());
            assert!(s.catalog.sync_state().unwrap().pending.is_empty());
            let finalized = read_receipt(&path).unwrap().unwrap();
            assert_eq!(finalized.state, UploadState::Synchronized);
            assert_eq!(finalized.append_proof, item.proof.proofs);
            assert_eq!(finalized.generation, item.proof.generation);
            assert!(
                s.cloud_uploads
                    .reports
                    .iter()
                    .any(|report| report["result"]["receiptFinalized"] == true && report["result"]["transferredBytes"] == 0)
            );
            let reports = s.cloud_uploads.reports.clone();
            s.poll_cloud_uploads();
            assert_eq!(s.cloud_uploads.reports, reports, "completed queue does not create another job");
            s.close_library().unwrap();
            drop(s);
            cleanup(root);
        }
    }

    #[test]
    fn receipt_binding_and_mutation_mismatches_retain_the_durable_queue() {
        let (mut s, root, item, original) = fixture(SyncProtocol::ConditionalHead);
        let managed = root.join("library/Managed");
        for mismatch in 0..4 {
            let mut receipt = original.clone();
            match mismatch {
                0 => receipt.library = Some("another-library".into()),
                1 => receipt.storage.as_mut().unwrap().endpoint = Some("http://127.0.0.1:19".into()),
                2 => receipt.publication_intent.values_mut().for_each(|digest| *digest = "b".repeat(64)),
                _ => receipt.imported.photo.id.0 += 1,
            }
            receipt.write(&managed).unwrap();
            assert!(write(&managed, &item).is_err());
            assert_eq!(s.catalog.upload_intents()[&item.id].finalization.as_ref(), Some(&item.proof));
        }
        s.close_library().unwrap();
        drop(s);
        cleanup(root);
    }

    #[test]
    fn written_sidecar_cannot_release_staging_until_queue_clear_is_durable() {
        for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
            let (mut initial, root, item, _) = fixture(protocol);
            initial.close_library().unwrap();
            drop(initial);
            let dir = root.join("library");
            let fail = Arc::new(AtomicBool::new(false));
            let mut s = Session::new().with_fs();
            s.open_library_in(
                LibraryStores {
                    dir: dir.clone(),
                    on_disk: true,
                    catalog: Box::new(FailingJournal { inner: FsStore::open(&dir).unwrap(), fail: fail.clone() }),
                    files: Box::new(FsStore::open(&dir).unwrap()),
                },
                false,
            )
            .unwrap();
            s.cloud_paused = true;
            fail.store(true, Ordering::Relaxed);
            drain(&mut s);
            let managed = dir.join("Managed");
            let path = managed.join("uploads").join(format!("{}.json", item.id));
            assert_eq!(read_receipt(&path).unwrap().unwrap().state, UploadState::Synchronized);
            assert_eq!(s.catalog.upload_intents()[&item.id].finalization.as_ref(), Some(&item.proof));
            s.media.asset_resolver.as_ref().unwrap().prepare_cache_removal(&AtomicBool::new(false)).unwrap();
            assert!(managed.join("blobs").join(&item.asset.original.id.0).is_file());
            drop(s); // Crash with the failed queue-clear operations still unlogged.
            let mut restarted = Session::new().with_fs();
            restarted.open_library(&dir, false).unwrap();
            assert_eq!(restarted.catalog.upload_intents()[&item.id].finalization.as_ref(), Some(&item.proof));
            restarted.cloud_paused = true;
            drain(&mut restarted);
            assert!(restarted.catalog.upload_intents()[&item.id].finalization.is_none());
            assert!(restarted.catalog.sync_state().unwrap().pending.is_empty());
            restarted.close_library().unwrap();
            drop(restarted);
            cleanup(root);
        }
    }

    #[test]
    fn stale_library_or_connection_receipt_results_do_not_change_current_queue_or_error() {
        for wrong_library in [true, false] {
            let (mut s, root, item, _) = fixture(SyncProtocol::ConditionalHead);
            let resolver = s.media.asset_resolver.clone().unwrap();
            let guard = s.activity.start("upload", "Held stale receipt", crate::activity::Cancel::Yes);
            let cancel = guard.cancel_flag().unwrap();
            let (sender, receiver) = mpsc::sync_channel(2);
            let (reply, response) = mpsc::sync_channel(1);
            sender.send(Event::Finalized { id: item.id.clone(), proof: item.proof.clone(), reply }).unwrap();
            s.cloud_uploads.pending.push(Pending {
                receiver,
                library: if wrong_library { Arc::new(()) } else { s.library_token() },
                connection_revision: resolver.connection_revision() + u64::from(!wrong_library),
                resolver,
                ids: BTreeMap::from([(item.id.clone(), item.intent.asset_id.clone())]),
                verification_ids: BTreeSet::new(),
                item_outcomes: BTreeMap::new(),
                connections: BTreeSet::new(),
                media_verified: BTreeSet::new(),
                maintenance: Some(Maintenance::Receipt),
                guard,
                cancel,
            });
            let before = s.catalog.to_snapshot();
            s.poll_cloud_uploads();
            assert!(response.recv().unwrap().is_err());
            assert_eq!(s.catalog.to_snapshot(), before);
            sender.send(Event::Finished(Err(error("held stale receipt failure")))).unwrap();
            s.poll_cloud_uploads();
            assert_eq!(s.catalog.to_snapshot(), before);
            drop(sender);
            s.cloud_paused = true;
            drain(&mut s);
            assert!(s.catalog.upload_intents()[&item.id].finalization.is_none());
            assert!(s.catalog.upload_intents()[&item.id].error.is_none());
            s.close_library().unwrap();
            drop(s);
            cleanup(root);
        }
    }
    #[test]
    fn cr31_ordinary_stage_markers_preserve_finalized_proof_and_explicit_failure_protects_data() {
        for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
            let (mut s, root, ready, mut stale) = fixture(protocol);
            s.cloud_paused = true;
            schedule(&mut s);
            drain(&mut s);
            let path = root.join("library/Managed/uploads").join(format!("{}.json", ready.id));
            let finalized = read_receipt(&path).unwrap().unwrap();
            assert_eq!(finalized.state, UploadState::Synchronized);
            let (mut items, skipped) = super::super::items(&mut s, &json!({"imports":[ready.id],"verify":true}), false).unwrap();
            assert!(skipped.is_empty());
            let mut item = items.remove(0);
            item.reverification = false;
            for state in [UploadState::Uploading, UploadState::ObjectsVerified, UploadState::Failed] {
                stale.state = state;
                stale.error = (state == UploadState::Failed).then(|| "obsolete worker failure".into());
                stale.generation = protocol.is_head().then_some(5);
                item.write_receipt(&stale, &AtomicBool::new(false)).unwrap();
                let saved = read_receipt(&path).unwrap().unwrap();
                assert_eq!(saved.state, UploadState::Synchronized);
                assert_eq!(saved.append_proof, finalized.append_proof);
                assert_eq!(saved.generation, finalized.generation);
                assert_eq!(saved.error, None);
            }
            item.reverification = true;
            stale.state = UploadState::Failed;
            stale.error = Some("confirmed remote integrity failure".into());
            item.write_receipt(&stale, &AtomicBool::new(false)).unwrap();
            let saved = read_receipt(&path).unwrap().unwrap();
            assert_eq!(saved.state, UploadState::Failed);
            assert_eq!(saved.append_proof, finalized.append_proof);
            assert_eq!(saved.generation, finalized.generation);
            s.close_library().unwrap();
            drop(s);
            drop(item);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn causal_receipts_allow_retained_photo_identity_distinct_from_adoption_request() {
        for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
            let (session, root, _, mut receipt) = fixture(protocol);
            receipt.imported.photo_id = "existing-photo-identity".into();
            receipt.validate().unwrap();
            receipt.version = 2;
            receipt.protocol = SyncProtocol::ConditionalHead;
            receipt.library = None;
            receipt.publication_intent.clear();
            receipt.append_proof.clear();
            assert!(receipt.validate().is_err(), "legacy initial-import identity constraints remain");
            drop(session);
            cleanup(root);
        }
    }
}
