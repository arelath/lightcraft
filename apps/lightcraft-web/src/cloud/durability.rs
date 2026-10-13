//! A device-local remote acknowledgement proof bridges the synchronous catalog mirror and
//! asynchronous browser storage. Pending mutations remain in memory until both incoming
//! catalog/cursor and this receipt have reached storage; reload can then finish discarding them.
use lightcraft_engine::catalog::replication::SyncState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const ACK_FILE: &str = "browser-cloud-ack.json";
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AckReceipt {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    append_digest: Option<String>,
    library: String,
    device: String,
    cursor: String,
    generation: u64,
    acknowledged: BTreeMap<String, String>,
}
impl AckReceipt {
    pub fn new(state: &SyncState, ids: &BTreeSet<String>, cursor: String, generation: u64) -> Result<Self, String> {
        if ids.len() > 10000 {
            return Err("browser acknowledgement exceeds its bounded count".into());
        }
        let mut acknowledged = BTreeMap::new();
        for id in ids {
            let receipt = state.received.get(id).ok_or("remote acknowledged an unknown mutation")?;
            acknowledged.insert(id.clone(), receipt.digest.clone());
        }
        Ok(Self { append_digest: None, version: 1, library: state.library.clone(), device: state.device.clone(), cursor, generation, acknowledged })
    }
    pub fn new_append(
        state: &SyncState,
        ids: &BTreeSet<String>,
        progress: &lightcraft_engine::catalog::replication::AppendProgress,
    ) -> Result<Self, String> {
        progress.validate(&state.library).map_err(|e| e.to_string())?;
        if state.protocol.is_head() || ids.len() > 10000 {
            return Err("invalid append acknowledgement".into());
        }
        let mut acknowledged = BTreeMap::new();
        for id in ids {
            let receipt = state.received.get(id).ok_or("unknown local mutation")?;
            if progress.remote.received.get(id).is_none_or(|r| r.digest != receipt.digest) {
                return Err("missing exact append publication proof".into());
            }
            acknowledged.insert(id.clone(), receipt.digest.clone());
        }
        Ok(Self {
            version: 2,
            library: state.library.clone(),
            device: state.device.clone(),
            cursor: String::new(),
            generation: 0,
            append_digest: Some(progress.digest().map_err(|e| e.to_string())?),
            acknowledged,
        })
    }
    /// The caller invokes this only after the receipt and incoming catalog writes completed.
    /// A stale proof is harmless; a malformed matching proof never drops local work.
    pub fn finish(&self, state: &mut SyncState) -> Result<bool, String> {
        if self.version == 2 {
            if self.library != state.library {
                return Ok(false);
            }
            if state.protocol.is_head() || self.generation != 0 || !self.cursor.is_empty() || self.acknowledged.len() > 10000 {
                return Err("invalid append acknowledgement receipt".into());
            }
            let Some(progress) = state.append.as_ref() else { return Ok(false) };
            if self.append_digest.as_deref() != Some(progress.digest().map_err(|e| e.to_string())?.as_str()) {
                return Ok(false);
            }
            for (id, digest) in &self.acknowledged {
                if state.received.get(id).is_none_or(|r| &r.digest != digest) || progress.remote.received.get(id).is_none_or(|r| &r.digest != digest)
                {
                    return Err("append receipt identity mismatch; work retained".into());
                }
            }
            let ids = self.acknowledged.keys().cloned().collect();
            let progress = (**progress).clone();
            state.acknowledge_append(&ids, progress).map_err(|e| e.to_string())?;
            return Ok(true);
        }
        if self.version != 1
            || self.acknowledged.len() > 10000
            || self.cursor.len() != 64
            || !self.cursor.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            || self.generation == 0
        {
            return Err("invalid browser acknowledgement receipt; pending work retained".into());
        }
        if self.library != state.library
            || self.device != state.device
            || state.cursor.as_ref() != Some(&self.cursor)
            || state.generation != self.generation
        {
            return Ok(false);
        }
        for (id, digest) in &self.acknowledged {
            if state.received.get(id).is_none_or(|receipt| &receipt.digest != digest) {
                return Err("browser acknowledgement receipt identity mismatch; pending work retained".into());
            }
        }
        let ids = self.acknowledged.keys().cloned().collect();
        state.acknowledge(&ids, self.cursor.clone(), self.generation).map_err(|e| e.to_string())?;
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::catalog::replication::Change;
    use serde_json::json;
    #[test]
    fn append_ack_requires_durable_remote_progress_and_preserves_later_and_historical_pending_ids() {
        use lightcraft_engine::{catalog::replication::SyncProtocol, cloud_append::Planner};
        let mut s = SyncState::new("library".into(), "browser".into(), "connection".into()).unwrap();
        s.protocol = SyncProtocol::AppendLog;
        let m = s.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(4) }]).unwrap().unwrap();
        let mut planner = Planner::new(&s, true).unwrap();
        planner.stage(m.clone()).unwrap();
        let p = planner.progress().unwrap();
        let receipt = AckReceipt::new_append(&s, &BTreeSet::from([m.revision.id()]), &p).unwrap();
        assert!(!receipt.finish(&mut s).unwrap());
        assert_eq!(s.pending.len(), 1);
        s.acknowledge_append(&BTreeSet::new(), p).unwrap();
        let saved = serde_json::to_vec(&s).unwrap();
        s.local_owner = "old".into();
        s.claim_owner("restored".into(), "new-author".into()).unwrap();
        s.enqueue(vec![Change { entity: "photo:one".into(), field: "flag".into(), base: None, value: json!("pick") }]).unwrap();
        receipt.finish(&mut s).unwrap();
        assert_eq!(s.pending.len(), 1);
        let mut reloaded: SyncState = serde_json::from_slice(&saved).unwrap();
        receipt.finish(&mut reloaded).unwrap();
        assert!(reloaded.pending.is_empty());
        let mut bad = receipt;
        bad.acknowledged.insert(m.revision.id(), "a".repeat(64));
        let before = s.clone();
        assert!(bad.finish(&mut s).is_err());
        assert_eq!(before, s);
    }
    #[test]
    fn asynchronous_ack_preserves_later_edits_and_can_finish_after_reload() {
        let mut state = SyncState::new("library".into(), "browser".into(), "connection".into()).unwrap();
        let first = state.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(4) }]).unwrap().unwrap();
        let cursor = "a".repeat(64);
        let ids = BTreeSet::from([first.revision.id()]);
        state.acknowledge(&BTreeSet::new(), cursor.clone(), 1).unwrap();
        let receipt = AckReceipt::new(&state, &ids, cursor, 1).unwrap();
        let saved = serde_json::to_vec(&state).unwrap();
        state.enqueue(vec![Change { entity: "photo:one".into(), field: "flag".into(), base: None, value: json!("pick") }]).unwrap();
        assert_eq!(state.pending.len(), 2, "pending survives a failed async save");
        receipt.finish(&mut state).unwrap();
        assert_eq!(state.pending.len(), 1, "later offline work remains pending");
        let mut reloaded: SyncState = serde_json::from_slice(&saved).unwrap();
        receipt.finish(&mut reloaded).unwrap();
        assert!(reloaded.pending.is_empty());
        let mut bad = receipt;
        bad.acknowledged.insert(first.revision.id(), "wrong digest".into());
        assert!(bad.finish(&mut state).is_err());
        assert_eq!(state.pending.len(), 1);
    }
}
