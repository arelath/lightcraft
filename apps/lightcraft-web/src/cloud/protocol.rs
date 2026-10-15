//! Browser orchestration of the shared bucket records and causal model. Every mutable
//! publication remains conditional, including response-loss recovery and URL refresh.
use super::transport::Provider;
use super::verify_blob;
use crate::backend::Backend;
use lightcraft_engine::assets::sha256;
use lightcraft_engine::catalog::assets::Asset;
use lightcraft_engine::catalog::replication::{Mutation, Portable, SyncState};
use lightcraft_engine::cloud_records::replay::{AUTO_CHECKPOINT_BATCHES, Cycle, WINDOW_BATCHES, Window};
use lightcraft_engine::cloud_records::{Batch, Checkpoint, CheckpointRef, Head, RecordRef};
use std::collections::{BTreeMap, BTreeSet};

use lightcraft_engine::cloud_outcome::HeadPosition;
pub use lightcraft_engine::cloud_outcome::Outcome;
struct Remote {
    head: Option<(Head, String)>,
    state: SyncState,
    tail_records: u64,
    recovery: Option<(RecordRef, u64)>,
}
fn encode(value: &impl serde::Serialize, max: usize) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > max {
        return Err("catalog object exceeds its bounded size; pending work retained".into());
    }
    Ok(bytes)
}
async fn object(provider: &Provider, reference: &RecordRef, max: usize) -> Result<Vec<u8>, String> {
    let reply = provider.request("GET", &reference.key, None, None, None, max).await?;
    if reply.status != 200 || reply.bytes.len() as u64 != reference.size || sha256(&reply.bytes) != reference.hash {
        return Err("remote catalog object is absent or failed SHA-256/length verification".into());
    }
    Ok(reply.bytes)
}
async fn read(provider: &Provider, library: &str, segmented: bool) -> Result<Remote, String> {
    let key = format!("{}/catalog/head.json", provider.connection.prefix);
    let reply = provider.request("GET", &key, None, None, None, 16 << 10).await?;
    let state = SyncState::new(library.into(), "remote".into(), provider.connection.id.clone()).map_err(|e| e.to_string())?;
    let mut remote = Remote { head: None, state, tail_records: 0, recovery: None };
    let mut proofs = lightcraft_engine::catalog::replication::archive::Witness::new();
    if reply.status == 404 {
        return Ok(remote);
    }
    if reply.status != 200 {
        return Err(format!("catalog head returned HTTP {}; conditional writes required", reply.status));
    }
    let etag = reply.etag.ok_or("catalog head has no exposed ETag; check bucket CORS")?;
    let head: Head = serde_json::from_slice(&reply.bytes).map_err(|_| "malformed catalog head")?;
    head.validate_scope(&provider.connection.prefix, library)?;
    let mut boundary = None;
    if let Some(reference) = &head.checkpoint {
        let checkpoint: Checkpoint =
            serde_json::from_slice(&object(provider, &reference.object, 64 << 20).await?).map_err(|_| "malformed checkpoint")?;
        if checkpoint.version != 1
            || checkpoint.generation != reference.generation
            || checkpoint.covers != reference.covers
            || checkpoint.imports.len() > 100_000
            || checkpoint.imports.iter().any(|(id, digest)| {
                !lightcraft_engine::catalog::assets::identifier(id)
                    || digest.len() != 64
                    || !digest.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            })
        {
            return Err("invalid checkpoint generation or import receipts".into());
        }
        if !checkpoint.imports.is_empty() {
            return Err("obsolete pre-release import checkpoint; use a fresh catalog and storage prefix; local data retained".into());
        }
        let verified = super::history::certify(provider, &remote.state, &checkpoint.state, &mut proofs).await?;
        remote.state.merge(&verified).map_err(|e| e.to_string())?;
        boundary = Some(reference.covers.clone());
    }
    let mut first = true;
    loop {
        let mut next = Some(head.tip.clone());
        let mut seen = Cycle::default();
        let mut window = Window::default();
        loop {
            let Some(reference) = next.take() else {
                if boundary.is_some() {
                    return Err("checkpoint boundary missing from history".into());
                }
                break;
            };
            if boundary.as_ref().is_some_and(|boundary| boundary.hash == reference.hash) {
                if boundary.as_ref() != Some(&reference) {
                    return Err("checkpoint boundary reference changed".into());
                }
                break;
            }
            seen.visit(&reference.hash)?;
            window.push(&reference)?;
            let batch: Batch = serde_json::from_slice(&object(provider, &reference, 2 << 20).await?).map_err(|_| "malformed change batch")?;
            batch.validate_scope(&provider.connection.prefix, library)?;
            next = batch.parent;
        }
        if first {
            remote.tail_records = window.count();
            first = false;
        }
        let budget = window.count().min(WINDOW_BATCHES);
        for _ in 0..budget {
            let reference = window.pop_oldest().ok_or("history ended before its verified prefix")?;
            let batch: Batch = serde_json::from_slice(&object(provider, &reference, 2 << 20).await?).map_err(|_| "malformed change batch")?;
            batch.validate_scope(&provider.connection.prefix, library)?;
            if batch.parent != boundary {
                return Err("history parent changed during replay".into());
            }
            boundary = Some(reference);
            for mutation in batch.mutations {
                if lightcraft_engine::catalog::replication::archive::covers(&remote.state.archives, &mutation.revision.id()) {
                    super::history::prove(provider, &remote.state, &mutation, &mut proofs).await?;
                } else {
                    remote.state.receive(&mutation).map_err(|e| e.to_string())?;
                }
            }
        }
        if budget >= window.count() {
            break;
        }
        if segmented {
            let generation = head
                .generation
                .checked_sub(window.count() - budget)
                .filter(|generation| *generation > 0)
                .ok_or("history generation does not cover its recovery prefix")?;
            remote.recovery = Some((boundary.ok_or("recovery prefix has no boundary")?, generation));
            break;
        }
    }
    remote.head = Some((head, etag));
    Ok(remote)
}
pub(super) async fn immutable(provider: &Provider, key: &str, bytes: &[u8]) -> Result<(), String> {
    let outcome = provider.request("PUT", key, None, Some("*"), Some(bytes), 16 << 10).await;
    match outcome {
        Ok(reply) if matches!(reply.status, 200 | 201 | 204) => Ok(()),
        Ok(reply) if reply.status != 412 => Err(format!("conditional object write returned HTTP {}; pending work retained", reply.status)),
        result => {
            let read = provider.request("GET", key, None, None, None, bytes.len()).await?;
            if read.status == 200 && read.bytes == bytes { Ok(()) } else { Err(result.err().unwrap_or_else(|| "immutable object collision".into())) }
        }
    }
}
pub(super) async fn dependencies(provider: &Provider, mutations: &[Mutation]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for change in mutations.iter().flat_map(|m| &m.changes).filter(|c| c.field == "content") {
        let mut asset: Asset = serde_json::from_value(change.value.get("asset").cloned().ok_or("missing immutable asset description")?)
            .map_err(|_| "invalid immutable asset")?;
        asset.original.remote.connection = provider.connection.id.clone();
        asset.validate()?;
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
            verify_blob(blob, &reply.bytes)?;
        }
    }
    Ok(())
}
pub async fn synchronize(
    provider: &Provider,
    _backend: &Backend,
    local: &SyncState,
    checkpoint_requested: bool,
    audit: bool,
) -> Result<Outcome, String> {
    if !local.protocol.is_head() {
        return super::append::synchronize(provider, local, checkpoint_requested, audit).await;
    }
    let marker = lightcraft_engine::cloud_append::manifest_key(&provider.connection.prefix, &local.library).map_err(|e| e.to_string())?;
    let existing = provider.request("GET", &marker, None, None, None, 16 << 10).await?;
    if existing.status == 200 {
        return Err("this library uses append mode; coordinate migration before continuing".into());
    }
    if existing.status != 404 {
        return Err("could not verify protocol compatibility; pending work retained".into());
    }
    local.validate().map_err(|e| e.to_string())?;
    let mut verified = BTreeSet::new();
    let mut archive_proofs = lightcraft_engine::catalog::replication::archive::Witness::new();
    for _ in 0..64 {
        let mut remote = read(provider, &local.library, true).await?;
        if local.require_existing && remote.head.is_none() {
            return Err("cannot join: remote library is absent; verify its destination and library ID; local work retained".into());
        }
        if remote.head.as_ref().map_or(0, |(head, _)| head.generation) < local.generation {
            return Err("remote catalog rolled back below the acknowledged generation; local work retained".into());
        }
        if let Some((covers, generation)) = remote.recovery.take() {
            let checkpoint = checkpoint(provider, &mut remote.state, covers, generation).await?;
            publish(provider, remote.head.as_ref().map(|(head, etag)| (head, etag.as_str())), Vec::new(), Some(checkpoint), &local.library).await?;
            continue;
        }
        let checkpoint_needed = checkpoint_requested || remote.tail_records >= AUTO_CHECKPOINT_BATCHES;
        super::history::hydrate(provider, &mut remote.state, local, &mut archive_proofs).await?;
        let mut pending = Vec::new();
        for mutation in local.pending.values() {
            if let Some(receipt) = remote.state.received.get(&mutation.revision.id()) {
                if receipt.digest != mutation.digest().map_err(|e| e.to_string())? {
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
            return Ok(
                if local.pending.is_empty()
                    && !checkpoint_requested
                    && local.cursor.as_deref() == Some(position.cursor())
                    && local.generation == position.generation()
                {
                    Outcome::unchanged(state, position)
                } else {
                    Outcome::head(state, position, local.pending.keys().cloned().collect())
                },
            );
        }
        let count = pending.len();
        let pending = lightcraft_engine::cloud_sync::publication_order(pending, &remote.state.clock).map_err(|e| e.to_string())?;
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
            checkpoint_ref = Some(checkpoint(provider, &mut remote.state, head.tip.clone(), head.generation).await?);
        }
        for mutation in &chunk {
            remote.state.receive(mutation).map_err(|e| e.to_string())?;
        }
        publish(provider, remote.head.as_ref().map(|(head, etag)| (head, etag.as_str())), chunk, checkpoint_ref, &local.library).await?;
    }
    Err("catalog busy or unreachable; pending work retained for retry".into())
}

pub(super) async fn migration_seed(provider: &Provider, library: &str) -> Result<Portable, String> {
    Ok(read(provider, library, false).await?.state.portable())
}

async fn checkpoint(provider: &Provider, state: &mut SyncState, covers: RecordRef, generation: u64) -> Result<CheckpointRef, String> {
    super::history::compact(provider, state, false).await?;
    let payload = Checkpoint { version: 1, generation, covers: covers.clone(), state: state.portable(), imports: BTreeMap::new() };
    let bytes = encode(&payload, 64 << 20)?;
    let hash = sha256(&bytes);
    let object = RecordRef { key: format!("{}/catalog/checkpoints/{}.json", provider.connection.prefix, hash.0), size: bytes.len() as u64, hash };
    immutable(provider, &object.key, &bytes).await?;
    Ok(CheckpointRef { object, covers, generation })
}
async fn publish(
    provider: &Provider,
    previous: Option<(&Head, &str)>,
    mutations: Vec<Mutation>,
    checkpoint: Option<CheckpointRef>,
    library: &str,
) -> Result<(), String> {
    let batch = Batch {
        version: 3,
        parent: previous.map(|(head, _)| head.tip.clone()),
        imports: Vec::new(),
        checkpoint_marker: mutations.is_empty(),
        mutations,
    };
    let bytes = encode(&batch, 2 << 20)?;
    let hash = sha256(&bytes);
    let tip = RecordRef { key: format!("{}/catalog/changes/{}.json", provider.connection.prefix, hash.0), size: bytes.len() as u64, hash };
    immutable(provider, &tip.key, &bytes).await?;
    let generation = previous.map_or(Ok(1), |(head, _)| head.generation.checked_add(1).ok_or("catalog generation exhausted"))?;
    let head = Head { version: 3, generation, tip, library: Some(library.into()), checkpoint };
    let bytes = encode(&head, 16 << 10)?;
    match provider
        .request(
            "PUT",
            &format!("{}/catalog/head.json", provider.connection.prefix),
            None,
            Some(previous.map_or("*", |(_, etag)| etag)),
            Some(&bytes),
            16 << 10,
        )
        .await
    {
        Ok(reply) if matches!(reply.status, 200 | 201 | 204 | 412) => Ok(()),
        Err(_) => Ok(()),
        Ok(reply) => Err(format!("conditional head write returned HTTP {}; pending work retained", reply.status)),
    }
}
