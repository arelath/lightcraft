//! Portable, bounded mutation reconciliation. No paths, credentials, I/O or UI dependencies.
//! Concurrent field values survive until a causally later edit resolves them. Memberships are
//! individual boolean fields, so additions of different photos never replace one another.
use crate::{CatalogError, Result, assets::identifier};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_MUTATION_BYTES: usize = 2 << 20;
pub const MAX_STATE_BYTES: usize = 64 << 20;
pub const MAX_DEVICES: usize = 4096;
pub const MAX_RECORDS: usize = 100_000;
pub const MAX_PENDING: usize = 10_000;
pub const MAX_FIELDS: usize = 10_000;
pub const MAX_VERSIONS: usize = 128;
mod append;
pub mod archive;
pub mod capacity;
pub use append::{AppendProgress, SyncProtocol, prefixes as publication_prefixes};
pub type Clock = BTreeMap<String, u64>;

fn invalid(message: &str) -> CatalogError {
    CatalogError::Invalid(message.into())
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stamp {
    pub sequence: u64,
    pub device: String,
}
impl Stamp {
    pub fn id(&self) -> String {
        format!("{}-{}", self.device, self.sequence)
    }
    fn validate(&self) -> Result<()> {
        if self.device.starts_with("baseline_") || self.device.starts_with("legacy_") {
            return Err(invalid("obsolete pre-release import replica; use a fresh catalog and storage prefix; local data retained"));
        }
        if !identifier(&self.device) || self.device.len() > 96 || self.sequence == 0 {
            return Err(invalid("invalid synchronization revision"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub entity: String,
    pub field: String,
    pub base: Option<Stamp>,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mutation {
    pub version: u32,
    pub library: String,
    pub revision: Stamp,
    pub context: Clock,
    pub changes: Vec<Change>,
}
fn valid_clock(clock: &Clock) -> bool {
    clock.len() <= MAX_DEVICES
        && clock
            .iter()
            .all(|(id, sequence)| identifier(id) && id.len() <= 96 && !id.starts_with("baseline_") && !id.starts_with("legacy_") && *sequence > 0)
}
pub fn entity(value: &str) -> bool {
    value.split_once(':').is_some_and(|(kind, id)| matches!(kind, "photo" | "album" | "stack" | "keyword" | "library") && identifier(id))
}
impl Mutation {
    pub fn validate(&self) -> Result<()> {
        self.revision.validate()?;
        if self.context.len() > MAX_DEVICES {
            return Err(invalid(
                "mutation context exceeds 4,096 authors; retired clocks cannot be removed by checkpoints; safe author-slot retirement is unsupported; current library and outbox retained",
            ));
        }
        if !matches!(self.version, 1 | 2)
            || !identifier(&self.library)
            || !valid_clock(&self.context)
            || self.context.get(&self.revision.device).copied().unwrap_or(0) != self.revision.sequence - 1
            || self.changes.is_empty()
            || self.changes.len() > 10_000
        {
            return Err(invalid("invalid or unsupported synchronization mutation"));
        }
        let mut fields = BTreeSet::new();
        for change in &self.changes {
            if !entity(&change.entity)
                || change.field.is_empty()
                || change.field.len() > 1024
                || change.field.chars().any(char::is_control)
                || !fields.insert((&change.entity, &change.field))
            {
                return Err(invalid("invalid or duplicate synchronized field"));
            }
            if let Some(base) = &change.base {
                base.validate()?;
                if self.context.get(&base.device).copied().unwrap_or(0) < base.sequence {
                    return Err(invalid("mutation base revision is outside its causal context"));
                }
            }
        }
        if serde_json::to_vec(self).map_err(|e| invalid(&e.to_string()))?.len() > MAX_MUTATION_BYTES {
            return Err(invalid("synchronization mutation exceeds 2 MiB; local data retained"));
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).map_err(|e| invalid(&e.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub context: Clock,
    pub digest: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Register(pub BTreeMap<String, Value>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncState {
    pub version: u32,
    pub library: String,
    pub device: String,
    /// Device-local connection binding, never part of a remote mutation/checkpoint.
    pub connection: String,
    #[serde(default, skip_serializing_if = "SyncProtocol::is_head")]
    pub protocol: SyncProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<Box<AppendProgress>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub local_owner: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub previous_authors: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub migrated_from_head: bool,
    /// A join must observe its existing destination before publishing local work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_existing: bool,
    pub clock: Clock,
    pub cursor: Option<String>,
    pub generation: u64,
    pub documents: BTreeMap<String, BTreeMap<String, Register>>,
    pub received: BTreeMap<String, Receipt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archives: archive::Archives,
    pub pending: BTreeMap<String, Mutation>,
}

/// Provider-independent checkpoint payload. All device-local state and unacknowledged work
/// are excluded; a returning device merges this with its own durable pending mutations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Portable {
    pub version: u32,
    pub library: String,
    pub clock: Clock,
    pub documents: BTreeMap<String, BTreeMap<String, Register>>,
    pub received: BTreeMap<String, Receipt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archives: archive::Archives,
    /// Transport-only authenticated witnesses. Never retained in local state/checkpoints.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub proofs: archive::Witness,
}
impl Portable {
    pub fn validate(&self) -> Result<()> {
        archive::validate_witness(&self.proofs)?;
        let state = SyncState {
            version: self.version,
            library: self.library.clone(),
            device: "checkpoint".into(),
            connection: "remote".into(),
            protocol: SyncProtocol::default(),
            append: None,
            local_owner: String::new(),
            previous_authors: BTreeSet::new(),
            migrated_from_head: false,
            require_existing: false,
            clock: self.clock.clone(),
            cursor: None,
            generation: 0,
            documents: self.documents.clone(),
            received: self.received.clone(),
            archives: self.archives.clone(),
            pending: BTreeMap::new(),
        };
        state.validate()
    }
}
impl SyncState {
    /// Restore the portable part of previously verified, device-local durable progress.
    /// Remote/untrusted checkpoints must instead use `merge` with authenticated witnesses.
    pub fn restore_durable(remote: &Portable, device: String, connection: String) -> Result<Self> {
        remote.validate()?;
        let mut state = Self::new(remote.library.clone(), device, connection)?;
        state.version = remote.version;
        state.clock = remote.clock.clone();
        state.documents = remote.documents.clone();
        state.received = remote.received.clone();
        state.archives = remote.archives.clone();
        state.validate()?;
        Ok(state)
    }
    pub fn portable(&self) -> Portable {
        Portable {
            version: self.version,
            library: self.library.clone(),
            clock: self.clock.clone(),
            documents: self.documents.clone(),
            received: self.received.clone(),
            archives: self.archives.clone(),
            proofs: BTreeMap::new(),
        }
    }
    /// Merge a verified checkpoint without replacing local identities, pending work or cursor.
    pub fn merge(&mut self, remote: &Portable) -> Result<()> {
        remote.validate()?;
        if remote.library != self.library {
            return Err(invalid("checkpoint belongs to another library"));
        }
        let mut next = self.clone();
        next.version = next.version.max(remote.version);
        next.archives = archive::merge(self, remote)?;
        for (id, receipt) in &remote.received {
            if next.received.get(id).is_some_and(|old| old != receipt) {
                return Err(invalid("checkpoint mutation receipt collision; local work retained"));
            }
            next.received.insert(id.clone(), receipt.clone());
        }
        for (device, sequence) in &remote.clock {
            next.clock.entry(device.clone()).and_modify(|v| *v = (*v).max(*sequence)).or_insert(*sequence);
        }
        for (entity, fields) in &remote.documents {
            for (field, register) in fields {
                let target = next.documents.entry(entity.clone()).or_default().entry(field.clone()).or_default();
                for (id, value) in &register.0 {
                    if target.0.get(id).is_some_and(|old| old != value) {
                        return Err(invalid("checkpoint mutation content collision; local work retained"));
                    }
                    target.0.insert(id.clone(), value.clone());
                }
            }
        }
        for fields in next.documents.values_mut() {
            for register in fields.values_mut() {
                let ids: Vec<_> = register.0.keys().cloned().collect();
                register.0.retain(|id, _| {
                    let Some(stamp) = Self::stamp(id) else { return true };
                    !ids.iter().any(|other| {
                        other != id
                            && next
                                .received
                                .get(other)
                                .is_some_and(|receipt| receipt.context.get(&stamp.device).copied().unwrap_or(0) >= stamp.sequence)
                    })
                });
                if !ids.is_empty() && register.0.is_empty() {
                    return Err(invalid("checkpoint causal revisions contain a cycle"));
                }
            }
        }
        next.prune_archived_receipts();
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub fn new(library: String, device: String, connection: String) -> Result<Self> {
        let state = Self {
            version: 3,
            library,
            device,
            connection,
            protocol: SyncProtocol::default(),
            append: None,
            local_owner: String::new(),
            previous_authors: BTreeSet::new(),
            migrated_from_head: false,
            require_existing: false,
            clock: Clock::new(),
            cursor: None,
            generation: 0,
            documents: BTreeMap::new(),
            received: BTreeMap::new(),
            archives: BTreeMap::new(),
            pending: BTreeMap::new(),
        };
        state.validate()?;
        Ok(state)
    }
    pub fn validate(&self) -> Result<()> {
        if self.clock.len() > MAX_DEVICES {
            return Err(invalid(
                "synchronization author limit reached (4,096); retired clocks are retained and safe author-slot retirement is unsupported; current library and outbox retained",
            ));
        }
        if self.documents.len() > MAX_RECORDS {
            return Err(invalid(
                "synchronization document limit reached (100,000); checkpoints cannot remove document identities or tombstones; current library and outbox retained",
            ));
        }
        if self.received.len() > MAX_RECORDS {
            return Err(invalid(
                "synchronization receipt limit reached (100,000); cloud.sync with checkpoint:true archives eligible historical receipts only; finish pending synchronization and resolve conflicts; current heads and pending proofs remain retained, so a smaller separate library may be needed",
            ));
        }
        if self.pending.len() > MAX_PENDING {
            return Err(invalid(
                "synchronization pending-change limit reached (10,000); finish uploads and synchronization before editing again; current library and outbox retained",
            ));
        }
        if !matches!(self.version, 1..=3) {
            return Err(invalid("unsupported synchronization state version; update LightCraft on every device; current library and outbox retained"));
        }
        if self.clock.keys().any(|device| device.starts_with("baseline_") || device.starts_with("legacy_")) {
            return Err(invalid("obsolete pre-release import state; use a fresh catalog and storage prefix; local data retained"));
        }
        if !matches!(self.version, 1..=3)
            || (self.version == 1 && !self.archives.is_empty())
            || !identifier(&self.library)
            || !identifier(&self.device)
            || self.device.len() > 96
            || !identifier(&self.connection)
            || !valid_clock(&self.clock)
            || self.cursor.as_ref().is_some_and(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(invalid("invalid or oversized synchronization state; preserved"));
        }
        archive::validate(&self.archives, &self.clock)?;
        if (!self.local_owner.is_empty() && !identifier(&self.local_owner))
            || self.previous_authors.len() > MAX_DEVICES
            || self.previous_authors.iter().any(|id| !identifier(id) || id.len() > 96 || id == &self.device)
        {
            return Err(invalid("invalid local synchronization ownership"));
        }
        if let Some(progress) = &self.append {
            if self.protocol != SyncProtocol::AppendLog || self.cursor.is_some() || self.generation != 0 {
                return Err(invalid("append progress cannot use a head cursor"));
            }
            progress.validate(&self.library)?;
        }
        if (self.migrated_from_head && self.protocol.is_head())
            || (self.protocol == SyncProtocol::AppendLog && (self.cursor.is_some() || self.generation != 0))
        {
            return Err(invalid("append mode cannot use a mutable-head cursor"));
        }
        for (id, receipt) in &self.received {
            let stamp = Self::stamp(id).ok_or_else(|| invalid("invalid synchronization receipt identity"))?;
            stamp.validate()?;
            if !identifier(id)
                || !valid_clock(&receipt.context)
                || receipt.context.get(&stamp.device).copied().unwrap_or(0) != stamp.sequence - 1
                || self.clock.get(&stamp.device).copied().unwrap_or(0) < stamp.sequence
                || receipt.context.iter().any(|(author, sequence)| self.clock.get(author).copied().unwrap_or(0) < *sequence)
                || receipt.digest.len() != 64
                || !receipt.digest.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid("invalid synchronization receipt"));
            }
        }
        for (id, mutation) in &self.pending {
            mutation.validate()?;
            if mutation.library != self.library
                || (mutation.revision.device != self.device && !self.previous_authors.contains(&mutation.revision.device))
                || mutation.revision.id() != *id
                || self.received.get(id).is_none_or(|r| mutation.digest().ok().as_ref() != Some(&r.digest))
            {
                return Err(invalid("pending mutation identity or receipt mismatch"));
            }
        }
        for (id, fields) in &self.documents {
            if fields.len() > MAX_FIELDS {
                return Err(invalid(
                    "synchronized entity field limit reached (10,000); avoid adding fields or membership identities; removed membership fields stay retained, so use smaller separate albums; current library and outbox retained",
                ));
            }
            if !entity(id) {
                return Err(invalid("invalid synchronized document"));
            }
            for (field, register) in fields {
                if register.0.len() > MAX_VERSIONS {
                    return Err(invalid(
                        "synchronized field revision limit reached (128); resolve retained revisions or make a new causal edit to the field; current library and outbox retained",
                    ));
                }
                if field.is_empty()
                    || field.len() > 1024
                    || field.chars().any(char::is_control)
                    || register.0.keys().any(|id| !self.received.contains_key(id))
                {
                    return Err(invalid("invalid synchronized field register"));
                }
            }
        }
        capacity::serialized_bytes(self)?;
        Ok(())
    }
    fn stamp(id: &str) -> Option<Stamp> {
        let (device, sequence) = id.rsplit_once('-')?;
        Some(Stamp { device: device.into(), sequence: sequence.parse().ok()? })
    }
    pub fn winning(&self, entity: &str, field: &str) -> Option<(Stamp, &Value)> {
        let register = self.documents.get(entity)?.get(field)?;
        register
            .0
            .iter()
            .filter_map(|(id, value)| Self::stamp(id).map(|stamp| (stamp, value)))
            .max_by_key(|(stamp, value)| (field == "deleted" && value.as_bool() == Some(true), stamp.clone()))
    }
    /// Atomic even for invalid replay, bounds failures or conflicting identities.
    pub fn receive(&mut self, mutation: &Mutation) -> Result<bool> {
        let mut next = self.clone();
        let changed = next.receive_inner(mutation)?;
        if changed {
            next.prune_archived_receipts();
            next.validate()?;
            *self = next;
        }
        Ok(changed)
    }
    fn receive_inner(&mut self, mutation: &Mutation) -> Result<bool> {
        mutation.validate()?;
        if mutation.library != self.library {
            return Err(invalid("mutation belongs to a different library"));
        }
        let id = mutation.revision.id();
        let digest = mutation.digest()?;
        if let Some(previous) = self.received.get(&id) {
            if previous.digest != digest {
                return Err(invalid("replayed mutation identity describes different data"));
            }
            return Ok(false);
        }
        if archive::covers(&self.archives, &id) {
            return Err(invalid("archived replay requires exact receipt proof; local work retained"));
        }
        for change in &mutation.changes {
            let register = self.documents.entry(change.entity.clone()).or_default().entry(change.field.clone()).or_default();
            let mut dominated = false;
            register.0.retain(|current, _| {
                let Some(stamp) = Self::stamp(current) else { return true };
                if self
                    .received
                    .get(current)
                    .is_some_and(|r| r.context.get(&mutation.revision.device).copied().unwrap_or(0) >= mutation.revision.sequence)
                {
                    dominated = true;
                }
                mutation.context.get(&stamp.device).copied().unwrap_or(0) < stamp.sequence
            });
            if !dominated {
                register.0.insert(id.clone(), change.value.clone());
            }
        }
        self.received.insert(id, Receipt { context: mutation.context.clone(), digest });
        if mutation.version == 2 {
            self.version = self.version.max(3);
        }
        self.clock
            .entry(mutation.revision.device.clone())
            .and_modify(|v| *v = (*v).max(mutation.revision.sequence))
            .or_insert(mutation.revision.sequence);
        for (device, sequence) in &mutation.context {
            self.clock.entry(device.clone()).and_modify(|v| *v = (*v).max(*sequence)).or_insert(*sequence);
        }
        Ok(true)
    }
    pub fn enqueue(&mut self, changes: Vec<Change>) -> Result<Option<Mutation>> {
        if changes.is_empty() {
            return Ok(None);
        }
        let sequence = self.clock.get(&self.device).copied().unwrap_or(0).checked_add(1).ok_or_else(|| invalid("device revision exhausted"))?;
        let mutation = Mutation {
            version: 2,
            library: self.library.clone(),
            revision: Stamp { sequence, device: self.device.clone() },
            context: self.clock.clone(),
            changes,
        };
        let mut next = self.clone();
        next.receive_inner(&mutation)?;
        next.pending.insert(mutation.revision.id(), mutation.clone());
        next.prune_archived_receipts();
        next.validate()?;
        *self = next;
        Ok(Some(mutation))
    }
    /// Call only in the same journal record as incoming catalog changes. A failed durable
    /// append leaves this state and its original pending mutation identities queued for retry.
    /// Rotate a restored/copied catalog's current author without rewriting old pending IDs.
    pub fn claim_owner(&mut self, owner: String, new_author: String) -> Result<bool> {
        self.claim_epoch(owner, new_author, false)
    }
    /// Explicitly rotate even when a backup returns to the same installation/path.
    pub fn begin_writing_epoch(&mut self, owner: String, new_author: String) -> Result<()> {
        self.claim_epoch(owner, new_author, true).map(|_| ())
    }
    fn claim_epoch(&mut self, owner: String, new_author: String, force: bool) -> Result<bool> {
        if !identifier(&owner) || !identifier(&new_author) || new_author.len() > 96 {
            return Err(invalid("invalid synchronization owner identity"));
        }
        if self.local_owner == owner && !force {
            return Ok(false);
        }
        let mut next = self.clone();
        if force && new_author == self.device {
            return Err(invalid("a new writing epoch requires a distinct author"));
        }
        if new_author != self.device && (self.clock.contains_key(&new_author) || self.previous_authors.contains(&new_author)) {
            return Err(invalid("new owner author already exists in history"));
        }
        if force || !next.local_owner.is_empty() || !next.pending.is_empty() || !next.received.is_empty() {
            next.previous_authors.insert(next.device.clone());
            next.device = new_author;
        }
        next.previous_authors.remove(&next.device);
        next.local_owner = owner;
        next.validate()?;
        *self = next;
        Ok(true)
    }
    pub fn acknowledge_append(&mut self, ids: &BTreeSet<String>, progress: AppendProgress) -> Result<()> {
        if self.protocol != SyncProtocol::AppendLog {
            return Err(invalid("append acknowledgement requires explicit append mode"));
        }
        progress.validate(&self.library)?;
        let mut next = self.clone();
        for id in ids {
            let local = next.received.get(id).ok_or_else(|| invalid("acknowledgement names an unknown mutation"))?;
            if progress.remote.received.get(id).is_none_or(|remote| remote.digest != local.digest) {
                return Err(invalid("append acknowledgement lacks exact remote publication proof"));
            }
        }
        for id in ids {
            next.pending.remove(id);
        }
        next.cursor = None;
        next.generation = 0;
        let mut progress = if let Some(previous) = &next.append { previous.merge(&progress)? } else { progress };
        progress.remote.proofs.clear();
        next.append = Some(Box::new(progress));
        next.prune_archived_receipts();
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub fn acknowledge(&mut self, ids: &BTreeSet<String>, cursor: String, generation: u64) -> Result<()> {
        if self.protocol != SyncProtocol::ConditionalHead {
            return Err(invalid("head acknowledgement cannot acknowledge append-log work"));
        }
        if generation == 0
            || generation < self.generation
            || (generation == self.generation && self.cursor.as_ref().is_some_and(|old| old != &cursor))
        {
            return Err(invalid("stale or inconsistent remote catalog cursor; pending work retained"));
        }
        let mut next = self.clone();
        next.pending.retain(|id, _| !ids.contains(id));
        next.prune_archived_receipts();
        next.cursor = Some(cursor);
        next.generation = generation;
        next.validate()?;
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_semantics_are_versioned_in_mutations_checkpoints_and_compaction() {
        let mut local = SyncState::new("library".into(), "author".into(), "personal".into()).unwrap();
        assert_eq!(local.version, 3);
        let change = local
            .enqueue(vec![Change { entity: "album:smart".into(), field: "smart".into(), base: None, value: json!({"onlyRestricted":true}) }])
            .unwrap()
            .unwrap();
        assert_eq!(change.version, 2);
        let mut future = change.clone();
        future.version = 3;
        assert!(future.validate().is_err());
        let mut legacy = SyncState::new("library".into(), "peer".into(), "personal".into()).unwrap();
        legacy.version = 2;
        legacy.receive(&change).unwrap();
        assert_eq!(legacy.version, 3);
        let plan = legacy.archive_plan().unwrap();
        legacy.compact_receipts(&plan).unwrap();
        assert_eq!(legacy.portable().version, 3, "compaction must retain the checkpoint reader barrier");
        let mut future = legacy.portable();
        future.version = 4;
        assert!(future.validate().is_err());
        let mut old = change;
        old.version = 1;
        old.changes[0].value = json!({"rating":4});
        old.validate().unwrap();
        let mut old_receiver = SyncState::new("library".into(), "old-reader-data".into(), "personal".into()).unwrap();
        old_receiver.version = 2;
        old_receiver.receive(&old).unwrap();
        assert_eq!(old_receiver.version, 2, "legacy records keep their original digest and version");
    }
    use serde_json::json;
    fn state(device: &str) -> SyncState {
        SyncState::new("library".into(), device.into(), "personal".into()).unwrap()
    }
    fn set(s: &mut SyncState, entity: &str, field: &str, value: Value) -> Mutation {
        let base = s.winning(entity, field).map(|(stamp, _)| stamp);
        s.enqueue(vec![Change { entity: entity.into(), field: field.into(), base, value }]).unwrap().unwrap()
    }
    #[test]
    fn same_owner_epoch_rotation_keeps_pending_ids_and_rotates_even_without_history() {
        let mut s = state("original");
        s.local_owner = "owner".into();
        s.begin_writing_epoch("owner".into(), "fresh".into()).unwrap();
        assert_eq!(s.device, "fresh");
        let original = set(&mut s, "photo:one", "rating", json!(4));
        let pending = s.pending.clone();
        let clock = s.clock.clone();
        s.begin_writing_epoch("owner".into(), "restored".into()).unwrap();
        assert_eq!(s.pending, pending);
        assert_eq!(s.clock, clock);
        assert!(s.previous_authors.contains("fresh"));
        let newer = set(&mut s, "photo:one", "flag", json!(1));
        assert_eq!(newer.revision.sequence, 1);
        assert_eq!(newer.context.get("fresh"), Some(&original.revision.sequence));
        let before = s.clone();
        assert!(s.begin_writing_epoch("owner".into(), "restored".into()).is_err());
        assert_eq!(s, before);
    }
    #[test]
    fn independent_fields_and_album_additions_converge_and_replay_is_idempotent() {
        let mut a = state("a");
        let mut b = state("b");
        let rating = set(&mut a, "photo:one", "rating", json!(4));
        let title = set(&mut b, "photo:one", "meta.title", json!("Paris"));
        a.receive(&title).unwrap();
        b.receive(&rating).unwrap();
        let first = set(&mut a, "album:trip", "member:one", json!(true));
        let second = set(&mut b, "album:trip", "member:two", json!(true));
        a.receive(&second).unwrap();
        b.receive(&first).unwrap();
        assert_eq!(a.documents, b.documents);
        assert!(!a.receive(&rating).unwrap());
        let before = a.clone();
        let mut bad = rating;
        bad.changes[0].value = json!(2);
        assert!(a.receive(&bad).is_err());
        assert_eq!(a, before);
    }
    #[test]
    fn competing_recipes_survive_and_a_causal_resolution_retires_both() {
        let mut a = state("a");
        let mut b = state("b");
        let first = set(&mut a, "photo:one", "develop", json!({"exposure":1}));
        let second = set(&mut b, "photo:one", "develop", json!({"exposure":2}));
        a.receive(&second).unwrap();
        b.receive(&first).unwrap();
        assert_eq!(a.documents, b.documents);
        assert_eq!(a.documents["photo:one"]["develop"].0.len(), 2);
        let resolved = set(&mut a, "photo:one", "develop", json!({"exposure":3}));
        b.receive(&resolved).unwrap();
        assert_eq!(a.documents, b.documents);
        assert_eq!(a.documents["photo:one"]["develop"].0.len(), 1);
    }
    #[test]
    fn tombstone_wins_concurrent_restore_but_offline_edits_remain_recoverable() {
        let mut a = state("a");
        let mut b = state("b");
        let removed = set(&mut a, "photo:one", "deleted", json!(true));
        let restored = set(&mut b, "photo:one", "deleted", json!(false));
        let edited = set(&mut b, "photo:one", "develop", json!({"exposure":2}));
        a.receive(&restored).unwrap();
        a.receive(&edited).unwrap();
        b.receive(&removed).unwrap();
        assert_eq!(a.documents, b.documents);
        assert_eq!(a.winning("photo:one", "deleted").unwrap().1, &json!(true));
        assert!(a.winning("photo:one", "develop").is_some());
        let explicit = set(&mut a, "photo:one", "deleted", json!(false));
        b.receive(&explicit).unwrap();
        assert_eq!(b.winning("photo:one", "deleted").unwrap().1, &json!(false));
    }
    #[test]
    fn older_replay_cannot_overwrite_a_causal_descendant_or_lose_unrelated_fields() {
        let mut a = state("a");
        let first = set(&mut a, "photo:one", "rating", json!(1));
        let second = set(&mut a, "photo:one", "rating", json!(5));
        let mut b = state("b");
        b.receive(&second).unwrap();
        b.receive(&first).unwrap();
        assert_eq!(b.winning("photo:one", "rating").unwrap().1, &json!(5));
    }

    #[test]
    fn obsolete_import_replicas_fail_before_changes_and_checkpoint_merge_keeps_pending_work() {
        let mut a = state("a");
        let authored = set(&mut a, "photo:one", "rating", json!(4));
        let baseline = Mutation {
            version: 1,
            library: "library".into(),
            revision: Stamp { sequence: 1, device: "baseline_fixture".into() },
            context: Clock::new(),
            changes: vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(0) }],
        };
        let mut remote = state("b");
        remote.receive(&authored).unwrap();
        let before = a.clone();
        assert!(a.receive(&baseline).unwrap_err().to_string().contains("obsolete pre-release"));
        assert_eq!(a, before);
        assert_eq!(a.documents, remote.documents);
        assert_eq!(a.winning("photo:one", "rating").unwrap().1, &json!(4));
        let pending = a.pending.clone();
        a.merge(&remote.portable()).unwrap();
        assert_eq!(a.pending, pending);
        assert!(a.acknowledge(&BTreeSet::new(), "a".repeat(64), 0).is_err());
        assert_eq!(a.pending, pending);
    }
}
