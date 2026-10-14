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
