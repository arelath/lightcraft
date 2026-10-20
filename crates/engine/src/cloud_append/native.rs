//! Native append transport. Listing is discovery, exact content is the publication proof.
use super::*;
use crate::assets::s3::S3Connection;
use lightcraft_catalog::replication::SyncProtocol;
use listing::ListRequest;
use std::sync::atomic::{AtomicBool, Ordering};

fn scan(connection: &S3Connection, mut request: ListRequest, cancel: &AtomicBool) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut keys = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    for _ in 0..MAX_RECORDS {
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        let page = connection.list_objects_v2(&request, cancel)?;
        keys.extend(page.keys);
        prefixes.extend(page.prefixes);
        if keys.len().saturating_add(prefixes.len()) > MAX_RECORDS {
            return Err(invalid("append listing limit exceeded"));
        }
        let Some(next) = page.next else { return Ok((keys, prefixes)) };
        if !tokens.insert(next.clone()) {
            return Err(invalid("listing repeated a continuation token"));
        }
        request.continuation = Some(next);
        request.start_after = None;
    }
    Err(invalid("too many append listing pages"))
}
fn list(prefix: String, delimiter: bool) -> ListRequest {
    ListRequest { prefix, delimiter, continuation: None, start_after: None }
}
/// Fresh cooperative control check before original media can be recreated.
pub(crate) fn ensure_readable(connection: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<()> {
    library_control(connection, library, cancel)?.may_read().map_err(AssetError::Blocked)
}
pub(crate) fn library_control(connection: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<crate::cloud_control::Control> {
    let (keys, _) = scan(connection, list(control::prefix(&connection.prefix, library)?, false), cancel)?;
    let mut records = Vec::new();
    for key in control::latest(&keys) {
        let reply = connection.get_small(&key, control::MAX_CONTROL_BYTES, cancel)?.ok_or(AssetError::Missing)?;
        records.push((key, reply.bytes));
    }
    control::effective(&connection.prefix, library, &records).map_err(AssetError::Blocked)
}
pub fn synchronize(connection: &S3Connection, local: &SyncState, checkpoint: bool, cancel: &AtomicBool) -> Result<crate::cloud_replication::Outcome> {
    synchronize_with_audit(connection, local, checkpoint, false, cancel)
}
pub fn synchronize_with_audit(
    connection: &S3Connection,
    local: &SyncState,
    checkpoint: bool,
    audit: bool,
    cancel: &AtomicBool,
) -> Result<crate::cloud_replication::Outcome> {
    synchronize_verified(connection, local, checkpoint, audit, cancel, &mut crate::cloud_replication::VerifiedMedia::default())
}
pub(crate) fn synchronize_verified(
    connection: &S3Connection,
    local: &SyncState,
    checkpoint: bool,
    audit: bool,
    cancel: &AtomicBool,
    media: &mut crate::cloud_replication::VerifiedMedia,
) -> Result<crate::cloud_replication::Outcome> {
    synchronize_mode(connection, local, ScanOptions { checkpoint, audit, receive_only: false, legacy: false }, cancel, media)
}

/// Receive a verified library snapshot without publishing manifests, mutations or checkpoints.
pub(crate) fn receive_only(connection: &S3Connection, local: &SyncState, cancel: &AtomicBool) -> Result<crate::cloud_replication::Outcome> {
    synchronize_mode(
        connection,
        local,
        ScanOptions { checkpoint: false, audit: true, receive_only: true, legacy: false },
        cancel,
        &mut Default::default(),
    )
}

/// Uncommitted recovery input, never a synchronization outcome or local ACK.
/// The owning head must commit this state and its next intake reference together.
pub struct LegacyReceived {
    pub state: lightcraft_catalog::replication::Portable,
    pub intake: crate::cloud_records::LegacyIntake,
}
/// Audit the original append root after its B2 successor became active. Its old
/// cooperative controls no longer grant authority and cannot block this read.
/// Every operation/checkpoint still passes the ordinary content and causal checks.
pub fn receive_legacy(
    connection: &S3Connection,
    head: &crate::cloud_records::Head,
    committed: &lightcraft_catalog::replication::Portable,
    audit: bool,
    cancel: &AtomicBool,
) -> Result<LegacyReceived> {
    head.validate_scope_typed(&connection.prefix, &committed.library).map_err(AssetError::Blocked)?;
    let control = head.control().map_err(invalid)?;
    control.may_publish().map_err(AssetError::Blocked)?;
    if control.epoch < 2 || !connection.b2_head_enabled(cancel)? {
        return Err(invalid("legacy recovery requires the active upgraded B2 authority"));
    }
    let intake = connection.b2_legacy_intake(head, cancel)?;
    let probe = intake.probe(committed, &connection.id).map_err(AssetError::Blocked)?;
    let outcome = synchronize_mode(
        connection,
        &probe,
        ScanOptions { checkpoint: false, audit, receive_only: true, legacy: true },
        cancel,
        &mut Default::default(),
    )?;
    let crate::cloud_replication::Outcome::Append { progress, .. } = outcome else {
        return Err(invalid("legacy receive did not return an append snapshot"));
    };
    Ok(LegacyReceived {
        state: progress.remote,
        intake: crate::cloud_records::LegacyIntake::new(&committed.library, progress.scan).map_err(AssetError::Blocked)?,
    })
}

struct ScanOptions {
    checkpoint: bool,
    audit: bool,
    receive_only: bool,
    legacy: bool,
}

fn synchronize_mode(
    connection: &S3Connection,
    local: &SyncState,
    options: ScanOptions,
    cancel: &AtomicBool,
    media: &mut crate::cloud_replication::VerifiedMedia,
) -> Result<crate::cloud_replication::Outcome> {
    let ScanOptions { checkpoint, audit, receive_only, legacy } = options;
    // An active or resumed integrity audit must not mistake a durable cache hit
    // from discovery for fresh remote evidence. The same mode covers nested receipt
    // proof reads, and the per-pass fetched map can still share fresh downloads.
    let fresh = audit || checkpoint || local.append.as_ref().is_none_or(|p| !p.scan.audit.complete);
    let fresh_connection = fresh.then(|| connection.fresh_metadata());
    let connection = fresh_connection.as_ref().unwrap_or(connection);
    if local.protocol != SyncProtocol::AppendLog {
        return Err(invalid("append synchronization requires explicit protocol selection"));
    }
    let root = root(&connection.prefix, &local.library)?;
    // Cooperative barrier: read the latest library control before any write.
    let blocked = if legacy {
        if !receive_only {
            return Err(invalid("legacy recovery cannot publish"));
        }
        None
    } else {
        control::publication(Ok(library_control(connection, &local.library, cancel)?)).map_err(AssetError::Blocked)?
    };
    let can_publish = !receive_only && blocked.is_none();
    let bytes = manifest(&local.library)?;
    let manifest_key = manifest_key(&connection.prefix, &local.library)?;
    if local.require_existing || receive_only {
        match connection.get_small(&manifest_key, 16 << 10, cancel)? {
            Some(existing) if existing.bytes == bytes => {}
            Some(_) => return Err(AssetError::Integrity),
            None if receive_only
                && !local.require_existing
                && local.append.as_ref().is_none_or(|p| p.frontiers.is_empty() && p.remote.clock.is_empty() && p.scan.checkpoints.is_empty()) => {}
            None => return Err(invalid("cannot join: append library is absent; verify its destination and library ID; local work retained")),
        }
    } else if can_publish {
        connection.put_immutable_verified(&manifest_key, &bytes, cancel)?;
    }
    let mut planner = Planner::new(local)?;
    let mut fetched = BTreeMap::new();
    let (checkpoints, _) = scan(connection, list(format!("{root}/checkpoints/"), false), cancel)?;
    for key in checkpoints {
        if !discovery::consumed(&planner, &root, &key)? {
            let size = read_checkpoint(connection, &mut planner, &root, &key, cancel)?;
            fetched.insert(key, size);
        }
    }
    let (_, authors) = scan(connection, list(format!("{root}/ops/"), true), cancel)?;
    if authors.len() > 4096 {
        return Err(invalid("too many append authors"));
    }
    for author_prefix in authors {
        let author = author_prefix.strip_prefix(&format!("{root}/ops/")).and_then(|s| s.strip_suffix('/')).ok_or(AssetError::Integrity)?;
        if !identifier(author) || author.len() > 96 {
            return Err(invalid("invalid append author prefix"));
        }
        let mut request = list(author_prefix.clone(), false);
        let frontier = planner.remote.clock.get(author).copied().unwrap_or(0);
        if frontier > 0 {
            // All hashes at the certified sequence precede '.'. Old identities
            // are checked by the separate resumable audit, not idle polls.
            request.start_after = Some(format!("{author_prefix}{frontier:020}."));
        }
        let mut tokens = BTreeSet::new();
        let mut complete = false;
        for _ in 0..MAX_RECORDS {
            let page = connection.list_objects_v2(&request, cancel)?;
            if !page.prefixes.is_empty() {
                return Err(AssetError::Integrity);
            }
            for key in page.keys {
                let size = read_operation(connection, &mut planner, &key, cancel)?;
                fetched.insert(key, size);
            }
            let Some(next) = page.next else {
                complete = true;
                break;
            };
            if !tokens.insert(next.clone()) {
                return Err(invalid("listing repeated a continuation token"));
            }
            request.continuation = Some(next);
            request.start_after = None;
        }
        if !complete {
            return Err(invalid("too many append listing pages"));
        }
    }
    audit_history(connection, &mut planner, &root, audit || checkpoint || local.append.is_none(), &fetched, cancel)?;
    planner.drain()?;
    crate::cloud_history::native::hydrate(connection, &mut planner.remote, local, &mut planner.proofs, cancel)?;
    let mut pending: Vec<_> = if !can_publish { Vec::new() } else { local.pending.values().collect() };
    pending.sort_by_key(|m| m.revision.clone());
    let checkpoint = checkpoint && can_publish;
    // Each author advances only after its dependencies are remotely verified. Uploads can
    // publish an early prefix while later imports still wait for their media transfers.
    loop {
        let mut changed = false;
        for mutation in &pending {
            if planner.remote.received.contains_key(&mutation.revision.id()) {
                continue;
            }
            if !planner.ready(mutation) {
                continue;
            }
            match crate::cloud_replication::dependencies_verified(connection, std::slice::from_ref(*mutation), cancel, media) {
                Ok(()) => {}
                Err(AssetError::Missing) => continue,
                Err(e) => return Err(e),
            }
            let key = mutation_key(&connection.prefix, mutation)?;
            // Validate in this worker-local trial before exposing an immutable op.
            // A failed PUT aborts the outcome, so none of this trial is acknowledged.
            planner.stage((*mutation).clone())?;
            planner.drain()?;
            connection.put_immutable_verified(&key, &encode(mutation)?, cancel)?;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    if checkpoint && planner.progress()?.gaps.is_empty() {
        crate::cloud_history::native::compact(connection, &mut planner.remote, true, cancel)?;
    }
    // Newly published effects may supersede an archived pending head. Promote all
    // pending proofs again immediately before acknowledgement, even without maintenance.
    crate::cloud_history::native::hydrate(connection, &mut planner.remote, local, &mut planner.proofs, cancel)?;
    let mut progress = planner.progress()?;
    if checkpoint && progress.gaps.is_empty() {
        let bytes = discovery::checkpoint_bytes(&progress)?;
        let key = format!("{root}/checkpoints/{}/{}.json", local.device, sha256(&bytes).0);
        connection.put_immutable_verified(&key, &bytes, cancel)?;
        discovery::consume(&mut planner, &root, &key)?;
        progress.scan = planner.scan.clone();
    }
    let acknowledged = planner.acknowledged(local)?;
    progress.remote = crate::cloud_history::native::certify(connection, local, &progress.remote, &mut planner.proofs, cancel)?;
    let capability = crate::cloud_outcome::Publication::from(blocked);
    let capability = if receive_only { capability.receiving() } else { capability };
    Ok(crate::cloud_replication::Outcome::append(progress, acknowledged).with_publication(capability))
}

fn read_checkpoint(connection: &S3Connection, planner: &mut Planner, root: &str, key: &str, cancel: &AtomicBool) -> Result<usize> {
    let reply = connection.history_record(key, MAX_OBJECT, cancel, |bytes| {
        let p: AppendProgress = serde_json::from_slice(bytes).map_err(|_| invalid("malformed append checkpoint"))?;
        p.validate(&planner.remote.library).map_err(ce)?;
        if p.manifest != sha256(&manifest(&planner.remote.library)?).0 || !p.gaps.is_empty() {
            return Err(AssetError::Integrity);
        }
        Ok(p)
    })?;
    let p = reply.value;
    let remote = crate::cloud_history::native::certify(connection, &planner.remote, &p.remote, &mut planner.proofs, cancel)?;
    planner.remote.merge(&remote).map_err(ce)?;
    discovery::consume(planner, root, key)?;
    Ok(reply.size)
}
fn read_operation(connection: &S3Connection, planner: &mut Planner, key: &str, cancel: &AtomicBool) -> Result<usize> {
    let reply = connection.history_record(key, lightcraft_catalog::replication::MAX_MUTATION_BYTES, cancel, |bytes| {
        decode_mutation(&connection.prefix, &planner.remote.library, key, bytes)
    })?;
    let mutation = reply.value;
    if lightcraft_catalog::replication::archive::covers(&planner.remote.archives, &mutation.revision.id()) {
        crate::cloud_history::native::prove(connection, &planner.remote, &mutation, &mut planner.proofs, cancel)?;
    }
    planner.stage_and_drain(mutation)?;
    if planner.proofs.len() > 8192 {
        planner.proofs.clear();
    }
    Ok(reply.size)
}
fn audit_history(
    connection: &S3Connection,
    planner: &mut Planner,
    root: &str,
    requested: bool,
    fetched: &BTreeMap<String, usize>,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut audit = discovery::Audit::new(&planner.scan, requested)?;
    let _traffic = crate::cloud_traffic::Scope::purpose(crate::cloud_traffic::Purpose::Audit);
    for namespace in ["checkpoints", "ops"] {
        let Some(mut request) = audit.request(root, namespace) else { continue };
        let mut tokens = BTreeSet::new();
        let mut complete = false;
        for _ in 0..MAX_RECORDS {
            if !audit.start_page()? {
                audit.save(&mut planner.scan);
                return Ok(());
            }
            let page = connection.list_objects_v2(&request, cancel)?;
            if !page.prefixes.is_empty() {
                return Err(AssetError::Integrity);
            }
            for key in page.keys {
                if !audit.available() {
                    audit.save(&mut planner.scan);
                    return Ok(());
                }
                // Only reverify consumed checkpoints here. A newly arriving
                // checkpoint belongs to the next discovery pass, before tail
                // staging; merging it now could strand covered staged records.
                if namespace == "checkpoints" && !discovery::consumed(planner, root, &key)? {
                    audit.save(&mut planner.scan);
                    return Ok(());
                }
                let size = if let Some(size) = fetched.get(&key) {
                    *size
                } else if namespace == "checkpoints" {
                    read_checkpoint(connection, planner, root, &key, cancel)?
                } else {
                    read_operation(connection, planner, &key, cancel)?
                };
                audit.verified(root, &key, size)?;
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
                return Err(invalid("listing repeated a continuation token"));
            }
            request.continuation = Some(next);
            request.start_after = None;
        }
        if !complete {
            return Err(invalid("too many append audit pages"));
        }
    }
    audit.save(&mut planner.scan);
    Ok(())
}
