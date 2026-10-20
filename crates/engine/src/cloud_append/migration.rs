//! Explicit, coordinated migration. Legacy objects remain available for recovery.
use crate::{EngineError, Result, Session};
use lightcraft_catalog::replication::{AppendProgress, publication_prefixes};
use serde_json::Value;
use std::sync::atomic::AtomicBool;
pub fn migrate(s: &mut Session, p: &Value) -> Result<Value> {
    let mut params = p.as_object().cloned().ok_or_else(|| EngineError::Other("migration parameters must be an object".into()))?;
    params.entry("plan").or_insert_with(|| Value::String("appendLog".into()));
    crate::cloud_replication::start_migration(s, &Value::Object(params))
}
/// Only the remotely committed seed is published; owner-thread persistence changes local mode.
pub(crate) fn seed(
    conn: &crate::assets::s3::S3Connection,
    state: &lightcraft_catalog::replication::SyncState,
    cancel: &AtomicBool,
) -> Result<crate::cloud_replication::Outcome> {
    let mut proofs = Default::default();
    let remote = crate::cloud_replication::read_with_witness(conn, &state.library, &mut proofs, cancel).map_err(|e| EngineError::Cloud(e.into()))?;
    let portable = remote.state.portable();
    let mut progress = AppendProgress {
        version: 1,
        manifest: crate::assets::sha256(&super::manifest(&state.library).map_err(|e| EngineError::Cloud(e.into()))?).0,
        frontiers: publication_prefixes(&portable)?,
        remote: portable,
        scan: Default::default(),
        gaps: Default::default(),
    };
    progress.validate(&state.library)?;
    conn.put_immutable_verified(
        &super::manifest_key(&conn.prefix, &state.library).map_err(|e| EngineError::Cloud(e.into()))?,
        &super::manifest(&state.library).map_err(|e| EngineError::Cloud(e.into()))?,
        cancel,
    )
    .map_err(|e| EngineError::Cloud(e.into()))?;
    let bytes = super::discovery::checkpoint_bytes(&progress).map_err(|e| EngineError::Cloud(e.into()))?;
    let key = format!(
        "{}/checkpoints/{}/{}.json",
        super::root(&conn.prefix, &state.library).map_err(|e| EngineError::Cloud(e.into()))?,
        state.device,
        crate::assets::sha256(&bytes).0
    );
    conn.put_immutable_verified(&key, &bytes, cancel).map_err(|e| EngineError::Cloud(e.into()))?;
    let root = super::root(&conn.prefix, &state.library).map_err(|e| EngineError::Cloud(e.into()))?;
    progress.scan.checkpoints.insert(super::discovery::relative(&root, &key).map_err(|e| EngineError::Cloud(e.into()))?.into());
    progress.remote =
        crate::cloud_history::native::certify(conn, state, &progress.remote, &mut proofs, cancel).map_err(|e| EngineError::Cloud(e.into()))?;
    Ok(crate::cloud_replication::Outcome::append(progress, Default::default()))
}
