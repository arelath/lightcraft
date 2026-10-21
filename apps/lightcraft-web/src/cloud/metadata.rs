//! Disposable immutable history bodies in one atomically replaced bounded snapshot. Concurrent
//! cancelled workers can lose cache entries, never grow an unindexed body directory.
use super::{Connection, Provider};
use crate::backend::Backend;
use lightcraft_engine::{assets::sha256, cloud_status::CloudFailure};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::BTreeMap};
const ENTRIES: usize = 128;
const BYTES: usize = 8 << 20;
const BODY: usize = 2 << 20;
const FILE: &str = "cloud/append-metadata-v1.json";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    bytes: Vec<u8>,
    hash: String,
    tick: u64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    destination: String,
    entries: BTreeMap<String, Entry>,
    tick: u64,
}
pub(super) struct Cache {
    backend: Backend,
    destination: String,
    state: RefCell<Option<Index>>,
}
pub(super) fn destination(connection: &Connection) -> String {
    sha256(
        serde_json::json!(["append-metadata-v1", connection.id, connection.prefix, connection.storage_origin, connection.bucket_root])
            .to_string()
            .as_bytes(),
    )
    .0
}
fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
impl Cache {
    pub fn new(backend: Backend, connection: &Connection) -> Self {
        Self { backend, destination: destination(connection), state: RefCell::new(None) }
    }
    async fn index(&self) -> Result<Index, CloudFailure> {
        if let Some(state) = self.state.borrow().as_ref() {
            return Ok(state.clone());
        }
        let index: Index = match self.backend.read_limited(FILE, BYTES as u64).await? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| CloudFailure::local("append metadata cache snapshot is damaged"))?,
            None => Index::default(),
        };
        let index = if index.destination != self.destination { Index { destination: self.destination.clone(), ..Default::default() } } else { index };
        if index.entries.len() > ENTRIES || index.entries.iter().any(|(key, e)| !hash(key) || !hash(&e.hash) || e.bytes.len() > BODY) {
            return Err(CloudFailure::local("append metadata cache exceeds its limits"));
        }
        *self.state.borrow_mut() = Some(index.clone());
        Ok(index)
    }
    pub async fn read(&self, key: &str, max: usize) -> Result<Option<Vec<u8>>, CloudFailure> {
        if self.state.borrow().is_none() {
            self.index().await?;
        }
        let mut state = self.state.borrow_mut();
        let index = state.as_mut().ok_or_else(|| CloudFailure::local("metadata cache unavailable"))?;
        let id = sha256(key.as_bytes()).0;
        let Some(entry) = index.entries.get_mut(&id) else { return Ok(None) };
        if entry.bytes.len() > max || sha256(&entry.bytes).0 != entry.hash {
            return Ok(None);
        }
        index.tick = index.tick.saturating_add(1);
        entry.tick = index.tick;
        let bytes = entry.bytes.clone();
        Ok(Some(bytes))
    }
    pub async fn save(&self, key: &str, bytes: &[u8]) -> Result<(), CloudFailure> {
        if bytes.len() > BODY {
            return Ok(());
        }
        let mut index = self.index().await?;
        let id = sha256(key.as_bytes()).0;
        index.tick = index.tick.saturating_add(1);
        index.entries.insert(id.clone(), Entry { bytes: bytes.to_vec(), hash: sha256(bytes).0, tick: index.tick });
        loop {
            let saved = serde_json::to_vec(&index).map_err(CloudFailure::from)?;
            if index.entries.len() <= ENTRIES && saved.len() <= BYTES {
                super::cache::storage::auxiliary(&self.backend, FILE, &saved).await?;
                *self.state.borrow_mut() = Some(index);
                return Ok(());
            }
            let Some(victim) = index.entries.iter().filter(|(key, _)| *key != &id).min_by_key(|(_, e)| e.tick).map(|(key, _)| key.clone()) else {
                return Ok(());
            };
            index.entries.remove(&victim);
        }
    }
}
impl Provider {
    pub(super) async fn history_record<T>(
        &self,
        key: &str,
        max: usize,
        decode: impl Fn(&[u8]) -> Result<T, CloudFailure>,
    ) -> Result<(T, usize), CloudFailure> {
        self.connection.validate_key(key)?;
        let check = |bytes: &[u8]| {
            lightcraft_engine::cloud_append::immutable_metadata(key, bytes).map_err(CloudFailure::from)?;
            decode(bytes)
        };
        if !self.fresh_history
            && let Some(cache) = &self.metadata
            && let Ok(Some(bytes)) = cache.read(key, max).await
            && let Ok(value) = check(&bytes)
        {
            return Ok((value, bytes.len()));
        }
        let reply = self.request("GET", key, None, None, None, max).await?;
        if reply.status == 404 {
            return Err(CloudFailure::integrity("immutable history record is missing"));
        }
        if reply.status != 200 {
            return Err(CloudFailure::http("immutable history record", reply.status));
        }
        let value = check(&reply.bytes)?;
        if let Some(cache) = &self.metadata {
            let _ = cache.save(key, &reply.bytes).await;
        }
        Ok((value, reply.bytes.len()))
    }
}

impl Provider {
    /// Immutable bytes are reusable only under this complete reference. Callers
    /// still decode the record and validate its scoped schema/causal contents.
    pub(super) async fn catalog_record(&self, reference: &lightcraft_engine::cloud_records::RecordRef, max: usize) -> Result<Vec<u8>, CloudFailure> {
        self.check_cancelled()?;
        self.connection.validate_key(&reference.key)?;
        let check = |bytes: &[u8]| bytes.len() <= max && bytes.len() as u64 == reference.size && sha256(bytes) == reference.hash;
        if !self.fresh_history
            && let Some(cache) = &self.metadata
            && let Ok(Some(bytes)) = cache.read(&reference.key, max).await
            && check(&bytes)
        {
            return Ok(bytes);
        }
        let reply = self.request("GET", &reference.key, None, None, None, max).await?;
        if reply.status == 404 {
            return Err(CloudFailure::integrity("immutable catalog history record is missing"));
        }
        if reply.status != 200 {
            return Err(CloudFailure::http("catalog object", reply.status));
        }
        if !check(&reply.bytes) {
            return Err(CloudFailure::integrity("remote catalog object failed SHA-256/length verification"));
        }
        if let Some(cache) = &self.metadata {
            let _ = cache.save(&reference.key, &reply.bytes).await;
        }
        Ok(reply.bytes)
    }
}
