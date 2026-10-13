//! Explicit, coordinated migration. Legacy objects remain available for recovery.
use crate::{EngineError, Result, Session};
use lightcraft_catalog::{
    Op,
    replication::{AppendProgress, SyncProtocol, publication_prefixes},
};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
pub fn migrate(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("writersStopped").and_then(Value::as_bool) != Some(true) {
        return Err(error("migration requires writersStopped:true after pausing every device; older clients must remain stopped until upgraded"));
    }
    if s.cloud_replication_jobs.running() || s.cloud_uploads.running() > 0 {
        return Err(error("finish or cancel active transfers before migration"));
    }
    s.persist()?;
    let mut state = s.catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    if !state.protocol.is_head() {
        return s.execute("cloud.syncStatus", p);
    }
    let lib = s.library.as_mut().ok_or_else(|| error("persistent library required"))?;
    let conn = lib.cloud_connections()?.into_iter().find(|c| c.id == state.connection).ok_or_else(|| error("storage connection unavailable"))?;
    let owner = super::owner::identity(&lib.dir)?;
    let cancel = AtomicBool::new(false);
    let remote = crate::cloud_replication::read(&conn, &state.library, &cancel).map_err(|e| error(e.to_string()))?;
    let portable = remote.state.portable();
    let progress = AppendProgress {
        version: 1,
        manifest: crate::assets::sha256(&super::manifest(&state.library).map_err(|e| error(e.to_string()))?).0,
        frontiers: publication_prefixes(&portable)?,
        remote: portable,
        gaps: Default::default(),
    };
    progress.validate(&state.library)?;
    conn.put_immutable_verified(
        &super::manifest_key(&conn.prefix, &state.library).map_err(|e| error(e.to_string()))?,
        &super::manifest(&state.library).map_err(|e| error(e.to_string()))?,
        &cancel,
    )
    .map_err(|e| error(e.to_string()))?;
    let bytes = super::encode(&progress).map_err(|e| error(e.to_string()))?;
    let key = format!(
        "{}/checkpoints/{}/{}.json",
        super::root(&conn.prefix, &state.library).map_err(|e| error(e.to_string()))?,
        state.device,
        crate::assets::sha256(&bytes).0
    );
    conn.put_immutable_verified(&key, &bytes, &cancel).map_err(|e| error(e.to_string()))?;
    state.protocol = SyncProtocol::AppendLog;
    state.migrated_from_head = true;
    state.cursor = None;
    state.generation = 0;
    state.claim_owner(owner, super::owner::random()?)?;
    let binding = Op::SetSyncState { state: Some(Box::new(state)) };
    let mut next = s.catalog.clone();
    next.apply(binding.clone())?;
    let incoming = crate::cloud_sync::incoming_append(&next, progress, &Default::default())?;
    let inverse = s.apply_catalog_mode(Op::Batch { ops: vec![binding, incoming] }, false)?;
    if let Err(e) = s.persist() {
        s.apply_received(inverse)?;
        return Err(e);
    }
    Ok(json!({"migrated":true,"protocol":"append-log-v1","legacyHistoryRetained":true,"pending":s.catalog.sync_state().map_or(0,|s|s.pending.len())}))
}
