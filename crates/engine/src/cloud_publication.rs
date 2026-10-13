//! Direct-S3 import publication: immutable content-addressed batches and a conditional head.
//! The importer verifies all dependencies first. Local acknowledgement is a separate durable step.

use crate::assets::s3::{PutCondition, S3Connection};
use crate::assets::{AssetError, Result, keys, sha256};
use lightcraft_catalog::Source;
use lightcraft_catalog::assets::identifier;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_BYTES: usize = 2 << 20;
const MAX_CHAIN: usize = 4096;

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
            || self.photo.width == 0
            || self.photo.height == 0
            || self.photo.width > 65535
            || self.photo.height > 65535
            || u64::from(self.photo.width).saturating_mul(u64::from(self.photo.height)) > 100_000_000
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
    pub(crate) fn validate(&self, connection: &S3Connection) -> Result<()> {
        if self.size == 0
            || self.size > MAX_BYTES as u64
            || self.hash.0.len() != 64
            || !self.hash.0.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            || self.key != keys::in_library(&connection.prefix, &format!("catalog/changes/{}.json", self.hash.0))?
        {
            return Err(invalid("invalid remote catalog batch reference"));
        }
        Ok(())
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
    let head: Head =
        serde_json::from_slice(&reply.bytes).map_err(|_| invalid("remote catalog head is malformed or from an unsupported version; preserved"))?;
    if !matches!(head.version, 1 | 2)
        || head.generation == 0
        || (head.version == 1 && (head.library.is_some() || head.checkpoint.is_some()))
        || (head.version == 2 && head.library.as_ref().is_none_or(|id| !identifier(id)))
    {
        return Err(invalid("unsupported remote catalog head; preserved"));
    }
    head.tip.validate(connection)?;
    if let Some(checkpoint) = &head.checkpoint {
        checkpoint.covers.validate(connection)?;
        if checkpoint.generation == 0
            || checkpoint.generation >= head.generation
            || checkpoint.object.size == 0
            || checkpoint.object.size > 64 << 20
            || checkpoint.object.hash.0.len() != 64
            || !checkpoint.object.hash.0.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            || checkpoint.object.key != keys::in_library(&connection.prefix, &format!("catalog/checkpoints/{}.json", checkpoint.object.hash.0))?
        {
            return Err(invalid("invalid remote checkpoint reference"));
        }
    }
    let etag = reply.etag.ok_or_else(|| invalid("remote head has no ETag; conditional synchronization is unavailable"))?;
    Ok(Some((head, etag)))
}

pub fn read_batch(connection: &S3Connection, reference: &RecordRef, cancel: &AtomicBool) -> Result<Batch> {
    reference.validate(connection)?;
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
    if let Some(parent) = &batch.parent {
        parent.validate(connection)?;
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
    Ok(batch)
}

/// Return the generation already containing this exact import, including a lost head response.
fn contains(connection: &S3Connection, head: &Head, imported: &Imported, cancel: &AtomicBool) -> Result<bool> {
    if let Some(reference) = &head.checkpoint {
        let checkpoint = crate::cloud_replication::read_checkpoint(connection, reference, cancel)?;
        if let Some(digest) = checkpoint.imports.get(&imported.mutation_id) {
            if digest != &sha256(&encode(imported)?).0 {
                return Err(invalid("checkpoint import identity describes different data"));
            }
            return Ok(true);
        }
    }
    let mut next = Some(head.tip.clone());
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_CHAIN {
        let Some(reference) = next.take() else { return Ok(false) };
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        if !seen.insert(reference.hash.clone()) {
            return Err(invalid("remote catalog chain contains a cycle"));
        }
        let batch = read_batch(connection, &reference, cancel)?;
        for existing in batch.imports {
            if existing.mutation_id == imported.mutation_id {
                if &existing != imported {
                    return Err(invalid("remote mutation identity describes different data; local import preserved"));
                }
                return Ok(true);
            }
        }
        next = batch.parent;
    }
    Err(invalid("remote catalog history exceeds the current limit; a checkpoint is required"))
}

/// All writes are conditional. Conflicting head writes rebase the immutable batch on
/// the current chain; a lost response is reconciled by mutation identity and exact content.
pub fn publish_import(connection: &S3Connection, imported: &Imported, cancel: &AtomicBool) -> Result<u64> {
    imported.validate()?;
    let head_key = keys::in_library(&connection.prefix, "catalog/head.json")?;
    for _ in 0..16 {
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        let current = read_head(connection, cancel)?;
        if let Some((head, _)) = &current
            && contains(connection, head, imported, cancel)?
        {
            return Ok(head.generation);
        }
        let version = if imported.base_asset.is_some() || !imported.asset.associated.is_empty() { 2 } else { 1 };
        let batch = Batch {
            version,
            parent: current.as_ref().map(|(head, _)| head.tip.clone()),
            imports: vec![imported.clone()],
            mutations: Vec::new(),
            checkpoint_marker: false,
        };
        let bytes = encode(&batch)?;
        let hash = sha256(&bytes);
        let reference =
            RecordRef { key: keys::in_library(&connection.prefix, &format!("catalog/changes/{}.json", hash.0))?, hash, size: bytes.len() as u64 };
        match connection.put_small(&reference.key, &bytes, PutCondition::Absent, cancel) {
            Ok(_) => {}
            Err(error @ (AssetError::Conflict | AssetError::Network(_))) => {
                // The write might have succeeded. An ETag alone does not prove its bytes.
                match connection.get_small(&reference.key, MAX_BYTES, cancel) {
                    Ok(Some(reply)) if reply.bytes == bytes => {}
                    _ => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
        let generation = current
            .as_ref()
            .map_or(Ok(1), |(head, _)| head.generation.checked_add(1).ok_or_else(|| invalid("remote catalog generation exhausted")))?;
        let new_head = Head {
            version: current.as_ref().map_or(1, |(head, _)| head.version),
            generation,
            tip: reference,
            library: current.as_ref().and_then(|(head, _)| head.library.clone()),
            checkpoint: current.as_ref().and_then(|(head, _)| head.checkpoint.clone()),
        };
        let condition = current.as_ref().map_or(PutCondition::Absent, |(_, etag)| PutCondition::Matches(etag));
        match connection.put_small(&head_key, &encode(&new_head)?, condition, cancel) {
            Ok(_) => return Ok(generation),
            Err(AssetError::Conflict | AssetError::Network(_)) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(AssetError::Network("remote catalog remained busy or unreachable; import and receipts retained for retry".into()))
}
