//! Per-operation authority observations and verified immutable metadata reuse.
use super::*;
use crate::assets::s3::b2::Observed;
use crate::cloud_records::RecordRef;
use std::collections::BTreeMap;

const OPERATION_BYTES: usize = 8 << 20;
const OPERATION_ENTRIES: usize = 32;
#[derive(Default)]
pub(crate) struct Operation {
    pub(crate) observed: Mutex<Option<Observed>>,
    pub(crate) legacy_pending: Mutex<Option<(RecordRef, bool)>>,
    bodies: Mutex<BTreeMap<[u8; 32], Arc<Vec<u8>>>>,
}
pub(crate) struct AuthorityWrite(Option<Arc<Operation>>);
impl Drop for AuthorityWrite {
    fn drop(&mut self) {
        if let Some(operation) = &self.0 {
            *operation.observed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }
}
impl S3Connection {
    pub(crate) fn authority_write(&self) -> AuthorityWrite {
        AuthorityWrite(self.metadata.operation.clone())
    }
    /// A new observation scope for exactly one conditional synchronization call.
    pub(crate) fn authority_operation(&self) -> Self {
        let mut connection = self.clone();
        connection.metadata.operation = Some(Arc::new(Operation::default()));
        connection
    }
    pub(crate) fn authority_context(&self) -> Self {
        if self.metadata.operation.is_some() { self.clone() } else { self.authority_operation() }
    }
    fn metadata_identity(&self, reference: &RecordRef) -> Result<[u8; 32]> {
        self.validate()?;
        keys::in_library("", &reference.key)?;
        let mut hash = Sha256::new();
        hash.update(b"lightcraft-bound-immutable-metadata-v1\0");
        let destination = crate::cloud_traffic::destination(self);
        for value in [destination.as_str(), reference.key.as_str(), reference.hash.0.as_str()] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        hash.update(reference.size.to_le_bytes());
        Ok(hash.finalize().into())
    }
    fn operation_remember(&self, id: [u8; 32], bytes: &[u8]) {
        let Some(operation) = &self.metadata.operation else { return };
        let mut bodies = operation.bodies.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let used = bodies.values().fold(0usize, |sum, body| sum.saturating_add(body.len()));
        if bodies.len() < OPERATION_ENTRIES && used.saturating_add(bytes.len()) <= OPERATION_BYTES {
            bodies.insert(id, Arc::new(bytes.to_vec()));
        }
    }
    /// Only after the owning uploader has read back these exact remote bytes.
    /// This proves an immutable body, never publication of its authority pointer.
    pub(crate) fn remember_verified_metadata(&self, reference: &RecordRef, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
        cancelled(cancel)?;
        if bytes.len() > 64 << 20 || bytes.len() as u64 != reference.size || crate::assets::sha256(bytes) != reference.hash {
            return Err(AssetError::Integrity);
        }
        let id = self.metadata_identity(reference)?;
        self.operation_remember(id, bytes);
        if let Some(cache) = &self.metadata.cache
            && let Err(AssetError::Cancelled) = cache.save(&id, bytes, cancel)
        {
            return Err(AssetError::Cancelled);
        }
        Ok(())
    }
    /// Caller validates the owning namespace/schema. Hash/length and the complete
    /// destination/reference bind both disk and operation-local reuse. Absence is
    /// never cached. Mutable authority must never pass through this API.
    pub(crate) fn metadata_record<T>(
        &self,
        reference: &RecordRef,
        max: usize,
        cancel: &AtomicBool,
        decode: impl Fn(&[u8]) -> Result<T>,
    ) -> Result<Option<Body<T>>> {
        cancelled(cancel)?;
        if max > 64 << 20 || reference.size == 0 || reference.size > max as u64 {
            return Err(AssetError::Integrity);
        }
        let id = self.metadata_identity(reference)?;
        let checked = |bytes: &[u8]| {
            // Preserve the owning decoder's newer-format diagnosis on failures.
            // No value is returned or stored without the exact content proof.
            let value = decode(bytes)?;
            if bytes.len() as u64 != reference.size || crate::assets::sha256(bytes) != reference.hash {
                return Err(AssetError::Integrity);
            }
            Ok(value)
        };
        if let Some(operation) = &self.metadata.operation {
            let bytes = operation.bodies.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&id).cloned();
            if let Some(bytes) = bytes {
                let value = checked(&bytes)?;
                cancelled(cancel)?;
                crate::cloud_traffic::reuse(true);
                return Ok(Some(Body { value, size: bytes.len() }));
            }
        }
        if !self.metadata.fresh
            && let Some(cache) = &self.metadata.cache
        {
            match cache.load(&id, max, cancel) {
                Ok(Some(bytes)) => match checked(&bytes) {
                    Ok(value) => {
                        cancelled(cancel)?;
                        self.operation_remember(id, &bytes);
                        crate::cloud_traffic::reuse(false);
                        return Ok(Some(Body { value, size: bytes.len() }));
                    }
                    Err(_) => {
                        if let Err(AssetError::Cancelled) = cache.forget(&id, cancel) {
                            return Err(AssetError::Cancelled);
                        }
                    }
                },
                Err(AssetError::Cancelled) => return Err(AssetError::Cancelled),
                Ok(None) | Err(_) => {}
            }
        }
        // Known lengths avoid reserving 64 MiB for a tiny intake in a bounded
        // verification slice. Small format markers retain diagnostic headroom.
        let limit = usize::try_from(reference.size).map_err(|_| AssetError::Integrity)?.max(16 << 10).min(max);
        let Some(reply) = self.get_small(&reference.key, limit, cancel)? else { return Ok(None) };
        let value = checked(&reply.bytes)?;
        cancelled(cancel)?;
        self.operation_remember(id, &reply.bytes);
        if let Some(cache) = &self.metadata.cache
            && let Err(AssetError::Cancelled) = cache.save(&id, &reply.bytes, cancel)
        {
            return Err(AssetError::Cancelled);
        }
        Ok(Some(Body { value, size: reply.bytes.len() }))
    }
}
