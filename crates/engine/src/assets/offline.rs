//! Live device-local offline selections. Owners and quality are durable; blob dependencies
//! are derived from the current catalog, never treated as a saved membership snapshot.

use super::cache::{OfflinePin, OfflineQuality};
use lightcraft_catalog::assets::{Blob, BlobId};
use lightcraft_catalog::{AlbumId, Catalog, PhotoId, Source};
use std::collections::{BTreeMap, BTreeSet};

pub struct Dependencies {
    pub pins: BTreeMap<String, OfflinePin>,
    pub blobs: BTreeMap<BlobId, Blob>,
    pub errors: BTreeMap<String, String>,
}

/// Derive each owner's current members and rendition without modifying the catalog. Missing
/// or deleted owners retain empty intent, so undo/restoration can make them useful again.
pub fn resolve(catalog: &Catalog, owners: &BTreeMap<String, OfflinePin>) -> Dependencies {
    let mut result = Dependencies { pins: BTreeMap::new(), blobs: BTreeMap::new(), errors: BTreeMap::new() };
    for (owner, pin) in owners {
        let ids = match owner.split_once(':') {
            Some(("photo", id)) => id.parse::<u64>().ok().map(PhotoId).into_iter().collect(),
            Some(("album", id)) => id.parse::<u64>().ok().map(AlbumId).map(|id| catalog.album_photos(id)).unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut selected = BTreeSet::new();
        for id in ids {
            let Some(photo) = catalog.photo(id).filter(|p| !p.deleted) else { continue };
            let Source::Managed { asset_id } = &photo.source else { continue };
            let blob = catalog.asset(asset_id).and_then(|asset| match pin.quality {
                OfflineQuality::Original => Some(&asset.original),
                OfflineQuality::Preview => asset.smart_preview.as_ref(),
            });
            match blob {
                Some(blob) => {
                    selected.insert(blob.id.clone());
                    result.blobs.insert(blob.id.clone(), blob.clone());
                }
                None => {
                    result.errors.entry(owner.clone()).or_insert_with(|| {
                        format!("{} has no available {:?} rendition; choose original quality or repair the asset", photo.file_name, pin.quality)
                    });
                }
            }
        }
        result.pins.insert(owner.clone(), OfflinePin { quality: pin.quality, blobs: selected.into_iter().collect() });
    }
    result
}
