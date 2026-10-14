//! Browser I/O adapter for the same archive proof model used by native synchronization.
use super::Provider;
use lightcraft_engine::{
    catalog::{
        CatalogError,
        replication::{
            Mutation, Portable, SyncState,
            archive::{self, Witness},
        },
    },
    cloud_history as core,
};

async fn fetch(provider: &Provider, library: &str, hash: &str, witness: &mut Witness) -> Result<(), String> {
    let key = core::key(&provider.connection.prefix, library, hash).map_err(|e| e.to_string())?;
    let reply = provider.request("GET", &key, None, None, None, archive::MAX_NODE_BYTES).await?;
    if reply.status != 200 {
        return Err("receipt archive unavailable; restore retained objects, local work retained".into());
    }
    core::insert(witness, hash, &reply.bytes).map_err(|e| e.to_string())
}
pub async fn certify(provider: &Provider, local: &SyncState, remote: &Portable, witness: &mut Witness) -> Result<Portable, String> {
    let mut needed = remote.proofs.clone();
    for _ in 0..=archive::MAX_WITNESS_NODES {
        match core::merge_request(local, remote, &needed) {
            Ok(remote) => return Ok(remote),
            Err(CatalogError::ArchiveRequired(hash)) => {
                if !witness.contains_key(&hash) {
                    fetch(provider, &local.library, &hash, witness).await?;
                }
                let node = witness.get(&hash).ok_or("missing verified receipt witness")?;
                needed.insert(hash, node.clone());
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("receipt proof budget exceeded; local work retained".into())
}
pub async fn prove(provider: &Provider, state: &SyncState, m: &Mutation, witness: &mut Witness) -> Result<(), String> {
    let receipt = core::receipt(m).map_err(|e| e.to_string())?;
    for _ in 0..65 {
        match archive::verify_receipt(&state.library, &state.archives, &m.revision.id(), &receipt, witness) {
            Ok(()) => return Ok(()),
            Err(CatalogError::ArchiveRequired(hash)) => fetch(provider, &state.library, &hash, witness).await?,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("receipt archive depth exceeded".into())
}
pub async fn hydrate(provider: &Provider, remote: &mut SyncState, local: &SyncState, witness: &mut Witness) -> Result<(), String> {
    for m in local.pending.values() {
        let id = m.revision.id();
        if archive::covers(&remote.archives, &id) {
            prove(provider, remote, m, witness).await?;
            remote.prove_archived(&id, core::receipt(m).map_err(|e| e.to_string())?, witness).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
pub async fn compact(provider: &Provider, remote: &mut SyncState, unconditional: bool) -> Result<(), String> {
    let plan = remote.archive_plan().map_err(|e| e.to_string())?;
    for (hash, node) in &plan.objects {
        let key = core::key(&provider.connection.prefix, &remote.library, hash).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(node).map_err(|e| e.to_string())?;
        if unconditional {
            provider.put_immutable(&key, &bytes).await?;
        } else {
            super::protocol::immutable(provider, &key, &bytes).await?;
            let stored = provider.request("GET", &key, None, None, None, archive::MAX_NODE_BYTES).await?;
            if stored.status != 200 || stored.bytes != bytes {
                return Err("receipt archive readback failed; local work retained".into());
            }
        }
    }
    remote.compact_receipts(&plan).map_err(|e| e.to_string())
}
