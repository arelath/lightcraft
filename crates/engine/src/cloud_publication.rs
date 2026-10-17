//! Direct-S3 causal publication records: immutable batches and a conditional head.
//! The importer verifies all dependencies first. Local acknowledgement is a separate durable step.

use crate::assets::s3::S3Connection;
use crate::assets::{AssetError, Result, keys, sha256};
use lightcraft_catalog::Source;
use lightcraft_catalog::assets::identifier;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

const MAX_BYTES: usize = 2 << 20;

pub use crate::cloud_records::{Batch, CheckpointRef, Head, Imported, RecordRef};

fn invalid(message: &str) -> AssetError {
    AssetError::Invalid(message.into())
}

impl Imported {
    pub fn validate(&self) -> Result<()> {
        if !identifier(&self.mutation_id)
            || !identifier(&self.photo_id)
            || !identifier(&self.asset_id.0)
            || self.base_asset.as_ref().is_some_and(|base| !identifier(&base.0) || base == &self.asset_id)
            || self.photo.source != (Source::Managed { asset_id: self.asset_id.clone() })
            || self.photo.file_name.is_empty()
            || self.photo.file_name.len() > 1024
            || self.photo.file_name.contains(['/', '\\'])
            || self.photo.file_name.chars().any(char::is_control)
            || !crate::input_limits::shared_dimensions(self.photo.width, self.photo.height)
            || self.photo.deleted
            || self.asset.smart_preview.is_none()
            || self.asset.thumbnail.is_none()
        {
            return Err(invalid("invalid remote import record"));
        }
        self.asset.validate().map_err(AssetError::Invalid)
    }
}

impl RecordRef {
    /// Validates a change-batch reference and returns its epoch namespace.
    pub(crate) fn validate(&self, connection: &S3Connection) -> Result<u64> {
        keys::in_library("", &self.key)?;
        self.validate_scope(&connection.prefix, "changes", MAX_BYTES as u64).map_err(|_| invalid("invalid remote catalog batch reference"))
    }
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("could not encode remote catalog metadata"))?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("remote catalog record exceeds 2 MiB; reduce metadata before publishing"));
    }
    Ok(bytes)
}

pub fn read_head(connection: &S3Connection, cancel: &AtomicBool) -> Result<Option<(Head, String)>> {
    let key = keys::in_library(&connection.prefix, "catalog/head.json")?;
    let Some(reply) = connection.get_small(&key, 16 << 10, cancel)? else { return Ok(None) };
    let head = Head::decode(&reply.bytes).map_err(AssetError::Blocked)?;
    if head.version < 2 {
        return Err(invalid("obsolete pre-release catalog head; use a fresh catalog and storage prefix; local data retained"));
    }
    let library = head.library.clone().filter(|id| identifier(id)).ok_or_else(|| invalid("unsupported remote catalog head; preserved"))?;
    keys::in_library("", &head.tip.key)?;
    head.validate_scope(&connection.prefix, &library).map_err(AssetError::Invalid)?;
    let etag = reply.etag.ok_or_else(|| invalid("remote head has no ETag; conditional synchronization is unavailable"))?;
    Ok(Some((head, etag)))
}

pub fn read_batch(connection: &S3Connection, reference: &RecordRef, cancel: &AtomicBool) -> Result<Batch> {
    let epoch = reference.validate(connection)?;
    let reply = connection.get_small(&reference.key, MAX_BYTES, cancel)?.ok_or(AssetError::Missing)?;
    if reply.bytes.len() as u64 != reference.size || sha256(&reply.bytes) != reference.hash {
        return Err(AssetError::Integrity);
    }
    let batch: Batch = serde_json::from_slice(&reply.bytes).map_err(|_| invalid("remote catalog batch is malformed; preserved"))?;
    if !matches!(batch.version, 1..=3)
        || (batch.imports.is_empty() && batch.mutations.is_empty() && !batch.checkpoint_marker)
        || batch.imports.len() > 1000
        || batch.mutations.len() > 1000
        || (batch.version < 3 && !batch.mutations.is_empty())
        || (batch.version < 3 && batch.checkpoint_marker)
        || (batch.version == 1 && batch.imports.iter().any(|imported| imported.base_asset.is_some() || !imported.asset.associated.is_empty()))
    {
        return Err(invalid("unsupported remote catalog batch"));
    }
    if let Some(parent) = &batch.parent
        && parent.validate(connection)? != epoch
    {
        return Err(invalid("remote batch parent is outside its epoch"));
    }
    let mut ids = BTreeSet::new();
    for imported in &batch.imports {
        imported.validate()?;
        if !ids.insert(&imported.mutation_id) {
            return Err(invalid("remote batch repeats a mutation identity"));
        }
    }
    let mut mutation_ids = BTreeSet::new();
    for mutation in &batch.mutations {
        mutation.validate().map_err(|error| invalid(&error.to_string()))?;
        if !mutation_ids.insert(mutation.revision.id()) || batch.mutations.first().is_some_and(|first| first.library != mutation.library) {
            return Err(invalid("remote batch repeats a mutation or mixes libraries"));
        }
    }
    if batch.version != 3 || !batch.imports.is_empty() {
        return Err(invalid("obsolete pre-release import batch; use a fresh catalog and storage prefix; local data retained"));
    }
    Ok(batch)
}
