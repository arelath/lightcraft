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
    if local.protocol != SyncProtocol::AppendLog {
        return Err(invalid("append synchronization requires explicit protocol selection"));
    }
    let root = root(&connection.prefix, &local.library)?;
    let bytes = manifest(&local.library)?;
    connection.put_immutable_verified(&manifest_key(&connection.prefix, &local.library)?, &bytes, cancel)?;
    let audit = audit || checkpoint || local.append.is_none();
    let mut planner = Planner::new(local, audit)?;
    let (checkpoints, _) = scan(connection, list(format!("{root}/checkpoints/"), false), cancel)?;
    for key in checkpoints {
        let reply = connection.get_small(&key, MAX_OBJECT, cancel)?.ok_or(AssetError::Missing)?;
        immutable_metadata(&key, &reply.bytes)?;
        planner.checkpoint(&reply.bytes)?;
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
        if !audit {
            let frontier = planner.remote.clock.get(author).copied().unwrap_or(0);
            if frontier > 0 {
                request.start_after = Some(format!("{author_prefix}{:020}-", frontier));
            }
        }
        let (objects, _) = scan(connection, request, cancel)?;
        for key in objects {
            let reply = connection.get_small(&key, lightcraft_catalog::replication::MAX_MUTATION_BYTES, cancel)?.ok_or(AssetError::Missing)?;
            planner.stage(decode_mutation(&connection.prefix, &local.library, &key, &reply.bytes)?)?;
        }
    }
    planner.drain()?;
    let mut pending: Vec<_> = local.pending.values().collect();
    pending.sort_by_key(|m| m.revision.clone());
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
            match crate::cloud_replication::dependencies(connection, std::slice::from_ref(*mutation), cancel) {
                Ok(()) => {}
                Err(AssetError::Missing) => continue,
                Err(e) => return Err(e),
            }
            let key = mutation_key(&connection.prefix, mutation)?;
            connection.put_immutable_verified(&key, &encode(mutation)?, cancel)?;
            planner.stage((*mutation).clone())?;
            planner.drain()?;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    let progress = planner.progress()?;
    if checkpoint && progress.gaps.is_empty() {
        let bytes = encode(&progress)?;
        let key = format!("{root}/checkpoints/{}/{}.json", local.device, sha256(&bytes).0);
        connection.put_immutable_verified(&key, &bytes, cancel)?;
    }
    let acknowledged = planner.acknowledged(local)?;
    Ok(crate::cloud_replication::Outcome {
        state: progress.remote.clone(),
        acknowledged,
        cursor: String::new(),
        generation: 0,
        append: Some(progress),
    })
}
