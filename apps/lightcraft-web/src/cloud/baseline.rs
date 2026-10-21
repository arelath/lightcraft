//! Disposable pure remote baseline. Only catalog ACK durability can remove pending IDs.
use super::{Provider, metadata, protocol::Remote};
use crate::backend::Backend;
use lightcraft_engine::{
    catalog::replication::{Portable, SyncState},
    cloud_records::{CheckpointRef, RecordRef},
    cloud_status::CloudFailure,
};
use serde::{Deserialize, Serialize};
const MAX: usize = 64 << 20;
const FILE: &str = "cloud/head-baseline-v2.json";
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Baseline {
    version: u32,
    destination: String,
    cursor: String,
    generation: u64,
    pub tip: RecordRef,
    pub checkpoint: Option<CheckpointRef>,
    remote: Portable,
}
impl Baseline {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub async fn load(backend: &Backend, provider: &Provider, local: &SyncState) -> Option<Self> {
        if provider.fresh_history || !local.protocol.is_head() {
            return None;
        }
        let bytes = backend.read_limited(FILE, MAX as u64).await.ok()??;
        let baseline: Self = serde_json::from_slice(&bytes).ok()?;
        if baseline.version != 2
            || baseline.destination != metadata::destination(&provider.connection)
            || Some(baseline.cursor.as_str()) != local.cursor.as_deref()
            || baseline.generation != local.generation
            || baseline.tip.hash.0 != baseline.cursor
            || baseline.remote.library != local.library
            || !baseline.remote.proofs.is_empty()
        {
            return None;
        }
        baseline.tip.validate_scope(&provider.connection.prefix, "changes", 2 << 20).ok()?;
        if let Some(c) = &baseline.checkpoint {
            c.object.validate_scope(&provider.connection.prefix, "checkpoints", 64 << 20).ok()?;
            c.covers.validate_scope(&provider.connection.prefix, "changes", 2 << 20).ok()?;
            if c.generation == 0 || c.generation >= baseline.generation {
                return None;
            }
        }
        baseline.state(provider).ok()?;
        Some(baseline)
    }
    pub fn observed(provider: &Provider, remote: &Remote) -> Option<Self> {
        if remote.recovery.is_some() {
            return None;
        }
        let (head, _) = remote.head.as_ref()?;
        let mut state = remote.state.portable();
        state.proofs.clear();
        Some(Self {
            version: 2,
            destination: metadata::destination(&provider.connection),
            cursor: head.tip.hash.0.clone(),
            generation: head.generation,
            tip: head.tip.clone(),
            checkpoint: head.checkpoint.clone(),
            remote: state,
        })
    }
    pub fn state(&self, provider: &Provider) -> Result<SyncState, CloudFailure> {
        let mut state = SyncState::restore_durable(&self.remote, "remote".into(), provider.connection.id.clone()).map_err(CloudFailure::from)?;
        state.cursor = Some(self.cursor.clone());
        state.generation = self.generation;
        Ok(state)
    }
    pub async fn save(&self, backend: &Backend) {
        let Ok(bytes) = serde_json::to_vec(self) else { return };
        if bytes.len() <= MAX {
            let _ = super::cache::storage::auxiliary(backend, FILE, &bytes).await;
        }
    }
}
