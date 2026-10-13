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
        Ok(Self { version: 1, library: state.library.clone(), device: state.device.clone(), cursor, generation, acknowledged })
    }
    /// The caller invokes this only after the receipt and incoming catalog writes completed.
    /// A stale proof is harmless; a malformed matching proof never drops local work.
    pub fn finish(&self, state: &mut SyncState) -> Result<bool, String> {
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
