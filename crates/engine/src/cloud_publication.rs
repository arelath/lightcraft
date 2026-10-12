//! Direct-S3 import publication: immutable content-addressed batches and a conditional head.
//! The importer verifies all dependencies first. Local acknowledgement is a separate durable step.

use crate::assets::s3::{PutCondition, S3Connection};
use crate::assets::{AssetError, Result, keys, sha256};
use lightcraft_catalog::assets::{Asset, AssetId, BlobId, identifier};
use lightcraft_catalog::{Photo, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_BYTES: usize = 2 << 20;
const MAX_CHAIN: usize = 4096;

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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Imported {
    pub mutation_id: String,
    pub photo_id: String,
    pub asset_id: AssetId,
    pub photo: Box<Photo>,
    pub asset: Box<Asset>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Batch {
    pub version: u32,
    pub parent: Option<RecordRef>,
    pub imports: Vec<Imported>,
}

fn invalid(message: &str) -> AssetError {
    AssetError::Invalid(message.into())
}

impl Imported {
    pub fn validate(&self) -> Result<()> {
        if !identifier(&self.mutation_id)
            || !identifier(&self.photo_id)
            || !identifier(&self.asset_id.0)
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
    fn validate(&self, connection: &S3Connection) -> Result<()> {
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

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
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
    if head.version != 1 || head.generation == 0 {
        return Err(invalid("unsupported remote catalog head; preserved"));
    }
    head.tip.validate(connection)?;
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
    if batch.version != 1 || batch.imports.is_empty() || batch.imports.len() > 1000 {
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
    Ok(batch)
}

/// Return the generation already containing this exact import, including a lost head response.
fn contains(connection: &S3Connection, head: &Head, imported: &Imported, cancel: &AtomicBool) -> Result<bool> {
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
        let batch = Batch { version: 1, parent: current.as_ref().map(|(head, _)| head.tip.clone()), imports: vec![imported.clone()] };
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
        let new_head = Head { version: 1, generation, tip: reference };
        let condition = current.as_ref().map_or(PutCondition::Absent, |(_, etag)| PutCondition::Matches(etag));
        match connection.put_small(&head_key, &encode(&new_head)?, condition, cancel) {
            Ok(_) => return Ok(generation),
            Err(AssetError::Conflict | AssetError::Network(_)) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(AssetError::Network("remote catalog remained busy or unreachable; import and receipts retained for retry".into()))
}
