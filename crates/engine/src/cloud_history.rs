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
    AssetError::Invalid(error.to_string())
}
pub fn merge_request(local: &SyncState, remote: &Portable, witness: &Witness) -> std::result::Result<Portable, CatalogError> {
    let mut remote = remote.clone();
    remote.proofs.extend(witness.clone());
    let mut test = local.clone();
    test.merge(&remote)?;
    Ok(remote)
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
        let mut needed = remote.proofs.clone();
        for _ in 0..=archive::MAX_WITNESS_NODES {
            match merge_request(local, remote, &needed) {
                Ok(remote) => return Ok(remote),
                Err(CatalogError::ArchiveRequired(hash)) => {
                    if !witness.contains_key(&hash) {
                        fetch(connection, &local.library, &hash, witness, cancel)?;
                    }
                    let node = witness.get(&hash).ok_or(AssetError::Integrity)?;
                    needed.insert(hash, node.clone());
                }
                Err(error) => return Err(ce(error)),
            }
        }
        Err(AssetError::Invalid("receipt proof budget exceeded; local work retained".into()))
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
                    Ok(_) | Err(AssetError::Conflict | AssetError::Network(_)) => {}
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
