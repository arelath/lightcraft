//! Disposable native immutable-history bodies. A cache hit proves bytes, never authority
//! or causal acceptance. Every caller decodes/certifies again; fresh audits bypass it.
use crate::assets::s3::S3Connection;
use crate::assets::{AssetError, Result, keys};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
};

mod storage;
use storage::State;

const MAX_ENTRIES: usize = 4096;
const MAX_BYTES: u64 = 256 << 20;
const MAX_SCAN: usize = 8192;
const HEADER: u64 = 80;
const MAGIC: &[u8; 8] = b"LCMETA01";

/// Runtime-only: credential rotation and cache placement do not change destination identity.
#[derive(Clone, Default)]
pub(crate) struct Binding {
    cache: Option<Arc<Cache>>,
    fresh: bool,
}
impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MetadataCache(runtime)")
    }
}
impl PartialEq for Binding {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Binding {}

pub(crate) struct Cache {
    root: PathBuf,
    state: Mutex<State>,
    max_entries: usize,
    max_bytes: u64,
}
impl Cache {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root, state: Mutex::default(), max_entries: MAX_ENTRIES, max_bytes: MAX_BYTES }
    }
}

pub(crate) struct Body<T> {
    pub value: T,
    pub size: usize,
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) { Err(AssetError::Cancelled) } else { Ok(()) }
}
fn identity(connection: &S3Connection, key: &str) -> Result<[u8; 32]> {
    connection.validate()?;
    let root = format!("{}/", keys::in_library(connection.prefix.trim_end_matches('/'), "sync-v2")?);
    let relative = key.strip_prefix(&root).ok_or_else(|| AssetError::Invalid("history key is outside its destination".into()))?;
    if !matches!(relative.split('/').nth(1), Some("ops" | "checkpoints" | "receipts")) {
        return Err(AssetError::Invalid("cache requires an immutable history namespace".into()));
    }
    keys::in_library("", key)?;
    let mut hash = Sha256::new();
    hash.update(b"lightcraft-immutable-history-v1\0");
    hash.update(crate::cloud_traffic::destination(connection).as_bytes());
    hash.update((key.len() as u64).to_le_bytes());
    hash.update(key.as_bytes());
    Ok(hash.finalize().into())
}

impl S3Connection {
    pub(crate) fn with_metadata_cache(mut self, cache: Option<Arc<Cache>>) -> Self {
        self.metadata = Binding { cache, fresh: false };
        self
    }
    /// Carries fresh-byte provenance through nested archive-proof reads as well.
    pub(crate) fn fresh_metadata(&self) -> Self {
        let mut connection = self.clone();
        connection.metadata.fresh = true;
        connection
    }
    /// The content-addressed key, body SHA-256/length and caller's schema checks
    /// must all match. Semantic/causal checks remain the owning planner's job.
    pub(crate) fn history_record<T>(&self, key: &str, max: usize, cancel: &AtomicBool, decode: impl Fn(&[u8]) -> Result<T>) -> Result<Body<T>> {
        cancelled(cancel)?;
        if max > crate::cloud_append::MAX_OBJECT {
            return Err(AssetError::Invalid("history body exceeds 64 MiB".into()));
        }
        let id = identity(self, key)?;
        let checked = |bytes: &[u8]| {
            crate::cloud_append::immutable_metadata(key, bytes)?;
            decode(bytes)
        };
        if !self.metadata.fresh
            && let Some(cache) = &self.metadata.cache
        {
            match cache.load(&id, max, cancel) {
                Ok(Some(bytes)) => match checked(&bytes) {
                    Ok(value) => {
                        cancelled(cancel)?;
                        crate::cloud_traffic::reuse(false);
                        return Ok(Body { value, size: bytes.len() });
                    }
                    Err(_) => {
                        if let Err(AssetError::Cancelled) = cache.forget(&id, cancel) {
                            return Err(AssetError::Cancelled);
                        }
                    }
                },
                Err(AssetError::Cancelled) => return Err(AssetError::Cancelled),
                // A disposable cache never prevents a fresh remote read.
                Ok(None) | Err(_) => {}
            }
        }
        let reply = self.get_small(key, max, cancel)?.ok_or(AssetError::Missing)?;
        let value = checked(&reply.bytes)?;
        cancelled(cancel)?;
        if let Some(cache) = &self.metadata.cache
            && let Err(AssetError::Cancelled) = cache.save(&id, &reply.bytes, cancel)
        {
            return Err(AssetError::Cancelled);
        }
        Ok(Body { value, size: reply.bytes.len() })
    }
}

#[cfg(test)]
mod tests;
