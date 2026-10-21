//! Disposable byte-verification receipts; only an exact provider version can reuse them.
use super::*;
use lightcraft_catalog::assets::{Blob, BlobId};
use serde::{Deserialize, Serialize};

const MAX_RECEIPT: usize = 16 << 10;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MediaVerification {
    StreamedSha256,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    version: u32,
    destination: String,
    key: String,
    hash: BlobId,
    size: u64,
    requested_version: Option<String>,
    actual_version: String,
    method: MediaVerification,
    verified: bool,
}
pub(crate) fn immutable_version(version: &str) -> bool {
    !version.is_empty() && version.len() <= 1024 && !version.eq_ignore_ascii_case("null") && !version.chars().any(char::is_control)
}
impl S3Connection {
    /// These providers define immutable object version IDs. Unknown compatible
    /// endpoints keep streamed verification until their guarantees are supported.
    pub(crate) fn version_receipts_supported(&self) -> bool {
        self.endpoint.is_none() || self.endpoint.as_deref().and_then(crate::cloud_setup::b2_runtime_endpoint).is_some()
    }
    fn media_receipt_identity(&self, blob: &Blob) -> Result<[u8; 32]> {
        self.validate_blob(blob)?;
        let binding = serde_json::to_vec(&(crate::cloud_traffic::destination(self), &blob.remote.key, &blob.id, blob.size, &blob.remote.version))
            .map_err(|_| AssetError::Integrity)?;
        let mut hash = Sha256::new();
        hash.update(b"lightcraft-version-bound-media-proof-v1\0");
        hash.update(&binding);
        Ok(hash.finalize().into())
    }
    pub(crate) fn media_receipt(&self, blob: &Blob, cancel: &AtomicBool) -> Result<Option<String>> {
        cancelled(cancel)?;
        if self.fresh_history() || !self.version_receipts_supported() {
            return Ok(None);
        }
        let Some(cache) = &self.metadata.cache else { return Ok(None) };
        let id = self.media_receipt_identity(blob)?;
        let bytes = match cache.load(&id, MAX_RECEIPT, cancel) {
            Ok(Some(bytes)) => bytes,
            Err(AssetError::Cancelled) => return Err(AssetError::Cancelled),
            Ok(None) | Err(_) => return Ok(None),
        };
        let checked = || -> Result<String> {
            let proof: Receipt = serde_json::from_slice(&bytes).map_err(|_| AssetError::Integrity)?;
            if proof.version != 1
                || !proof.verified
                || proof.destination != crate::cloud_traffic::destination(self)
                || proof.key != blob.remote.key
                || proof.hash != blob.id
                || proof.size != blob.size
                || proof.requested_version != blob.remote.version
                || !immutable_version(&proof.actual_version)
                || blob.remote.version.as_ref().is_some_and(|version| version != &proof.actual_version)
            {
                return Err(AssetError::Integrity);
            }
            match proof.method {
                MediaVerification::StreamedSha256 => {}
            }
            Ok(proof.actual_version)
        };
        match checked() {
            Ok(version) => {
                cancelled(cancel)?;
                Ok(Some(version))
            }
            Err(_) => {
                if let Err(AssetError::Cancelled) = cache.forget(&id, cancel) {
                    return Err(AssetError::Cancelled);
                }
                Ok(None)
            }
        }
    }
    /// The caller must have verified the complete expected bytes with this method.
    /// Upload response headers or persisted verified-key markers do not qualify.
    pub(crate) fn remember_media_receipt(
        &self,
        blob: &Blob,
        actual_version: Option<&str>,
        method: MediaVerification,
        cancel: &AtomicBool,
    ) -> Result<()> {
        cancelled(cancel)?;
        if !self.version_receipts_supported() {
            return Ok(());
        }
        let (Some(cache), Some(version)) = (&self.metadata.cache, actual_version.filter(|version| immutable_version(version))) else { return Ok(()) };
        if blob.remote.version.as_ref().is_some_and(|requested| requested != version) {
            return Ok(());
        }
        let id = self.media_receipt_identity(blob)?;
        let proof = Receipt {
            version: 1,
            destination: crate::cloud_traffic::destination(self),
            key: blob.remote.key.clone(),
            hash: blob.id.clone(),
            size: blob.size,
            requested_version: blob.remote.version.clone(),
            actual_version: version.to_owned(),
            method,
            verified: true,
        };
        let Ok(bytes) = serde_json::to_vec(&proof) else { return Ok(()) };
        if bytes.len() <= MAX_RECEIPT
            && let Err(AssetError::Cancelled) = cache.save(&id, &bytes, cancel)
        {
            return Err(AssetError::Cancelled);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
