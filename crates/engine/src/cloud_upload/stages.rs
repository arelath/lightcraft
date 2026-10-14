//! Media readiness, coordinated metadata publication, and local proof finalization.
use super::*;
use crate::cloud_replication::VerifiedMedia;

pub(super) struct VerifiedUpload {
    receipt: Receipt,
}

/// Constructed only from fully verified media and exact remotely checked mutation proof.
#[derive(Clone)]
pub(super) struct CoveredUpload {
    id: String,
    asset: Asset,
    asset_id: lightcraft_catalog::assets::AssetId,
    global: String,
    proof: UploadFinalization,
}
impl CoveredUpload {
    pub(super) fn id(&self) -> &str {
        &self.id
    }
    pub(super) fn asset_id(&self) -> &lightcraft_catalog::assets::AssetId {
        &self.asset_id
    }
    pub(super) fn queue(&self, catalog: &lightcraft_catalog::Catalog, acknowledged: Option<&UploadIntent>) -> Result<Op> {
        let state = catalog.sync_state().ok_or_else(|| error("publication library was disabled; proof retained"))?;
        let mut intent = acknowledged
            .or_else(|| catalog.upload_intents().get(&self.id))
            .cloned()
            .ok_or_else(|| error("publication intent disappeared; proof retained"))?;
        if state.library != self.proof.library
            || state.protocol != self.proof.protocol
            || intent.asset_id != self.asset_id
            || catalog.cloud_identity(&format!("photo:{}", intent.photo_id.0)) != Some(self.global.as_str())
            || catalog.asset(&self.asset_id) != Some(&self.asset)
            || self.asset.original.remote.connection != state.connection
            || self.asset.blobs().any(|blob| !intent.verified.contains(&blob.remote.key))
        {
            return Err(error("covered upload binding changed; receipt proof retained"));
        }
        let mut proof = self.proof.clone();
        if let Some(previous) = &intent.finalization {
            if previous.protocol != proof.protocol
                || previous.library != proof.library
                || previous.proofs.iter().any(|(id, digest)| proof.proofs.get(id).is_some_and(|new| new != digest))
            {
                return Err(error("covered upload publication proof conflicts; staging retained"));
            }
            proof.proofs.extend(previous.proofs.clone());
            proof.generation = proof.generation.max(previous.generation);
        }
        intent.state = UploadState::Synchronized;
        intent.error = None;
        intent.finalization = Some(proof);
        Ok(Op::SetUploadIntent { id: self.id.clone(), intent: Some(Box::new(intent)) })
    }
}

/// Pure owner-side transaction preparation. Stale history can request a fresh scan
/// without throwing away separately certified receipt completion evidence.
pub(super) fn completion_ops(
    catalog: &lightcraft_catalog::Catalog,
    outcome: &crate::cloud_replication::Outcome,
    started_generation: u64,
    covered: Vec<CoveredUpload>,
    ids: &BTreeMap<String, lightcraft_catalog::assets::AssetId>,
) -> Result<(Vec<Op>, bool)> {
    let reconciled = crate::cloud_sync::completed_outcome(catalog, outcome, outcome.acknowledged(), started_generation);
    let (mut ops, refresh) = match reconciled {
        Err(reason) if crate::cloud_sync::obsolete_completion_needs_proof(catalog, outcome, &reason) => (Vec::new(), true),
        result => (
            match result? {
                Op::Batch { ops } => ops,
                op => vec![op],
            },
            false,
        ),
    };
    for item in covered {
        if ids.get(item.id()) != Some(item.asset_id()) {
            return Err(error("publication belongs to another upload attempt"));
        }
        let acknowledged = ops.iter().rev().find_map(|op| match op {
            Op::SetUploadIntent { id, intent } if id == item.id() => intent.as_deref(),
            _ => None,
        });
        let queued = item.queue(catalog, acknowledged)?;
        ops.push(queued);
    }
    Ok((ops, refresh))
}

