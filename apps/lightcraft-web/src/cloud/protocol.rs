//! Browser orchestration of the shared bucket records and causal model. Every mutable
//! publication remains conditional, including response-loss recovery and URL refresh.
use super::transport::Provider;
use super::verify_blob;
use crate::backend::Backend;
use lightcraft_engine::assets::sha256;
use lightcraft_engine::catalog::assets::Asset;
use lightcraft_engine::catalog::replication::{Mutation, Portable, Stamp, SyncState};
use lightcraft_engine::cloud_records::{Batch, Checkpoint, CheckpointRef, Head, Imported, RecordRef};
use std::collections::{BTreeMap, BTreeSet};

pub struct Outcome {
    pub state: Portable,
    pub acknowledged: BTreeSet<String>,
    pub cursor: String,
    pub generation: u64,
}
struct Remote {
    head: Option<(Head, String)>,
    state: SyncState,
    imports: BTreeMap<String, String>,
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
fn legacy(remote: &mut Remote, imported: &Imported) -> Result<(), String> {
    let digest = sha256(&encode(imported, 2 << 20)?).0;
    if let Some(old) = remote.imports.get(&imported.mutation_id) {
        if old != &digest {
            return Err("legacy import identity collision".into());
        }
        return Ok(());
    }
    let changes = lightcraft_engine::cloud_sync::legacy_changes(imported, &remote.state).map_err(|e| e.to_string())?;
    let device = format!(
        "{}_{}",
        if imported.base_asset.is_some() { "legacy" } else { "baseline" },
        sha256(format!("legacy-import-v1/{}", imported.mutation_id).as_bytes()).0
    );
    let revision = Stamp { device, sequence: 1 };
    let existing = remote.state.received.get(&revision.id());
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
    remote.state.receive(&Mutation { version: 1, library: remote.state.library.clone(), revision, context, changes }).map_err(|e| e.to_string())?;
    remote.imports.insert(imported.mutation_id.clone(), digest);
    Ok(())
}
async fn read(provider: &Provider, library: &str) -> Result<Remote, String> {
    let key = format!("{}/catalog/head.json", provider.connection.prefix);
    let reply = provider.request("GET", &key, None, None, None, 16 << 10).await?;
    let state = SyncState::new(library.into(), "remote".into(), provider.connection.id.clone()).map_err(|e| e.to_string())?;
    let mut remote = Remote { head: None, state, imports: BTreeMap::new() };
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
        remote.state.merge(&checkpoint.state).map_err(|e| e.to_string())?;
        remote.imports = checkpoint.imports;
        boundary = Some(reference.covers.hash.clone());
    }
    let mut next = Some(head.tip.clone());
    let mut seen = BTreeSet::new();
    let mut batches = Vec::new();
    let mut complete = false;
    for _ in 0..4096 {
        let Some(reference) = next.take() else {
            if boundary.is_some() {
                return Err("checkpoint boundary missing from history".into());
            }
            complete = true;
            break;
        };
        if boundary.as_ref() == Some(&reference.hash) {
            complete = true;
            break;
        }
        if !seen.insert(reference.hash.clone()) {
            return Err("catalog history contains a cycle".into());
        }
        let batch: Batch = serde_json::from_slice(&object(provider, &reference, 2 << 20).await?).map_err(|_| "malformed change batch")?;
        batch.validate_scope(&provider.connection.prefix, library)?;
        next = batch.parent.clone();
        batches.push(batch);
    }
    if !complete {
        return Err("catalog tail exceeds 4096 records; checkpoint required".into());
    }
    remote.head = Some((head, etag));
    for batch in batches.into_iter().rev() {
        for imported in batch.imports {
            legacy(&mut remote, &imported)?;
        }
        for mutation in batch.mutations {
            remote.state.receive(&mutation).map_err(|e| e.to_string())?;
        }
    }
    Ok(remote)
}
async fn immutable(provider: &Provider, key: &str, bytes: &[u8]) -> Result<(), String> {
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
async fn dependencies(provider: &Provider, mutations: &[Mutation]) -> Result<(), String> {
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
pub async fn synchronize(provider: &Provider, _backend: &Backend, local: &SyncState, checkpoint_requested: bool) -> Result<Outcome, String> {
    local.validate().map_err(|e| e.to_string())?;
    let mut verified = BTreeSet::new();
    for _ in 0..64 {
        let mut remote = read(provider, &local.library).await?;
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
            && (!checkpoint_requested
                || remote.head.as_ref().is_none_or(|(h, _)| h.checkpoint.as_ref().is_some_and(|c| c.generation.saturating_add(1) == h.generation)))
        {
            let (head, _) = remote.head.ok_or("empty remote library; initial metadata must be queued")?;
            return Ok(Outcome {
                state: remote.state.portable(),
                acknowledged: local.pending.keys().cloned().collect(),
                cursor: head.tip.hash.0,
                generation: head.generation,
            });
        }
        let count = pending.len();
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
        let mut checkpoint = remote.head.as_ref().and_then(|(h, _)| h.checkpoint.clone());
        let parent = remote.head.as_ref().map(|(h, _)| h.tip.clone());
        if checkpoint_requested && let Some((head, _)) = &remote.head {
            let payload = Checkpoint {
                version: 1,
                generation: head.generation,
                covers: head.tip.clone(),
                state: remote.state.portable(),
                imports: remote.imports.clone(),
            };
            let bytes = encode(&payload, 64 << 20)?;
            let hash = sha256(&bytes);
            let object =
                RecordRef { key: format!("{}/catalog/checkpoints/{}.json", provider.connection.prefix, hash.0), size: bytes.len() as u64, hash };
            immutable(provider, &object.key, &bytes).await?;
            checkpoint = Some(CheckpointRef { object, covers: head.tip.clone(), generation: head.generation });
        }
        for mutation in &chunk {
            remote.state.receive(mutation).map_err(|e| e.to_string())?;
        }
        let checkpoint_marker = chunk.is_empty();
        let bytes = encode(&Batch { version: 3, parent, imports: Vec::new(), mutations: chunk, checkpoint_marker }, 2 << 20)?;
        let hash = sha256(&bytes);
        let tip = RecordRef { key: format!("{}/catalog/changes/{}.json", provider.connection.prefix, hash.0), size: bytes.len() as u64, hash };
        immutable(provider, &tip.key, &bytes).await?;
        let generation = remote.head.as_ref().map_or(Ok(1), |(h, _)| h.generation.checked_add(1).ok_or("catalog generation exhausted"))?;
        let head = Head { version: 2, generation, tip, library: Some(local.library.clone()), checkpoint };
        let condition = remote.head.as_ref().map_or("*", |(_, etag)| etag.as_str());
        let key = format!("{}/catalog/head.json", provider.connection.prefix);
        let bytes = encode(&head, 16 << 10)?;
        match provider.request("PUT", &key, None, Some(condition), Some(&bytes), 16 << 10).await {
            Ok(reply) if matches!(reply.status, 200 | 201 | 204 | 412) => continue,
            Err(_) => continue,
            Ok(reply) => return Err(format!("conditional head write returned HTTP {}; pending work retained", reply.status)),
        }
    }
    Err("catalog busy or unreachable; pending work retained for retry".into())
}
