//! Origin-wide managed cache policy. Only verified cloud cache entries are disposable;
//! imported originals and every library's offline owners remain outside eviction.
use lightcraft_engine::assets::cache::OfflineQuality;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const POLICY_KEY: &str = "cloud/cache-policy.json";
pub const POLICY_LIMIT: u64 = 16 << 20;
pub const HEADROOM: u64 = 64 << 20;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub size: u64,
    pub used: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Owner {
    pub quality: OfflineQuality,
    pub blobs: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    version: u32,
    pub budget: u64,
    pub headroom: u64,
    pub entries: BTreeMap<String, Entry>,
    /// Namespace -> owner -> last durably resolved dependencies. Closed libraries
    /// retain protection; opening one resolves owner/quality against its catalog.
    pub libraries: BTreeMap<String, BTreeMap<String, Owner>>,
}
impl Default for Policy {
    fn default() -> Self {
        Self { version: 1, budget: 128 << 20, headroom: HEADROOM, entries: BTreeMap::new(), libraries: BTreeMap::new() }
    }
}
fn hash(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
impl Policy {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > POLICY_LIMIT {
            return Err("browser cache policy exceeds 16 MiB; cache retained for repair".into());
        }
        let policy: Self = serde_json::from_slice(bytes).map_err(|_| "browser cache policy is unreadable; offline data retained for repair")?;
        if policy.version != 1
            || policy.entries.len() > 100_000
            || policy.libraries.len() > 4096
            || policy.budget > 4u64 << 30
            || !(HEADROOM..=(16u64 << 30)).contains(&policy.headroom)
            || policy.entries.iter().any(|(id, e)| !hash(id) || e.size == 0 || e.size > super::MAX_BROWSER_BLOB)
            || policy.libraries.iter().any(|(dir, owners)| {
                !crate::backup::valid_library_dir(dir)
                    || owners.len() > 4096
                    || owners.iter().any(|(owner, p)| {
                        (!matches!(owner.as_str(), "all:originals" | "all:previews")
                            && !owner.split_once(':').is_some_and(|(kind, id)| {
                                matches!(kind, "photo" | "album") && id.parse::<u64>().is_ok_and(|id| id > 0 && id < u64::MAX)
                            }))
                            || p.blobs.len() > 100_000
                            || p.blobs.iter().any(|id| !hash(id))
                    })
            })
        {
            return Err("invalid or future browser cache policy; offline data retained for repair".into());
        }
        Ok(policy)
    }
    pub fn protected(&self) -> BTreeSet<&str> {
        self.libraries.values().flat_map(|owners| owners.values()).flat_map(|p| p.blobs.iter().map(String::as_str)).collect()
    }
    pub fn bytes(&self) -> u64 {
        self.entries.values().fold(0u64, |n, e| n.saturating_add(e.size))
    }
    /// Pure admission plan. Estimates are hints; write failures still retain the
    /// catalog dirty queue. Unknown/unindexed files never become candidates.
    pub fn evictions(&self, incoming: u64, free: u64, reserve: u64, budget: u64, keep: Option<&str>) -> Result<Vec<String>, String> {
        let protected = self.protected();
        let mut candidates: Vec<_> = self.entries.iter().filter(|(id, _)| Some(id.as_str()) != keep && !protected.contains(id.as_str())).collect();
        candidates.sort_by_key(|(id, e)| (e.used, *id));
        let mut total = self.bytes();
        let mut available = free;
        let mut gone = Vec::new();
        let old = keep.and_then(|id| self.entries.get(id)).map_or(0, |e| e.size);
        for (id, e) in candidates {
            if available >= incoming.saturating_add(reserve) && total.saturating_sub(old).saturating_add(incoming) <= budget {
                break;
            }
            available = available.saturating_add(e.size);
            total = total.saturating_sub(e.size);
            gone.push(id.clone());
        }
        if available < incoming.saturating_add(reserve) || total.saturating_sub(old).saturating_add(incoming) > budget {
            return Err("browser cache has insufficient space after protecting offline copies and catalog headroom; unpin copies or free site storage, then retry".into());
        }
        Ok(gone)
    }
}

pub const OWNERS_FILE: &str = "browser-offline.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    owners: BTreeMap<String, lightcraft_engine::assets::cache::OfflinePin>,
}
pub fn encode_intent(owners: &BTreeMap<String, lightcraft_engine::assets::cache::OfflinePin>) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(&Intent { version: 1, owners: owners.clone() }).map_err(|e| e.to_string())?;
    decode_intent(&bytes)?;
    Ok(bytes)
}
pub fn decode_intent(bytes: &[u8]) -> Result<BTreeMap<String, lightcraft_engine::assets::cache::OfflinePin>, String> {
    if bytes.len() as u64 > POLICY_LIMIT {
        return Err("offline intent exceeds 16 MiB; choose smaller offline selections".into());
    }
    let intent: Intent = serde_json::from_slice(bytes).map_err(|_| "browser offline intent is unreadable; saved copies retained")?;
    if intent.version != 1 {
        return Err("unsupported browser offline intent; saved copies retained".into());
    }
    let mut policy = Policy::default();
    policy.libraries.insert(
        "library".into(),
        intent
            .owners
            .iter()
            .map(|(owner, pin)| (owner.clone(), Owner { quality: pin.quality, blobs: pin.blobs.iter().map(|id| id.0.clone()).collect() }))
            .collect(),
    );
    Policy::decode(&serde_json::to_vec(&policy).map_err(|e| e.to_string())?)?;
    if intent.owners.values().any(|pin| !pin.blobs.is_empty()) {
        return Err("offline intent must retain owners and quality, not frozen membership".into());
    }
    Ok(intent.owners)
}

