//! Native durable staging ownership. Catalog acknowledgement and storage binding
//! are separate proofs; neither a local file nor an ETag alone permits eviction.

use super::s3::S3Connection;
use super::{AssetError, Result};
use lightcraft_catalog::assets::{Asset, AssetId};
use lightcraft_catalog::assets::{Blob, BlobId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Intent {
    pub asset_id: AssetId,
    pub asset: Arc<Asset>,
    pub acknowledged: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Policy {
    pub imports: BTreeMap<String, Intent>,
    pub connections: BTreeMap<String, S3Connection>,
}

pub(crate) struct Proofs {
    pub verified: BTreeMap<String, (AssetId, Asset)>,
    pub protected: BTreeSet<BlobId>,
}

pub(crate) fn blobs(asset: &Asset) -> impl Iterator<Item = &Blob> {
    asset.blobs()
}

pub(crate) fn candidates(root: &Path, policy: &Policy) -> Result<BTreeMap<BlobId, Blob>> {
    let mut proofs = crate::cloud_upload::staging_proofs(root, policy).map_err(|error| AssetError::Disk(error.to_string()))?;
    for intent in policy.imports.values().filter(|intent| !intent.acknowledged) {
        proofs.protected.extend(blobs(&intent.asset).map(|blob| blob.id.clone()));
    }
    crate::cloud_import::visit_staging(root, |manifest| {
        let approved = manifest.prepared
            && proofs.verified.get(&manifest.id).is_some_and(|(asset_id, asset)| asset_id == &manifest.asset_id && asset == &manifest.asset);
        if !approved {
            proofs.protected.extend(blobs(&manifest.asset).map(|blob| blob.id.clone()));
        }
        Ok(())
    })
    .map_err(|error| AssetError::Disk(error.to_string()))?;
    let mut result = BTreeMap::new();
    for (_, asset) in proofs.verified.values() {
        for blob in blobs(asset).filter(|blob| !proofs.protected.contains(&blob.id)) {
            if result.get(&blob.id).is_some_and(|previous: &Blob| previous.size != blob.size) {
                return Err(AssetError::Invalid("shared staged blob has conflicting lengths; preserved".into()));
            }
            result.insert(blob.id.clone(), blob.clone());
        }
    }
    Ok(result)
}
