//! Browser transport for the shared append codecs, scheduler and exact publication receipts.
use super::{Provider, protocol::Outcome};
use core::listing::ListRequest;
use lightcraft_engine::{assets::sha256, catalog::replication::SyncState, cloud_append as core};
use std::collections::BTreeSet;
fn ce(e: lightcraft_engine::assets::AssetError) -> String {
    e.to_string()
}
fn list(prefix: String, delimiter: bool) -> ListRequest {
    ListRequest { prefix, delimiter, continuation: None, start_after: None }
}
async fn scan(provider: &Provider, mut request: ListRequest) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let mut keys = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    for _ in 0..core::MAX_RECORDS {
        let page = provider.list(&request).await?;
        keys.extend(page.keys);
        prefixes.extend(page.prefixes);
        if keys.len().saturating_add(prefixes.len()) > core::MAX_RECORDS {
            return Err("append listing limit exceeded".into());
        }
        let Some(next) = page.next else { return Ok((keys, prefixes)) };
        if !tokens.insert(next.clone()) {
            return Err("repeated continuation token".into());
        }
        request.continuation = Some(next);
        request.start_after = None;
    }
    Err("too many append listing pages".into())
}
async fn get(provider: &Provider, key: &str, limit: usize) -> Result<Vec<u8>, String> {
    let r = provider.request("GET", key, None, None, None, limit).await?;
    if r.status != 200 {
        return Err(format!("append record returned HTTP {}; local work retained", r.status));
    }
    Ok(r.bytes)
}
pub async fn synchronize(provider: &Provider, local: &SyncState, checkpoint: bool, audit: bool) -> Result<Outcome, String> {
    if provider.connection.bucket_root.is_none() {
        return Err("append mode requires a configured bucketRoot and a signer supporting list/putImmutable".into());
    }
    let root = core::root(&provider.connection.prefix, &local.library).map_err(ce)?;
    let manifest_key = core::manifest_key(&provider.connection.prefix, &local.library).map_err(ce)?;
    let manifest = core::manifest(&local.library).map_err(ce)?;
    if local.require_existing {
        let existing = provider.request("GET", &manifest_key, None, None, None, 16 << 10).await?;
        if existing.status == 404 {
            return Err("cannot join: append library is absent; verify its destination and library ID; local work retained".into());
        }
        if existing.status != 200 || existing.bytes != manifest {
            return Err("append manifest could not be verified; local work retained".into());
        }
    } else {
        provider.put_immutable(&manifest_key, &manifest).await?;
    }
    let audit = audit || checkpoint || local.append.is_none();
    let mut planner = core::Planner::new(local, audit).map_err(ce)?;
    let (checkpoints, _) = scan(provider, list(format!("{root}/checkpoints/"), false)).await?;
    for key in checkpoints {
        let bytes = get(provider, &key, core::MAX_OBJECT).await?;
        core::immutable_metadata(&key, &bytes).map_err(ce)?;
        let p: lightcraft_engine::catalog::replication::AppendProgress = serde_json::from_slice(&bytes).map_err(|_| "malformed append checkpoint")?;
        p.validate(&local.library).map_err(|e| e.to_string())?;
        if p.manifest != sha256(&core::manifest(&local.library).map_err(ce)?).0 || !p.gaps.is_empty() {
            return Err("incomplete append checkpoint".into());
        }
        let remote = super::history::certify(provider, &planner.remote, &p.remote, &mut planner.proofs).await?;
        planner.remote.merge(&remote).map_err(|e| e.to_string())?;
    }
    let (_, authors) = scan(provider, list(format!("{root}/ops/"), true)).await?;
    if authors.len() > 4096 {
        return Err("too many append authors".into());
    }
    for prefix in authors {
        let author = prefix.strip_prefix(&format!("{root}/ops/")).and_then(|s| s.strip_suffix('/')).ok_or("invalid author prefix")?;
        if !lightcraft_engine::catalog::assets::identifier(author) || author.len() > 96 {
            return Err("invalid append author".into());
        }
        let mut request = list(prefix.clone(), false);
        if !audit {
            let frontier = planner.remote.clock.get(author).copied().unwrap_or(0);
            if frontier > 0 {
                request.start_after = Some(format!("{prefix}{frontier:020}-"));
            }
        }
        let mut tokens = BTreeSet::new();
        let mut complete = false;
        for _ in 0..core::MAX_RECORDS {
            let page = provider.list(&request).await?;
            if !page.prefixes.is_empty() {
                return Err("unexpected append operation prefixes".into());
            }
            for key in page.keys {
                let bytes = get(provider, &key, lightcraft_engine::catalog::replication::MAX_MUTATION_BYTES).await?;
                let m = core::decode_mutation(&provider.connection.prefix, &local.library, &key, &bytes).map_err(ce)?;
                if lightcraft_engine::catalog::replication::archive::covers(&planner.remote.archives, &m.revision.id()) {
                    super::history::prove(provider, &planner.remote, &m, &mut planner.proofs).await?;
                }
                planner.stage(m).map_err(ce)?;
                if planner.proofs.len() > 8192 {
                    planner.proofs.clear();
                }
            }
            let Some(next) = page.next else {
                complete = true;
                break;
            };
            if !tokens.insert(next.clone()) {
                return Err("repeated continuation token".into());
            }
            request.continuation = Some(next);
            request.start_after = None;
        }
        if !complete {
            return Err("too many append listing pages".into());
        }
    }
    planner.drain().map_err(ce)?;
    super::history::hydrate(provider, &mut planner.remote, local, &mut planner.proofs).await?;
    let mut pending: Vec<_> = local.pending.values().collect();
    pending.sort_by_key(|m| m.revision.clone());
    loop {
        let mut changed = false;
        for m in &pending {
            if planner.remote.received.contains_key(&m.revision.id()) || !planner.ready(m) {
                continue;
            }
            super::protocol::dependencies(provider, std::slice::from_ref(*m)).await?;
            planner.stage((*m).clone()).map_err(ce)?;
            planner.drain().map_err(ce)?;
            // Do not expose an op that this verified remote projection cannot accept.
            provider.put_immutable(&core::mutation_key(&provider.connection.prefix, m).map_err(ce)?, &core::encode(m).map_err(ce)?).await?;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    if checkpoint && planner.progress().map_err(ce)?.gaps.is_empty() {
        super::history::compact(provider, &mut planner.remote, true).await?;
    }
    super::history::hydrate(provider, &mut planner.remote, local, &mut planner.proofs).await?;
    let mut p = planner.progress().map_err(ce)?;
    if checkpoint && p.gaps.is_empty() {
        let bytes = core::encode(&p).map_err(ce)?;
        provider.put_immutable(&format!("{root}/checkpoints/{}/{}.json", local.device, sha256(&bytes).0), &bytes).await?;
    }
    p.remote = super::history::certify(provider, local, &p.remote, &mut planner.proofs).await?;
    Ok(Outcome::append(p, planner.acknowledged(local).map_err(ce)?))
}

pub async fn migrate(provider: &Provider, local: &SyncState) -> Result<Outcome, String> {
    if provider.connection.bucket_root.is_none() {
        return Err("migration requires bucketRoot and an append-capable signer".into());
    }
    let mut planner = core::Planner::new(local, true).map_err(ce)?;
    let seed = super::protocol::migration_seed(provider, &local.library, &mut planner.proofs).await?;
    let seed = super::history::certify(provider, &planner.remote, &seed, &mut planner.proofs).await?;
    planner.remote.merge(&seed).map_err(|e| e.to_string())?;
    let mut p = planner.progress().map_err(ce)?;
    provider
        .put_immutable(&core::manifest_key(&provider.connection.prefix, &local.library).map_err(ce)?, &core::manifest(&local.library).map_err(ce)?)
        .await?;
    let bytes = core::encode(&p).map_err(ce)?;
    let root = core::root(&provider.connection.prefix, &local.library).map_err(ce)?;
    provider.put_immutable(&format!("{root}/checkpoints/{}/{}.json", local.device, sha256(&bytes).0), &bytes).await?;
    p.remote = super::history::certify(provider, local, &p.remote, &mut planner.proofs).await?;
    Ok(Outcome::append(p, Default::default()))
}
