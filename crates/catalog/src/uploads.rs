//! Device-local upload intent and receipts. These are part of the local journal transaction,
//! but must be excluded from shared catalog checkpoints and mutation publication.

use crate::PhotoId;
use crate::assets::{Asset, AssetId, identifier};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UploadState {
    SavedLocally,
    Uploading,
    Failed,
    ObjectsVerified,
    Published,
    Synchronized,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadIntent {
    pub asset_id: AssetId,
    pub photo_id: PhotoId,
    pub state: UploadState,
    #[serde(default)]
    pub verified: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl UploadIntent {
    pub fn validate(&self, asset: &Asset) -> Result<(), String> {
        if !identifier(&self.asset_id.0) || self.photo_id.0 == 0 || self.photo_id.0 == u64::MAX {
            return Err("invalid upload identity".into());
        }
        asset.validate()?;
        if self.verified.len() > asset.blobs().count() || self.verified.iter().any(|key| !asset.blobs().any(|blob| &blob.remote.key == key)) {
            return Err("upload verification refers to another asset".into());
        }
        if self.error.as_ref().is_some_and(|error| error.len() > 4096 || error.contains('\0')) {
            return Err("invalid upload error".into());
        }
        if matches!(self.state, UploadState::ObjectsVerified | UploadState::Published | UploadState::Synchronized)
            && asset.blobs().any(|blob| !self.verified.contains(&blob.remote.key))
        {
            return Err("upload completion requires verification of every object key".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{Blob, BlobId, RemoteLocation};
    use crate::{Catalog, Journal, MemStore, Op, Photo, Source};

    #[test]
    fn imported_photo_asset_and_intent_replay_atomically_and_survive_compaction() {
        let mut catalog = Catalog::new();
        let asset_id = AssetId("staged".into());
        let asset = Asset {
            associated: Default::default(),
            original: Blob {
                id: BlobId("a".repeat(64)),
                size: 12,
                remote: RemoteLocation { connection: "personal".into(), key: "originals/a".into(), version: None },
            },
            smart_preview: None,
            thumbnail: None,
        };
        let intent = UploadIntent {
            asset_id: asset_id.clone(),
            photo_id: PhotoId(1),
            state: UploadState::SavedLocally,
            verified: Default::default(),
            error: None,
        };
        let op = Op::Batch {
            ops: vec![
                Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) },
                Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), Source::Managed { asset_id }, "stage.dng", "DNG", 4, 3, "")) },
                Op::SetUploadIntent { id: "import-1".into(), intent: Some(Box::new(intent.clone())) },
            ],
        };
        let inverse = catalog.apply(op.clone()).unwrap();
        let store = MemStore::default();
        let (mut journal, _, _) = Journal::open(Box::new(store.clone())).unwrap();
        journal.append(&[op]).unwrap();
        let (_, recovered, _) = Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!(recovered.upload_intents().get("import-1"), Some(&intent));
        assert!(recovered.photo(PhotoId(1)).is_some());
        journal.snapshot(&recovered).unwrap();
        let (_, compacted, _) = Journal::open(Box::new(store)).unwrap();
        assert_eq!(compacted.upload_intents(), recovered.upload_intents());
        catalog.apply(inverse).unwrap();
        assert!(catalog.upload_intents().is_empty() && catalog.assets().is_empty() && catalog.photo(PhotoId(1)).is_none());
    }

    #[test]
    fn invalid_intent_rolls_back_its_whole_import_batch() {
        let mut catalog = Catalog::new();
        let before = catalog.to_snapshot();
        let photo = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "test", "PNG", 4, 3, "");
        let intent = UploadIntent {
            asset_id: AssetId("absent".into()),
            photo_id: PhotoId(1),
            state: UploadState::SavedLocally,
            verified: Default::default(),
            error: None,
        };
        assert!(
            catalog
                .apply(Op::Batch {
                    ops: vec![Op::AddPhoto { photo: Box::new(photo) }, Op::SetUploadIntent { id: "intent".into(), intent: Some(Box::new(intent)) }]
                })
                .is_err()
        );
        // Allocators can advance on a rolled-back attempt; the photo and intent cannot remain.
        assert!(catalog.photo(PhotoId(1)).is_none());
        assert!(catalog.upload_intents().is_empty());
        let mut old: serde_json::Value = serde_json::from_str(&before).unwrap();
        old.as_object_mut().unwrap().remove("upload_intents");
        assert!(Catalog::from_snapshot(&old.to_string()).unwrap().upload_intents().is_empty());
    }
}
