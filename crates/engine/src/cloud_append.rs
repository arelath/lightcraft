//! Shared append-log records and causal scheduling. No mutable remote coordination object;
//! library control records under `control/` are a cooperative barrier (see `control`).
use crate::assets::{AssetError, Result, keys, sha256};
use lightcraft_catalog::assets::{Blob, identifier};
use lightcraft_catalog::replication::{AppendProgress, Mutation, SyncState, publication_prefixes};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_OBJECT: usize = 64 << 20;
pub const MAX_RECORDS: usize = 100_000;
fn invalid(message: impl Into<String>) -> AssetError {
    AssetError::Invalid(message.into())
}
fn ce(error: lightcraft_catalog::CatalogError) -> AssetError {
    error.into()
}
pub fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("cannot encode append record"))?;
    if bytes.len() > MAX_OBJECT {
        return Err(AssetError::Capacity("append object exceeds 64 MiB".into()));
    }
    Ok(bytes)
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub library: String,
    pub protocol: String,
}
pub fn manifest(library: &str) -> Result<Vec<u8>> {
    if !identifier(library) || library.len() > 96 {
        return Err(invalid("invalid append library identity"));
    }
    encode(&Manifest { version: 1, library: library.into(), protocol: "append-log-v1".into() })
}
pub fn root(prefix: &str, library: &str) -> Result<String> {
    manifest(library)?;
    keys::in_library(prefix, &format!("sync-v2/{library}"))
}
pub fn manifest_key(prefix: &str, library: &str) -> Result<String> {
    Ok(format!("{}/manifest/{}.json", root(prefix, library)?, sha256(&manifest(library)?).0))
}
pub fn mutation_key(prefix: &str, mutation: &Mutation) -> Result<String> {
    mutation.validate().map_err(ce)?;
    Ok(format!(
        "{}/ops/{}/{:020}-{}.json",
        root(prefix, &mutation.library)?,
        mutation.revision.device,
        mutation.revision.sequence,
        mutation.digest().map_err(ce)?
    ))
}
pub fn decode_mutation(prefix: &str, library: &str, key: &str, bytes: &[u8]) -> Result<Mutation> {
    if bytes.len() > lightcraft_catalog::replication::MAX_MUTATION_BYTES {
        return Err(invalid("oversized append mutation"));
    }
    let m: Mutation = serde_json::from_slice(bytes).map_err(|_| invalid("malformed append mutation"))?;
    if m.library != library || mutation_key(prefix, &m)? != key || encode(&m)? != bytes {
        return Err(AssetError::Integrity);
    }
    Ok(m)
}
/// Narrow write capability: metadata must use its exact hash in an append namespace.
pub fn immutable_metadata(key: &str, bytes: &[u8]) -> Result<()> {
    if key.len() > 2048 || key.contains('\\') || key.chars().any(char::is_control) || key.split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
    {
        return Err(invalid("invalid immutable metadata key"));
    }
    let parts: Vec<_> = key.split('/').collect();
    let start = parts
        .iter()
        .enumerate()
        .find_map(|(i, p)| (*p == "sync-v2" && matches!(parts.len().saturating_sub(i), 4 | 5)).then_some(i))
        .ok_or_else(|| invalid("unconditional metadata requires append namespace"))?;
    let tail = parts.get(start..).ok_or(AssetError::Integrity)?;
    let valid = match tail {
        [_, library, "manifest", name] => identifier(library) && *name == format!("{}.json", sha256(bytes).0),
        [_, library, "receipts", name] => {
            identifier(library)
                && *name == format!("{}.json", sha256(bytes).0)
                && lightcraft_catalog::replication::archive::Node::decode(&sha256(bytes).0, bytes).is_ok_and(|node| node.library == *library)
        }
        [_, library, "control", _] => {
            identifier(library)
                && serde_json::from_slice::<crate::cloud_control::Control>(bytes)
                    .ok()
                    .filter(|c| control::encode(c).is_ok_and(|canonical| canonical == bytes))
                    .zip(parts.get(..start).map(|storage| storage.join("/")))
                    .and_then(|(c, storage)| control::key(&storage, &c).ok())
                    .is_some_and(|expected| expected == key)
        }
        [_, library, "checkpoints", author, name] => identifier(library) && identifier(author) && *name == format!("{}.json", sha256(bytes).0),
        [_, library, "ops", author, name] => {
            let m: Mutation = serde_json::from_slice(bytes).map_err(|_| invalid("invalid immutable mutation"))?;
            m.validate().map_err(ce)?;
            m.library == *library
                && m.revision.device == *author
                && *name == format!("{:020}-{}.json", m.revision.sequence, m.digest().map_err(ce)?)
                && encode(&m)? == bytes
        }
        _ => false,
    };
    if !valid || bytes.len() > MAX_OBJECT {
        return Err(invalid("unconditional write is not a content-addressed append record"));
    }
    Ok(())
}
pub fn immutable_blob(blob: &Blob) -> Result<()> {
    blob.validate().map_err(invalid)?;
    let name = blob.remote.key.rsplit('/').next().ok_or(AssetError::Integrity)?;
    if blob.remote.version.is_some()
        || !(name == blob.id.0
            || name == format!("{}.lcsp", blob.id.0)
            || name == format!("{}.png", blob.id.0)
            || name == format!("{}.xmp", blob.id.0))
    {
        return Err(invalid("unconditional media write requires a content hash key"));
    }
    Ok(())
}
pub struct Planner {
    pub remote: SyncState,
    pub proofs: lightcraft_catalog::replication::archive::Witness,
    pub scan: lightcraft_catalog::replication::AppendScan,
    staged: BTreeMap<String, Mutation>,
    staged_bytes: usize,
    manifest: String,
}
impl Planner {
    pub fn new(local: &SyncState) -> Result<Self> {
        local.validate().map_err(ce)?;
        let mut remote = SyncState::new(local.library.clone(), "remote".into(), local.connection.clone()).map_err(ce)?;
        if let Some(progress) = &local.append {
            progress.validate(&local.library).map_err(ce)?;
            remote = SyncState::restore_durable(&progress.remote, "remote".into(), local.connection.clone()).map_err(ce)?;
        }
        Ok(Self {
            remote,
            proofs: BTreeMap::new(),
            scan: local.append.as_ref().map(|p| p.scan.clone()).unwrap_or_default(),
            staged: BTreeMap::new(),
            staged_bytes: 0,
            manifest: sha256(&manifest(&local.library)?).0,
        })
    }
    pub fn checkpoint(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_OBJECT {
            return Err(invalid("oversized append checkpoint"));
        }
        let p: AppendProgress = serde_json::from_slice(bytes).map_err(|_| invalid("malformed append checkpoint"))?;
        p.validate(&self.remote.library).map_err(ce)?;
        if p.manifest != self.manifest || !p.gaps.is_empty() {
            return Err(invalid("checkpoint has incomplete publication coverage"));
        }
        self.remote.merge(&p.remote).map_err(ce)
    }
    pub fn stage(&mut self, m: Mutation) -> Result<()> {
        m.validate().map_err(ce)?;
        let id = m.revision.id();
        let digest = m.digest().map_err(ce)?;
        if lightcraft_catalog::replication::archive::covers(&self.remote.archives, &id) {
            lightcraft_catalog::replication::archive::verify_receipt(
                &self.remote.library,
                &self.remote.archives,
                &id,
                &crate::cloud_history::receipt(&m).map_err(ce)?,
                &self.proofs,
            )
            .map_err(ce)?;
            return Ok(());
        }
        if m.library != self.remote.library
            || self.remote.received.get(&id).is_some_and(|r| r.digest != digest)
            || self.staged.get(&id).is_some_and(|old| old != &m)
        {
            return Err(invalid("append author/sequence collision; both objects retained, publication stopped"));
        }
        if !self.remote.received.contains_key(&id) && !self.staged.contains_key(&id) {
            let size = encode(&m)?.len();
            if self.staged_bytes.saturating_add(size) > MAX_OBJECT {
                return Err(AssetError::Capacity("staged causal dependencies exceed 64 MiB; local work retained".into()));
            }
            self.staged_bytes = self.staged_bytes.saturating_add(size);
            self.staged.insert(id, m);
        }
        if self.staged.len().saturating_add(self.remote.received.len()) > MAX_RECORDS {
            return Err(AssetError::Capacity("append record limit exceeded; local work retained".into()));
        }
        Ok(())
    }
    pub fn ready(&self, m: &Mutation) -> bool {
        self.remote.clock.get(&m.revision.device).copied().unwrap_or(0).checked_add(1) == Some(m.revision.sequence)
            && m.context.iter().all(|(author, seq)| self.remote.clock.get(author).copied().unwrap_or(0) >= *seq)
    }
    /// Discovery releases ready payloads immediately. The staging budget bounds
    /// unresolved causal dependencies, not the size of an otherwise ready history.
    pub fn stage_and_drain(&mut self, mutation: Mutation) -> Result<()> {
        self.stage(mutation)?;
        self.drain()
    }
    pub fn drain(&mut self) -> Result<()> {
        loop {
            let ready = self.staged.values().find(|m| self.ready(m)).map(|m| m.revision.id());
            let Some(id) = ready else { break };
            let m = self.staged.remove(&id).ok_or(AssetError::Integrity)?;
            self.staged_bytes = self.staged_bytes.saturating_sub(encode(&m)?.len());
            self.remote.receive(&m).map_err(ce)?;
        }
        Ok(())
    }
    pub fn progress(&mut self) -> Result<AppendProgress> {
        self.drain()?;
        let p = AppendProgress {
            version: 1,
            manifest: self.manifest.clone(),
            frontiers: publication_prefixes(&self.remote.portable()).map_err(ce)?,
            remote: self.remote.portable(),
            gaps: self.staged.keys().take(1000).cloned().collect(),
            scan: self.scan.clone(),
        };
        p.validate(&self.remote.library).map_err(ce)?;
        Ok(p)
    }
    pub fn acknowledged(&self, local: &SyncState) -> Result<BTreeSet<String>> {
        let mut result = BTreeSet::new();
        for (id, m) in &local.pending {
            if let Some(r) = self.remote.received.get(id) {
                if r.digest != m.digest().map_err(ce)? {
                    return Err(AssetError::Integrity);
                }
                result.insert(id.clone());
            } else if lightcraft_catalog::replication::archive::covers(&self.remote.archives, id) {
                return Err(invalid("archived acknowledgement requires hydrated exact receipt proof; pending work retained"));
            }
        }
        Ok(result)
    }
}

