//! Native conditional S3 synchronization protocol. Mutable head writes always carry an ETag;
//! immutable records and checkpoints are created conditionally and verified after uncertain writes.
use crate::assets::s3::{PutCondition, S3Connection};
use crate::assets::{AssetError, Result, keys, sha256};
use crate::cloud_publication::{self as publication, Batch, CheckpointRef, Head, RecordRef};
use lightcraft_catalog::assets::{Asset, identifier};
use lightcraft_catalog::replication::{Mutation, SyncState};
use std::collections::{BTreeMap, BTreeSet};
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
    if !checkpoint.imports.is_empty() {
        return Err(invalid("obsolete pre-release import checkpoint; use a fresh catalog and storage prefix; local data retained"));
    }
    Ok(checkpoint)
}
// Ordinary histories checkpoint well before the former hard reader boundary.
const AUTO_CHECKPOINT_BATCHES: u64 = 1024;
const RECOVERY_SEGMENT_BATCHES: u64 = 4096;
mod replay;
pub struct Remote {
    pub head: Option<(Head, String)>,
    pub state: SyncState,
    tail_records: u64,
    // A verified oldest prefix ready for a durable recovery checkpoint. Pending
    // local mutations must not be applied until the remaining history is replayed.
    recovery: Option<(RecordRef, u64)>,
}
fn tail_length(head: &Head) -> u64 {
    head.generation.saturating_sub(head.checkpoint.as_ref().map_or(0, |checkpoint| checkpoint.generation))
}
/// Read a verified checkpoint plus its tail with one decoded batch resident.
/// Migration reads the full history; synchronization can checkpoint bounded prefixes.
pub fn read(connection: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<Remote> {
    read_window(connection, library, None, cancel)
}
fn read_window(connection: &S3Connection, library: &str, segment: Option<u64>, cancel: &AtomicBool) -> Result<Remote> {
    let head = publication::read_head(connection, cancel)?;
    if head.as_ref().is_some_and(|(head, _)| head.library.as_ref().is_some_and(|id| id != library)) {
        return Err(invalid("storage contains another library; join using its library identity before synchronization"));
    }
    let state = SyncState::new(library.into(), "remote".into(), connection.id.clone()).map_err(catalog_error)?;
    let mut remote = Remote { head, state, tail_records: 0, recovery: None };
    let mut archive_proofs = lightcraft_catalog::replication::archive::Witness::new();
    let Some((head, _)) = &remote.head else { return Ok(remote) };
    if let Some(reference) = &head.checkpoint {
        let checkpoint = read_checkpoint(connection, reference, cancel)?;
        let verified = crate::cloud_history::native::certify(connection, &remote.state, &checkpoint.state, &mut archive_proofs, cancel)?;
        remote.state.merge(&verified).map_err(catalog_error)?;
    }
    let mut boundary = head.checkpoint.as_ref().map(|reference| reference.covers.clone());
    let mut first = true;
    loop {
        let mut tail = replay::Tail::scan(connection, head.tip.clone(), boundary, cancel)?;
        if first {
            remote.tail_records = tail.count();
            first = false;
        }
        let budget = tail.count().min(segment.unwrap_or(RECOVERY_SEGMENT_BATCHES));
        for _ in 0..budget {
            let (_, batch) = tail.next(connection, cancel)?.ok_or_else(|| invalid("history replay ended before its verified prefix"))?;
            for mutation in batch.mutations {
                crate::cloud_history::native::receive(connection, &mut remote.state, &mutation, &mut archive_proofs, cancel)?;
            }
        }
        if budget >= tail.count() {
            break;
        }
        boundary = tail.boundary.clone();
        if segment.is_some() {
            let generation = head
                .generation
                .checked_sub(tail.count() - budget)
                .filter(|generation| *generation > 0)
                .ok_or_else(|| invalid("history generation does not cover the recovery prefix"))?;
            let reference = boundary.ok_or_else(|| invalid("recovery prefix has no boundary"))?;
            remote.recovery = Some((reference, generation));
            break;
        }
    }
    Ok(remote)
}
/// Durable cursors seed only acknowledged state. Pending local revisions never seed reads.
fn read_incremental(connection: &S3Connection, local: &SyncState, cancel: &AtomicBool) -> Result<Remote> {
    if !local.pending.is_empty() || local.cursor.is_none() {
        return read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), cancel);
    }
    let head = publication::read_head(connection, cancel)?;
    let Some((current, _)) = &head else { return read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), cancel) };
    if current.library.as_deref() != Some(local.library.as_str()) || current.generation < local.generation {
        return Err(invalid("remote library or generation changed inconsistently"));
    }
    // A cursor is only a hash; discover its full reference without buffering batches.
    let mut index = replay::Index::new()?;
    let mut seen = replay::Cycle::default();
    let mut next = Some(current.tip.clone());
    let boundary = current.checkpoint.as_ref().map(|checkpoint| &checkpoint.covers.hash);
    while let Some(reference) = next.take() {
        canceled(cancel)?;
        if local.cursor.as_deref() == Some(reference.hash.0.as_str()) {
            if index.count > RECOVERY_SEGMENT_BATCHES {
                return read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), cancel);
            }
            let state = SyncState::restore_durable(&local.portable(), "remote".into(), connection.id.clone()).map_err(catalog_error)?;
            let tail_records = tail_length(current);
            let mut remote = Remote { head, state, tail_records, recovery: None };
            let mut parent = Some(reference);
            let mut archive_proofs = lightcraft_catalog::replication::archive::Witness::new();
            while let Some(reference) = index.pop()? {
                canceled(cancel)?;
                let batch = publication::read_batch(connection, &reference, cancel)?;
                if batch.parent != parent {
                    return Err(AssetError::Integrity);
                }
                parent = Some(reference);
                for mutation in batch.mutations {
                    crate::cloud_history::native::receive(connection, &mut remote.state, &mutation, &mut archive_proofs, cancel)?;
                }
            }
            return Ok(remote);
        }
        if boundary == Some(&reference.hash) {
            break;
        }
        index.push(&reference)?;
        seen.visit(&reference.hash)?;
        let batch = publication::read_batch(connection, &reference, cancel)?;
        next = batch.parent;
    }
    read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), cancel)
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
pub(crate) fn dependencies_verified(connection: &S3Connection, mutations: &[Mutation], cancel: &AtomicBool, media: &mut VerifiedMedia) -> Result<()> {
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
            if !media.verify(connection, blob, cancel, &mut |_| {})? {
                return Err(AssetError::Missing);
            }
        }
    }
    Ok(())
}
mod media;
pub(crate) use media::{SharedMedia, VerifiedMedia};

