//! Native conditional S3 synchronization protocol. Mutable head writes always carry an ETag;
//! immutable records and checkpoints are created conditionally and verified after uncertain writes.
use crate::assets::s3::{PutCondition, S3Connection};
use crate::assets::{AssetError, BlobStore, Result, keys, sha256};
use crate::cloud_publication::{self as publication, Batch, CheckpointRef, Head, Imported, RecordRef};
use lightcraft_catalog::assets::{Asset, identifier};
use lightcraft_catalog::replication::{Mutation, Portable, Stamp, SyncState};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_CHECKPOINT: usize = 64 << 20;
fn invalid(message: impl Into<String>) -> AssetError {
    AssetError::Invalid(message.into())
}
fn canceled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) { Err(AssetError::Cancelled) } else { Ok(()) }
}
fn catalog_error(error: lightcraft_catalog::CatalogError) -> AssetError {
    invalid(error.to_string())
}

pub use crate::cloud_records::Checkpoint;
pub fn read_checkpoint(connection: &S3Connection, reference: &CheckpointRef, cancel: &AtomicBool) -> Result<Checkpoint> {
    let reply = connection.get_small(&reference.object.key, MAX_CHECKPOINT, cancel)?.ok_or(AssetError::Missing)?;
    if reply.bytes.len() as u64 != reference.object.size || sha256(&reply.bytes) != reference.object.hash {
        return Err(AssetError::Integrity);
    }
    let checkpoint: Checkpoint = serde_json::from_slice(&reply.bytes).map_err(|_| invalid("malformed remote checkpoint; local data retained"))?;
    if checkpoint.version != 1
        || checkpoint.generation != reference.generation
        || checkpoint.covers != reference.covers
        || checkpoint.imports.len() > 100_000
        || checkpoint
            .imports
            .iter()
            .any(|(id, digest)| !identifier(id) || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f')))
    {
        return Err(invalid("invalid checkpoint generation or import receipts"));
    }
    checkpoint.state.validate().map_err(catalog_error)?;
    Ok(checkpoint)
}
pub struct Remote {
    pub head: Option<(Head, String)>,
    pub state: SyncState,
    pub imports: BTreeMap<String, String>,
}
fn legacy(remote: &mut Remote, imported: &Imported) -> Result<()> {
    let digest = sha256(&publication::encode(imported)?).0;
    if let Some(old) = remote.imports.get(&imported.mutation_id) {
        if old != &digest {
            return Err(invalid("remote import identity collision"));
        }
        return Ok(());
    }
    let changes = crate::cloud_sync::legacy_changes(imported, &remote.state).map_err(|e| invalid(e.to_string()))?;
    let device = format!(
        "{}_{}",
        if imported.base_asset.is_some() { "legacy" } else { "baseline" },
        sha256(format!("legacy-import-v1/{}", imported.mutation_id).as_bytes()).0
    );
    let stamp = Stamp { sequence: 1, device };
    let existing = remote.state.received.get(&stamp.id());
    if existing.is_none()
        && let Some((_, content)) = remote.state.winning(&format!("photo:{}", imported.photo_id), "content")
    {
        let current = content.get("assetId").and_then(serde_json::Value::as_str);
        if imported.base_asset.as_ref().is_none_or(|base| current != Some(base.0.as_str())) {
            remote.imports.insert(imported.mutation_id.clone(), digest);
            return Ok(());
        }
    }
    let context = existing
        .map_or_else(|| if imported.base_asset.is_some() { remote.state.clock.clone() } else { BTreeMap::new() }, |receipt| receipt.context.clone());
    let mutation = Mutation { version: 1, library: remote.state.library.clone(), revision: stamp, context, changes };
    remote.state.receive(&mutation).map_err(catalog_error)?;
    remote.imports.insert(imported.mutation_id.clone(), digest);
    Ok(())
}
/// Read a verified checkpoint plus its incremental tail. Retained older objects are not
/// traversed after the checkpoint boundary; offline local work is merged by the owner later.
pub fn read(connection: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<Remote> {
    let head = publication::read_head(connection, cancel)?;
    if head.as_ref().is_some_and(|(head, _)| head.library.as_ref().is_some_and(|id| id != library)) {
        return Err(invalid("storage contains another library; join using its library identity before synchronization"));
    }
    let state = SyncState::new(library.into(), "remote".into(), connection.id.clone()).map_err(catalog_error)?;
    let mut remote = Remote { head, state, imports: BTreeMap::new() };
    let Some((head, _)) = &remote.head else { return Ok(remote) };
    let mut next = Some(head.tip.clone());
    let mut boundary = None;
    if let Some(reference) = &head.checkpoint {
        let checkpoint = read_checkpoint(connection, reference, cancel)?;
        remote.state.merge(&checkpoint.state).map_err(catalog_error)?;
        remote.imports = checkpoint.imports;
        boundary = Some(reference.covers.hash.clone());
    }
    let mut batches = Vec::new();
    let mut seen = BTreeSet::new();
    let mut complete = false;
    for _ in 0..4096 {
        canceled(cancel)?;
        let Some(reference) = next.take() else {
            if boundary.is_some() {
                return Err(invalid("checkpoint boundary is absent from its history"));
            }
            complete = true;
            break;
        };
        if boundary.as_ref() == Some(&reference.hash) {
            complete = true;
            break;
        }
        if !seen.insert(reference.hash.clone()) {
            return Err(invalid("remote catalog chain contains a cycle"));
        }
        let batch = publication::read_batch(connection, &reference, cancel)?;
        next = batch.parent.clone();
        batches.push(batch);
    }
    if !complete && next.is_some() {
        return Err(invalid("remote catalog history exceeds 4096 records; checkpoint required"));
    }
    if boundary.is_some() && next.is_none() && !complete {
        return Err(invalid("checkpoint boundary is absent from its history"));
    }
    for batch in batches.into_iter().rev() {
        for imported in batch.imports {
            legacy(&mut remote, &imported)?;
        }
        for mutation in batch.mutations {
            remote.state.receive(&mutation).map_err(catalog_error)?;
        }
    }
    Ok(remote)
}
/// Use the durable acknowledged cursor when it is still in the retained tail. A checkpoint
/// covering that cursor triggers a full bootstrap; pending local revisions never seed reads.
fn read_incremental(connection: &S3Connection, local: &SyncState, cancel: &AtomicBool) -> Result<Remote> {
    if !local.pending.is_empty() || local.cursor.is_none() {
        return read(connection, &local.library, cancel);
    }
    let head = publication::read_head(connection, cancel)?;
    let Some((current, _)) = &head else { return read(connection, &local.library, cancel) };
    if current.library.as_deref() != Some(local.library.as_str()) || current.generation < local.generation {
        return Err(invalid("remote library or generation changed inconsistently"));
    }
    let mut next = Some(current.tip.clone());
    let boundary = current.checkpoint.as_ref().map(|c| &c.covers.hash);
    let mut batches = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..4096 {
        canceled(cancel)?;
        let Some(reference) = next.take() else { break };
        if local.cursor.as_deref() == Some(reference.hash.0.as_str()) {
            let mut state = SyncState::new(local.library.clone(), "remote".into(), connection.id.clone()).map_err(catalog_error)?;
            state.merge(&local.portable()).map_err(catalog_error)?;
            let mut remote = Remote { head, state, imports: BTreeMap::new() };
            for batch in batches.into_iter().rev() {
                let batch: Batch = batch;
                for imported in batch.imports {
                    legacy(&mut remote, &imported)?;
                }
                for mutation in batch.mutations {
                    remote.state.receive(&mutation).map_err(catalog_error)?;
                }
            }
            return Ok(remote);
        }
        if boundary == Some(&reference.hash) {
            break;
        }
        if !seen.insert(reference.hash.clone()) {
            return Err(invalid("remote catalog chain contains a cycle"));
        }
        let batch = publication::read_batch(connection, &reference, cancel)?;
        next = batch.parent.clone();
        batches.push(batch);
    }
    read(connection, &local.library, cancel)
}
fn immutable(connection: &S3Connection, key: &str, bytes: &[u8], cancel: &AtomicBool, max: usize) -> Result<()> {
    match connection.put_small(key, bytes, PutCondition::Absent, cancel) {
        Ok(_) => Ok(()),
        Err(error @ (AssetError::Conflict | AssetError::Network(_))) => match connection.get_small(key, max, cancel) {
            Ok(Some(reply)) if reply.bytes == bytes => Ok(()),
            _ => Err(error),
        },
        Err(error) => Err(error),
    }
}
#[derive(Default)]
struct Verify {
    hash: Sha256,
    size: u64,
    limit: u64,
}
impl Write for Verify {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.size.saturating_add(bytes.len() as u64) > self.limit {
            return Err(std::io::Error::other("remote blob exceeds its declared length"));
        }
        self.hash.update(bytes);
        self.size = self.size.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn dependencies(connection: &S3Connection, mutations: &[Mutation], cancel: &AtomicBool) -> Result<()> {
    let mut verified = BTreeSet::new();
    for change in mutations.iter().flat_map(|mutation| &mutation.changes).filter(|change| change.field == "content") {
        let mut asset: Asset = serde_json::from_value(change.value.get("asset").cloned().ok_or_else(|| invalid("mutation content has no asset"))?)
            .map_err(|_| invalid("invalid mutation asset description"))?;
        asset.original.remote.connection = connection.id.clone();
        if let Some(proxy) = &mut asset.smart_preview {
            proxy.remote.connection = connection.id.clone();
        }
        if let Some(thumbnail) = &mut asset.thumbnail {
            thumbnail.remote.connection = connection.id.clone();
        }
        for associated in asset.associated.values_mut() {
            associated.remote.connection = connection.id.clone();
        }
        asset.validate().map_err(invalid)?;
        for blob in asset.blobs() {
            if !verified.insert((blob.id.clone(), blob.remote.key.clone(), blob.remote.version.clone())) {
                continue;
            }
            canceled(cancel)?;
            let mut writer = Verify { limit: blob.size, ..Default::default() };
            connection.download(blob, &mut writer, cancel, &mut |_| {})?;
            if writer.size != blob.size || format!("{:x}", writer.hash.finalize()) != blob.id.0 {
                return Err(AssetError::Integrity);
            }
        }
    }
    Ok(())
}
pub struct Outcome {
    pub append: Option<lightcraft_catalog::replication::AppendProgress>,
    pub state: Portable,
    pub acknowledged: BTreeSet<String>,
    pub cursor: String,
    pub generation: u64,
}
/// Reconcile each failed conditional head update by reading the competing history. Mutation
/// IDs and payloads never change across attempts, including uncertain publication responses.
pub fn synchronize(connection: &S3Connection, local: &SyncState, checkpoint_requested: bool, cancel: &AtomicBool) -> Result<Outcome> {
    if local.protocol == lightcraft_catalog::replication::SyncProtocol::AppendLog {
        return crate::cloud_append::native::synchronize(connection, local, checkpoint_requested, cancel);
    }
    if connection.get_small(&crate::cloud_append::manifest_key(&connection.prefix, &local.library)?, 16 << 10, cancel)?.is_some() {
        return Err(invalid("this library has an append manifest; stop conditional-head writers and explicitly migrate before continuing"));
    }
    local.validate().map_err(catalog_error)?;
    if local.pending.is_empty() && !checkpoint_requested {
        let current = publication::read_head(connection, cancel)?;
        if let Some((head, _)) = current
            && local.cursor.as_deref() == Some(head.tip.hash.0.as_str())
        {
            if head.library.as_deref() != Some(local.library.as_str()) || head.generation != local.generation {
                return Err(invalid("remote library/cursor generation changed inconsistently"));
            }
            return Ok(Outcome {
                append: None,
                state: local.portable(),
                acknowledged: BTreeSet::new(),
                cursor: head.tip.hash.0,
                generation: head.generation,
            });
        }
    }
    let mut dependency_proof = BTreeSet::new();
    for _ in 0..64 {
        canceled(cancel)?;
        let mut remote = if checkpoint_requested { read(connection, &local.library, cancel)? } else { read_incremental(connection, local, cancel)? };
        let mut pending = Vec::new();
        for mutation in local.pending.values() {
            if let Some(receipt) = remote.state.received.get(&mutation.revision.id()) {
                if receipt.digest != mutation.digest().map_err(catalog_error)? {
                    return Err(invalid("remote mutation identity collision; local work retained"));
                }
            } else {
                pending.push(mutation.clone());
            }
        }
        if pending.is_empty()
            && (!checkpoint_requested
                || remote
                    .head
                    .as_ref()
                    .is_none_or(|(head, _)| head.checkpoint.as_ref().is_some_and(|c| c.generation.saturating_add(1) == head.generation)))
        {
            let Some((head, _)) = remote.head else {
                return Err(invalid("remote catalog is empty; initial library metadata must be queued first"));
            };
            return Ok(Outcome {
                append: None,
                state: remote.state.portable(),
                acknowledged: local.pending.keys().cloned().collect(),
                cursor: head.tip.hash.0,
                generation: head.generation,
            });
        }
        let pending_count = pending.len();
        let mut chunk = Vec::new();
        for mutation in pending.into_iter().take(256) {
            chunk.push(mutation);
            let trial = Batch {
                version: 3,
                parent: remote.head.as_ref().map(|(head, _)| head.tip.clone()),
                imports: Vec::new(),
                mutations: chunk.clone(),
                checkpoint_marker: false,
            };
            if publication::encode(&trial).is_err() {
                chunk.pop();
                break;
            }
        }
        if chunk.is_empty() && pending_count > 0 {
            return Err(invalid("pending mutation cannot fit a 2 MiB change batch"));
        }
        let newly_referenced: Vec<_> = chunk.iter().filter(|mutation| !dependency_proof.contains(&mutation.revision.id())).cloned().collect();
        dependencies(connection, &newly_referenced, cancel)?;
        dependency_proof.extend(newly_referenced.iter().map(|mutation| mutation.revision.id()));
        let mut checkpoint = remote.head.as_ref().and_then(|(head, _)| head.checkpoint.clone());
        let mut parent = remote.head.as_ref().map(|(head, _)| head.tip.clone());
        if checkpoint_requested && let Some((head, _)) = &remote.head {
            let payload = Checkpoint {
                version: 1,
                generation: head.generation,
                covers: head.tip.clone(),
                state: remote.state.portable(),
                imports: remote.imports.clone(),
            };
            let bytes = serde_json::to_vec(&payload).map_err(|_| invalid("could not encode checkpoint"))?;
            if bytes.len() > MAX_CHECKPOINT {
                return Err(invalid("checkpoint exceeds 64 MiB; local work retained"));
            }
            let hash = sha256(&bytes);
            let object = RecordRef {
                key: keys::in_library(&connection.prefix, &format!("catalog/checkpoints/{}.json", hash.0))?,
                size: bytes.len() as u64,
                hash,
            };
            immutable(connection, &object.key, &bytes, cancel, MAX_CHECKPOINT)?;
            checkpoint = Some(CheckpointRef { object, covers: head.tip.clone(), generation: head.generation });
            // Keep the inclusive boundary as parent; normal readers stop before loading it.
            parent = Some(head.tip.clone());
        }
        for mutation in &chunk {
            remote.state.receive(mutation).map_err(catalog_error)?;
        }
        let checkpoint_marker = chunk.is_empty();
        let batch = Batch { version: 3, parent, imports: Vec::new(), mutations: chunk, checkpoint_marker };
        let bytes = publication::encode(&batch)?;
        let hash = sha256(&bytes);
        let tip =
            RecordRef { key: keys::in_library(&connection.prefix, &format!("catalog/changes/{}.json", hash.0))?, size: bytes.len() as u64, hash };
        immutable(connection, &tip.key, &bytes, cancel, 2 << 20)?;
        let generation =
            remote.head.as_ref().map_or(Ok(1), |(head, _)| head.generation.checked_add(1).ok_or_else(|| invalid("remote generation exhausted")))?;
        let new_head = Head { version: 2, generation, tip, library: Some(local.library.clone()), checkpoint };
        let condition = remote.head.as_ref().map_or(PutCondition::Absent, |(_, etag)| PutCondition::Matches(etag));
        let key = keys::in_library(&connection.prefix, "catalog/head.json")?;
        match connection.put_small(&key, &publication::encode(&new_head)?, condition, cancel) {
            Ok(_) | Err(AssetError::Conflict | AssetError::Network(_)) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(AssetError::Network("catalog remained busy or unreachable; durable mutations retained for retry".into()))
}

mod worker;
pub use worker::{Jobs, prepare_migration, start, start_migration};