pub mod control;
pub mod discovery;
pub mod listing;
#[cfg(not(target_arch = "wasm32"))]
pub mod migration;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use crate::cloud_owner as owner;

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::replication::{Change, SyncProtocol};
    use serde_json::json;
    fn state(author: &str) -> SyncState {
        let mut s = SyncState::new("library".into(), author.into(), "s3".into()).unwrap();
        s.protocol = SyncProtocol::AppendLog;
        s
    }
    fn edit(s: &mut SyncState, n: u32) -> Mutation {
        s.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(n) }]).unwrap().unwrap()
    }
    #[test]
    fn unresolved_planner_capacity_reaches_health_with_maintenance_recovery() {
        use crate::cloud_status::{CloudFailure, PollHealth};
        let mut writer = state("writer");
        edit(&mut writer, 1); // The reader never receives this required predecessor.
        let template = writer
            .enqueue(vec![Change { entity: "photo:one".into(), field: "meta.caption".into(), base: None, value: json!("x".repeat(512 << 10)) }])
            .unwrap()
            .unwrap();
        let mut planner = Planner::new(&state("reader")).unwrap();
        let failure = loop {
            let mut mutation = template.clone();
            let sequence = planner.staged.len() as u64 + 2;
            mutation.revision.sequence = sequence;
            mutation.context.insert("writer".into(), sequence - 1);
            match planner.stage_and_drain(mutation) {
                Ok(()) => assert!(planner.staged_bytes <= MAX_OBJECT),
                Err(error) => break CloudFailure::from(error),
            }
        };
        assert!(planner.remote.clock.is_empty());
        assert!(planner.remote.documents.is_empty());
        assert!(planner.staged_bytes <= MAX_OBJECT);
        let mut health = PollHealth::default();
        assert_eq!(health.fail_typed(&failure, Some(1000), None), None);
        let health = health.view_at(true, false, false, false, true, Some(1000));
        assert_eq!(health["lastError"]["kind"], "capacity");
        assert_eq!(health["lastError"]["recovery"], "reviewCapacity");
        assert_eq!(health["retry"]["scheduled"], false);
        assert!(health["lastError"]["message"].as_str().unwrap().contains("staged causal dependencies"));
    }
    #[test]
    fn catalog_capacity_in_append_helpers_keeps_its_kind() {
        assert!(matches!(ce(lightcraft_catalog::CatalogError::Capacity("retained receipts".into())), AssetError::Capacity(_)));
    }
    #[test]
    fn discovery_releases_three_hundred_large_replacements_and_unblocks_other_authors() {
        let mut writer = state("writer");
        let mut planner = Planner::new(&state("reader")).unwrap();
        for n in 0..300 {
            let base = writer.winning("photo:one", "meta.caption").map(|(stamp, _)| stamp.clone());
            let mutation = writer
                .enqueue(vec![Change {
                    entity: "photo:one".into(),
                    field: "meta.caption".into(),
                    base,
                    value: json!(format!("{n:06}:{}", "z".repeat((256 << 10) - 7))),
                }])
                .unwrap()
                .unwrap();
            writer.pending.clear(); // The writer publishes incrementally, not as a 75 MiB outbox.
            if n == 0 {
                let mut dependent = state("dependent");
                dependent.receive(&mutation).unwrap();
                planner.stage_and_drain(edit(&mut dependent, 4)).unwrap();
                assert_eq!(planner.staged.len(), 1, "the missing cross-author predecessor stays staged");
                assert!(planner.remote.clock.is_empty());
            }
            planner.stage_and_drain(mutation).unwrap();
            assert!(planner.staged.is_empty());
            assert_eq!(planner.staged_bytes, 0, "ready superseded payloads are released during discovery");
        }
        let progress = planner.progress().unwrap();
        assert_eq!(progress.frontiers.get("writer"), Some(&300));
        assert_eq!(progress.frontiers.get("dependent"), Some(&1));
        assert!(progress.gaps.is_empty());
        assert!(planner.remote.winning("photo:one", "meta.caption").unwrap().1.as_str().unwrap().starts_with("000299:"));
    }
    #[test]
    fn gaps_and_causal_dependencies_never_advance_frontiers_and_forks_fail_atomically() {
        let mut a = state("a");
        let first = edit(&mut a, 1);
        let second = edit(&mut a, 2);
        let mut b = state("b");
        b.receive(&second).unwrap();
        let dependent = edit(&mut b, 3);
        let mut planner = Planner::new(&a).unwrap();
        planner.stage(second.clone()).unwrap();
        planner.stage(dependent).unwrap();
        let incomplete = planner.progress().unwrap();
        assert!(incomplete.frontiers.is_empty());
        assert_eq!(incomplete.gaps.len(), 2);
        planner.stage(first.clone()).unwrap();
        let p = planner.progress().unwrap();
        assert_eq!(p.frontiers.get("a"), Some(&2));
        assert_eq!(p.frontiers.get("b"), Some(&1));
        assert!(p.gaps.is_empty());
        let before = planner.remote.clone();
        let mut fork = first;
        fork.changes[0].value = json!(5);
        assert!(planner.stage(fork).is_err());
        assert_eq!(before, planner.remote);
        let mut invalid = p.clone();
        invalid.remote.clock.insert("missing".into(), 1);
        assert!(invalid.validate("library").is_err());
    }
    #[test]
    fn remote_only_checkpoint_and_exact_ack_preserve_unsent_work_and_restored_ids() {
        let mut a = state("a");
        let first = edit(&mut a, 1);
        let mut planner = Planner::new(&a).unwrap();
        planner.stage(first.clone()).unwrap();
        let p = planner.progress().unwrap();
        let second = edit(&mut a, 2);
        a.local_owner = "original".into();
        a.claim_owner("restore".into(), "new-author".into()).unwrap();
        assert!(a.pending.contains_key(&first.revision.id()) && a.pending.contains_key(&second.revision.id()));
        assert!(!p.remote.received.contains_key(&second.revision.id()));
        a.acknowledge_append(&BTreeSet::from([first.revision.id()]), p.clone()).unwrap();
        assert_eq!(a.pending.len(), 1);
        let before = a.clone();
        assert!(a.acknowledge_append(&BTreeSet::from([second.revision.id()]), p).is_err());
        assert_eq!(before, a);
        assert!(a.acknowledge(&BTreeSet::new(), "a".repeat(64), 1).is_err());
        assert!(a.claim_owner("third".into(), "a".into()).is_err());
    }
    #[test]
    fn canonical_hash_keys_and_narrow_unconditional_capabilities() {
        let mut a = state("device");
        let m = edit(&mut a, 4);
        let key = mutation_key("prefix", &m).unwrap();
        let bytes = encode(&m).unwrap();
        assert_eq!(decode_mutation("prefix", "library", &key, &bytes).unwrap(), m);
        immutable_metadata(&key, &bytes).unwrap();
        for bad in ["prefix/catalog/head.json", "prefix/sync-v2/library/ops/device/00000000000000000001-wrong.json"] {
            assert!(immutable_metadata(bad, &bytes).is_err());
        }
        assert!(decode_mutation("prefix", "other", &key, &bytes).is_err());
        let mut spaced = bytes;
        spaced.push(b' ');
        assert!(decode_mutation("prefix", "library", &key, &spaced).is_err());
    }
    #[test]
    fn incomparable_remote_checkpoints_merge_without_including_unsent_projection() {
        let mut a = state("a");
        let mut b = state("b");
        let first = edit(&mut a, 1);
        let second = edit(&mut b, 3);
        let mut pa = Planner::new(&a).unwrap();
        pa.stage(first).unwrap();
        let ca = pa.progress().unwrap();
        let mut pb = Planner::new(&b).unwrap();
        pb.stage(second).unwrap();
        let cb = pb.progress().unwrap();
        let mut local = state("third");
        local.merge(&ca.remote).unwrap();
        let unsent = edit(&mut local, 5);
        let mut p = Planner::new(&local).unwrap();
        p.checkpoint(&encode(&ca).unwrap()).unwrap();
        p.checkpoint(&encode(&cb).unwrap()).unwrap();
        let merged = p.progress().unwrap();
        assert_eq!(merged.remote.received.len(), 2);
        assert!(!merged.remote.received.contains_key(&unsent.revision.id()));
        assert_eq!(merged.frontiers.get("a"), Some(&1));
        assert_eq!(merged.frontiers.get("b"), Some(&1));
        assert_eq!(merged.remote.documents["photo:one"]["rating"].0.len(), 2, "incomparable versions preserved");
    }
}
