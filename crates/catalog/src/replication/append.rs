//! Device-local append-log progress. Causal knowledge is not a publication frontier.
use super::{Clock, Portable, Stamp, invalid};
use crate::{Result, assets::identifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncProtocol {
    #[default]
    #[serde(rename = "conditional-head-v1")]
    ConditionalHead,
    #[serde(rename = "append-log-v1")]
    AppendLog,
}
impl SyncProtocol {
    pub fn is_head(&self) -> bool {
        *self == Self::ConditionalHead
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppendProgress {
    pub version: u32,
    pub manifest: String,
    pub frontiers: Clock,
    /// Only effects of verified remote publications, never the local pending projection.
    pub remote: Portable,
    #[serde(default)]
    pub gaps: BTreeSet<String>,
}
pub fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
pub fn stamp(id: &str) -> Result<Stamp> {
    let (device, sequence) = id.rsplit_once('-').ok_or_else(|| invalid("invalid append revision identity"))?;
    let stamp = Stamp { device: device.into(), sequence: sequence.parse().map_err(|_| invalid("invalid append sequence"))? };
    stamp.validate()?;
    Ok(stamp)
}
/// Prefixes are certified by exact receipts, not maximum observed clocks.
pub fn prefixes(remote: &Portable) -> Result<Clock> {
    remote.validate()?;
    let mut sequences: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for id in remote.received.keys() {
        let s = stamp(id)?;
        sequences.entry(s.device).or_default().insert(s.sequence);
    }
    let mut result: Clock = remote.archives.iter().map(|(author, prefix)| (author.clone(), prefix.through)).collect();
    for (author, values) in sequences {
        let mut prefix = result.get(&author).copied().unwrap_or(0);
        for sequence in values {
            if sequence <= prefix {
                continue;
            }
            if prefix.checked_add(1) != Some(sequence) {
                break;
            }
            prefix = sequence;
        }
        if prefix > 0 {
            result.insert(author, prefix);
        }
    }
    Ok(result)
}
impl AppendProgress {
    /// Combine independently verified publications; owner completion order is arbitrary.
    pub fn merge(&self, incoming: &Self) -> Result<Self> {
        self.validate(&self.remote.library)?;
        incoming.validate(&self.remote.library)?;
        if self.manifest != incoming.manifest {
            return Err(invalid("append publication manifest changed; local progress retained"));
        }
        let mut remote = super::SyncState::restore_durable(&self.remote, "publication".into(), "remote".into())?;
        remote.merge(&incoming.remote)?;
        let mut remote = remote.portable();
        // Incoming transport may promote archived receipts needed by a later
        // browser ACK transaction. Merge authenticated them before pruning;
        // keep these witnesses until the following remote scan replaces them.
        remote.received.extend(incoming.remote.received.clone());
        let frontiers = prefixes(&remote)?;
        let mut gaps = self.gaps.clone();
        gaps.extend(incoming.gaps.iter().cloned());
        gaps.retain(|id| !stamp(id).is_ok_and(|s| frontiers.get(&s.device).copied().unwrap_or(0) >= s.sequence));
        let merged = Self { version: 1, manifest: self.manifest.clone(), frontiers, remote, gaps: gaps.into_iter().take(1000).collect() };
        merged.validate(&merged.remote.library)?;
        Ok(merged)
    }
    pub fn validate(&self, library: &str) -> Result<()> {
        if self.version != 1
            || !hash(&self.manifest)
            || self.remote.library != library
            || self.gaps.len() > 1000
            || self.gaps.iter().any(|id| !identifier(id) || id.len() > 256)
        {
            return Err(invalid("invalid append progress; pending work retained"));
        }
        if prefixes(&self.remote)? != self.frontiers || self.remote.clock != self.frontiers {
            return Err(invalid("append checkpoint/progress contains a gap or unobserved causal dependency"));
        }
        Ok(())
    }
    /// A local durability proof, never a synthetic globally ordered catalog head.
    pub fn digest(&self) -> Result<String> {
        self.validate(&self.remote.library)?;
        let mut durable = self.clone();
        durable.remote.proofs.clear();
        let bytes = serde_json::to_vec(&durable).map_err(|e| invalid(&e.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::{Change, SyncState};
    use serde_json::json;

    fn progress(state: &SyncState) -> AppendProgress {
        let remote = state.portable();
        AppendProgress { version: 1, manifest: "a".repeat(64), frontiers: prefixes(&remote).unwrap(), remote, gaps: BTreeSet::new() }
    }
    fn edit(state: &mut SyncState, field: &str, value: u8) {
        state.enqueue(vec![Change { entity: "photo:one".into(), field: field.into(), base: None, value: json!(value) }]).unwrap();
    }
    #[test]
    fn completion_order_cannot_regress_frontiers_remote_effects_or_resolved_gaps() {
        let mut state = SyncState::new("library".into(), "a".into(), "storage".into()).unwrap();
        state.protocol = SyncProtocol::AppendLog;
        edit(&mut state, "rating", 1);
        let mut old = progress(&state);
        old.gaps.insert("a-2".into());
        edit(&mut state, "rating", 5);
        let new = progress(&state);
        state.acknowledge_append(&state.pending.keys().cloned().collect(), new.clone()).unwrap();
        state.acknowledge_append(&BTreeSet::new(), old.clone()).unwrap();
        assert_eq!(state.append.as_deref(), Some(&new));
        assert_eq!(old.merge(&new).unwrap(), new.merge(&old).unwrap());
        assert!(state.pending.is_empty());
    }
    #[test]
    fn merge_retains_incoming_archived_receipts_until_delayed_ack_finishes() {
        let mut state = SyncState::new("library".into(), "a".into(), "storage".into()).unwrap();
        state.protocol = SyncProtocol::AppendLog;
        edit(&mut state, "rating", 1);
        let first = state.received["a-1"].clone();
        let old = progress(&state);
        state.acknowledge_append(&BTreeSet::new(), old.clone()).unwrap();
        edit(&mut state, "rating", 5);
        let mut remote = SyncState::restore_durable(&state.portable(), "remote".into(), "storage".into()).unwrap();
        let plan = remote.archive_plan().unwrap();
        remote.compact_receipts(&plan).unwrap();
        remote.prove_archived("a-1", first.clone(), &plan.objects).unwrap();
        let mut incoming = progress(&remote);
        incoming.remote.proofs = plan.objects;
        state.merge(&incoming.remote).unwrap();
        state.acknowledge_append(&BTreeSet::new(), incoming).unwrap();
        assert_eq!(state.append.as_ref().unwrap().remote.received.get("a-1"), Some(&first));
        // Mimic reloading between the receive and browser ACK transactions.
        let mut loaded: SyncState = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let saved = *loaded.append.clone().unwrap();
        loaded.acknowledge_append(&BTreeSet::from(["a-1".into()]), saved).unwrap();
        assert_eq!(loaded.pending.keys().cloned().collect::<Vec<_>>(), vec!["a-2".to_owned()]);
    }
    #[test]
    fn independent_verified_progress_merges_and_collisions_remain_errors() {
        let mut a = SyncState::new("library".into(), "a".into(), "storage".into()).unwrap();
        let mut b = SyncState::new("library".into(), "b".into(), "storage".into()).unwrap();
        edit(&mut a, "rating", 4);
        edit(&mut b, "flag", 1);
        let ap = progress(&a);
        let bp = progress(&b);
        let merged = ap.merge(&bp).unwrap();
        assert_eq!(merged, bp.merge(&ap).unwrap());
        assert_eq!(merged.frontiers, Clock::from([("a".into(), 1), ("b".into(), 1)]));
        let mut fork = ap.clone();
        fork.remote.received.get_mut("a-1").unwrap().digest = "f".repeat(64);
        assert!(ap.merge(&fork).is_err());
        let mut other_manifest = bp;
        other_manifest.manifest = "b".repeat(64);
        assert!(ap.merge(&other_manifest).is_err());
    }
}
