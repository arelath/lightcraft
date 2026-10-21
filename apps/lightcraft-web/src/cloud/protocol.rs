//! Browser orchestration of the shared bucket records and causal model. Every mutable
//! publication remains conditional, including response-loss recovery and URL refresh.
use super::transport::Provider;
use super::verify_blob;
use crate::backend::Backend;
use lightcraft_engine::assets::sha256;
use lightcraft_engine::catalog::assets::Asset;
use lightcraft_engine::catalog::replication::{Mutation, Portable, SyncState};
use lightcraft_engine::cloud_outcome::Publication;
use lightcraft_engine::cloud_records::replay::{AUTO_CHECKPOINT_BATCHES, BodyWindow, Cycle};
use lightcraft_engine::cloud_records::{Batch, Checkpoint, CheckpointRef, Head, RecordRef};
use lightcraft_engine::cloud_status::{CloudFailure, FailureKind};
use std::collections::{BTreeMap, BTreeSet};

use lightcraft_engine::cloud_outcome::HeadPosition;
pub use lightcraft_engine::cloud_outcome::Outcome;
pub(super) struct Remote {
    pub(super) head: Option<(Head, String)>,
    pub(super) state: SyncState,
    tail_records: u64,
    pub(super) recovery: Option<(RecordRef, u64)>,
}
pub(super) fn encode(value: &impl serde::Serialize, max: usize) -> Result<Vec<u8>, CloudFailure> {
    let bytes = serde_json::to_vec(value).map_err(CloudFailure::from)?;
    if bytes.len() > max {
        return Err(CloudFailure::new(
            lightcraft_engine::cloud_status::FailureKind::Capacity,
            "catalog object exceeds its bounded size; pending work retained",
        ));
    }
    Ok(bytes)
}
pub(super) async fn object(provider: &Provider, reference: &RecordRef, max: usize) -> Result<Vec<u8>, CloudFailure> {
    provider.catalog_record(reference, max).await
}
pub(super) async fn read(
    provider: &Provider,
    library: &str,
    segmented: bool,
    proofs: &mut lightcraft_engine::catalog::replication::archive::Witness,
) -> Result<Remote, CloudFailure> {
    let fresh = provider.fresh_provider();
    read_cached(&fresh, library, segmented, proofs, None).await
}
async fn read_cached(
    provider: &Provider,
    library: &str,
    segmented: bool,
    proofs: &mut lightcraft_engine::catalog::replication::archive::Witness,
    baseline: Option<&super::baseline::Baseline>,
) -> Result<Remote, CloudFailure> {
    let observed = read_head(provider, library).await?;
    let Some((head, etag)) = observed else {
        return Ok(Remote {
            head: None,
            state: SyncState::new(library.into(), "remote".into(), provider.connection.id.clone()).map_err(CloudFailure::from)?,
            tail_records: 0,
            recovery: None,
        });
    };
    let seed = if !provider.fresh_history { baseline } else { None };
    if let Some(known) = seed {
        let state = known.state(provider)?;
        if head.generation < state.generation {
            return Err(CloudFailure::integrity("remote catalog rolled back below the acknowledged generation"));
        }
        if head.tip.hash == known.tip.hash && (head.tip != known.tip || head.generation != state.generation || head.checkpoint != known.checkpoint) {
            return Err(CloudFailure::integrity("remote history reference changed at the acknowledged head; local work retained"));
        }
        if head.checkpoint == known.checkpoint
            && let Some(remote) = replay_observed(provider, library, &head, &etag, segmented, proofs, Some(known)).await?
        {
            return Ok(remote);
        }
    }
    replay_observed(provider, library, &head, &etag, segmented, proofs, None)
        .await?
        .ok_or_else(|| CloudFailure::integrity("checkpoint boundary missing from history"))
}
async fn replay_observed(
    provider: &Provider,
    library: &str,
    head: &Head,
    etag: &str,
    segmented: bool,
    proofs: &mut lightcraft_engine::catalog::replication::archive::Witness,
    baseline: Option<&super::baseline::Baseline>,
) -> Result<Option<Remote>, CloudFailure> {
    let state = SyncState::new(library.into(), "remote".into(), provider.connection.id.clone()).map_err(CloudFailure::from)?;
    let mut remote = Remote {
        head: Some((head.clone(), etag.into())),
        state,
        tail_records: head.generation.saturating_sub(head.checkpoint.as_ref().map_or(0, |c| c.generation)),
        recovery: None,
    };
    let mut boundary = None;
    if let Some(known) = baseline {
        remote.state = known.state(provider)?;
        boundary = Some(known.tip.clone());
    } else if let Some(reference) = &head.checkpoint {
        let checkpoint: Checkpoint =
            serde_json::from_slice(&object(provider, &reference.object, 64 << 20).await?).map_err(|_| "malformed checkpoint")?;
        if checkpoint.version != 1
            || checkpoint.generation != reference.generation
            || checkpoint.covers != reference.covers
            || !checkpoint.imports.is_empty()
        {
            return Err(CloudFailure::integrity("invalid or obsolete checkpoint; local data retained"));
        }
        let verified = super::history::certify(provider, &remote.state, &checkpoint.state, proofs).await?;
        remote.state.merge(&verified).map_err(CloudFailure::from)?;
        boundary = Some(reference.covers.clone());
    }
    loop {
        let mut next = Some(head.tip.clone());
        let mut seen = Cycle::default();
        let mut window = BodyWindow::default();
        loop {
            let Some(reference) = next.take() else {
                if boundary.is_some() {
                    return if baseline.is_some() { Ok(None) } else { Err(CloudFailure::integrity("checkpoint boundary missing from history")) };
                }
                break;
            };
            if boundary.as_ref().is_some_and(|b| b.hash == reference.hash) {
                if boundary.as_ref() != Some(&reference) {
                    return Err(CloudFailure::integrity("history boundary reference changed"));
                }
                break;
            }
            seen.visit(&reference.hash)?;
            let bytes = object(provider, &reference, 2 << 20).await?;
            let batch: Batch = serde_json::from_slice(&bytes).map_err(|_| "malformed change batch")?;
            batch.validate_at(&provider.connection.prefix, library, &reference)?;
            next = batch.parent;
            window.push(&reference, bytes)?;
            if window.count() % 64 == 0 {
                provider.history_turn().await?;
            }
        }
        let budget = window.retained();
        let count = window.count();
        for index in 0..budget {
            if index % 64 == 0 {
                provider.history_turn().await?;
            }
            let (reference, bytes) = window.pop_oldest().ok_or("history ended before its verified prefix")?;
            let batch: Batch = serde_json::from_slice(&bytes).map_err(|_| "malformed spooled change batch")?;
            batch.validate_at(&provider.connection.prefix, library, &reference)?;
            if batch.parent != boundary {
                return Err(CloudFailure::integrity("history parent changed during replay"));
            }
            boundary = Some(reference);
            for mutation in batch.mutations {
                if lightcraft_engine::catalog::replication::archive::covers(&remote.state.archives, &mutation.revision.id()) {
                    super::history::prove(provider, &remote.state, &mutation, proofs).await?;
                } else {
                    remote.state.receive(&mutation).map_err(CloudFailure::from)?;
                }
            }
        }
        if budget >= count {
            break;
        }
        if segmented {
            let minimum = baseline.map_or(head.checkpoint.as_ref().map_or(0, |c| c.generation), |known| known.generation());
            let generation = head
                .generation
                .checked_sub(count - budget)
                .filter(|g| *g > minimum)
                .ok_or("history generation does not cover its recovery prefix")?;
            remote.recovery = Some((boundary.ok_or("recovery prefix has no boundary")?, generation));
            break;
        }
    }
    Ok(Some(remote))
}

