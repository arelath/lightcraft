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
}
#[derive(Clone, Serialize, Deserialize)]
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
    pub fn validate_scope(&self, prefix: &str, namespace: &str, max: u64) -> Result<(), String> {
        if self.hash.0.len() != 64
            || !self.hash.0.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            || self.size == 0
            || self.size > max
            || self.key != format!("{prefix}/catalog/{namespace}/{}.json", self.hash.0)
        {
            return Err("invalid or out-of-scope catalog object reference".into());
        }
        Ok(())
    }
}
impl Head {
    pub fn validate_scope(&self, prefix: &str, library: &str) -> Result<(), String> {
        if self.generation == 0
            || !matches!(self.version, 1..=3)
            || (self.version == 1 && (self.library.is_some() || self.checkpoint.is_some()))
            || (self.version >= 2 && self.library.as_deref() != Some(library))
        {
            return Err("invalid catalog head or library identity".into());
        }
        self.tip.validate_scope(prefix, "changes", 2 << 20)?;
        if let Some(checkpoint) = &self.checkpoint {
            if checkpoint.generation == 0 || checkpoint.generation >= self.generation {
                return Err("invalid checkpoint generation".into());
            }
            checkpoint.object.validate_scope(prefix, "checkpoints", 64 << 20)?;
            checkpoint.covers.validate_scope(prefix, "changes", 2 << 20)?;
        }
        if self.version < 2 {
            return Err("obsolete pre-release catalog head; use a fresh catalog and storage prefix; local data retained".into());
        }
        Ok(())
    }
}
impl Batch {
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
