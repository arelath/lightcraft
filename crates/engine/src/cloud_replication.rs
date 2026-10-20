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
    AssetError::from(error)
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
const AUTO_CHECKPOINT_BATCHES: u64 = crate::cloud_records::replay::AUTO_CHECKPOINT_BATCHES;
const RECOVERY_SEGMENT_BATCHES: u64 = crate::cloud_records::replay::WINDOW_BATCHES;
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
    read_with_witness(connection, library, &mut Default::default(), cancel)
}
pub(crate) fn read_with_witness(
    connection: &S3Connection,
    library: &str,
    proofs: &mut lightcraft_catalog::replication::archive::Witness,
    cancel: &AtomicBool,
) -> Result<Remote> {
    read_window(connection, library, None, proofs, cancel)
}
fn read_window(
    connection: &S3Connection,
    library: &str,
    segment: Option<u64>,
    archive_proofs: &mut lightcraft_catalog::replication::archive::Witness,
    cancel: &AtomicBool,
) -> Result<Remote> {
    let head = publication::read_head(connection, cancel)?;
    if head.as_ref().is_some_and(|(head, _)| head.library.as_ref().is_some_and(|id| id != library)) {
        return Err(invalid("storage contains another library; join using its library identity before synchronization"));
    }
    let state = SyncState::new(library.into(), "remote".into(), connection.id.clone()).map_err(catalog_error)?;
    let mut remote = Remote { head, state, tail_records: 0, recovery: None };
    let Some((head, _)) = &remote.head else { return Ok(remote) };
    if let Some(reference) = &head.checkpoint {
        let checkpoint = read_checkpoint(connection, reference, cancel)?;
        let verified = crate::cloud_history::native::certify(connection, &remote.state, &checkpoint.state, archive_proofs, cancel)?;
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
        let budget = tail.retained().min(segment.unwrap_or(RECOVERY_SEGMENT_BATCHES));
        for _ in 0..budget {
            let (_, batch) = tail.next(connection, cancel)?.ok_or_else(|| invalid("history replay ended before its verified prefix"))?;
            for mutation in batch.mutations {
                crate::cloud_history::native::receive(connection, &mut remote.state, &mutation, archive_proofs, cancel)?;
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
fn read_incremental(
    connection: &S3Connection,
    local: &SyncState,
    archive_proofs: &mut lightcraft_catalog::replication::archive::Witness,
    cancel: &AtomicBool,
) -> Result<Remote> {
    if !local.pending.is_empty() || local.cursor.is_none() {
        return read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), archive_proofs, cancel);
    }
    let head = publication::read_head(connection, cancel)?;
    let Some((current, _)) = &head else { return read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), archive_proofs, cancel) };
    if current.library.as_deref() != Some(local.library.as_str()) || current.generation < local.generation {
        return Err(invalid("remote library or generation changed inconsistently"));
    }
    // A cursor is only a hash; discover its full reference without buffering batches.
    let mut index = replay::Index::new()?;
    let mut seen = crate::cloud_records::replay::Cycle::default();
    let mut next = Some(current.tip.clone());
    let boundary = current.checkpoint.as_ref().map(|checkpoint| &checkpoint.covers.hash);
    while let Some(reference) = next.take() {
        canceled(cancel)?;
        if local.cursor.as_deref() == Some(reference.hash.0.as_str()) {
            // An acknowledged baseline is already certified. If scratch capacity
            // retained only its oldest new prefix, checkpoint that prefix through
            // the existing recovery path instead of downloading the scan again.
            let prefix_generation = if index.retained() < index.count {
                Some(
                    current
                        .generation
                        .checked_sub(index.count - index.retained())
                        .filter(|generation| *generation > local.generation)
                        .ok_or_else(|| invalid("history generation does not cover the acknowledged recovery prefix"))?,
                )
            } else {
                None
            };
            let state = SyncState::restore_durable(&local.portable(), "remote".into(), connection.id.clone()).map_err(catalog_error)?;
            let tail_records = tail_length(current);
            let mut remote = Remote { head, state, tail_records, recovery: None };
            let mut parent = Some(reference);
            while let Some((reference, batch)) = index.pop(connection, cancel)? {
                canceled(cancel)?;
                if batch.parent != parent {
                    return Err(AssetError::Integrity);
                }
                parent = Some(reference);
                for mutation in batch.mutations {
                    crate::cloud_history::native::receive(connection, &mut remote.state, &mutation, archive_proofs, cancel)?;
                }
            }
            if let Some(generation) = prefix_generation {
                remote.recovery = Some((parent.ok_or_else(|| invalid("recovery prefix has no boundary"))?, generation));
            }
            return Ok(remote);
        }
        if boundary == Some(&reference.hash) {
            break;
        }
        seen.visit(&reference.hash).map_err(invalid)?;
        let (bytes, batch) = publication::read_batch_bytes(connection, &reference, cancel)?;
        index.push(&reference, &bytes, cancel)?;
        next = batch.parent;
    }
    drop(index);
    read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), archive_proofs, cancel)
}
fn immutable(connection: &S3Connection, key: &str, bytes: &[u8], cancel: &AtomicBool, max: usize) -> Result<()> {
    if connection.b2_head_enabled(cancel)? {
        return connection.b2_put_catalog_immutable(key, bytes, cancel);
    }
    match connection.put_small(key, bytes, PutCondition::Absent, cancel) {
        Ok(_) => Ok(()),
        Err(error) if error.uncertain_write() => match connection.get_small(key, max, cancel) {
            Ok(Some(reply)) if reply.bytes == bytes => Ok(()),
            Ok(Some(_)) => Err(AssetError::Integrity),
            Err(read_error) => Err(read_error),
            _ => Err(error),
        },
        Err(error) => Err(error),
    }
}
pub(crate) fn dependencies_verified(connection: &S3Connection, mutations: &[Mutation], cancel: &AtomicBool, media: &mut VerifiedMedia) -> Result<()> {
    for change in mutations.iter().flat_map(|mutation| &mutation.changes).filter(|change| change.field == "content") {
        let asset: Asset = serde_json::from_value(change.value.get("asset").cloned().ok_or_else(|| invalid("mutation content has no asset"))?)
            .map_err(|_| invalid("invalid mutation asset description"))?;
        let asset = asset.rebound(&connection.id).map_err(invalid)?;
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

pub use crate::cloud_outcome::Outcome;
use crate::cloud_outcome::{HeadPosition, Publication};
/// A detach reads cloud state but never creates or publishes cloud objects.
pub(crate) fn receive_only(connection: &S3Connection, local: &SyncState, cancel: &AtomicBool) -> Result<Outcome> {
    if local.protocol == lightcraft_catalog::replication::SyncProtocol::AppendLog {
        return crate::cloud_append::native::receive_only(connection, local, cancel);
    }
    publication::ensure_head_protocol(connection, &local.library, cancel)?;
    let mut proofs = Default::default();
    let remote = read_with_witness(connection, &local.library, &mut proofs, cancel)?;
    let Some((head, _)) = remote.head else {
        if local.require_existing || local.generation > 0 {
            return Err(invalid("cloud library is absent; reconnect before turning sync off"));
        }
        return Ok(Outcome::empty(remote.state.portable()).with_publication(Publication::ReceiveOnly));
    };
    if head.generation < local.generation {
        return Err(invalid("cloud library rolled back; local work retained"));
    }
    let capability = Publication::head(Some(&head), &local.library).map_err(AssetError::Blocked)?.receiving();
    let state = crate::cloud_history::native::certify(connection, local, &remote.state.portable(), &mut proofs, cancel)?;
    Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?, BTreeSet::new()).with_publication(capability))
}
/// Reconcile each failed conditional head update by reading the competing history. Mutation
/// IDs and payloads never change across attempts, including uncertain publication responses.
pub fn synchronize(connection: &S3Connection, local: &SyncState, checkpoint_requested: bool, cancel: &AtomicBool) -> Result<Outcome> {
    synchronize_verified(connection, local, checkpoint_requested, cancel, &mut VerifiedMedia::default())
}
pub fn synchronize_with_audit(
    connection: &S3Connection,
    local: &SyncState,
    checkpoint_requested: bool,
    audit: bool,
    cancel: &AtomicBool,
) -> Result<Outcome> {
    synchronize_verified_audit(connection, local, checkpoint_requested, audit, cancel, &mut VerifiedMedia::default())
}
pub(crate) fn synchronize_verified(
    connection: &S3Connection,
    local: &SyncState,
    checkpoint_requested: bool,
    cancel: &AtomicBool,
    media: &mut VerifiedMedia,
) -> Result<Outcome> {
    synchronize_verified_audit(connection, local, checkpoint_requested, false, cancel, media)
}
fn synchronize_verified_audit(
    connection: &S3Connection,
    local: &SyncState,
    checkpoint_requested: bool,
    audit: bool,
    cancel: &AtomicBool,
    media: &mut VerifiedMedia,
) -> Result<Outcome> {
    if local.protocol == lightcraft_catalog::replication::SyncProtocol::AppendLog {
        return crate::cloud_append::native::synchronize_verified(connection, local, checkpoint_requested, audit, cancel, media);
    }
    publication::ensure_head_protocol(connection, &local.library, cancel)?;
    local.validate().map_err(catalog_error)?;
    if local.pending.is_empty() && !checkpoint_requested {
        let current = publication::read_head(connection, cancel)?;
        if let Some((head, _)) = current
            && local.cursor.as_deref() == Some(head.tip.hash.0.as_str())
        {
            if head.library.as_deref() != Some(local.library.as_str()) || head.generation != local.generation {
                return Err(invalid("remote library/cursor generation changed inconsistently"));
            }
            if head.legacy_append.is_none() && tail_length(&head) < AUTO_CHECKPOINT_BATCHES {
                let capability = Publication::head(Some(&head), &local.library).map_err(AssetError::Blocked)?;
                return Ok(Outcome::unchanged(local.portable(), HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?)
                    .with_publication(capability));
            }
        }
    }
    let mut dependency_proof = BTreeSet::new();
    let mut archive_proofs = lightcraft_catalog::replication::archive::Witness::new();
    let mut intake_checked = false;
    for _ in 0..64 {
        canceled(cancel)?;
        let mut remote = if checkpoint_requested {
            read_window(connection, &local.library, Some(RECOVERY_SEGMENT_BATCHES), &mut archive_proofs, cancel)?
        } else {
            read_incremental(connection, local, &mut archive_proofs, cancel)?
        };
        if local.require_existing && remote.head.is_none() {
            return Err(invalid("cannot join: remote library is absent; verify its destination and library ID; local work retained"));
        }
        if remote.head.as_ref().map_or(0, |(head, _)| head.generation) < local.generation {
            return Err(invalid("remote catalog rolled back below the acknowledged generation; local work retained"));
        }
        // Checked on every reread, against the exact head the CAS below will replace.
        let capability = Publication::head(remote.head.as_ref().map(|(head, _)| head), &local.library).map_err(AssetError::Blocked)?;
        if let Some((covers, generation)) = remote.recovery.take() {
            let Some((head, etag)) = &remote.head else {
                return Err(invalid("recovery prefix has no head"));
            };
            if let Publication::Blocked(blocked) = &capability {
                return Err(AssetError::Blocked(blocked.clone()));
            }
            let checkpoint = checkpoint(connection, &mut remote.state, covers, generation, epoch(head)?, cancel)?;
            // Each prefix becomes durable through CAS before replaying the next.
            // Cancellation/restart resumes from this published boundary, with all
            // local pending IDs unchanged. A competitor simply causes a new scan.
            publish(connection, Some((head, etag)), Vec::new(), Some(checkpoint), &local.library, cancel)?;
            continue;
        }
        // Recovery input has no authority until its checkpoint and scan are
        // committed together. Reread even after success or a lost CAS response.
        // One attempt per poll prevents a completed requested audit restarting
        // itself on every retry, and bounds work under competing publishers.
        if !intake_checked && capability.is_allowed() {
            intake_checked = true;
            match legacy::admit(connection, &remote, audit, cancel) {
                Ok(true) => continue,
                Err(error) if error.uncertain_write() => continue,
                Err(error) => return Err(error),
                Ok(false) => {}
            }
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
            return Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?, acknowledged)
                .with_publication(capability));
        }
        if let Publication::Blocked(blocked) = capability {
            // Receive-only: apply remote state, acknowledge what it already contains and
            // keep the rest pending until the library is publishable again.
            let Some((head, _)) = &remote.head else { return Err(AssetError::Blocked(blocked)) };
            let state = crate::cloud_history::native::certify(connection, local, &remote.state.portable(), &mut archive_proofs, cancel)?;
            let position = HeadPosition::new(head.tip.hash.0.clone(), head.generation).map_err(invalid)?;
            return Ok(Outcome::head_blocked(state, position, acknowledged, blocked));
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
            checkpoint_ref = Some(checkpoint(connection, &mut remote.state, head.tip.clone(), head.generation, epoch(head)?, cancel)?);
        }
        for mutation in &chunk {
            remote.state.receive(mutation).map_err(catalog_error)?;
        }
        publish(connection, remote.head.as_ref().map(|(head, etag)| (head, etag.as_str())), chunk, checkpoint_ref, &local.library, cancel)?;
    }
    Err(AssetError::Network("catalog remained busy or unreachable; durable mutations retained for retry".into()))
}