pub(super) async fn read_head(provider: &Provider, library: &str) -> Result<Option<(Head, String)>, CloudFailure> {
    if b2_enabled(provider).await? {
        let snapshot = provider
            .b2_read(false)
            .await?
            .ok_or_else(|| CloudFailure::from(lightcraft_engine::cloud_control::Blocked::FinishUpgrade("B2 initialization".into())))?;
        snapshot.head.control()?.may_read()?;
        snapshot.head.validate_scope_typed(&provider.connection.prefix, library)?;
        if snapshot.head.legacy_append.is_none() {
            return Err(CloudFailure::integrity("B2 authority lost its original append binding"));
        }
        return Ok(Some((*snapshot.head, snapshot.token)));
    }
    let key = format!("{}/catalog/head.json", provider.connection.prefix);
    let reply = provider.request("GET", &key, None, None, None, 16 << 10).await?;
    if reply.status == 404 {
        return Ok(None);
    }
    if reply.status != 200 {
        return Err(CloudFailure::http("catalog head", reply.status));
    }
    let etag = reply.etag.ok_or("catalog head has no exposed ETag; check bucket CORS")?;
    let head = Head::decode(&reply.bytes)?;
    if head.legacy_append.is_some() {
        return Err(CloudFailure::new(FailureKind::Unsupported, "this library needs its configured B2 control service; pending work retained")
            .with_remedy(lightcraft_engine::cloud_status::Recovery::EditConnection));
    }
    head.validate_scope_typed(&provider.connection.prefix, library)?;
    Ok(Some((head, etag)))
}
pub(super) async fn b2_enabled(provider: &Provider) -> Result<bool, CloudFailure> {
    if !provider.connection.supports_b2_authority() {
        return Ok(false);
    }
    let reply = provider.request("GET", &format!("{}/catalog/b2-authority.json", provider.connection.prefix), None, None, None, 4096).await?;
    match reply.status {
        200 => Ok(true),
        404 => Ok(false),
        status => Err(CloudFailure::http("library authority descriptor", status)),
    }
}
pub(super) fn retry_cas(failure: &CloudFailure) -> bool {
    failure.retryable() && failure.retry_after_ms.is_none()
}
/// A control token selects exactly one authority. Never fall back from B2 CAS to
/// a raw S3 head after an error, or after a descriptor appeared meanwhile.
pub(super) async fn write_head(provider: &Provider, expected: Option<&str>, head: &Head) -> Result<(), CloudFailure> {
    if let Some(token) = expected.filter(|token| token.starts_with("b2:")) {
        return provider.b2_write(token, head).await;
    }
    if b2_enabled(provider).await? {
        return Err(CloudFailure::new(FailureKind::Unsupported, "this library's publication authority changed; rejoin it before publishing")
            .with_remedy(lightcraft_engine::cloud_status::Recovery::Rejoin));
    }
    let bytes = encode(head, 16 << 10)?;
    let reply = provider
        .request("PUT", &format!("{}/catalog/head.json", provider.connection.prefix), None, Some(expected.unwrap_or("*")), Some(&bytes), 16 << 10)
        .await?;
    match reply.status {
        200 | 201 | 204 => Ok(()),
        status => Err(CloudFailure::http("conditional head write", status)),
    }
}
pub(super) async fn immutable(provider: &Provider, key: &str, bytes: &[u8]) -> Result<(), CloudFailure> {
    if provider.connection.supports_b2_authority() {
        return match provider.connection.b2_immutable_kind(key, bytes)? {
            super::ImmutableKind::Catalog => provider.put_catalog(key, bytes).await,
            super::ImmutableKind::Append => provider.put_immutable(key, bytes).await,
        };
    }
    let outcome = provider.request("PUT", key, None, Some("*"), Some(bytes), 16 << 10).await;
    match outcome {
        Ok(reply) if matches!(reply.status, 200 | 201 | 204) => Ok(()),
        Ok(reply) if reply.status != 412 => Err(CloudFailure::http("conditional object write", reply.status)),
        Err(error) if !error.retryable() => Err(error),
        result => {
            let read = provider.request("GET", key, None, None, None, bytes.len()).await?;
            if read.status == 200 && read.bytes == bytes {
                Ok(())
            } else if read.status == 200 {
                Err(CloudFailure::integrity("immutable object collision"))
            } else {
                Err(result.err().unwrap_or_else(|| CloudFailure::http("immutable object readback", read.status)))
            }
        }
    }
}
pub(super) async fn dependencies(provider: &Provider, mutations: &[Mutation]) -> Result<(), CloudFailure> {
    let mut seen = BTreeSet::new();
    for change in mutations.iter().flat_map(|m| &m.changes).filter(|c| c.field == "content") {
        let asset: Asset = serde_json::from_value(change.value.get("asset").cloned().ok_or("missing immutable asset description")?)
            .map_err(|_| "invalid immutable asset")?;
        let asset = asset.rebound(&provider.connection.id)?;
        for blob in asset.blobs() {
            if !seen.insert((blob.id.clone(), blob.remote.key.clone(), blob.remote.version.clone())) {
                continue;
            }
            if blob.size > super::MAX_BROWSER_BLOB {
                return Err("referenced original exceeds the browser transfer limit".into());
            }
            let reply = provider.request("GET", &blob.remote.key, blob.remote.version.as_deref(), None, None, blob.size as usize).await?;
            if reply.status != 200 {
                return Err("required cloud dependency is not stored; upload it before publishing metadata".into());
            }
            verify_blob(blob, &reply.bytes).map_err(CloudFailure::integrity)?;
        }
    }
    Ok(())
}
pub async fn synchronize(
    provider: &Provider,
    backend: &Backend,
    local: &SyncState,
    checkpoint_requested: bool,
    audit: bool,
) -> Result<Outcome, CloudFailure> {
    if !local.protocol.is_head() {
        return super::append::synchronize(provider, local, checkpoint_requested, audit).await;
    }
    let marker = lightcraft_engine::cloud_append::manifest_key(&provider.connection.prefix, &local.library).map_err(CloudFailure::from)?;
    let existing = provider.request("GET", &marker, None, None, None, 16 << 10).await?;
    if existing.status == 200 {
        let upgraded = read_head(provider, &local.library).await?;
        if !upgraded.as_ref().is_some_and(|(head, token)| token.starts_with("b2:") && head.legacy_append.is_some()) {
            return Err(CloudFailure::from(lightcraft_engine::assets::AssetError::Moved(
                "this library moved to append synchronization; rejoin it before continuing".into(),
            )));
        }
        if existing.bytes != lightcraft_engine::cloud_append::manifest(&local.library).map_err(CloudFailure::from)? {
            return Err(CloudFailure::integrity("the original append manifest changed"));
        }
    }
    if !matches!(existing.status, 200 | 404) {
        return Err("could not verify protocol compatibility; pending work retained".into());
    }
    local.validate().map_err(CloudFailure::from)?;
    let mut verified = BTreeSet::new();
    let mut archive_proofs = lightcraft_engine::catalog::replication::archive::Witness::new();
    let mut baseline = if checkpoint_requested { None } else { super::baseline::Baseline::load(backend, provider, local).await };
    let mut legacy_attempted = false;
    for _ in 0..64 {
        let mut remote = read_cached(provider, &local.library, true, &mut archive_proofs, baseline.as_ref()).await?;
        if local.require_existing && remote.head.is_none() {
            return Err("cannot join: remote library is absent; verify its destination and library ID; local work retained".into());
        }
        if remote.head.as_ref().map_or(0, |(head, _)| head.generation) < local.generation {
            return Err("remote catalog rolled back below the acknowledged generation; local work retained".into());
        }
        // Checked on every reread, against the exact head the CAS below will replace.
        let capability = Publication::head(remote.head.as_ref().map(|(head, _)| head), &local.library)?;
        if let Some((covers, generation)) = remote.recovery.take() {
            if let Publication::Blocked(blocked) = &capability {
                return Err(blocked.clone().into());
            }
            let epoch = remote.head.as_ref().map_or(Ok(1), |(head, _)| head.control().map(|c| c.epoch))?;
            let checkpoint = checkpoint(provider, &mut remote.state, covers, generation, epoch).await?;
            publish(provider, remote.head.as_ref().map(|(head, etag)| (head, etag.as_str())), Vec::new(), Some(checkpoint), &local.library).await?;
            continue;
        }
        if !legacy_attempted && capability.is_allowed() && remote.head.as_ref().is_some_and(|(h, _)| h.legacy_append.is_some()) {
            legacy_attempted = true;
            match super::legacy::admit(provider, &remote, audit).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(failure) if retry_cas(&failure) => continue,
                Err(failure) => return Err(failure),
            }
        }
        let checkpoint_needed = checkpoint_requested || remote.tail_records >= AUTO_CHECKPOINT_BATCHES;
        super::history::hydrate(provider, &mut remote.state, local, &mut archive_proofs).await?;
        if !checkpoint_requested && !provider.fresh_history {
            baseline = super::baseline::Baseline::observed(provider, &remote);
        }
        let mut pending = Vec::new();
        for mutation in local.pending.values() {
            if let Some(receipt) = remote.state.received.get(&mutation.revision.id()) {
                if receipt.digest != mutation.digest().map_err(CloudFailure::from)? {
                    return Err("remote mutation identity collision".into());
                }
            } else {
                pending.push(mutation.clone());
            }
        }
        if pending.is_empty()
            && (!checkpoint_needed
                || remote.head.as_ref().is_none_or(|(h, _)| h.checkpoint.as_ref().is_some_and(|c| c.generation.saturating_add(1) == h.generation)))
        {
            let (head, _) = remote.head.ok_or("empty remote library; initial metadata must be queued")?;
            let state = super::history::certify(provider, local, &remote.state.portable(), &mut archive_proofs).await?;
            let position = HeadPosition::new(head.tip.hash.0, head.generation)?;
            if let Some(known) = &baseline {
                known.save(backend).await;
            }
            return Ok(
                if local.pending.is_empty()
                    && !checkpoint_requested
                    && local.cursor.as_deref() == Some(position.cursor())
                    && local.generation == position.generation()
                {
                    Outcome::unchanged(state, position).with_publication(capability)
                } else {
                    Outcome::head(state, position, local.pending.keys().cloned().collect()).with_publication(capability)
                },
            );
        }
        if let Publication::Blocked(blocked) = capability {
            // Receive-only: apply remote state, keep pending work until publishable again.
            let (head, _) = remote.head.ok_or_else(|| CloudFailure::from(blocked.clone()))?;
            let state = super::history::certify(provider, local, &remote.state.portable(), &mut archive_proofs).await?;
            let acknowledged = local.pending.keys().filter(|id| remote.state.received.contains_key(*id)).cloned().collect();
            if let Some(known) = &baseline {
                known.save(backend).await;
            }
            return Ok(Outcome::head_blocked(state, HeadPosition::new(head.tip.hash.0, head.generation)?, acknowledged, blocked));
        }
        let count = pending.len();
        let pending = lightcraft_engine::cloud_sync::publication_order(pending, &remote.state.clock).map_err(CloudFailure::from)?;
        let mut chunk = Vec::new();
        for mutation in pending.into_iter().take(256) {
            chunk.push(mutation);
            if encode(
                &Batch {
                    version: 3,
                    parent: remote.head.as_ref().map(|(h, _)| h.tip.clone()),
                    imports: Vec::new(),
                    mutations: chunk.clone(),
                    checkpoint_marker: false,
                },
                2 << 20,
            )
            .is_err()
            {
                chunk.pop();
                break;
            }
        }
        if count > 0 && chunk.is_empty() {
            return Err("pending mutation cannot fit the bounded change batch".into());
        }
        let newly_referenced: Vec<_> = chunk.iter().filter(|m| !verified.contains(&m.revision.id())).cloned().collect();
        dependencies(provider, &newly_referenced).await?;
        verified.extend(newly_referenced.iter().map(|m| m.revision.id()));
        let mut checkpoint_ref = remote.head.as_ref().and_then(|(head, _)| head.checkpoint.clone());
        if checkpoint_needed && let Some((head, _)) = &remote.head {
            let epoch = head.control()?.epoch;
            checkpoint_ref = Some(checkpoint(provider, &mut remote.state, head.tip.clone(), head.generation, epoch).await?);
        }
        for mutation in &chunk {
            remote.state.receive(mutation).map_err(CloudFailure::from)?;
        }
        publish(provider, remote.head.as_ref().map(|(head, etag)| (head, etag.as_str())), chunk, checkpoint_ref, &local.library).await?;
    }
    Err(CloudFailure::new(FailureKind::Contention, "catalog busy or unreachable; pending work retained for retry"))
}

