//! Shared receipt-archive transport boundary. Model requests are hash-only; each object
//! is bounded and verified before it becomes an owner-thread merge/acknowledgement witness.
use crate::assets::{AssetError, Result, keys};
use lightcraft_catalog::{
    CatalogError,
    replication::{
        Mutation, Portable, Receipt, SyncState,
        archive::{self, Node, Witness},
    },
};

pub fn key(prefix: &str, library: &str, hash: &str) -> Result<String> {
    if !lightcraft_catalog::assets::identifier(library) || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f')) {
        return Err(AssetError::Invalid("invalid receipt archive identity".into()));
    }
    keys::in_library(prefix, &format!("sync-v2/{library}/receipts/{hash}.json"))
}
pub fn receipt(mutation: &Mutation) -> std::result::Result<Receipt, CatalogError> {
    mutation.validate()?;
    Ok(Receipt { context: mutation.context.clone(), digest: mutation.digest()? })
}
pub fn insert(witness: &mut Witness, hash: &str, bytes: &[u8]) -> Result<()> {
    if witness.len() >= archive::MAX_WITNESS_NODES {
        return Err(AssetError::Invalid("receipt proof budget exceeded; local work retained".into()));
    }
    let node = Node::decode(hash, bytes).map_err(ce)?;
    witness.insert(hash.into(), node);
    Ok(())
}
pub fn ce(error: CatalogError) -> AssetError {
    AssetError::from(error)
}
/// Plan exact proofs once for both the main catalog and retained append ACKs.
pub fn merge_session(local: &SyncState, remote: &Portable) -> std::result::Result<archive::MergeSession, CatalogError> {
    let mut session = archive::MergeSession::new(local, remote)?;
    if let Some(progress) = &local.append {
        let published = SyncState::restore_durable(&progress.remote, "publication".into(), "remote".into())?;
        session.include(&published)?;
    }
    Ok(session)
}
#[cfg(not(target_arch = "wasm32"))]
pub mod native {
    use super::*;
    use crate::assets::s3::S3Connection;
    use std::sync::atomic::AtomicBool;
    pub fn fetch(connection: &S3Connection, library: &str, hash: &str, witness: &mut Witness, cancel: &AtomicBool) -> Result<()> {
        let object = connection
            .get_small(&key(&connection.prefix, library, hash)?, archive::MAX_NODE_BYTES, cancel)?
            .ok_or_else(|| AssetError::Invalid("receipt archive is missing; restore retained archive objects, local work retained".into()))?;
        insert(witness, hash, &object.bytes)
    }
    pub fn certify(connection: &S3Connection, local: &SyncState, remote: &Portable, witness: &mut Witness, cancel: &AtomicBool) -> Result<Portable> {
        let mut session = merge_session(local, remote).map_err(ce)?;
        for _ in 0..=64 {
            let missing = session.missing().map_err(ce)?;
            if missing.is_empty() {
                return Ok(session.finish().map_err(ce)?.remote);
            }
            for hash in missing {
                if !witness.contains_key(&hash) {
                    fetch(connection, &local.library, &hash, witness, cancel)?;
                }
                let node = witness.get(&hash).ok_or(AssetError::Integrity)?;
                session.provide(&hash, node.clone()).map_err(ce)?;
            }
        }
        Err(AssetError::Invalid("receipt proof depth exceeded; local work retained".into()))
    }
    pub fn prove(connection: &S3Connection, state: &SyncState, m: &Mutation, witness: &mut Witness, cancel: &AtomicBool) -> Result<()> {
        let receipt = receipt(m).map_err(ce)?;
        for _ in 0..65 {
            match archive::verify_receipt(&state.library, &state.archives, &m.revision.id(), &receipt, witness) {
                Ok(()) => return Ok(()),
                Err(CatalogError::ArchiveRequired(hash)) => fetch(connection, &state.library, &hash, witness, cancel)?,
                Err(error) => return Err(ce(error)),
            }
        }
        Err(AssetError::Integrity)
    }
    pub fn hydrate(connection: &S3Connection, remote: &mut SyncState, local: &SyncState, witness: &mut Witness, cancel: &AtomicBool) -> Result<()> {
        for m in local.pending.values() {
            let id = m.revision.id();
            if archive::covers(&remote.archives, &id) {
                prove(connection, remote, m, witness, cancel)?;
                remote.prove_archived(&id, receipt(m).map_err(ce)?, witness).map_err(ce)?;
            }
        }
        Ok(())
    }
    pub fn receive(connection: &S3Connection, remote: &mut SyncState, m: &Mutation, witness: &mut Witness, cancel: &AtomicBool) -> Result<()> {
        if archive::covers(&remote.archives, &m.revision.id()) {
            prove(connection, remote, m, witness, cancel)
        } else {
            remote.receive(m).map(|_| ()).map_err(ce)
        }
    }
    pub fn published(
        connection: &S3Connection,
        remote: &Portable,
        id: &str,
        digest: &str,
        witness: &mut Witness,
        cancel: &AtomicBool,
    ) -> Result<bool> {
        if let Some(receipt) = remote.received.get(id) {
            if receipt.digest != digest {
                return Err(AssetError::Integrity);
            }
            return Ok(true);
        }
        if !archive::covers(&remote.archives, id) {
            return Ok(false);
        }
        for _ in 0..66 {
            match archive::verify_digest(&remote.library, &remote.archives, id, digest, witness) {
                Ok(()) => return Ok(true),
                Err(CatalogError::ArchiveRequired(hash)) => fetch(connection, &remote.library, &hash, witness, cancel)?,
                Err(error) => return Err(ce(error)),
            }
        }
        Err(AssetError::Integrity)
    }
    pub fn compact(connection: &S3Connection, remote: &mut SyncState, unconditional: bool, cancel: &AtomicBool) -> Result<()> {
        let plan = remote.archive_plan().map_err(ce)?;
        for (hash, node) in &plan.objects {
            let bytes = serde_json::to_vec(node).map_err(|e| AssetError::Invalid(e.to_string()))?;
            let key = key(&connection.prefix, &remote.library, hash)?;
            if unconditional {
                connection.put_immutable_verified(&key, &bytes, cancel)?;
            } else {
                match connection.put_small(&key, &bytes, crate::assets::s3::PutCondition::Absent, cancel) {
                    Ok(_) => {}
                    Err(error) if error.uncertain_write() => {}
                    Err(error) => return Err(error),
                }
                let stored = connection.get_small(&key, archive::MAX_NODE_BYTES, cancel)?.ok_or(AssetError::Missing)?;
                if stored.bytes != bytes {
                    return Err(AssetError::Integrity);
                }
            }
        }
        // Witnesses are fetched on demand; keeping all historical nodes would defeat compaction.
        remote.compact_receipts(&plan).map_err(ce)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::replication::{AppendProgress, Change, SyncProtocol};
    use serde_json::json;
    #[test]
    fn generated_paths_are_carried_to_owner_ack_after_a_checkpoint_covers_many_pending_edits() {
        let mut local = SyncState::new("library".into(), "author".into(), "storage".into()).unwrap();
        local.protocol = SyncProtocol::AppendLog;
        let change = |value| vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(value) }];
        local.enqueue(change(1)).unwrap();
        local.enqueue(change(2)).unwrap();
        let old = AppendProgress {
            version: 1,
            manifest: "a".repeat(64),
            frontiers: local.clock.clone(),
            remote: local.portable(),
            scan: Default::default(),
            gaps: Default::default(),
        };
        local.acknowledge_append(&local.pending.keys().cloned().collect(), old).unwrap();
        for value in 0..64 {
            local.enqueue(change(value % 6)).unwrap();
        }
        let ids = local.pending.keys().cloned().collect();
        let mut remote = SyncState::restore_durable(&local.portable(), "remote".into(), "storage".into()).unwrap();
        let plan = remote.archive_plan().unwrap();
        remote.compact_receipts(&plan).unwrap();
        for mutation in local.pending.values() {
            remote.prove_archived(&mutation.revision.id(), receipt(mutation).unwrap(), &plan.objects).unwrap();
        }
        let mut session = merge_session(&local, &remote.portable()).unwrap();
        assert!(session.missing().unwrap().is_empty(), "all paths can be reconstructed locally");
        let result = session.finish().unwrap();
        assert_eq!(result.stats.witness_checks, 0, "no remote archive fetch is needed");
        assert!(!result.remote.proofs.is_empty(), "generated paths must survive the certification session");
        let incoming = AppendProgress {
            version: 1,
            manifest: "a".repeat(64),
            frontiers: remote.clock.clone(),
            remote: result.remote,
            scan: Default::default(),
            gaps: Default::default(),
        };
        local.merge(&incoming.remote).unwrap();
        local.acknowledge_append(&ids, incoming).unwrap();
        assert!(local.pending.is_empty());
        assert_eq!(local.append.as_ref().unwrap().frontiers.get("author"), Some(&66));
    }
    #[test]
    fn certification_includes_older_append_publication_progress_with_a_shared_witness_cache() {
        let state = |author: &str| SyncState::new("library".into(), author.into(), "storage".into()).unwrap();
        let mut author = state("author");
        let change = |value| vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(value) }];
        let first = author.enqueue(change(1)).unwrap().unwrap();
        let second = author.enqueue(change(5)).unwrap().unwrap();
        let mut old = state("publication");
        old.receive(&first).unwrap();
        let mut remote = old.clone();
        remote.receive(&second).unwrap();
        let plan = remote.archive_plan().unwrap();
        remote.compact_receipts(&plan).unwrap();
        let mut local = remote.clone();
        local.device = "peer".into();
        local.protocol = SyncProtocol::AppendLog;
        local.append = Some(Box::new(AppendProgress {
            version: 1,
            manifest: "a".repeat(64),
            frontiers: old.clock.clone(),
            remote: old.portable(),
            scan: Default::default(),
            gaps: Default::default(),
        }));
        local.validate().unwrap();
        assert!(archive::MergeSession::new(&local, &remote.portable()).unwrap().missing().unwrap().is_empty());
        let mut session = merge_session(&local, &remote.portable()).unwrap();
        let missing = session.missing().unwrap();
        assert_eq!(missing.len(), 1, "the retained publication state requires an additional exact prefix proof");
        for hash in missing {
            session.provide(&hash, plan.objects[&hash].clone()).unwrap();
        }
        let result = session.finish().unwrap();
        assert_eq!(result.stats.forest_builds, 3);
        assert_eq!(result.stats.witness_checks, 1);
        assert_eq!(result.stats.state_merges, 2);
        let incoming = AppendProgress {
            version: 1,
            manifest: "a".repeat(64),
            frontiers: remote.clock.clone(),
            remote: result.remote,
            scan: Default::default(),
            gaps: Default::default(),
        };
        local.acknowledge_append(&Default::default(), incoming).unwrap();
        assert_eq!(local.append.as_ref().unwrap().frontiers.get("author"), Some(&2));
    }
}