#[cfg(target_arch = "wasm32")]
pub mod offline;
#[cfg(any(target_arch = "wasm32", test))]
mod recovery;
#[cfg(target_arch = "wasm32")]
pub mod storage;

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(p: &mut Policy, id: char, used: u64) {
        p.entries.insert(id.to_string().repeat(64), Entry { size: 8, used });
    }
    #[test]
    fn quota_eviction_keeps_closed_library_pins_and_current_payload() {
        let mut p = Policy::default();
        entry(&mut p, 'a', 1);
        entry(&mut p, 'b', 2);
        entry(&mut p, 'c', 3);
        p.libraries
            .insert("library".into(), [("photo:1".into(), Owner { quality: OfflineQuality::Original, blobs: ["a".repeat(64)].into() })].into());
        assert_eq!(p.evictions(8, 0, 0, 100, Some(&"c".repeat(64))).unwrap(), vec!["b".repeat(64)]);
        assert!(p.evictions(17, 0, 0, 100, Some(&"c".repeat(64))).is_err());
        assert_eq!(p.entries.len(), 3, "planning never removes data");
    }
    #[test]
    fn catalog_headroom_is_separate_from_cache_budget_and_unknown_files() {
        let mut p = Policy::default();
        entry(&mut p, 'a', 1);
        entry(&mut p, 'b', 2);
        assert_eq!(p.evictions(8, 8, 8, 100, None).unwrap(), vec!["a".repeat(64)]);
        assert_eq!(p.evictions(4, 4, 0, u64::MAX, None).unwrap(), Vec::<String>::new());
        assert!(p.evictions(u64::MAX, 0, HEADROOM, 100, None).is_err());
    }
    #[test]
    fn malformed_and_future_policy_fail_closed() {
        let p = Policy::default();
        let mut v = serde_json::to_value(&p).unwrap();
        v["version"] = serde_json::json!(2);
        assert!(Policy::decode(&serde_json::to_vec(&v).unwrap()).is_err());
        v["version"] = serde_json::json!(1);
        v["entries"] = serde_json::json!({"../originals/owned":{"size":8,"used":0}});
        assert!(Policy::decode(&serde_json::to_vec(&v).unwrap()).is_err());
    }
    #[test]
    fn backed_up_intent_retains_empty_owners_and_quality_without_snapshot_members() {
        use lightcraft_engine::assets::cache::OfflinePin;
        let owners = [
            ("album:4".into(), OfflinePin { quality: OfflineQuality::Preview, blobs: Vec::new() }),
            ("all:originals".into(), OfflinePin { quality: OfflineQuality::Original, blobs: Vec::new() }),
        ]
        .into();
        let bytes = encode_intent(&owners).unwrap();
        assert_eq!(decode_intent(&bytes).unwrap(), owners);
        let mut snapshot = owners.clone();
        snapshot.get_mut("album:4").unwrap().blobs.push(lightcraft_engine::catalog::assets::BlobId("a".repeat(64)));
        assert!(encode_intent(&snapshot).is_err());
        let mut future: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        future["version"] = serde_json::json!(2);
        assert!(decode_intent(&serde_json::to_vec(&future).unwrap()).is_err());
        assert!(crate::backup::restore_key("library/browser-offline.json", "library-restored").is_some());
    }
    #[test]
    fn generated_eviction_plans_never_release_any_librarys_pins() {
        for count in [1, 4, 19, 100] {
            let mut policy = Policy::default();
            let mut owners = BTreeMap::new();
            for n in 0..count {
                let id = format!("{n:064x}");
                policy.entries.insert(id.clone(), Entry { size: 7, used: (count - n) as u64 });
                if n % 3 == 0 {
                    owners.insert(format!("photo:{}", n + 1), Owner { quality: OfflineQuality::Original, blobs: [id].into() });
                }
            }
            policy.libraries.insert("library-closed".into(), owners);
            for free in [0, 7, 1000] {
                for incoming in [0, 1, 20, 1000] {
                    if let Ok(gone) = policy.evictions(incoming, free, 0, u64::MAX, None) {
                        assert!(gone.iter().all(|id| !policy.protected().contains(id.as_str())));
                        let reclaimed: u64 = gone.iter().map(|id| policy.entries[id].size).sum();
                        assert!(free + reclaimed >= incoming);
                    }
                }
            }
        }
    }
}
