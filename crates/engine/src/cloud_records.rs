//! Provider-independent wire records for the direct-bucket protocol. Native and browser
//! transports validate these before reconciling with the shared causal catalog model.
use lightcraft_catalog::assets::{Asset, AssetId, BlobId, identifier};
use lightcraft_catalog::{Photo, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordRef {
    pub key: String,
    pub hash: BlobId,
    pub size: u64,
}
/// Highest supported head format. Ordinary heads remain version 4; version 5 also
/// binds recovery of a retired append source to this same publication authority.
pub const HEAD_VERSION: u32 = 5;
const BASE_HEAD_VERSION: u32 = 4;
mod legacy;
pub use legacy::{LegacyIntake, MAX_INTAKE};
pub mod b2_catalog;
pub mod b2_control;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Head {
    pub version: u32,
    pub generation: u64,
    pub tip: RecordRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_append: Option<RecordRef>,
}
use crate::cloud_control::{Blocked, Control};
/// Key of a catalog object in an epoch namespace (see `cloud_control::epoch_root`).
pub fn object_key(prefix: &str, epoch: u64, namespace: &str, hash: &str) -> String {
    let object = format!("{}/{namespace}/{hash}.json", crate::cloud_control::epoch_root(epoch));
    if prefix.is_empty() { object } else { format!("{prefix}/{object}") }
}
/// The epoch whose namespace contains `key`, if `key` is exactly that epoch's object.
pub fn object_epoch(prefix: &str, namespace: &str, key: &str, hash: &str) -> Option<u64> {
    let object = if prefix.is_empty() { key } else { key.strip_prefix(prefix)?.strip_prefix('/')? };
    let epoch = match object.strip_prefix("catalog/epochs/") {
        Some(rest) => rest.split('/').next()?.parse::<u64>().ok().filter(|epoch| *epoch >= 2)?,
        None => 1,
    };
    (key == object_key(prefix, epoch, namespace, hash)).then_some(epoch)
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointRef {
    pub object: RecordRef,
    pub covers: RecordRef,
    pub generation: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Imported {
    pub mutation_id: String,
    pub photo_id: String,
    pub asset_id: AssetId,
    pub photo: Box<Photo>,
    pub asset: Box<Asset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_asset: Option<AssetId>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Batch {
    pub version: u32,
    pub parent: Option<RecordRef>,
    pub imports: Vec<Imported>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutations: Vec<lightcraft_catalog::replication::Mutation>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checkpoint_marker: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Checkpoint {
    pub version: u32,
    pub generation: u64,
    pub covers: RecordRef,
    pub state: lightcraft_catalog::replication::Portable,
    pub imports: std::collections::BTreeMap<String, String>,
}
impl RecordRef {
    /// Validates the reference and returns the epoch namespace that contains it.
    pub fn validate_scope(&self, prefix: &str, namespace: &str, max: u64) -> Result<u64, String> {
        if self.hash.0.len() != 64
            || !self.hash.0.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            || self.size == 0
            || self.size > max
        {
            return Err("invalid or out-of-scope catalog object reference".into());
        }
        object_epoch(prefix, namespace, &self.key, &self.hash.0).ok_or_else(|| "invalid or out-of-scope catalog object reference".into())
    }
}
impl Head {
    /// Decode a head, checking a newer control envelope before the rest of the record
    /// so a future format yields an "update LightCraft" error rather than "malformed".
    pub fn decode(bytes: &[u8]) -> Result<Self, Blocked> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| Blocked::Invalid("remote catalog head is malformed; preserved, local work retained".into()))?;
        if let Some(control) = value.get("control") {
            Control::decode(control)?;
        }
        if value.get("version").and_then(serde_json::Value::as_u64).is_some_and(|version| version > u64::from(HEAD_VERSION)) {
            return Err(Blocked::UpdateToRead("cloud catalog head is from a newer LightCraft".into()));
        }
        serde_json::from_value(value)
            .map_err(|_| Blocked::Invalid("remote catalog head is malformed or from an unsupported version; preserved".into()))
    }
    /// The authoritative control: embedded since version 4, implicit for older heads.
    pub fn control(&self) -> Result<Control, String> {
        let library = self.library.as_deref().ok_or("catalog head has no library identity")?;
        match &self.control {
            Some(control) => Ok(control.clone()),
            None => Ok(Control::initial(library)),
        }
    }
    /// Compatibility adapter for callers whose public contract is a textual audit error.
    pub fn validate_scope(&self, prefix: &str, library: &str) -> Result<(), String> {
        self.validate_scope_typed(prefix, library).map_err(|blocked| blocked.to_string())
    }
    /// Keep compatibility/control provenance through publication and receive paths.
    pub fn validate_scope_typed(&self, prefix: &str, library: &str) -> Result<(), Blocked> {
        if self.generation == 0
            || !matches!(self.version, 1..=HEAD_VERSION)
            || (self.version == 1 && (self.library.is_some() || self.checkpoint.is_some()))
            || (self.version >= 2 && self.library.as_deref() != Some(library))
            || ((self.version >= BASE_HEAD_VERSION) != self.control.is_some())
            || (self.version < 5 && self.legacy_append.is_some())
        {
            return Err(Blocked::Invalid("invalid catalog head or library identity".into()));
        }
        if self.version < 2 {
            return Err(Blocked::Invalid("obsolete pre-release catalog head; use a fresh catalog and storage prefix; local data retained".into()));
        }
        let control = self.control().map_err(Blocked::Invalid)?;
        control.may_read()?;
        if control.library != library || control.active_metadata_root != crate::cloud_control::epoch_root(control.epoch) {
            return Err(Blocked::Invalid("catalog head control names another library or namespace".into()));
        }
        if self.tip.validate_scope(prefix, "changes", 2 << 20).map_err(Blocked::Invalid)? != control.epoch {
            return Err(Blocked::Invalid("catalog head tip is outside its active epoch".into()));
        }
        if let Some(checkpoint) = &self.checkpoint {
            if checkpoint.generation == 0 || checkpoint.generation >= self.generation {
                return Err(Blocked::Invalid("invalid checkpoint generation".into()));
            }
            if checkpoint.object.validate_scope(prefix, "checkpoints", 64 << 20).map_err(Blocked::Invalid)? != control.epoch
                || checkpoint.covers.validate_scope(prefix, "changes", 2 << 20).map_err(Blocked::Invalid)? != control.epoch
            {
                return Err(Blocked::Invalid("catalog checkpoint is outside its active epoch".into()));
            }
        }
        if let Some(migration) = &control.migration
            && migration.source.generation >= self.generation
        {
            return Err(Blocked::Invalid("migration source is not before the maintenance head".into()));
        }
        if let Some(legacy) = &self.legacy_append
            && legacy.validate_scope(prefix, "legacy", MAX_INTAKE).map_err(Blocked::Invalid)? != 1
        {
            return Err(Blocked::Invalid("legacy intake is outside its original namespace".into()));
        }
        Ok(())
    }
    /// The permanent deletion barrier has no personal history or live references.
    /// Ordinary readers still refuse it with `Deleted`; erasure can verify it exactly.
    pub fn validate_deleted(&self, prefix: &str, library: &str) -> Result<(), Blocked> {
        let control = self.control.as_ref().ok_or_else(|| Blocked::Invalid("deletion barrier has no control".into()))?;
        control.validate()?;
        let zero = "0".repeat(64);
        if !matches!(self.version, BASE_HEAD_VERSION..=HEAD_VERSION)
            || self.library.as_deref() != Some(library)
            || control.library != library
            || control.state != crate::cloud_control::State::Deleted
            || control.active_metadata_root != crate::cloud_control::epoch_root(control.epoch)
            || self.generation != 1
            || self.checkpoint.is_some()
            || self.legacy_append.is_some()
            || self.tip.hash.0 != zero
            || self.tip.size != 1
            || self.tip.key != object_key(prefix, control.epoch, "changes", &zero)
        {
            return Err(Blocked::Invalid("invalid permanent deletion barrier".into()));
        }
        Ok(())
    }
    /// The head that replaces `previous`: always the current format, carrying the
    /// observed control forward (or a transition's new control) unchanged.
    pub fn next(previous: Option<&Head>, tip: RecordRef, library: &str, checkpoint: Option<CheckpointRef>, control: Control) -> Result<Self, String> {
        let generation = previous.map_or(Some(1), |head| head.generation.checked_add(1)).ok_or("catalog generation exhausted")?;
        let legacy_append = previous.and_then(|head| head.legacy_append.clone());
        let version = if legacy_append.is_some() { HEAD_VERSION } else { BASE_HEAD_VERSION };
        Ok(Self { version, generation, tip, library: Some(library.into()), checkpoint, control: Some(control), legacy_append })
    }
}
/// Ordinary publication check, made on each reread immediately before its CAS.
pub fn publishable(head: Option<&Head>, library: &str) -> Result<Control, crate::cloud_control::Blocked> {
    let control = match head {
        Some(head) => head.control().map_err(crate::cloud_control::Blocked::Invalid)?,
        None => Control::initial(library),
    };
    control.may_publish()?;
    Ok(control)
}
impl Batch {
    /// A history chain never crosses an epoch: a batch and its parent share a namespace.
    pub fn validate_at(&self, prefix: &str, library: &str, reference: &RecordRef) -> Result<(), String> {
        self.validate_scope(prefix, library)?;
        let epoch = reference.validate_scope(prefix, "changes", 2 << 20)?;
        if let Some(parent) = &self.parent
            && parent.validate_scope(prefix, "changes", 2 << 20)? != epoch
        {
            return Err("change batch parent is outside its epoch".into());
        }
        Ok(())
    }
    pub fn validate_scope(&self, prefix: &str, library: &str) -> Result<(), String> {
        if !matches!(self.version, 1..=3)
            || self.imports.len() > 1000
            || self.mutations.len() > 1000
            || (self.version < 3 && (!self.mutations.is_empty() || self.checkpoint_marker))
            || (self.imports.is_empty() && self.mutations.is_empty() && !(self.version == 3 && self.checkpoint_marker))
        {
            return Err("invalid change batch version or count".into());
        }
        if let Some(parent) = &self.parent {
            parent.validate_scope(prefix, "changes", 2 << 20)?;
        }
        let mut ids = BTreeSet::new();
        for imported in &self.imports {
            if !identifier(&imported.mutation_id)
                || !ids.insert(imported.mutation_id.clone())
                || !identifier(&imported.photo_id)
                || !identifier(&imported.asset_id.0)
                || imported.photo.source != (Source::Managed { asset_id: imported.asset_id.clone() })
                || imported.photo.file_name.is_empty()
                || imported.photo.file_name.len() > 1024
                || imported.photo.file_name.contains(['/', '\\'])
                || imported.photo.file_name.chars().any(char::is_control)
                || imported.photo.deleted
                || !crate::input_limits::shared_dimensions(imported.photo.width, imported.photo.height)
                || imported.asset.smart_preview.is_none()
                || imported.asset.thumbnail.is_none()
                || imported.base_asset.as_ref().is_some_and(|base| !identifier(&base.0) || base == &imported.asset_id)
            {
                return Err("invalid imported photo record".into());
            }
            imported.asset.validate()?;
        }
        for mutation in &self.mutations {
            mutation.validate().map_err(|e| e.to_string())?;
            if mutation.library != library || !ids.insert(mutation.revision.id()) {
                return Err("change batch library or mutation collision".into());
            }
        }
        if self.version != 3 || !self.imports.is_empty() {
            return Err("obsolete pre-release import batch; use a fresh catalog and storage prefix; local data retained".into());
        }
        Ok(())
    }
}

/// Shared bounded traversal primitives; they do not alter wire records.
pub mod replay;

#[cfg(test)]
mod typed_failure_tests {
    use super::*;
    #[test]
    fn constructed_head_scope_keeps_reader_compatibility_provenance() {
        let mut control = Control::initial("library");
        control.minimum_reader = crate::cloud_control::READER + 1;
        control.minimum_writer = control.minimum_reader;
        control.schema = control.minimum_reader;
        control.validate().unwrap();
        let head = Head {
            version: HEAD_VERSION,
            generation: 1,
            tip: RecordRef { key: "unused".into(), hash: BlobId("a".repeat(64)), size: 1 },
            library: Some("library".into()),
            checkpoint: None,
            control: Some(control),
            legacy_append: None,
        };
        assert!(matches!(head.validate_scope_typed("prefix", "library"), Err(Blocked::UpdateToRead(_))));
    }
}
