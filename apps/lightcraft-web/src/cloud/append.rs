//! Browser transport for the shared append codecs, scheduler and exact publication receipts.
use super::{Provider, protocol::Outcome};
use core::listing::ListRequest;
use lightcraft_engine::cloud_status::CloudFailure;
use lightcraft_engine::{assets::sha256, catalog::replication::SyncState, cloud_append as core};
use std::collections::{BTreeMap, BTreeSet};
fn ce(e: lightcraft_engine::assets::AssetError) -> CloudFailure {
    e.into()
}
fn list(prefix: String, delimiter: bool) -> ListRequest {
    ListRequest { prefix, delimiter, continuation: None, start_after: None }
}
async fn scan(provider: &Provider, mut request: ListRequest) -> Result<(BTreeSet<String>, BTreeSet<String>), CloudFailure> {
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
async fn get(provider: &Provider, key: &str, limit: usize) -> Result<Vec<u8>, CloudFailure> {
    let r = provider.request("GET", key, None, None, None, limit).await?;
    if r.status != 200 {
        return Err(CloudFailure::http("append record", r.status));
    }
    Ok(r.bytes)
}
pub async fn synchronize(provider: &Provider, local: &SyncState, checkpoint: bool, audit: bool) -> Result<Outcome, CloudFailure> {
    synchronize_mode(provider, local, checkpoint, audit, false, false).await
}
pub(super) async fn receive_only(provider: &Provider, local: &SyncState) -> Result<Outcome, CloudFailure> {
    let mut probe = local.clone();
    probe.require_existing = true;
    synchronize_mode(provider, &probe, false, true, true, false).await
}
pub(super) struct LegacyReceived {
    pub state: lightcraft_engine::catalog::replication::Portable,
    pub intake: lightcraft_engine::cloud_records::LegacyIntake,
}
pub(super) async fn receive_legacy(
    provider: &Provider,
    head: &lightcraft_engine::cloud_records::Head,
    committed: &lightcraft_engine::catalog::replication::Portable,
    audit: bool,
) -> Result<LegacyReceived, CloudFailure> {
    head.validate_scope_typed(&provider.connection.prefix, &committed.library)?;
    let control = head.control()?;
    control.may_publish()?;
    if control.epoch < 2 {
        return Err(CloudFailure::integrity("legacy recovery requires an active upgraded authority"));
    }
    let intake = super::legacy::intake(provider, head).await?;
    let probe = intake.probe(committed, &provider.connection.id)?;
    let outcome = synchronize_mode(provider, &probe, false, audit, true, true).await?;
    let progress = outcome.append_progress().ok_or("legacy recovery has no verified discovery progress")?;
    Ok(LegacyReceived {
        state: outcome.state().clone(),
        intake: lightcraft_engine::cloud_records::LegacyIntake::new(&committed.library, progress.scan.clone())?,
    })
}
async fn synchronize_mode(
    provider: &Provider,
    local: &SyncState,
    checkpoint: bool,
    audit: bool,
    receive_only: bool,
    legacy: bool,
) -> Result<Outcome, CloudFailure> {
    if provider.connection.bucket_root.is_none() {
        return Err("append mode requires a configured bucketRoot and a signer supporting list/putImmutable".into());
    }
    let root = core::root(&provider.connection.prefix, &local.library).map_err(ce)?;
    let manifest_key = core::manifest_key(&provider.connection.prefix, &local.library).map_err(ce)?;
    let manifest = core::manifest(&local.library).map_err(ce)?;
    // Cooperative barrier: read the latest library control before any write.
    let blocked = if legacy {
        if !receive_only {
            return Err(CloudFailure::integrity("legacy recovery must not publish"));
        }
        None
    } else {
        core::control::publication(Ok(library_control(provider, &local.library).await?))?
    };
    let can_publish = !receive_only && blocked.is_none();
    if local.require_existing {
        let existing = provider.request("GET", &manifest_key, None, None, None, 16 << 10).await?;
        if existing.status == 404 {
            return Err("cannot join: append library is absent; verify its destination and library ID; local work retained".into());
        }
        if existing.status != 200 || existing.bytes != manifest {
            return Err("append manifest could not be verified; local work retained".into());
        }
    } else if can_publish {
        provider.put_immutable(&manifest_key, &manifest).await?;
    }
    let mut planner = core::Planner::new(local).map_err(ce)?;
    let mut fetched = BTreeMap::new();
    let (checkpoints, _) = scan(provider, list(format!("{root}/checkpoints/"), false)).await?;
    for key in checkpoints {
        if !core::discovery::consumed(&planner, &root, &key).map_err(ce)? {
            let size = read_checkpoint(provider, &mut planner, &root, &key).await?;
            fetched.insert(key, size);
        }
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
        let frontier = planner.remote.clock.get(author).copied().unwrap_or(0);
        if frontier > 0 {
            request.start_after = Some(format!("{prefix}{frontier:020}."));
        }
        let mut tokens = BTreeSet::new();
        let mut complete = false;
        for _ in 0..core::MAX_RECORDS {
            let page = provider.list(&request).await?;
            if !page.prefixes.is_empty() {
                return Err("unexpected append operation prefixes".into());
            }
            for key in page.keys {
                let size = read_operation(provider, &mut planner, &key).await?;
                fetched.insert(key, size);
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
    audit_history(provider, &mut planner, &root, audit || checkpoint || local.append.is_none(), &fetched).await?;
    planner.drain().map_err(ce)?;
    super::history::hydrate(provider, &mut planner.remote, local, &mut planner.proofs).await?;
    let mut pending: Vec<_> = if can_publish { local.pending.values().collect() } else { Vec::new() };
    pending.sort_by_key(|m| m.revision.clone());
    let checkpoint = checkpoint && can_publish;
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
        let bytes = core::discovery::checkpoint_bytes(&p).map_err(ce)?;
        let key = format!("{root}/checkpoints/{}/{}.json", local.device, sha256(&bytes).0);
        provider.put_immutable(&key, &bytes).await?;
        core::discovery::consume(&mut planner, &root, &key).map_err(ce)?;
        p.scan = planner.scan.clone();
    }
    p.remote = super::history::certify(provider, local, &p.remote, &mut planner.proofs).await?;
    let acknowledged = planner.acknowledged(local).map_err(ce)?;
    let outcome = match blocked {
        Some(blocked) => Outcome::append_blocked(p, acknowledged, blocked),
        None => Outcome::append(p, acknowledged),
    };
    let capability = outcome.publication().clone();
    Ok(if receive_only { outcome.with_publication(capability.receiving()) } else { outcome })
}
pub(super) async fn library_control(provider: &Provider, library: &str) -> Result<lightcraft_engine::cloud_control::Control, CloudFailure> {
    let (keys, _) = scan(provider, list(core::control::prefix(&provider.connection.prefix, library).map_err(ce)?, false)).await?;
    let mut records = Vec::new();
    for key in core::control::latest(&keys) {
        records.push((key.clone(), get(provider, &key, core::control::MAX_CONTROL_BYTES).await?));
    }
    Ok(core::control::effective(&provider.connection.prefix, library, &records)?)
}

pub async fn migrate(provider: &Provider, local: &SyncState) -> Result<Outcome, CloudFailure> {
    if provider.connection.bucket_root.is_none() {
        return Err("migration requires bucketRoot and an append-capable signer".into());
    }
    let mut planner = core::Planner::new(local).map_err(ce)?;
    let seed = super::protocol::migration_seed(provider, &local.library, &mut planner.proofs).await?;
    let seed = super::history::certify(provider, &planner.remote, &seed, &mut planner.proofs).await?;
    planner.remote.merge(&seed).map_err(CloudFailure::from)?;
    let mut p = planner.progress().map_err(ce)?;
    provider
        .put_immutable(&core::manifest_key(&provider.connection.prefix, &local.library).map_err(ce)?, &core::manifest(&local.library).map_err(ce)?)
        .await?;
    let bytes = core::discovery::checkpoint_bytes(&p).map_err(ce)?;
    let root = core::root(&provider.connection.prefix, &local.library).map_err(ce)?;
    let key = format!("{root}/checkpoints/{}/{}.json", local.device, sha256(&bytes).0);
    provider.put_immutable(&key, &bytes).await?;
    core::discovery::consume(&mut planner, &root, &key).map_err(ce)?;
    p.scan = planner.scan.clone();
    p.remote = super::history::certify(provider, local, &p.remote, &mut planner.proofs).await?;
    Ok(Outcome::append(p, Default::default()))
}

async fn read_checkpoint(provider: &Provider, planner: &mut core::Planner, root: &str, key: &str) -> Result<usize, CloudFailure> {
    let bytes = get(provider, key, core::MAX_OBJECT).await?;
    core::immutable_metadata(key, &bytes).map_err(ce)?;
    let p: lightcraft_engine::catalog::replication::AppendProgress = serde_json::from_slice(&bytes).map_err(|_| "malformed append checkpoint")?;
    p.validate(&planner.remote.library).map_err(CloudFailure::from)?;
    if p.manifest != sha256(&core::manifest(&planner.remote.library).map_err(ce)?).0 || !p.gaps.is_empty() {
        return Err("incomplete append checkpoint".into());
    }
    let remote = super::history::certify(provider, &planner.remote, &p.remote, &mut planner.proofs).await?;
    planner.remote.merge(&remote).map_err(CloudFailure::from)?;
    core::discovery::consume(planner, root, key).map_err(ce)?;
    Ok(bytes.len())
}
async fn read_operation(provider: &Provider, planner: &mut core::Planner, key: &str) -> Result<usize, CloudFailure> {
    let bytes = get(provider, key, lightcraft_engine::catalog::replication::MAX_MUTATION_BYTES).await?;
    let mutation = core::decode_mutation(&provider.connection.prefix, &planner.remote.library, key, &bytes).map_err(ce)?;
    if lightcraft_engine::catalog::replication::archive::covers(&planner.remote.archives, &mutation.revision.id()) {
        super::history::prove(provider, &planner.remote, &mutation, &mut planner.proofs).await?;
    }
    planner.stage_and_drain(mutation).map_err(ce)?;
    if planner.proofs.len() > 8192 {
        planner.proofs.clear();
    }
    Ok(bytes.len())
}
async fn audit_history(
    provider: &Provider,
    planner: &mut core::Planner,
    root: &str,
    requested: bool,
    fetched: &BTreeMap<String, usize>,
) -> Result<(), CloudFailure> {
    let mut audit = core::discovery::Audit::new(&planner.scan, requested).map_err(ce)?;
    for namespace in ["checkpoints", "ops"] {
        let Some(mut request) = audit.request(root, namespace) else { continue };
        let mut tokens = BTreeSet::new();
        let mut complete = false;
        for _ in 0..core::MAX_RECORDS {
            if !audit.start_page().map_err(ce)? {
                audit.save(&mut planner.scan);
                return Ok(());
            }
            let page = provider.list(&request).await?;
            if !page.prefixes.is_empty() {
                return Err("unexpected append audit prefixes".into());
            }
            for key in page.keys {
                if !audit.available() {
                    audit.save(&mut planner.scan);
                    return Ok(());
                }
                // Only reverify consumed checkpoints here. A newly arriving
                // checkpoint belongs to the next discovery pass, before tail
                // staging; merging it now could strand covered staged records.
                if namespace == "checkpoints" && !core::discovery::consumed(planner, root, &key).map_err(ce)? {
                    audit.save(&mut planner.scan);
                    return Ok(());
                }
                let size = if let Some(size) = fetched.get(&key) {
                    *size
                } else if namespace == "checkpoints" {
                    read_checkpoint(provider, planner, root, &key).await?
                } else {
                    read_operation(provider, planner, &key).await?
                };
                audit.verified(root, &key, size).map_err(ce)?;
            }
            let Some(next) = page.next else {
                audit.finished(namespace);
                complete = true;
                break;
            };
            if !audit.available() {
                audit.save(&mut planner.scan);
                return Ok(());
            }
            if !tokens.insert(next.clone()) {
                return Err("repeated continuation token".into());
            }
            request.continuation = Some(next);
            request.start_after = None;
        }
        if !complete {
            return Err("too many append audit pages".into());
        }
    }
    audit.save(&mut planner.scan);
    Ok(())
}