impl Item {
    fn load_receipt(&self, cancel: &AtomicBool, abort: bool) -> Result<Receipt> {
        let path = self.root.join("uploads").join(format!("{}.json", self.id));
        let frozen = self
            .sync
            .as_ref()
            .into_iter()
            .flat_map(|s| s.pending.values())
            .filter(|m| {
                m.changes.iter().any(|c| {
                    c.entity == format!("photo:{}", self.imported.photo_id)
                        && c.field == "content"
                        && c.value.get("assetId").and_then(Value::as_str) == Some(self.imported.asset_id.0.as_str())
                })
            })
            .map(|m| m.digest().map(|digest| (m.revision.id(), digest)).map_err(|e| error(e.to_string())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut receipt = read_receipt(&path)?.unwrap_or_else(|| Receipt {
            version: self.sync.as_ref().map_or(2, |s| if s.protocol.is_head() { 4 } else { 3 }),
            protocol: self.sync.as_ref().map_or(Default::default(), |s| s.protocol),
            library: self.sync.as_ref().map(|s| s.library.clone()),
            append_proof: Default::default(),
            publication_intent: if self.sync.is_some() { frozen.clone() } else { Default::default() },
            id: self.id.clone(),
            imported: self.imported.clone(),
            storage: Some(self.connection.storage_snapshot()),
            state: UploadState::SavedLocally,
            verified: Default::default(),
            multipart: Default::default(),
            generation: None,
            error: None,
        });
        if receipt.id != self.id || receipt.imported.asset_id != self.imported.asset_id || receipt.imported.asset != self.imported.asset {
            return Err(error("upload receipt describes another asset or rendition; preserved"));
        }
        if receipt.protocol.is_head()
            && let Some(local) = self.sync.as_ref().filter(|s| s.migrated_from_head && !s.protocol.is_head())
        {
            if receipt.storage.as_ref().is_some_and(|storage| !storage.same_destination(&self.connection)) {
                return Err(error("migration upload destination mismatch; receipt retained"));
            }
            receipt.protocol = local.protocol;
            receipt.library = Some(local.library.clone());
            receipt.version = 3;
            receipt.generation = None;
            receipt.storage = Some(self.connection.storage_snapshot());
            self.write_receipt(&receipt, cancel)?;
        }
        if !receipt.protocol.is_head() && receipt.publication_intent.is_empty() {
            receipt.publication_intent = frozen.clone();
        }
        if receipt.protocol != self.sync.as_ref().map_or(Default::default(), |s| s.protocol)
            || (receipt.library.is_some() && receipt.library.as_ref() != self.sync.as_ref().map(|s| &s.library))
        {
            return Err(error("upload receipt protocol/library differs; explicit migration or original settings required"));
        }
        if receipt.storage.as_ref().is_some_and(|storage| !storage.same_destination(&self.connection)) {
            return Err(error(
                "upload receipt belongs to another storage destination; restore the original connection settings; staged bytes preserved",
            ));
        }
        if receipt.storage.is_none() {
            if !receipt.multipart.is_empty() && !abort {
                return Err(error(
                    "legacy multipart receipt has no destination binding; explicitly abort it using its original connection settings, then retry; staged bytes preserved",
                ));
            }
            receipt.storage = Some(self.connection.storage_snapshot());
            receipt.version = 2;
        }
        if let Some(local) = self.sync.as_ref().filter(|s| s.protocol.is_head()) {
            receipt.version = 4;
            receipt.library = Some(local.library.clone());
            if receipt.publication_intent.is_empty() {
                receipt.publication_intent = frozen.clone();
            }
        }
        Ok(receipt)
    }
    pub(super) fn ensure_media(
        &self,
        events: &mpsc::SyncSender<Event>,
        cancel: &AtomicBool,
        task: &UploadProgress,
        media: &mut VerifiedMedia,
    ) -> Result<VerifiedUpload> {
        let mut receipt = self.load_receipt(cancel, false)?;
        let mut remote_invalid = false;
        let result = (|| -> Result<()> {
            receipt.state = UploadState::Uploading;
            receipt.error = None;
            self.write_receipt(&receipt, cancel)?;
            self.phase(&receipt, events, cancel)?;
            for blob in blobs(&receipt.imported.asset).cloned().collect::<Vec<_>>() {
                let verified = self.remote_verified(&blob, cancel, task, media).map_err(|e| {
                    remote_invalid |= matches!(e, AssetError::Integrity);
                    error(e.to_string())
                })?;
                if !verified {
                    remote_invalid = true;
                    self.upload_blob(&mut receipt, &blob, cancel, task, media)?;
                    if !self.remote_verified(&blob, cancel, task, media).map_err(|e| error(e.to_string()))? {
                        return Err(error("uploaded object is not visible; retain the original and retry"));
                    }
                }
                if let Some(upload) = receipt.multipart.get(&blob.remote.key) {
                    // Completion may have lost its response, or another writer won.
                    // A recorded unfinished upload is safe to retire after byte verification.
                    self.resolver
                        .background_transfer(&blob, cancel, || self.connection.abort_multipart(&blob, &upload.id, cancel))
                        .map_err(|e| error(e.to_string()))?;
                    receipt.multipart.remove(&blob.remote.key);
                }
                receipt.verified.insert(blob.remote.key.clone());
                self.write_receipt(&receipt, cancel)?;
                self.phase(&receipt, events, cancel)?;
            }
            remote_invalid = false; // Every remote object is now byte verified.
            receipt.state = UploadState::ObjectsVerified;
            self.write_receipt(&receipt, cancel)?;
            self.phase(&receipt, events, cancel)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(VerifiedUpload { receipt }),
            Err(reason) => {
                self.failed(&mut receipt, &reason, events, cancel, remote_invalid);
                Err(reason)
            }
        }
    }
    pub(super) fn abort(&self, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) -> Result<Value> {
        let mut receipt = self.load_receipt(cancel, true)?;
        let result = (|| {
            for blob in blobs(&receipt.imported.asset).cloned().collect::<Vec<_>>() {
                if let Some(upload) = receipt.multipart.get(&blob.remote.key) {
                    self.resolver
                        .background_transfer(&blob, cancel, || self.connection.abort_multipart(&blob, &upload.id, cancel))
                        .map_err(|e| error(e.to_string()))?;
                    receipt.multipart.remove(&blob.remote.key);
                    self.write_receipt(&receipt, cancel)?;
                }
            }
            receipt.state =
                if receipt.generation.is_some() || !receipt.append_proof.is_empty() { UploadState::Published } else { UploadState::SavedLocally };
            receipt.error = None;
            self.write_receipt(&receipt, cancel)?;
            self.phase(&receipt, events, cancel)?;
            Ok(json!({"import":self.id,"aborted":true,"originalPreserved":true}))
        })();
        if let Err(reason) = &result {
            self.failed(&mut receipt, reason, events, cancel, false);
        }
        result
    }
    fn finalize_receipt(&self, receipt: &mut Receipt, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) -> Result<Value> {
        receipt.state = UploadState::Published;
        self.write_receipt(receipt, cancel)?;
        self.phase(receipt, events, cancel)?;
        receipt.state = UploadState::Synchronized;
        self.phase(receipt, events, cancel)?;
        self.write_receipt(receipt, cancel)?;
        if let Some(local) = &self.sync {
            let proof = UploadFinalization {
                version: 1,
                protocol: local.protocol,
                library: local.library.clone(),
                generation: if local.protocol.is_head() { receipt.generation } else { None },
                proofs: receipt.append_proof.clone(),
            };
            finalize::acknowledge(&self.id, proof, events, cancel)?;
        }
        Ok(json!({"import":self.id,"synchronized":true,"generation":receipt.generation}))
    }
    pub(super) fn failed(&self, receipt: &mut Receipt, e: &EngineError, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool, remote_invalid: bool) {
        receipt.state = UploadState::Failed;
        let mut message = e.to_string().replace('\0', "�");
        if message.len() > 4096 {
            let mut end = 4096;
            while !message.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            message.truncate(end);
        }
        receipt.error = Some(message);
        let _ = self.write_receipt(receipt, &AtomicBool::new(false));
        let _ = self.send_phase(receipt, events, cancel, remote_invalid);
    }
    pub(super) fn item_result(&self, events: &mpsc::SyncSender<Event>, result: &Result<Value>) {
        let error = result.as_ref().err().map(|reason| UploadBlock::new(&reason.to_string(), false).reason);
        let _ = events.send(Event::ItemResult { attempt: self.attempt, id: self.id.clone(), error });
    }
}

/// The per-library gate covers one ready batch through owner durability. Receipt
/// finalization follows outside it; its exact proof queue already survives a crash.
pub(super) fn publish_ready(
    ready: &mut [(&Item, VerifiedUpload)],
    events: &mpsc::SyncSender<Event>,
    cancel: &AtomicBool,
    task: &UploadProgress,
    media: &mut VerifiedMedia,
) -> Result<Vec<Result<Value>>> {
    let first = ready.first().map(|(item, _)| *item).ok_or_else(|| error("no verified media ready for publication"))?;
    let local = first.sync.as_ref().ok_or_else(|| error("enable cloud synchronization before publishing; originals retained"))?;
    if ready.iter().any(|(item, _)| {
        !item.connection.same_destination(&first.connection)
            || item.sync.as_ref().is_none_or(|state| state.library != local.library || state.protocol != local.protocol || state.clock != local.clock)
    }) {
        return Err(error("verified upload batch spans different publication bindings; originals retained"));
    }
    media.publication();
    let permit = first.coordinator.acquire(cancel)?;
    task.detail("Publishing verified library changes");
    let outcome = crate::cloud_replication::synchronize_verified(&first.connection, local, false, cancel, media).map_err(|e| error(e.to_string()))?;
    let mut published_ids = BTreeSet::new();
    let mut covered = Vec::new();
    let mut witness = Default::default();
    for (item, verified) in ready.iter_mut() {
        let receipt = &mut verified.receipt;
        let expected = if !receipt.publication_intent.is_empty() {
            receipt.publication_intent.clone()
        } else if !receipt.append_proof.is_empty() {
            receipt.append_proof.clone()
        } else {
            outcome
                .state()
                .documents
                .get(&format!("photo:{}", item.imported.photo_id))
                .and_then(|fields| fields.get("content"))
                .into_iter()
                .flat_map(|r| r.0.iter())
                .filter(|(_, v)| v.get("assetId").and_then(Value::as_str) == Some(item.imported.asset_id.0.as_str()))
                .filter_map(|(id, _)| outcome.state().received.get(id).map(|r| (id.clone(), r.digest.clone())))
                .collect()
        };
        let mut published = !expected.is_empty();
        for (id, digest) in &expected {
            published &= crate::cloud_history::native::published(&first.connection, outcome.state(), id, digest, &mut witness, cancel)
                .map_err(|e| error(e.to_string()))?;
        }
        if published {
            receipt.append_proof = expected;
            if local.protocol.is_head() {
                receipt.generation = outcome.head_position().map(|position| position.generation());
            }
            covered.push(CoveredUpload {
                id: item.id.clone(),
                asset: (*receipt.imported.asset).clone(),
                asset_id: receipt.imported.asset_id.clone(),
                global: receipt.imported.photo_id.clone(),
                proof: UploadFinalization {
                    version: 1,
                    protocol: local.protocol,
                    library: local.library.clone(),
                    generation: receipt.generation,
                    proofs: receipt.append_proof.clone(),
                },
            });
            published_ids.insert(item.id.clone());
        }
    }
    let (reply, response) = mpsc::sync_channel(1);
    events
        .send(Event::Publication { outcome: Box::new(outcome), started_generation: local.generation, covered, reply })
        .map_err(|_| error("upload owner closed; exact publication retained"))?;
    loop {
        match response.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => {
                result?;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(error("upload owner closed")),
            Err(_) if cancel.load(Ordering::Relaxed) => return Err(error("upload cancelled; publication retained")),
            Err(_) => {}
        }
    }
    drop(permit);
    Ok(ready
        .iter_mut()
        .map(|(item, verified)| {
            let result = if published_ids.contains(&item.id) {
                item.finalize_receipt(&mut verified.receipt, events, cancel)
            } else {
                task.detail("Objects verified · waiting for other causal imports");
                Ok(json!({"import":item.id,"objectsVerified":true,"publicationPending":true}))
            };
            if let Err(reason) = &result {
                item.failed(&mut verified.receipt, reason, events, cancel, false);
            }
            item.item_result(events, &result);
            result
        })
        .collect())
}

pub(super) fn fail_ready(ready: &mut [(&Item, VerifiedUpload)], reason: &EngineError, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) {
    for (item, verified) in ready {
        item.failed(&mut verified.receipt, reason, events, cancel, false);
        let result = Err(error(reason.to_string()));
        item.item_result(events, &result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cr31_late_ready_upload_gets_durable_proof_after_outbox_was_already_acknowledged() {
        for protocol in [lightcraft_catalog::replication::SyncProtocol::ConditionalHead, lightcraft_catalog::replication::SyncProtocol::AppendLog] {
            let root = std::env::temp_dir().join(format!(
                "lightcraft-cr31-late-{}-{}",
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
            s.execute("cloud.configure",&json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9","profile":"disabled"})).unwrap();
            s.execute("cloud.syncEnable", &json!({"connection":"personal","protocol":protocol})).unwrap();
            s.execute("cloud.import", &json!({"connection":"personal","paths":[source]})).unwrap();
            let id = s.catalog.upload_intents().keys().next().unwrap().clone();
            let mut intent = s.catalog.upload_intents()[&id].clone();
            let asset = s.catalog.asset(&intent.asset_id).unwrap().clone();
            let state = s.catalog.sync_state().unwrap().clone();
            let proofs = state
                .pending
                .iter()
                .filter(|(_, m)| {
                    m.changes
                        .iter()
                        .any(|c| c.field == "content" && c.value.get("assetId").and_then(Value::as_str) == Some(intent.asset_id.0.as_str()))
                })
                .map(|(id, m)| (id.clone(), m.digest().unwrap()))
                .collect();
            let covered = CoveredUpload {
                id: id.clone(),
                asset: asset.clone(),
                asset_id: intent.asset_id.clone(),
                global: s.catalog.cloud_identity(&format!("photo:{}", intent.photo_id.0)).unwrap().into(),
                proof: UploadFinalization {
                    version: 1,
                    protocol,
                    library: state.library.clone(),
                    generation: protocol.is_head().then_some(11),
                    proofs,
                },
            };
            // Another worker ACKs before this import's verified phase reaches the owner.
            let ids = state.pending.keys().cloned().collect();
            let published = if protocol.is_head() {
                let mut remote =
                    lightcraft_catalog::replication::SyncState::restore_durable(&state.portable(), "other".into(), "personal".into()).unwrap();
                remote
                    .enqueue(vec![lightcraft_catalog::replication::Change {
                        entity: format!("library:{}", state.library),
                        field: "label.Green".into(),
                        base: None,
                        value: json!("Older green"),
                    }])
                    .unwrap();
                remote.portable()
            } else {
                state.portable()
            };
            let op = if protocol.is_head() {
                crate::cloud_sync::incoming_checkpoint(&s.catalog, &published, &ids, "a".repeat(64), 11).unwrap()
            } else {
                let remote = published.clone();
                let progress = lightcraft_catalog::replication::AppendProgress {
                    version: 1,
                    manifest: "a".repeat(64),
                    frontiers: lightcraft_catalog::replication::publication_prefixes(&remote).unwrap(),
                    remote,
                    gaps: BTreeSet::new(),
                };
                crate::cloud_sync::incoming_append(&s.catalog, progress, &ids).unwrap()
            };
            s.apply_received(op).unwrap();
            assert!(s.catalog.sync_state().unwrap().pending.is_empty());
            assert!(s.catalog.upload_intents()[&id].finalization.is_none());
            assert!(covered.queue(&s.catalog, None).is_err(), "cannot queue unverified media");
            intent.verified = asset.blobs().map(|b| b.remote.key.clone()).collect();
            intent.state = UploadState::ObjectsVerified;
            s.apply_system(Op::SetUploadIntent { id: id.clone(), intent: Some(Box::new(intent)) }).unwrap();
            let op = covered.queue(&s.catalog, None).unwrap();
            s.apply_received(op).unwrap();
            s.persist().unwrap();
            assert_eq!(s.catalog.upload_intents()[&id].finalization, Some(covered.proof.clone()));
            if protocol.is_head() {
                let mut newer = lightcraft_catalog::replication::SyncState::restore_durable(&published, "other".into(), "personal".into()).unwrap();
                newer
                    .enqueue(vec![lightcraft_catalog::replication::Change {
                        entity: format!("library:{}", state.library),
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
                let op = crate::cloud_sync::incoming_checkpoint(&s.catalog, &portable, &BTreeSet::new(), "b".repeat(64), 12).unwrap();
                s.apply_received(op).unwrap();
                let mut latest = s.catalog.upload_intents()[&id].clone();
                latest.finalization = None;
                s.apply_system(Op::SetUploadIntent { id: id.clone(), intent: Some(Box::new(latest)) }).unwrap();
                let old =
                    crate::cloud_replication::Outcome::head(published, crate::cloud_outcome::HeadPosition::new("a".repeat(64), 11).unwrap(), ids);
                let (ops, refresh) =
                    completion_ops(&s.catalog, &old, 0, vec![covered.clone()], &BTreeMap::from([(id.clone(), covered.asset_id.clone())])).unwrap();
                assert!(refresh, "obsolete archive evidence must request a rescan");
                assert_eq!(ops.len(), 1, "receipt proof still gets journaled");
                s.apply_received(Op::Batch { ops }).unwrap();
                s.persist().unwrap();
                assert_eq!(s.catalog.sync_state().unwrap().generation, 12);
            }
            drop(s);
            let mut restored = Session::new().with_fs();
            restored.open_library(root.join("library"), false).unwrap();
            assert_eq!(restored.catalog.upload_intents()[&id].finalization, Some(covered.proof));
            drop(restored);
            let canonical = std::fs::canonicalize(&root).unwrap();
            assert!(canonical.starts_with(std::fs::canonicalize(std::env::temp_dir()).unwrap()));
            // Preview workers can briefly hold files after closing; keep this tiny test fixture.
        }
    }
}