pub(super) async fn migration_seed(
    provider: &Provider,
    library: &str,
    proofs: &mut lightcraft_engine::catalog::replication::archive::Witness,
) -> Result<Portable, CloudFailure> {
    Ok(read(provider, library, false, proofs).await?.state.portable())
}

pub(super) async fn checkpoint(
    provider: &Provider,
    state: &mut SyncState,
    covers: RecordRef,
    generation: u64,
    epoch: u64,
) -> Result<CheckpointRef, CloudFailure> {
    super::history::compact(provider, state, false).await?;
    let payload = Checkpoint { version: 1, generation, covers: covers.clone(), state: state.portable(), imports: BTreeMap::new() };
    let bytes = encode(&payload, 64 << 20)?;
    let hash = sha256(&bytes);
    let key = lightcraft_engine::cloud_records::object_key(&provider.connection.prefix, epoch, "checkpoints", &hash.0);
    let object = RecordRef { key, size: bytes.len() as u64, hash };
    immutable(provider, &object.key, &bytes).await?;
    Ok(CheckpointRef { object, covers, generation })
}
/// An ordinary commit: refuses unless the observed head is publishable, then carries
/// its control forward unchanged so the CAS also guards the maintenance state.
async fn publish(
    provider: &Provider,
    previous: Option<(&Head, &str)>,
    mutations: Vec<Mutation>,
    checkpoint: Option<CheckpointRef>,
    library: &str,
) -> Result<(), CloudFailure> {
    let control = lightcraft_engine::cloud_records::publishable(previous.map(|(head, _)| head), library)?;
    let batch = Batch {
        version: 3,
        parent: previous.map(|(head, _)| head.tip.clone()),
        imports: Vec::new(),
        checkpoint_marker: mutations.is_empty(),
        mutations,
    };
    let bytes = encode(&batch, 2 << 20)?;
    let hash = sha256(&bytes);
    let key = lightcraft_engine::cloud_records::object_key(&provider.connection.prefix, control.epoch, "changes", &hash.0);
    let tip = RecordRef { key, size: bytes.len() as u64, hash };
    immutable(provider, &tip.key, &bytes).await?;
    let head = Head::next(previous.map(|(head, _)| head), tip, library, checkpoint, control)?;
    match write_head(provider, previous.map(|(_, token)| token), &head).await {
        Err(error) if retry_cas(&error) => Ok(()),
        result => result,
    }
}

/// Receive the verified remote state without publishing (after an upgrade transition).
pub(super) async fn receive_only(provider: &Provider, local: &SyncState) -> Result<Outcome, CloudFailure> {
    let mut proofs = lightcraft_engine::catalog::replication::archive::Witness::new();
    let remote = read(provider, &local.library, false, &mut proofs).await?;
    let (head, _) = remote.head.ok_or("cloud library has no head")?;
    let capability = Publication::head(Some(&head), &local.library)?;
    let state = super::history::certify(provider, local, &remote.state.portable(), &mut proofs).await?;
    let acknowledged = local.pending.keys().filter(|id| remote.state.received.contains_key(*id)).cloned().collect();
    Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation)?, acknowledged).with_publication(capability))
}
