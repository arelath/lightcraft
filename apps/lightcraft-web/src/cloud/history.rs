//! Browser I/O adapter for the same archive proof model used by native synchronization.
use super::Provider;
use lightcraft_engine::cloud_status::CloudFailure;
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

async fn fetch(provider: &Provider, library: &str, hash: &str, witness: &mut Witness) -> Result<(), CloudFailure> {
    let key = core::key(&provider.connection.prefix, library, hash).map_err(CloudFailure::from)?;
    let (node, _) =
        provider.history_record(&key, archive::MAX_NODE_BYTES, |bytes| archive::Node::decode(hash, bytes).map_err(CloudFailure::from)).await?;
    if witness.len() >= archive::MAX_WITNESS_NODES {
        return Err("receipt proof budget exceeded; local work retained".into());
    }
    witness.insert(hash.into(), node);
    Ok(())
}
pub async fn certify(provider: &Provider, local: &SyncState, remote: &Portable, witness: &mut Witness) -> Result<Portable, CloudFailure> {
    let mut session = core::merge_session(local, remote).map_err(CloudFailure::from)?;
    for _ in 0..=64 {
        let missing = session.missing().map_err(CloudFailure::from)?;
        if missing.is_empty() {
            return Ok(session.finish().map_err(CloudFailure::from)?.remote);
        }
        for hash in missing {
            if !witness.contains_key(&hash) {
                fetch(provider, &local.library, &hash, witness).await?;
            }
            let node = witness.get(&hash).ok_or("missing verified receipt witness")?;
            session.provide(&hash, node.clone()).map_err(CloudFailure::from)?;
        }
    }
    Err("receipt proof depth exceeded; local work retained".into())
}
pub async fn prove(provider: &Provider, state: &SyncState, m: &Mutation, witness: &mut Witness) -> Result<(), CloudFailure> {
    let receipt = core::receipt(m).map_err(CloudFailure::from)?;
    for _ in 0..65 {
        match archive::verify_receipt(&state.library, &state.archives, &m.revision.id(), &receipt, witness) {
            Ok(()) => return Ok(()),
            Err(CatalogError::ArchiveRequired(hash)) => fetch(provider, &state.library, &hash, witness).await?,
            Err(error) => return Err(error.into()),
        }
    }
    Err("receipt archive depth exceeded".into())
}
pub async fn hydrate(provider: &Provider, remote: &mut SyncState, local: &SyncState, witness: &mut Witness) -> Result<(), CloudFailure> {
    for m in local.pending.values() {
        let id = m.revision.id();
        if archive::covers(&remote.archives, &id) {
            prove(provider, remote, m, witness).await?;
            remote.prove_archived(&id, core::receipt(m).map_err(CloudFailure::from)?, witness).map_err(CloudFailure::from)?;
        }
    }
    Ok(())
}
pub async fn compact(provider: &Provider, remote: &mut SyncState, unconditional: bool) -> Result<(), CloudFailure> {
    let plan = remote.archive_plan().map_err(CloudFailure::from)?;
    for (hash, node) in &plan.objects {
        let key = core::key(&provider.connection.prefix, &remote.library, hash).map_err(CloudFailure::from)?;
        let bytes = serde_json::to_vec(node).map_err(CloudFailure::from)?;
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
    remote.compact_receipts(&plan).map_err(CloudFailure::from)
}