fn epoch(head: &Head) -> Result<u64> {
    Ok(head.control().map_err(invalid)?.epoch)
}
pub(crate) fn checkpoint(
    connection: &S3Connection,
    state: &mut SyncState,
    covers: RecordRef,
    generation: u64,
    epoch: u64,
    cancel: &AtomicBool,
) -> Result<CheckpointRef> {
    crate::cloud_history::native::compact(connection, state, false, cancel)?;
    let payload = Checkpoint { version: 1, generation, covers: covers.clone(), state: state.portable(), imports: BTreeMap::new() };
    let bytes = serde_json::to_vec(&payload).map_err(|_| invalid("could not encode checkpoint"))?;
    if bytes.len() > MAX_CHECKPOINT {
        return Err(invalid("checkpoint exceeds 64 MiB; local work retained"));
    }
    let hash = sha256(&bytes);
    let key = keys::in_library("", &crate::cloud_records::object_key(&connection.prefix, epoch, "checkpoints", &hash.0))?;
    let object = RecordRef { key, size: bytes.len() as u64, hash };
    immutable(connection, &object.key, &bytes, cancel, MAX_CHECKPOINT)?;
    Ok(CheckpointRef { object, covers, generation })
}
/// An ordinary commit: refuses unless the observed head is publishable, then carries
/// its control forward unchanged so the CAS also guards the maintenance state.
fn publish(
    connection: &S3Connection,
    previous: Option<(&Head, &str)>,
    mutations: Vec<Mutation>,
    checkpoint: Option<CheckpointRef>,
    library: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    let control = crate::cloud_records::publishable(previous.map(|(head, _)| head), library).map_err(|blocked| invalid(blocked.to_string()))?;
    let parent = previous.map(|(head, _)| head.tip.clone());
    match commit(connection, previous, parent, mutations, checkpoint, None, control, library, cancel) {
        Ok(_) => Ok(()),
        Err(error) if error.uncertain_write() => Ok(()),
        Err(error) => Err(error),
    }
}
/// Write one immutable batch in `control`'s epoch namespace, then conditionally replace
/// the head with it. Conflict/network outcomes are returned: ordinary publishers reread,
/// transitions reread and confirm their own control revision/token.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit(
    connection: &S3Connection,
    previous: Option<(&Head, &str)>,
    parent: Option<RecordRef>,
    mutations: Vec<Mutation>,
    checkpoint: Option<CheckpointRef>,
    generation: Option<u64>,
    control: Control,
    library: &str,
    cancel: &AtomicBool,
) -> Result<Head> {
    let batch = Batch { version: 3, parent, imports: Vec::new(), checkpoint_marker: mutations.is_empty(), mutations };
    let bytes = publication::encode(&batch)?;
    let tip = batch_ref(connection, control.epoch, &bytes)?;
    immutable(connection, &tip.key, &bytes, cancel, 2 << 20)?;
    let mut head = Head::next(previous.map(|(head, _)| head), tip, library, checkpoint, control).map_err(invalid)?;
    if let Some(generation) = generation {
        if previous.is_some_and(|(old, _)| generation <= old.generation) {
            return Err(invalid("head generation must increase"));
        }
        head.generation = generation;
    }
    head.validate_scope_typed(&connection.prefix, library).map_err(AssetError::Blocked)?;
    publication::write_head(connection, previous.map(|(_, token)| token), &head, cancel)?;
    Ok(head)
}
pub(crate) fn batch_ref(connection: &S3Connection, epoch: u64, bytes: &[u8]) -> Result<RecordRef> {
    let hash = sha256(bytes);
    let key = keys::in_library("", &crate::cloud_records::object_key(&connection.prefix, epoch, "changes", &hash.0))?;
    Ok(RecordRef { key, size: bytes.len() as u64, hash })
}
pub(crate) fn write_immutable(connection: &S3Connection, key: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
    immutable(connection, key, bytes, cancel, MAX_CHECKPOINT)
}
use crate::cloud_control::Control;

mod legacy;
mod worker;
pub(crate) use worker::Coordinator;
pub use worker::{Jobs, pause_publication, prepare_migration, resume_publication, start, start_migration, verify};
pub(crate) use worker::{request_poll, retry_after_reconnect};
