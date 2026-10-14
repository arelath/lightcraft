//! Durable upload receipts live beside staged originals. Workers persist multipart
//! progress themselves; owner-thread phase changes are acknowledged only after journal save.

use crate::activity::{TaskGuard, TaskHandle};
use crate::assets::s3::S3Connection;
use crate::assets::{AssetError, AssetResolver, BlobStore, TransferManager, sha256};
use crate::cloud_publication::Imported;
use crate::{EngineError, Result, Session};
use lightcraft_catalog::assets::{Asset, Blob, BlobId, identifier};
use lightcraft_catalog::uploads::{UploadIntent, UploadState};
use lightcraft_catalog::{Op, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const PART_BYTES: usize = 8 << 20;
const MAX_RECEIPT: u64 = 4 << 20;
mod cleanup;
pub use cleanup::{observed as observed_cleanup, start as cleanup};
fn error(value: impl Into<String>) -> EngineError {
    EngineError::Other(value.into())
}
fn io(value: std::io::Error) -> EngineError {
    error(value.to_string())
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Part {
    etag: String,
    hash: BlobId,
    size: u64,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Multipart {
    id: String,
    parts: BTreeMap<u16, Part>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    version: u32,
    #[serde(default, skip_serializing_if = "lightcraft_catalog::replication::SyncProtocol::is_head")]
    protocol: lightcraft_catalog::replication::SyncProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    library: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    append_proof: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    publication_intent: BTreeMap<String, String>,
    id: String,
    imported: Imported,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<S3Connection>,
    state: UploadState,
    verified: BTreeSet<String>,
    multipart: BTreeMap<String, Multipart>,
    generation: Option<u64>,
    error: Option<String>,
}

impl Receipt {
    fn validate(&self) -> Result<()> {
        if !matches!(self.version, 1..=4)
            || (self.version == 1 && self.storage.is_some())
            || (self.version == 2 && self.storage.is_none())
            || (self.version == 3
                && (self.storage.is_none()
                    || self.protocol != lightcraft_catalog::replication::SyncProtocol::AppendLog
                    || self.library.as_ref().is_none_or(|s| !identifier(s))))
            || (self.version == 4 && (self.storage.is_none() || !self.protocol.is_head() || self.library.as_ref().is_none_or(|s| !identifier(s))))
            || (self.version < 3
                && (!self.protocol.is_head() || self.library.is_some() || !self.append_proof.is_empty() || !self.publication_intent.is_empty()))
            || self.append_proof.len() > 100_000
            || self.append_proof.iter().any(|(id, hash)| !identifier(id) || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
            || self.publication_intent.len() > 100_000
            || self.publication_intent.iter().any(|(id, hash)| !identifier(id) || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
            || !identifier(&self.id)
            || self.id != self.imported.mutation_id
            || (self.imported.base_asset.is_none() && self.id != self.imported.photo_id)
            || self.multipart.len() > 19
            || self.error.as_ref().is_some_and(|message| message.len() > 4096 || message.contains('\0'))
        {
            return Err(error("invalid upload receipt; staged data preserved"));
        }
        self.imported.validate().map_err(|e| error(e.to_string()))?;
        if let Some(storage) = &self.storage {
            if storage.profile.is_some() {
                return Err(error("upload destination binding contains device credential selection; preserved"));
            }
            for blob in blobs(&self.imported.asset) {
                storage.validate_blob(blob).map_err(|e| error(e.to_string()))?;
            }
        }
        let intent = UploadIntent {
            asset_id: self.imported.asset_id.clone(),
            photo_id: self.imported.photo.id,
            state: self.state,
            verified: self.verified.clone(),
            error: self.error.clone(),
        };
        intent.validate(&self.imported.asset).map_err(error)?;
        for (key, upload) in &self.multipart {
            let blob = blobs(&self.imported.asset)
                .find(|blob| &blob.remote.key == key)
                .ok_or_else(|| error("multipart receipt refers to another object"))?;
            if upload.id.is_empty() || upload.id.len() > 1024 || upload.id.chars().any(char::is_control) || upload.parts.len() > 512 {
                return Err(error("invalid multipart receipt; preserved"));
            }
            for (number, part) in &upload.parts {
                let offset = u64::from(*number)
                    .checked_sub(1)
                    .and_then(|number| number.checked_mul(PART_BYTES as u64))
                    .ok_or_else(|| error("invalid part offset"))?;
                if offset >= blob.size
                    || part.size != blob.size.saturating_sub(offset).min(PART_BYTES as u64)
                    || part.hash.0.len() != 64
                    || !part.hash.0.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                    || part.etag.is_empty()
                    || part.etag == "*"
                    || part.etag.len() > 512
                    || part.etag.chars().any(char::is_control)
                {
                    return Err(error("invalid multipart part receipt; preserved"));
                }
            }
        }
        Ok(())
    }
    fn write(&self, root: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|e| error(e.to_string()))?;
        if bytes.len() as u64 > MAX_RECEIPT {
            return Err(error("upload receipt exceeds 4 MiB"));
        }
        std::fs::create_dir_all(root.join("uploads")).map_err(io)?;
        lightcraft_catalog::safe_file::write_atomic(&root.join("uploads").join(format!("{}.json", self.id)), &bytes).map_err(io)
    }
}

fn blobs(asset: &Asset) -> impl Iterator<Item = &Blob> {
    asset.blobs()
}

fn read_receipt(path: &Path) -> Result<Option<Receipt>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    if file.metadata().map_err(io)?.len() > MAX_RECEIPT {
        return Err(error("oversized upload receipt; preserved"));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(MAX_RECEIPT as usize + 1).map_err(|_| error("not enough memory to recover upload receipt"))?;
    file.take(MAX_RECEIPT + 1).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err(error("upload receipt grew while reading; preserved"));
    }
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| error("invalid or future upload receipt; preserved for recovery"))?;
    receipt.validate()?;
    Ok(Some(receipt))
}

pub(crate) fn staging_proofs(root: &Path, policy: &crate::assets::managed::Policy) -> Result<crate::assets::managed::Proofs> {
    let mut result = crate::assets::managed::Proofs { verified: BTreeMap::new(), protected: BTreeSet::new() };
    let dir = root.join("uploads");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(error("upload receipt directory is redirected or invalid; staged data preserved")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(e) => return Err(io(e)),
    }
    for (index, entry) in std::fs::read_dir(&dir).map_err(io)?.enumerate() {
        if index >= 100_000 {
            return Err(error("too many upload receipts to maintain safely; staged data preserved"));
        }
        let entry = entry.map_err(io)?;
        if entry.path().extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        if !entry.file_type().map_err(io)?.is_file() {
            return Err(error("upload receipt is not a regular file; staged data preserved"));
        }
        let receipt = read_receipt(&entry.path())?.ok_or_else(|| error("receipt changed while reading; staged data preserved"))?;
        if entry.file_name() != std::ffi::OsStr::new(&format!("{}.json", receipt.id)) {
            return Err(error("upload receipt identity differs from filename; preserved"));
        }
        let approved = policy.imports.get(&receipt.id).is_some_and(|intent| {
            intent.acknowledged && intent.asset_id == receipt.imported.asset_id && intent.asset.as_ref() == receipt.imported.asset.as_ref()
        }) && (receipt.generation.is_some() || (receipt.version == 3 && !receipt.append_proof.is_empty()))
            && receipt.multipart.is_empty()
            && blobs(&receipt.imported.asset).all(|blob| receipt.verified.contains(&blob.remote.key))
            && receipt
                .storage
                .as_ref()
                .is_some_and(|storage| policy.connections.get(&storage.id).is_some_and(|connection| storage.same_destination(connection)));
        if approved {
            result.verified.insert(receipt.id, (receipt.imported.asset_id, *receipt.imported.asset));
        } else {
            result.protected.extend(blobs(&receipt.imported.asset).map(|blob| blob.id.clone()));
        }
    }
    Ok(result)
}

enum Event {
    Publication {
        outcome: Box<crate::cloud_replication::Outcome>,
        started_generation: u64,
        reply: mpsc::SyncSender<Result<()>>,
    },
    Phase {
        attempt: u64,
        remote_invalid: bool,
        id: String,
        state: UploadState,
        verified: BTreeSet<String>,
        error: Option<String>,
        reply: mpsc::SyncSender<Result<()>>,
    },
    Finished(Result<Value>),
}

struct Item {
    coordinator: Arc<crate::cloud_replication::Coordinator>,
    attempt: u64,
    /// Only verification explicitly started after acknowledgement may invalidate it.
    reverification: bool,
    sync: Option<lightcraft_catalog::replication::SyncState>,
    id: String,
    imported: Imported,
    connection: S3Connection,
    root: PathBuf,
    resolver: Arc<AssetResolver>,
}

struct UploadProgress {
    handle: TaskHandle,
    bytes: Arc<AtomicU64>,
}
impl UploadProgress {
    fn detail(&self, text: &str) {
        self.handle.detail(text);
    }
    fn progress(&self, done: u64, total: u64) {
        self.handle.progress(done, total);
    }
    fn observe(&self, previous: &mut u64, done: u64, offset: u64, total: u64) {
        let delta = done.saturating_sub(*previous);
        let _ = self.bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| Some(value.saturating_add(delta)));
        *previous = done;
        self.progress(offset.saturating_add(done), total);
    }
}

impl Item {
    fn write_receipt(&self, receipt: &Receipt, cancel: &AtomicBool) -> Result<()> {
        let _guard = self.resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
        receipt.write(&self.root)
    }

    fn phase(&self, receipt: &Receipt, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) -> Result<()> {
        self.send_phase(receipt, events, cancel, false)
    }

    fn send_phase(&self, receipt: &Receipt, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool, remote_invalid: bool) -> Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        events
            .send(Event::Phase {
                attempt: self.attempt,
                remote_invalid: remote_invalid && self.reverification,
                id: self.id.clone(),
                state: receipt.state,
                verified: receipt.verified.clone(),
                error: receipt.error.clone(),
                reply,
            })
            .map_err(|_| error("upload owner closed; receipts retained"))?;
        loop {
            match response.recv_timeout(Duration::from_millis(20)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(error("upload owner closed; receipts retained")),
                Err(mpsc::RecvTimeoutError::Timeout) if cancel.load(Ordering::Relaxed) => {
                    return Err(error("upload cancelled; staged data and resumable receipts retained"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn remote_verified(&self, blob: &Blob, cancel: &AtomicBool, task: &UploadProgress) -> crate::assets::Result<bool> {
        struct Check {
            hash: Sha256,
            count: u64,
            expected: u64,
        }
        impl Write for Check {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.count = self.count.saturating_add(bytes.len() as u64);
                if self.count > self.expected {
                    return Err(std::io::Error::other("remote object exceeds its declared size"));
                }
                self.hash.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        task.detail(&format!("{} · verifying {}", self.imported.photo.file_name, self.rendition(blob)));
        let mut check = Check { hash: Sha256::new(), count: 0, expected: blob.size };
        let mut observed = 0;
        let result = self.resolver.background_transfer(blob, cancel, || {
            self.connection.download(blob, &mut check, cancel, &mut |done| task.observe(&mut observed, done, 0, blob.size))
        });
        match result {
            Err(_) if check.count > blob.size => Err(AssetError::Integrity),
            Err(AssetError::Missing) => Ok(false),
            Err(error) => Err(error),
            Ok(()) if check.count == blob.size && format!("{:x}", check.hash.finalize()) == blob.id.0 => Ok(true),
            Ok(()) => Err(AssetError::Integrity),
        }
    }

    fn rendition(&self, blob: &Blob) -> &'static str {
        if blob.remote.key == self.imported.asset.original.remote.key {
            "original"
        } else if self.imported.asset.smart_preview.as_ref().is_some_and(|preview| preview.remote.key == blob.remote.key) {
            "editable preview"
        } else if self.imported.asset.associated.values().any(|document| document.remote.key == blob.remote.key) {
            "XMP document"
        } else {
            "thumbnail"
        }
    }

    fn upload_blob(&self, receipt: &mut Receipt, blob: &Blob, cancel: &AtomicBool, task: &UploadProgress) -> Result<()> {
        if !self.root.join("blobs").join(&blob.id.0).is_file() {
            return Err(error("durable staged bytes are missing; recover the original before uploading"));
        }
        let lease = self.resolver.acquire(blob, cancel).map_err(|e| error(e.to_string()))?;
        if blob.size <= PART_BYTES as u64 {
            let mut file = std::fs::File::open(lease.path()).map_err(io)?;
            task.detail(&format!("{} · uploading {}", self.imported.photo.file_name, self.rendition(blob)));
            let mut observed = 0;
            let result = self.resolver.background_transfer(blob, cancel, || {
                if self.sync.as_ref().is_some_and(|s| !s.protocol.is_head()) {
                    self.connection.put_blob_immutable(blob, &mut file, cancel, &mut |done| task.observe(&mut observed, done, 0, blob.size))
                } else {
                    self.connection.put_blob(blob, &mut file, cancel, &mut |done| task.observe(&mut observed, done, 0, blob.size))
                }
            });
            match result {
                Ok(()) => return Ok(()),
                Err(e @ (AssetError::Conflict | AssetError::Network(_))) => {
                    if self.remote_verified(blob, cancel, task).map_err(|e| error(e.to_string()))? {
                        return Ok(());
                    }
                    return Err(error(e.to_string()));
                }
                Err(e) => return Err(error(e.to_string())),
            }
        }
        for _ in 0..2 {
            if cancel.load(Ordering::Relaxed) {
                return Err(error("upload cancelled; multipart receipt retained"));
            }
            if !receipt.multipart.contains_key(&blob.remote.key) {
                let id = self
                    .resolver
                    .background_transfer(blob, cancel, || self.connection.start_multipart(blob, cancel))
                    .map_err(|e| error(e.to_string()))?;
                receipt.multipart.insert(blob.remote.key.clone(), Multipart { id: id.clone(), parts: BTreeMap::new() });
                if let Err(error) = self.write_receipt(receipt, cancel) {
                    // No part may be sent before its upload identity is durable.
                    let _ = self.connection.abort_multipart(blob, &id, &AtomicBool::new(false));
                    return Err(error);
                }
            }
            let upload_id = receipt.multipart.get(&blob.remote.key).ok_or_else(|| error("multipart receipt disappeared"))?.id.clone();
            let mut file = std::fs::File::open(lease.path()).map_err(io)?;
            let mut offset = 0u64;
            let mut number = 1u16;
            let mut restart = false;
            while offset < blob.size {
                if cancel.load(Ordering::Relaxed) {
                    return Err(error("upload cancelled; multipart receipt retained"));
                }
                let count = usize::try_from(blob.size.saturating_sub(offset).min(PART_BYTES as u64)).map_err(|_| error("invalid part length"))?;
                let mut bytes = Vec::new();
                bytes.try_reserve_exact(count).map_err(|_| error("not enough memory for multipart upload"))?;
                bytes.resize(count, 0);
                file.read_exact(&mut bytes).map_err(io)?;
                let hash = sha256(&bytes);
                let saved = receipt.multipart.get(&blob.remote.key).and_then(|upload| upload.parts.get(&number));
                if !saved.is_some_and(|part| part.hash == hash && part.size == count as u64) {
                    task.detail(&format!("{} · uploading {} / part {number}", self.imported.photo.file_name, self.rendition(blob)));
                    let mut observed = 0;
                    let token = self.resolver.background_transfer(blob, cancel, || {
                        self.connection
                            .upload_part(blob, &upload_id, number, &bytes, cancel, &mut |done| task.observe(&mut observed, done, offset, blob.size))
                    });
                    match token {
                        Ok(etag) => {
                            receipt
                                .multipart
                                .get_mut(&blob.remote.key)
                                .ok_or_else(|| error("multipart receipt disappeared"))?
                                .parts
                                .insert(number, Part { etag, hash, size: count as u64 });
                            self.write_receipt(receipt, cancel)?;
                        }
                        Err(AssetError::Missing) => {
                            restart = true;
                            break;
                        }
                        Err(e) => return Err(error(e.to_string())),
                    }
                }
                offset = offset.saturating_add(count as u64);
                number = number.checked_add(1).ok_or_else(|| error("multipart part number exhausted"))?;
                task.progress(offset, blob.size);
            }
            if !restart {
                let parts = receipt
                    .multipart
                    .get(&blob.remote.key)
                    .ok_or_else(|| error("multipart receipt disappeared"))?
                    .parts
                    .iter()
                    .map(|(number, part)| (*number, part.etag.clone()))
                    .collect::<Vec<_>>();
                task.detail(&format!("{} · completing {}", self.imported.photo.file_name, self.rendition(blob)));
                let completed = self.resolver.background_transfer(blob, cancel, || {
                    if self.sync.as_ref().is_some_and(|s| !s.protocol.is_head()) {
                        self.connection.complete_multipart_immutable(blob, &upload_id, &parts, cancel)
                    } else {
                        self.connection.complete_multipart(blob, &upload_id, &parts, cancel)
                    }
                });
                match completed {
                    Ok(()) => return Ok(()),
                    Err(e @ (AssetError::Missing | AssetError::Conflict | AssetError::Network(_))) => {
                        if self.remote_verified(blob, cancel, task).map_err(|e| error(e.to_string()))? {
                            return Ok(());
                        }
                        if matches!(e, AssetError::Missing | AssetError::Conflict) {
                            restart = true;
                        } else {
                            return Err(error(e.to_string()));
                        }
                    }
                    Err(e) => return Err(error(e.to_string())),
                }
            }
            if restart {
                self.resolver
                    .background_transfer(blob, cancel, || self.connection.abort_multipart(blob, &upload_id, cancel))
                    .map_err(|e| error(e.to_string()))?;
                receipt.multipart.remove(&blob.remote.key);
                self.write_receipt(receipt, cancel)?;
            }
        }
        Err(error("multipart upload repeatedly expired or conflicted; staged bytes and receipts retained"))
    }

    fn run(
        &self,
        abort: bool,
        events: &mpsc::SyncSender<Event>,
        cancel: &AtomicBool,
        task: &UploadProgress,
        defer_publication: bool,
    ) -> Result<Value> {
        let path = self.root.join("uploads").join(format!("{}.json", self.id));
        let frozen = self
            .sync
            .as_ref()
            .into_iter()
            .flat_map(|s| s.pending.values())
            .filter(|m| {
                m.changes.iter().any(|c| {
                    c.entity == format!("photo:{}", self.imported.photo_id)
                        && c.field == "content"
                        && c.value.get("assetId").and_then(Value::as_str) == Some(self.imported.asset_id.0.as_str())
                })
            })
            .map(|m| m.digest().map(|digest| (m.revision.id(), digest)).map_err(|e| error(e.to_string())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut receipt = read_receipt(&path)?.unwrap_or_else(|| Receipt {
            version: self.sync.as_ref().map_or(2, |s| if s.protocol.is_head() { 4 } else { 3 }),
            protocol: self.sync.as_ref().map_or(Default::default(), |s| s.protocol),
            library: self.sync.as_ref().map(|s| s.library.clone()),
            append_proof: Default::default(),
            publication_intent: if self.sync.is_some() { frozen.clone() } else { Default::default() },
            id: self.id.clone(),
            imported: self.imported.clone(),
            storage: Some(self.connection.storage_snapshot()),
            state: UploadState::SavedLocally,
            verified: Default::default(),
            multipart: Default::default(),
            generation: None,
            error: None,
        });
        if receipt.id != self.id || receipt.imported.asset_id != self.imported.asset_id || receipt.imported.asset != self.imported.asset {
            return Err(error("upload receipt describes another asset or rendition; preserved"));
        }
        if receipt.protocol.is_head()
            && let Some(local) = self.sync.as_ref().filter(|s| s.migrated_from_head && !s.protocol.is_head())
        {
            if receipt.storage.as_ref().is_some_and(|storage| !storage.same_destination(&self.connection)) {
                return Err(error("migration upload destination mismatch; receipt retained"));
            }
            receipt.protocol = local.protocol;
            receipt.library = Some(local.library.clone());
            receipt.version = 3;
            receipt.generation = None;
            receipt.storage = Some(self.connection.storage_snapshot());
            self.write_receipt(&receipt, cancel)?;
        }
        if !receipt.protocol.is_head() && receipt.publication_intent.is_empty() {
            receipt.publication_intent = frozen.clone();
        }
        if receipt.protocol != self.sync.as_ref().map_or(Default::default(), |s| s.protocol)
            || (receipt.library.is_some() && receipt.library.as_ref() != self.sync.as_ref().map(|s| &s.library))
        {
            return Err(error("upload receipt protocol/library differs; explicit migration or original settings required"));
        }
        if receipt.storage.as_ref().is_some_and(|storage| !storage.same_destination(&self.connection)) {
            return Err(error(
                "upload receipt belongs to another storage destination; restore the original connection settings; staged bytes preserved",
            ));
        }
        if receipt.storage.is_none() {
            if !receipt.multipart.is_empty() && !abort {
                return Err(error(
                    "legacy multipart receipt has no destination binding; explicitly abort it using its original connection settings, then retry; staged bytes preserved",
                ));
            }
            receipt.storage = Some(self.connection.storage_snapshot());
            receipt.version = 2;
        }
        if let Some(local) = self.sync.as_ref().filter(|s| s.protocol.is_head()) {
            receipt.version = 4;
            receipt.library = Some(local.library.clone());
            if receipt.publication_intent.is_empty() {
                receipt.publication_intent = frozen.clone();
            }
        }
        let mut remote_invalid = false;
        let result = (|| {
            receipt.state = UploadState::Uploading;
            receipt.error = None;
            self.write_receipt(&receipt, cancel)?;
            self.phase(&receipt, events, cancel)?;
            for blob in blobs(&receipt.imported.asset).cloned().collect::<Vec<_>>() {
                if abort {
                    if let Some(upload) = receipt.multipart.get(&blob.remote.key) {
                        self.resolver
                            .background_transfer(&blob, cancel, || self.connection.abort_multipart(&blob, &upload.id, cancel))
                            .map_err(|e| error(e.to_string()))?;
                        receipt.multipart.remove(&blob.remote.key);
                        self.write_receipt(&receipt, cancel)?;
                    }
                    continue;
                }
                let verified = self.remote_verified(&blob, cancel, task).map_err(|e| {
                    remote_invalid |= matches!(e, AssetError::Integrity);
                    error(e.to_string())
                })?;
                if !verified {
                    remote_invalid = true;
                    self.upload_blob(&mut receipt, &blob, cancel, task)?;
                    if !self.remote_verified(&blob, cancel, task).map_err(|e| error(e.to_string()))? {
                        return Err(error("uploaded object is not visible; retain the original and retry"));
                    }
                }
                if let Some(upload) = receipt.multipart.get(&blob.remote.key) {
                    // Completion may have lost its response, or another writer won.
                    // A recorded unfinished upload is safe to retire after byte verification.
                    self.resolver
                        .background_transfer(&blob, cancel, || self.connection.abort_multipart(&blob, &upload.id, cancel))
                        .map_err(|e| error(e.to_string()))?;
                    receipt.multipart.remove(&blob.remote.key);
                }
                receipt.verified.insert(blob.remote.key.clone());
                self.write_receipt(&receipt, cancel)?;
                self.phase(&receipt, events, cancel)?;
            }
            if abort {
                receipt.state =
                    if receipt.generation.is_some() || !receipt.append_proof.is_empty() { UploadState::Published } else { UploadState::SavedLocally };
                self.write_receipt(&receipt, cancel)?;
                self.phase(&receipt, events, cancel)?;
                return Ok(json!({"import": self.id, "aborted": true, "originalPreserved": true}));
            }
            remote_invalid = false; // Every remote object is now byte verified.
            receipt.state = UploadState::ObjectsVerified;
            self.write_receipt(&receipt, cancel)?;
            self.phase(&receipt, events, cancel)?;
            task.detail(&format!("{} · publishing", self.imported.photo.file_name));
            if defer_publication && self.sync.as_ref().is_some_and(|s| !s.protocol.is_head()) {
                return Ok(json!({"import":self.id,"objectsVerified":true,"publicationPending":true}));
            }
            if let Some(local) = self.sync.as_ref() {
                let _permit = self.coordinator.acquire(cancel)?;
                let outcome = crate::cloud_replication::synchronize(&self.connection, local, false, cancel).map_err(|e| error(e.to_string()))?;
                let proof = &outcome.state;
                let target = proof.documents.get(&format!("photo:{}", self.imported.photo_id)).and_then(|fields| fields.get("content"));
                let expected = if !receipt.publication_intent.is_empty() {
                    receipt.publication_intent.clone()
                } else if !receipt.append_proof.is_empty() {
                    receipt.append_proof.clone()
                } else {
                    target
                        .into_iter()
                        .flat_map(|r| r.0.iter())
                        .filter(|(_, v)| v.get("assetId").and_then(Value::as_str) == Some(self.imported.asset_id.0.as_str()))
                        .filter_map(|(id, _)| proof.received.get(id).map(|r| (id.clone(), r.digest.clone())))
                        .collect()
                };
                let mut published = !expected.is_empty();
                let mut witness = Default::default();
                for (id, digest) in &expected {
                    published &= crate::cloud_history::native::published(&self.connection, proof, id, digest, &mut witness, cancel)
                        .map_err(|e| error(e.to_string()))?;
                }
                if published {
                    receipt.append_proof = expected;
                    if local.protocol.is_head() {
                        receipt.generation = Some(outcome.generation);
                    }
                    self.write_receipt(&receipt, cancel)?;
                }
                let (reply, response) = mpsc::sync_channel(1);
                events
                    .send(Event::Publication { outcome: Box::new(outcome), started_generation: local.generation, reply })
                    .map_err(|_| error("upload owner closed; exact publication retained"))?;
                loop {
                    match response.recv_timeout(Duration::from_millis(20)) {
                        Ok(r) => {
                            r?;
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(error("upload owner closed")),
                        Err(_) if cancel.load(Ordering::Relaxed) => return Err(error("upload cancelled; publication retained")),
                        Err(_) => {}
                    }
                }
                if !published {
                    task.detail("Objects verified · waiting for other causal imports");
                    return Ok(json!({"import":self.id,"objectsVerified":true,"publicationPending":true}));
                }
            } else {
                return Err(error("enable cloud synchronization with cloud.syncEnable before uploading; staged originals retained"));
            }
            receipt.state = UploadState::Published;
            self.write_receipt(&receipt, cancel)?;
            self.phase(&receipt, events, cancel)?;
            receipt.state = UploadState::Synchronized;
            self.phase(&receipt, events, cancel)?; // Durable local acknowledgement before completion.
            if let Err(e) = self.write_receipt(&receipt, cancel) {
                log::warn!("final upload receipt marker: {e}; journal acknowledgement is durable");
            }
            Ok(json!({"import": self.id, "synchronized": true, "generation": receipt.generation}))
        })();
        if let Err(e) = &result {
            receipt.state = UploadState::Failed;
            let mut message = e.to_string().replace('\0', "�");
            if message.len() > 4096 {
                let mut end = 4096;
                while !message.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                message.truncate(end);
            }
            receipt.error = Some(message);
            let _ = self.write_receipt(&receipt, &AtomicBool::new(false));
            let _ = self.send_phase(&receipt, events, cancel, remote_invalid);
        }
        result
    }
}

struct Pending {
    receiver: mpsc::Receiver<Event>,
    library: Arc<()>,
    resolver: Arc<AssetResolver>,
    connection_revision: u64,
    ids: BTreeMap<String, lightcraft_catalog::assets::AssetId>,
    connections: BTreeSet<String>,
    maintenance: Option<String>,
    guard: TaskGuard,
    cancel: Arc<AtomicBool>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Uploads {
    pending: Vec<Pending>,
    pub reports: Vec<Value>,
    requested: bool,
    include_failed: bool,
    attempted: BTreeSet<String>,
}
impl Uploads {
    pub fn active_files(&self, catalog: &lightcraft_catalog::Catalog) -> usize {
        self.pending
            .iter()
            .filter(|p| p.maintenance.is_none())
            .flat_map(|p| p.ids.keys())
            .filter_map(|id| catalog.upload_intents().get(id))
            .filter(|i| i.state != UploadState::Synchronized)
            .map(|i| i.photo_id)
            .collect::<BTreeSet<_>>()
            .len()
    }
    pub fn pause(&self) {
        for pending in self.pending.iter().filter(|p| p.maintenance.is_none()) {
            pending.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn running(&self) -> usize {
        self.pending.len()
    }
    pub fn request_resume(&mut self, include_failed: bool) {
        self.requested = true;
        self.include_failed |= include_failed;
        if include_failed {
            self.attempted.clear();
        }
    }
}

/// Retained immutable content is still publishable after deletion/revision. Its original
/// mutation must precede the tombstone; completing it never restores the local photo.
fn retained_content(catalog: &lightcraft_catalog::Catalog, intent: &UploadIntent) -> bool {
    let Some(asset) = catalog.asset(&intent.asset_id) else { return false };
    let Some(state) = catalog.sync_state().filter(|s| s.connection == asset.original.remote.connection) else { return false };
    let Some(global) = catalog.cloud_identity(&format!("photo:{}", intent.photo_id.0)) else { return false };
    state
        .documents
        .get(&format!("photo:{global}"))
        .and_then(|fields| fields.get("content"))
        .is_some_and(|register| register.0.values().any(|value| value.get("assetId").and_then(Value::as_str) == Some(intent.asset_id.0.as_str())))
        || state.pending.values().any(|mutation| {
            mutation.changes.iter().any(|change| {
                change.entity == format!("photo:{global}")
                    && change.field == "content"
                    && change.value.get("assetId").and_then(Value::as_str) == Some(intent.asset_id.0.as_str())
            })
        })
}

fn items(s: &mut Session, p: &Value, abort: bool) -> Result<(Vec<Item>, Vec<Value>)> {
    let verify = p.get("verify").map(|value| value.as_bool().ok_or_else(|| error("verify must be true or false"))).transpose()?.unwrap_or(false);
    if abort && verify {
        return Err(error("verification is available through cloud.upload, not multipart abort"));
    }
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("open a persistent native library first"))?;
    resolver.validate_storage().map_err(|e| error(e.to_string()))?;
    let root = resolver.durable_root().ok_or_else(|| error("this library has no durable managed storage"))?;
    let connections = s.library.as_mut().ok_or_else(|| error("open a library first"))?.cloud_connections()?;
    if abort && p.get("imports").is_none() {
        return Err(error("explicit import identities are required to abort multipart uploads"));
    }
    let selected = p
        .get("imports")
        .map(|value| {
            let values = value.as_array().ok_or_else(|| error("imports must be an array of import identities"))?;
            if values.is_empty() || values.len() > 32 {
                return Err(error("select between 1 and 32 imports per upload task"));
            }
            values
                .iter()
                .map(|value| value.as_str().filter(|value| identifier(value)).map(str::to_owned).ok_or_else(|| error("invalid import identity")))
                .collect::<Result<BTreeSet<_>>>()
        })
        .transpose()?;
    let mut result = Vec::new();
    let mut skipped = Vec::new();
    for (id, intent) in s.catalog.upload_intents() {
        if selected.as_ref().is_some_and(|selected| !selected.contains(id)) || (intent.state == UploadState::Synchronized && !verify) {
            continue;
        }
        if result.len() == 32 {
            break;
        }
        if s.cloud_uploads.pending.iter().any(|pending| pending.ids.contains_key(id)) {
            continue;
        }
        let Some(asset) = s.catalog.asset(&intent.asset_id) else {
            log::warn!("upload {id}: asset is absent; staged original preserved; other imports continue");
            skipped.push(json!({"import":id,"error":"upload asset is absent; staged original preserved"}));
            continue;
        };
        let photo = s.catalog.photo(intent.photo_id);
        let retained = retained_content(&s.catalog, intent);
        if photo.is_none_or(|p| p.deleted || !matches!(p.source, Source::Managed { .. })) && !retained {
            log::warn!("upload {id}: removed photo has no synchronized content; staged original retained");
            skipped.push(json!({"import":id,"error":"removed photo has no synchronized content; staged original retained"}));
            continue;
        }
        // An external or DNG revision can supersede this photo while the earlier import is
        // still pending. Its immutable staged description remains publishable under the same
        // global identity; never relabel the new photo's bytes as the old asset.
        let staged;
        let photo = if let Some(photo) = photo.filter(|p| !p.deleted && p.source == (Source::Managed { asset_id: intent.asset_id.clone() })) {
            photo.as_ref()
        } else {
            let manifest = match crate::cloud_import::read_manifest(&root, id) {
                Ok(manifest) => manifest,
                Err(e) => {
                    log::warn!("upload {id}: {e}; other imports continue");
                    skipped.push(json!({"import":id,"error":e.to_string()}));
                    continue;
                }
            };
            if !manifest.prepared
                || manifest.asset_id != intent.asset_id
                || manifest.asset != *asset
                || manifest.target.as_ref().is_some_and(|t| t.photo_id != intent.photo_id)
            {
                log::warn!("upload {id}: staged identity differs; preserved");
                skipped.push(json!({"import":id,"error":"staged identity differs or is incomplete; preserved"}));
                continue;
            }
            let Some(snapshot) = manifest.photo else {
                log::warn!("upload {id}: staged photo description missing; preserved");
                skipped.push(json!({"import":id,"error":"staged photo description missing; preserved"}));
                continue;
            };
            staged = snapshot;
            staged.as_ref()
        };
        let Some(connection) = connections.iter().find(|connection| connection.id == asset.original.remote.connection).cloned() else {
            skipped.push(json!({"import":id,"error":"configure the import's cloud connection before uploading"}));
            continue;
        };
        if s.cloud_uploads.pending.iter().any(|pending| pending.maintenance.as_deref() == Some(connection.id.as_str())) {
            if selected.is_some() {
                return Err(error("upload retention cleanup is using this connection; wait or cancel cleanup before resuming"));
            }
            continue;
        }
        if blobs(asset).any(|blob| blob.remote.connection != connection.id) {
            return Err(error("import renditions refer to different connections"));
        }
        let base_asset = match crate::cloud_import::revision_target(&root, id) {
            Ok(target) => target.map(|target| target.base_asset),
            Err(e) => {
                skipped.push(json!({"import":id,"error":e.to_string()}));
                continue;
            }
        };
        let imported = Imported {
            mutation_id: id.clone(),
            photo_id: s.catalog.cloud_identity(&format!("photo:{}", intent.photo_id.0)).unwrap_or(id).to_owned(),
            asset_id: intent.asset_id.clone(),
            photo: Box::new({
                let mut snapshot = photo.clone();
                snapshot.id = intent.photo_id;
                snapshot
            }),
            asset: Box::new(asset.clone()),
            base_asset,
        };
        imported.validate().map_err(|e| error(e.to_string()))?;
        let sync = s.catalog.sync_state().filter(|state| state.connection == connection.id).cloned();
        if !abort && sync.is_none() {
            skipped.push(json!({"import":id,"error":"this import uses another synchronization connection; open its library or restore the original connection; staged originals retained"}));
            continue;
        }
        result.push(Item {
            coordinator: s.cloud_replication_jobs.coordinator.clone(),
            attempt: 0,
            reverification: verify && intent.state == UploadState::Synchronized,
            sync,
            id: id.clone(),
            imported,
            connection,
            root: root.clone(),
            resolver: resolver.clone(),
        });
    }
    if let Some(selected) = selected
        && selected.iter().any(|id| !s.catalog.upload_intents().contains_key(id))
    {
        return Err(error("unknown import identity"));
    }
    result.sort_by_key(|item| {
        item.sync.as_ref().and_then(|s| {
            s.pending
                .values()
                .filter(|m| {
                    m.changes
                        .iter()
                        .any(|c| c.field == "content" && c.value.get("assetId").and_then(Value::as_str) == Some(item.imported.asset_id.0.as_str()))
                })
                .map(|m| m.revision.clone())
                .min()
        })
    });
    Ok((result, skipped))
}

pub fn start(s: &mut Session, p: &Value) -> Result<Value> {
    start_impl(s, p, false, None)
}
pub fn abort(s: &mut Session, p: &Value) -> Result<Value> {
    start_impl(s, p, true, None)
}

pub(crate) fn remove_local(s: &mut Session, p: &Value, blobs: Vec<Blob>, observer: Option<&mut dyn FnMut(u64, u64, &str) -> bool>) -> Result<Value> {
    let wait = p
        .get("wait")
        .map(|value| value.as_bool().ok_or_else(|| error("wait must be true or false")))
        .transpose()?
        .unwrap_or(!crate::assets::is_cached_only());
    if wait && crate::assets::is_cached_only() {
        return Err(error("desktop local-copy removal runs in the background; omit wait"));
    }
    if s.cloud_uploads.pending.len() >= 4 {
        return Err(error("four storage tasks are running; wait or cancel before removing local copies"));
    }
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("open a persistent library first"))?;
    let task = s.activity.start("cache", "Remove Local Originals", crate::activity::Cancel::Yes);
    let id = task.id();
    let handle = task.handle();
    let cancel = task.cancel_flag().ok_or_else(|| error("cache removal cancellation unavailable"))?;
    let worker_cancel = cancel.clone();
    let worker_resolver = resolver.clone();
    let (sender, receiver) = mpsc::sync_channel(2);
    std::thread::Builder::new()
        .name("cloud-local-copy".into())
        .spawn(move || {
            let result = crate::guard::catch("local cloud copy removal", || {
                handle.detail("Checking uploaded storage and staging ownership");
                let prepared = worker_resolver.prepare_cache_removal(&worker_cancel);
                let mut removed = Vec::new();
                let mut failed = Vec::new();
                let total = blobs.len() as u64;
                for (index, blob) in blobs.into_iter().enumerate() {
                    if worker_cancel.load(Ordering::Relaxed) {
                        return Err(error("local-copy removal cancelled; remaining data preserved"));
                    }
                    let result = match &prepared {
                        Ok(()) => worker_resolver.evict_worker(&blob, &worker_cancel),
                        Err(error) => Err(error.clone()),
                    };
                    match result {
                        Ok(()) => removed.push(blob.id),
                        Err(error) => failed.push(json!({"blob":blob.id,"error":error.to_string()})),
                    }
                    handle.progress(index.saturating_add(1) as u64, total);
                }
                Ok(json!({"removed":removed,"failed":failed}))
            })
            .unwrap_or_else(|message| Err(error(message)));
            worker_resolver.assets_changed();
            let _ = sender.send(Event::Finished(result));
        })
        .map_err(io)?;
    s.cloud_uploads.pending.push(Pending {
        receiver,
        library: s.library_token(),
        connection_revision: resolver.connection_revision(),
        resolver,
        ids: BTreeMap::new(),
        connections: BTreeSet::new(),
        maintenance: None,
        guard: task,
        cancel: cancel.clone(),
    });
    if wait { wait_task(s, id, cancel, Arc::new(AtomicU64::new(0)), observer) } else { Ok(json!({"background":true,"task":id})) }
}

pub fn observed_removal(s: &mut Session, p: &Value, observer: &mut dyn FnMut(u64, u64, &str) -> bool) -> Result<Value> {
    s.execute_fn("cloud.removeLocalCopy", |s| crate::cmd::cloud::remove_local_copy_impl(s, p, Some(observer)))
}

/// The serving thread polls its transport hook while workers transfer bytes.
/// A zero total means unknown: reads for verification and retry traffic are included.
pub fn observed(s: &mut Session, p: &Value, abort: bool, observer: &mut dyn FnMut(u64, u64, &str) -> bool) -> Result<Value> {
    s.execute_fn(if abort { "cloud.abortUploads" } else { "cloud.upload" }, |s| start_impl(s, p, abort, Some(observer)))
}

fn start_impl(s: &mut Session, p: &Value, abort: bool, observer: Option<&mut dyn FnMut(u64, u64, &str) -> bool>) -> Result<Value> {
    if !abort && s.catalog.sync_state().is_none() {
        return Err(error(
            "enable cloud synchronization with cloud.syncEnable before uploading; choose the storage protocol explicitly; staged originals retained",
        ));
    }
    if s.cloud_paused && !abort {
        return Err(error("cloud syncing is paused; use cloud.resume first"));
    }
    if s.cloud_replication_jobs.migrating() || s.cloud_replication_jobs.preparing_migration() {
        return Err(error("uploads are paused for migration; finish migration or resume synchronization first"));
    }
    s.claim_cloud_owner()?;
    let wait = p
        .get("wait")
        .map(|value| value.as_bool().ok_or_else(|| error("wait must be true or false")))
        .transpose()?
        .unwrap_or(!crate::assets::is_cached_only());
    if wait && crate::assets::is_cached_only() {
        return Err(error("desktop uploads run in the background; omit wait"));
    }
    if s.cloud_uploads.pending.len() >= 4 {
        return Err(error("four upload tasks are already running; wait or cancel before retrying"));
    }
    s.persist()?; // A queued import must be durable before any network operation.
    let (mut items, skipped) = items(s, p, abort)?;
    if items.is_empty() {
        return Ok(json!({"tasks": [], "results": [], "skipped":skipped, "remaining": 0}));
    }
    let task =
        s.activity.start("upload", if abort { "Abort Resumable Cloud Uploads" } else { "Upload Cloud Originals" }, crate::activity::Cancel::Yes);
    task.set_unit(crate::activity::Unit::Bytes);
    let id = task.id();
    for item in &mut items {
        item.attempt = id;
    }
    let handle = task.handle();
    let bytes = Arc::new(AtomicU64::new(0));
    let worker_bytes = bytes.clone();
    let cancel = task.cancel_flag().ok_or_else(|| error("upload cancellation flag unavailable"))?;
    let ids = items.iter().map(|item| (item.id.clone(), item.imported.asset_id.clone())).collect();
    let connections = items.iter().map(|item| item.connection.id.clone()).collect();
    let (sender, receiver) = mpsc::sync_channel(2);
    let worker_cancel = cancel.clone();
    let resolver = items.first().map(|item| item.resolver.clone()).ok_or_else(|| error("upload task is empty"))?;
    let owner_resolver = resolver.clone();
    let connection_revision = resolver.connection_revision();
    std::thread::Builder::new()
        .name("cloud-upload".into())
        .spawn(move || {
            let progress = UploadProgress { handle, bytes: worker_bytes };
            let result = crate::guard::catch("cloud upload worker", || {
                let mut results = Vec::new();
                let mut failures = Vec::new();
                // A single causal import mutation can contain several photos. Verify every
                // selected asset before publishing any of those photos' metadata.
                for item in &items {
                    if worker_cancel.load(Ordering::Relaxed) {
                        return Err(error("upload cancelled; originals and multipart receipts retained"));
                    }
                    let append = !abort && item.sync.as_ref().is_some_and(|s| !s.protocol.is_head());
                    let uploaded = item.run(abort, &sender, &worker_cancel, &progress, append);
                    if append {
                        if uploaded.is_err() {
                            let _ = item.run(abort, &sender, &worker_cancel, &progress, true);
                        }
                        continue;
                    }
                    match uploaded {
                        Ok(result) => results.push(result),
                        Err(error) => failures.push(error.to_string()),
                    }
                }
                for item in items.iter().filter(|item| !abort && item.sync.as_ref().is_some_and(|s| !s.protocol.is_head())) {
                    match item.run(false, &sender, &worker_cancel, &progress, false) {
                        Ok(value) => results.push(value),
                        Err(e) => failures.push(e.to_string()),
                    }
                }
                if let Some(first) = failures.first() {
                    return Err(error(format!("{} upload(s) failed; {} completed: {first}", failures.len(), results.len())));
                }
                Ok(json!({"results": results,"skipped":skipped}))
            })
            .unwrap_or_else(|message| Err(error(message)));
            let _ = sender.send(Event::Finished(result));
            resolver.assets_changed();
        })
        .map_err(io)?;
    s.cloud_uploads.pending.push(Pending {
        receiver,
        library: s.library_token(),
        resolver: owner_resolver,
        connection_revision,
        ids,
        connections,
        maintenance: None,
        guard: task,
        cancel: cancel.clone(),
    });
    if wait {
        return wait_task(s, id, cancel, bytes, observer);
    }
    Ok(json!({"background": true, "task": id}))
}

fn wait_task(
    s: &mut Session,
    id: u64,
    cancel: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    mut observer: Option<&mut dyn FnMut(u64, u64, &str) -> bool>,
) -> Result<Value> {
    loop {
        s.poll_cloud_replication();
        s.poll_cloud_uploads();
        if let Some(observer) = observer.as_mut() {
            let detail = s.activity.list().into_iter().find(|task| task.id == id).map(|task| task.detail).unwrap_or_default();
            if !observer(bytes.load(Ordering::Relaxed), 0, &detail) {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Some(report) = s.cloud_uploads.reports.iter().find(|report| report["task"] == id) {
            if report["ok"] == true {
                return Ok(report["result"].clone());
            }
            return Err(error(report["error"].as_str().unwrap_or("cloud upload work failed")));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Session {
    /// Native hosts pump this after polling owner events. Headless one-shot commands
    /// explicitly use `cloud.upload` so local import completion never waits for a bucket.
    pub fn resume_cloud_uploads(&mut self) {
        if self.cloud_paused || self.cloud_replication_jobs.migrating() || self.cloud_replication_jobs.preparing_migration() {
            return;
        }
        if !self.cloud_uploads.requested || self.cloud_uploads.pending.len() >= 4 {
            return;
        }
        let ids = self
            .catalog
            .upload_intents()
            .iter()
            .filter(|(id, intent)| {
                intent.state != UploadState::Synchronized
                    && (self.cloud_uploads.include_failed || intent.state != UploadState::Failed)
                    && !self.cloud_uploads.attempted.contains(*id)
                    && !self.cloud_uploads.pending.iter().any(|pending| pending.ids.contains_key(*id))
                    && !self.catalog.asset(&intent.asset_id).is_some_and(|asset| {
                        self.cloud_uploads
                            .pending
                            .iter()
                            .any(|pending| pending.maintenance.as_deref() == Some(asset.original.remote.connection.as_str()))
                    })
            })
            .take(1)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            if !self.cloud_uploads.pending.is_empty() {
                return;
            }
            self.cloud_uploads.requested = false;
            self.cloud_uploads.include_failed = false;
            return;
        }
        self.cloud_uploads.attempted.extend(ids.iter().cloned());
        if let Err(e) = start(self, &json!({"imports": ids, "wait": false})) {
            log::warn!("queued cloud uploads: {e}; originals remain durable");
        }
    }

    pub fn poll_cloud_uploads(&mut self) {
        // Publication can materialize remote edits. Wait until a slider/command group
        // finishes, just as the ordinary synchronization owner does.
        if self.interaction.is_some() {
            return;
        }
        let mut uploads = std::mem::take(&mut self.cloud_uploads);
        uploads.pending.retain(|pending| {
            for _ in 0..8 {
                match pending.receiver.try_recv() {
                    Ok(Event::Publication { outcome, started_generation, reply }) => {
                        let result = (|| {
                            if !Arc::ptr_eq(&pending.library, &self.library_token())
                                || pending.connection_revision != pending.resolver.connection_revision()
                            {
                                return Err(error("library or connection changed; append publication retained"));
                            }
                            let reconciled = if let Some(progress) = outcome.append.clone() {
                                crate::cloud_sync::incoming_append(&self.catalog, progress, &outcome.acknowledged)
                            } else {
                                crate::cloud_sync::completed_checkpoint(&self.catalog,&outcome.state,&outcome.acknowledged,outcome.cursor.clone(),outcome.generation,started_generation)
                            };
                            let op = match reconciled {
                                Err(error) if crate::cloud_sync::obsolete_completion_needs_proof(&self.catalog, &outcome, &error) => {
                                    crate::cloud_replication::request_poll(self);
                                    return Ok(()); // The outbox awaits a fresh scan of the newer archives.
                                }
                                result => result?,
                            };
                            let inverse = self.apply_catalog_mode(op, false)?;
                            if let Err(e) = self.persist() {
                                self.apply_received(inverse)?;
                                return Err(e);
                            }
                            Ok(())
                        })();
                        let _ = reply.send(result);
                    }
                    Ok(Event::Phase { attempt, remote_invalid, id, state, verified, error: message, reply }) => {
                        let result = (|| {
                            if attempt != pending.guard.id() {
                                return Err(error("obsolete upload attempt; current acknowledgement retained"));
                            }
                            if !Arc::ptr_eq(&pending.library, &self.library_token()) {
                                return Err(error("library changed; staged data preserved"));
                            }
                            if pending.connection_revision != pending.resolver.connection_revision() {
                                return Err(error("storage connection changed; retry using its current settings; staged data preserved"));
                            }
                            let mut intent =
                                self.catalog.upload_intents().get(&id).ok_or_else(|| error("import was removed; staged original preserved"))?.clone();
                            let previous = intent.clone();
                            if pending.ids.get(&id) != Some(&intent.asset_id) {
                                return Err(error("import's asset changed; staged data preserved"));
                            }
                            if self.catalog.photo(intent.photo_id).is_none_or(|photo| photo.deleted || !matches!(photo.source, Source::Managed { .. }))
                                && !retained_content(&self.catalog, &intent) {
                                return Err(error("photo was deleted or changed; upload stopped and original preserved"));
                            }
                            let paused = self.cloud_paused && pending.cancel.load(Ordering::Relaxed) && state == UploadState::Failed;
                            // Another worker may have acknowledged this shared mutation while
                            // this worker still holds an older phase. Never undo that proof.
                            if intent.state != UploadState::Synchronized || (state == UploadState::Failed && remote_invalid) {
                                intent.state = if paused { UploadState::SavedLocally } else { state };
                                intent.verified = verified;
                                intent.error = if paused { None } else { message };
                            }
                            if intent == previous {
                                return Ok(());
                            }
                            self.apply_system(Op::SetUploadIntent { id: id.clone(), intent: Some(Box::new(intent)) })?;
                            if let Err(e) = self.persist() {
                                // Do not display synchronized after a failed local acknowledgement.
                                // Both operations remain pending, preserving the previous durable state.
                                self.apply_system(Op::SetUploadIntent { id, intent: Some(Box::new(previous)) })?;
                                return Err(e);
                            }
                            Ok(()) // Worker may proceed only after durable acknowledgement.
                        })();
                        let _ = reply.send(result);
                    }
                    Ok(Event::Finished(result)) => {
                        uploads.reports.push(match result {
                            Ok(value) => json!({"task": pending.guard.id(), "ok": true, "result": value}),
                            Err(e) => json!({"task": pending.guard.id(), "ok": false, "error": e.to_string(), "paused":self.cloud_paused && pending.cancel.load(Ordering::Relaxed)}),
                        });
                        return false;
                    }
                    Err(mpsc::TryRecvError::Empty) => return true,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        uploads
                            .reports
                            .push(json!({"task": pending.guard.id(), "ok": false, "error": "upload worker closed; durable receipts retained"}));
                        return false;
                    }
                }
            }
            true
        });
        if uploads.reports.len() > 32 {
            uploads.reports.drain(..uploads.reports.len() - 32);
        }
        self.cloud_uploads = uploads;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::assets::{AssetId, RemoteLocation};
    use lightcraft_catalog::{FsStore, Photo, PhotoId, Store};

    struct FailingAppend {
        inner: FsStore,
        fail: Arc<AtomicBool>,
    }
    impl Store for FailingAppend {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.inner.read(name)
        }
        fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.inner.write_atomic(name, bytes)
        }
        fn append(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
            if self.fail.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("injected full disk during acknowledgement"));
            }
            self.inner.append(name, bytes)
        }
        fn truncate(&mut self, name: &str, size: u64) -> std::io::Result<()> {
            self.inner.truncate(name, size)
        }
        fn describe(&self) -> String {
            self.inner.describe()
        }
    }

    fn session(root: &Path) -> (Session, Arc<AtomicBool>) {
        let mut session = Session::new().with_fs();
        let fail = Arc::new(AtomicBool::new(false));
        session
            .open_library_in(
                crate::library::LibraryStores {
                    catalog: Box::new(FailingAppend { inner: FsStore::open(root).unwrap(), fail: fail.clone() }),
                    files: Box::new(FsStore::open(root).unwrap()),
                    on_disk: true,
                    dir: root.to_path_buf(),
                },
                false,
            )
            .unwrap();
        let asset_id = AssetId("upload-ack-test".into());
        let asset = Asset {
            associated: Default::default(),
            original: Blob {
                id: sha256(b"original"),
                size: 8,
                remote: RemoteLocation { connection: "test".into(), key: "originals/test".into(), version: None },
            },
            smart_preview: None,
            thumbnail: None,
        };
        session
            .apply_system(Op::Batch {
                ops: vec![
                    Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) },
                    Op::AddPhoto {
                        photo: Box::new(Photo::new(PhotoId(1), Source::Managed { asset_id: asset_id.clone() }, "test.png", "PNG", 2, 2, "")),
                    },
                    Op::SetUploadIntent {
                        id: "upload-ack-test".into(),
                        intent: Some(Box::new(UploadIntent {
                            asset_id,
                            photo_id: PhotoId(1),
                            state: UploadState::Published,
                            verified: BTreeSet::from(["originals/test".into()]),
                            error: None,
                        })),
                    },
                ],
            })
            .unwrap();
        session.persist().unwrap();
        (session, fail)
    }

    fn acknowledgement(session: &mut Session, library: Arc<()>) -> mpsc::Receiver<Result<()>> {
        acknowledgement_with_revision(session, library, None)
    }
    fn acknowledgement_with_revision(session: &mut Session, library: Arc<()>, revision: Option<u64>) -> mpsc::Receiver<Result<()>> {
        let (sender, receiver) = mpsc::sync_channel(2);
        let (reply, response) = mpsc::sync_channel(1);
        let intent = session.catalog.upload_intents()["upload-ack-test"].clone();
        let guard = session.activity.start("upload", "Synthetic acknowledgement", crate::activity::Cancel::Yes);
        sender
            .send(Event::Phase {
                attempt: guard.id(),
                remote_invalid: false,
                id: "upload-ack-test".into(),
                state: UploadState::Synchronized,
                verified: intent.verified,
                error: None,
                reply,
            })
            .unwrap();
        let cancel = guard.cancel_flag().unwrap();
        let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
        let connection_revision = revision.unwrap_or_else(|| resolver.connection_revision());
        session.cloud_uploads.pending.push(Pending {
            receiver,
            library,
            resolver,
            connection_revision,
            ids: BTreeMap::from([("upload-ack-test".into(), intent.asset_id)]),
            connections: BTreeSet::from(["test".into()]),
            maintenance: None,
            guard,
            cancel,
        });
        // An in-flight upload must finish its durable acknowledgement before cutover.
        let before = session.catalog.to_snapshot();
        let error = crate::cloud_replication::start_migration(session, &json!({"writersStopped":true})).unwrap_err();
        assert!(error.to_string().contains("finish or cancel uploads"));
        assert_eq!(session.catalog.to_snapshot(), before);
        session.poll_cloud_uploads();
        response
    }

    #[test]
    fn obsolete_failure_cannot_undo_shared_ack_but_confirmed_remote_damage_remains_visible() {
        let root = std::env::temp_dir().join(format!("lightcraft-upload-order-{}", std::process::id()));
        let (mut s, _) = session(&root);
        for (remote_invalid, wrong_attempt) in [(false, false), (false, true), (true, false)] {
            let mut intent = s.catalog.upload_intents()["upload-ack-test"].clone();
            intent.state = UploadState::Synchronized;
            intent.error = None;
            s.apply_system(Op::SetUploadIntent { id: "upload-ack-test".into(), intent: Some(Box::new(intent.clone())) }).unwrap();
            s.persist().unwrap();
            let (sender, receiver) = mpsc::sync_channel(2);
            let (reply, response) = mpsc::sync_channel(1);
            let guard = s.activity.start("upload", "Held older failure", crate::activity::Cancel::Yes);
            let cancel = guard.cancel_flag().unwrap();
            let resolver = s.media.asset_resolver.as_ref().unwrap().clone();
            sender
                .send(Event::Phase {
                    attempt: if wrong_attempt { guard.id() + 1 } else { guard.id() },
                    remote_invalid,
                    id: "upload-ack-test".into(),
                    state: UploadState::Failed,
                    verified: intent.verified,
                    error: Some("held upload failure".into()),
                    reply,
                })
                .unwrap();
            s.cloud_uploads.pending.push(Pending {
                receiver,
                library: s.library_token(),
                connection_revision: resolver.connection_revision(),
                resolver,
                ids: BTreeMap::from([("upload-ack-test".into(), intent.asset_id)]),
                connections: BTreeSet::from(["test".into()]),
                maintenance: None,
                guard,
                cancel,
            });
            let revision = s.catalog.revision;
            s.poll_cloud_uploads();
            assert_eq!(response.recv().unwrap().is_err(), wrong_attempt);
            let intent = &s.catalog.upload_intents()["upload-ack-test"];
            assert_eq!(intent.state, if remote_invalid { UploadState::Failed } else { UploadState::Synchronized });
            if remote_invalid {
                assert_eq!(intent.error.as_deref(), Some("held upload failure"));
            } else {
                assert_eq!(s.catalog.revision, revision);
            }
            drop(sender);
            s.poll_cloud_uploads();
        }
    }
    #[test]
    fn enabling_sync_restarts_staged_imports_attempted_while_disabled() {
        let root = std::env::temp_dir().join(format!(
            "lightcraft-upload-setup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let (mut s, _) = session(&root);
        s.execute("cloud.configure", &json!({"id":"test","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:1"}))
            .unwrap();
        s.cloud_uploads.request_resume(true);
        s.resume_cloud_uploads();
        assert!(s.cloud_uploads.attempted.contains("upload-ack-test"));
        assert_eq!(s.cloud_uploads.running(), 0);
        s.execute("cloud.syncEnable", &json!({"connection":"test"})).unwrap();
        assert!(s.cloud_uploads.requested);
        assert!(s.cloud_uploads.attempted.is_empty());
        assert!(s.catalog.sync_state().is_some());
        s.close_library().unwrap();
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upload_owner_acknowledgement_waits_for_active_slider_group() {
        let root = std::env::temp_dir().join(format!(
            "lightcraft-upload-interaction-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let (mut s, _) = session(&root);
        s.selection = crate::Selection::single(PhotoId(1));
        s.begin_interaction("slider").unwrap();
        let token = s.library_token();
        let response = acknowledgement(&mut s, token);
        assert!(matches!(response.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(s.catalog.upload_intents()["upload-ack-test"].state, UploadState::Published);
        s.end_interaction().unwrap();
        s.poll_cloud_uploads();
        assert!(response.recv_timeout(Duration::from_secs(1)).unwrap().is_ok());
        assert_eq!(s.catalog.upload_intents()["upload-ack-test"].state, UploadState::Synchronized);
        s.close_library().unwrap();
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_local_acknowledgement_never_displays_synchronized_and_retries_durably() {
        let root = std::env::temp_dir().join(format!(
            "lightcraft-upload-ack-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let (mut session, fail) = session(&root);
        fail.store(true, Ordering::Relaxed);
        let library = session.library_token();
        assert!(acknowledgement(&mut session, library).recv().unwrap().is_err());
        assert_eq!(session.catalog.upload_intents()["upload-ack-test"].state, UploadState::Published);
        fail.store(false, Ordering::Relaxed);
        session.persist().unwrap();
        session.close_library().unwrap();
        drop(session);
        let mut reopened = Session::new().with_fs();
        reopened.open_library(&root, false).unwrap();
        assert_eq!(reopened.catalog.upload_intents()["upload-ack-test"].state, UploadState::Published);
        let library = reopened.library_token();
        assert!(acknowledgement(&mut reopened, library).recv().unwrap().is_ok());
        reopened.close_library().unwrap();
        drop(reopened);
        let mut final_session = Session::new().with_fs();
        final_session.open_library(&root, false).unwrap();
        assert_eq!(final_session.catalog.upload_intents()["upload-ack-test"].state, UploadState::Synchronized);
        final_session.close_library().unwrap();
        drop(final_session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_transfer_using_old_connection_settings_cannot_be_acknowledged() {
        let root = std::env::temp_dir().join(format!("lightcraft-upload-connection-{}", std::process::id()));
        let (mut session, _) = session(&root);
        session
            .execute(
                "cloud.configure",
                &json!({"id":"test","bucket":"photos","region":"us-east-1","prefix":"originals","endpoint":"http://127.0.0.1:9"}),
            )
            .unwrap();
        let library = session.library_token();
        assert!(acknowledgement_with_revision(&mut session, library, Some(0)).recv().unwrap().is_err());
        assert_eq!(session.catalog.upload_intents()["upload-ack-test"].state, UploadState::Published);
        session.close_library().unwrap();
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_old_library_worker_cannot_acknowledge_the_current_catalog() {
        let root = std::env::temp_dir().join(format!("lightcraft-upload-library-{}", std::process::id()));
        let (mut session, _) = session(&root);
        assert!(acknowledgement(&mut session, Arc::new(())).recv().unwrap().is_err());
        assert_eq!(session.catalog.upload_intents()["upload-ack-test"].state, UploadState::Published);
        session.close_library().unwrap();
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}