use crate::cloud_outcome::HeadPosition;
pub use crate::cloud_outcome::Outcome;
/// Reconcile each failed conditional head update by reading the competing history. Mutation
/// IDs and payloads never change across attempts, including uncertain publication responses.
pub fn synchronize(connection: &S3Connection, local: &SyncState, checkpoint_requested: bool, cancel: &AtomicBool) -> Result<Outcome> {
    synchronize_verified(connection, local, checkpoint_requested, cancel, &mut VerifiedMedia::default())
}
pub(crate) fn synchronize_verified(
    connection: &S3Connection,
    local: &SyncState,
    checkpoint_requested: bool,
    cancel: &AtomicBool,
    media: &mut VerifiedMedia,
) -> Result<Outcome> {
    if local.protocol == lightcraft_catalog::replication::SyncProtocol::AppendLog {
        return crate::cloud_append::native::synchronize_verified(connection, local, checkpoint_requested, false, cancel, media);
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
            if tail_length(&head) < AUTO_CHECKPOINT_BATCHES {
                return Ok(Outcome::unchanged(local.portable(), HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?));
            }
        }
    }
    let mut dependency_proof = BTreeSet::new();
    let mut archive_proofs = lightcraft_catalog::replication::archive::Witness::new();
    for _ in 0..64 {
        canceled(cancel)?;
        let mut remote = if checkpoint_requested {
            read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), cancel)?
        } else {
            read_incremental(connection, local, cancel)?
        };
        if remote.head.as_ref().map_or(0, |(head, _)| head.generation) < local.generation {
            return Err(invalid("remote catalog rolled back below the acknowledged generation; local work retained"));
        }
        if let Some((covers, generation)) = remote.recovery.take() {
            let Some((head, etag)) = &remote.head else {
                return Err(invalid("recovery prefix has no head"));
            };
            let checkpoint = checkpoint(connection, &mut remote.state, covers, generation, cancel)?;
            // Each prefix becomes durable through CAS before replaying the next.
            // Cancellation/restart resumes from this published boundary, with all
            // local pending IDs unchanged. A competitor simply causes a new scan.
            publish(connection, Some((head, etag)), Vec::new(), Some(checkpoint), &local.library, cancel)?;
            continue;
        }
        let checkpoint_needed = checkpoint_requested || remote.tail_records >= AUTO_CHECKPOINT_BATCHES;
        crate::cloud_history::native::hydrate(connection, &mut remote.state, local, &mut archive_proofs, cancel)?;
        let mut pending = Vec::new();
        let mut acknowledged = BTreeSet::new();
        for mutation in local.pending.values() {
            if let Some(receipt) = remote.state.received.get(&mutation.revision.id()) {
                if receipt.digest != mutation.digest().map_err(catalog_error)? {
                    return Err(invalid("remote mutation identity collision; local work retained"));
                }
                acknowledged.insert(mutation.revision.id());
            } else {
                pending.push(mutation.clone());
            }
        }
        if pending.is_empty()
            && (!checkpoint_needed
                || remote
                    .head
                    .as_ref()
                    .is_none_or(|(head, _)| head.checkpoint.as_ref().is_some_and(|c| c.generation.saturating_add(1) == head.generation)))
        {
            let Some((head, _)) = remote.head else {
                return Err(invalid("remote catalog is empty; initial library metadata must be queued first"));
            };
            let state = crate::cloud_history::native::certify(connection, local, &remote.state.portable(), &mut archive_proofs, cancel)?;
            return Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?, acknowledged));
        }
        let pending_count = pending.len();
        pending = crate::cloud_sync::publication_order(pending, &remote.state.clock).map_err(|e| invalid(e.to_string()))?;
        let mut chunk = Vec::new();
        let mut waiting_for_media = false;
        for mutation in pending.into_iter().take(256) {
            // Preserve the author's causal prefix, but still receive remote changes when
            // an import's bytes have not arrived. Dependency waiting is ordinary progress.
            if !dependency_proof.contains(&mutation.revision.id()) {
                match dependencies_verified(connection, std::slice::from_ref(&mutation), cancel, media) {
                    Ok(()) => {
                        dependency_proof.insert(mutation.revision.id());
                    }
                    Err(AssetError::Missing) => {
                        waiting_for_media = true;
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
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
            if !waiting_for_media {
                return Err(invalid("pending mutation cannot fit a 2 MiB change batch"));
            }
            let state = crate::cloud_history::native::certify(connection, local, &remote.state.portable(), &mut archive_proofs, cancel)?;
            return Ok(if let Some((head, _)) = &remote.head {
                Outcome::head(state, HeadPosition::new(head.tip.hash.0.clone(), head.generation).map_err(invalid)?, acknowledged)
            } else {
                if !acknowledged.is_empty() {
                    return Err(invalid("empty remote head cannot acknowledge mutations"));
                }
                Outcome::empty(state)
            });
        }
        let newly_referenced: Vec<_> = chunk.iter().filter(|mutation| !dependency_proof.contains(&mutation.revision.id())).cloned().collect();
        dependencies_verified(connection, &newly_referenced, cancel, media)?;
        dependency_proof.extend(newly_referenced.iter().map(|mutation| mutation.revision.id()));
        let mut checkpoint_ref = remote.head.as_ref().and_then(|(head, _)| head.checkpoint.clone());
        if checkpoint_needed && let Some((head, _)) = &remote.head {
            checkpoint_ref = Some(checkpoint(connection, &mut remote.state, head.tip.clone(), head.generation, cancel)?);
        }
        for mutation in &chunk {
            remote.state.receive(mutation).map_err(catalog_error)?;
        }
        publish(connection, remote.head.as_ref().map(|(head, etag)| (head, etag.as_str())), chunk, checkpoint_ref, &local.library, cancel)?;
    }
    Err(AssetError::Network("catalog remained busy or unreachable; durable mutations retained for retry".into()))
}

fn checkpoint(connection: &S3Connection, state: &mut SyncState, covers: RecordRef, generation: u64, cancel: &AtomicBool) -> Result<CheckpointRef> {
    crate::cloud_history::native::compact(connection, state, false, cancel)?;
    let payload = Checkpoint { version: 1, generation, covers: covers.clone(), state: state.portable(), imports: BTreeMap::new() };
    let bytes = serde_json::to_vec(&payload).map_err(|_| invalid("could not encode checkpoint"))?;
    if bytes.len() > MAX_CHECKPOINT {
        return Err(invalid("checkpoint exceeds 64 MiB; local work retained"));
    }
    let hash = sha256(&bytes);
    let object =
        RecordRef { key: keys::in_library(&connection.prefix, &format!("catalog/checkpoints/{}.json", hash.0))?, size: bytes.len() as u64, hash };
    immutable(connection, &object.key, &bytes, cancel, MAX_CHECKPOINT)?;
    Ok(CheckpointRef { object, covers, generation })
}
fn publish(
    connection: &S3Connection,
    previous: Option<(&Head, &str)>,
    mutations: Vec<Mutation>,
    checkpoint: Option<CheckpointRef>,
    library: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    let batch = Batch {
        version: 3,
        parent: previous.map(|(head, _)| head.tip.clone()),
        imports: Vec::new(),
        checkpoint_marker: mutations.is_empty(),
        mutations,
    };
    let bytes = publication::encode(&batch)?;
    let hash = sha256(&bytes);
    let tip = RecordRef { key: keys::in_library(&connection.prefix, &format!("catalog/changes/{}.json", hash.0))?, size: bytes.len() as u64, hash };
    immutable(connection, &tip.key, &bytes, cancel, 2 << 20)?;
    let generation = previous.map_or(Ok(1), |(head, _)| head.generation.checked_add(1).ok_or_else(|| invalid("remote generation exhausted")))?;
    let head = Head { version: 3, generation, tip, library: Some(library.into()), checkpoint };
    let condition = previous.map_or(PutCondition::Absent, |(_, etag)| PutCondition::Matches(etag));
    match connection.put_small(&keys::in_library(&connection.prefix, "catalog/head.json")?, &publication::encode(&head)?, condition, cancel) {
        Ok(_) | Err(AssetError::Conflict | AssetError::Network(_)) => Ok(()),
        Err(error) => Err(error),
    }
}

mod worker;
pub(crate) use worker::Coordinator;
pub(crate) use worker::request_poll;
pub use worker::{Jobs, pause_publication, prepare_migration, resume_publication, start, start_migration};
