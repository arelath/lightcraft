//! Device-local upload intent and receipts. These are part of the local journal transaction,
//! but must be excluded from shared catalog checkpoints and mutation publication.

use crate::PhotoId;
use crate::assets::{Asset, AssetId, identifier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Exact verified publication evidence awaiting its device-local receipt write.
/// This queue is journaled with the shared ACK and never enters portable state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadFinalization {
    pub version: u32,
    pub protocol: crate::replication::SyncProtocol,
    pub library: String,
    pub generation: Option<u64>,
    pub proofs: BTreeMap<String, String>,
}

impl UploadFinalization {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !identifier(&self.library)
            || self.proofs.is_empty()
            || self.proofs.len() > crate::replication::MAX_PENDING
            || self.proofs.iter().any(|(id, digest)| !identifier(id) || digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            || (self.protocol.is_head() && self.generation.is_none_or(|generation| generation == 0))
            || (!self.protocol.is_head() && self.generation.is_some())
        {
            return Err("invalid upload receipt-finalization proof; staged data retained".into());
        }
        Ok(())
    }
}

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

/// Preparation failed before a media attempt. This never revokes a verified ACK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadBlock {
    pub reason: String,
    pub verify: bool,
}
impl UploadBlock {
    pub fn new(reason: &str, verify: bool) -> Self {
        let mut reason = reason.replace('\0', "�");
        if reason.len() > 4096 {
            let mut end = 4096;
            while !reason.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            reason.truncate(end);
        }
        if reason.is_empty() {
            reason = "upload preparation failed; restore the import's staging and connection settings, then retry".into();
        }
        Self { reason, verify }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalization: Option<UploadFinalization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<UploadBlock>,
}

impl UploadIntent {
    pub fn validate(&self, asset: &Asset) -> Result<(), String> {
        if !identifier(&self.asset_id.0) || self.photo_id.0 == 0 || self.photo_id.0 == u64::MAX {
            return Err("invalid upload identity".into());
        }
        asset.validate()?;
        if self.blocked.as_ref().is_some_and(|block| block.reason.is_empty() || block.reason.len() > 4096 || block.reason.contains('\0')) {
            return Err("invalid upload preparation error".into());
        }
        if let Some(proof) = &self.finalization {
            proof.validate()?;
            if self.state != UploadState::Synchronized {
                return Err("receipt finalization requires a durable synchronization acknowledgement".into());
            }
        }
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
            finalization: None,
            blocked: None,
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
        for protocol in [crate::replication::SyncProtocol::ConditionalHead, crate::replication::SyncProtocol::AppendLog] {
            let mut complete = intent.clone();
            complete.state = UploadState::Synchronized;
            complete.verified.insert("originals/a".into());
            complete.finalization = Some(UploadFinalization {
                version: 1,
                protocol,
                library: "library".into(),
                generation: protocol.is_head().then_some(7),
                proofs: BTreeMap::from([("device-1".into(), "b".repeat(64))]),
            });
            let op = Op::SetUploadIntent { id: "import-1".into(), intent: Some(Box::new(complete.clone())) };
            catalog.apply(op.clone()).unwrap();
            let files = MemStore::default();
            let (mut durable, _, _) = Journal::open(Box::new(files.clone())).unwrap();
            durable.snapshot(&catalog).unwrap();
            durable.append(&[op]).unwrap();
            let (_, loaded, _) = Journal::open(Box::new(files.clone())).unwrap();
            assert_eq!(loaded.upload_intents()["import-1"], complete);
            durable.snapshot(&loaded).unwrap();
            assert_eq!(Journal::open(Box::new(files)).unwrap().1.upload_intents()["import-1"], complete);
            for invalid in 0..5 {
                let mut bad = complete.clone();
                let proof = bad.finalization.as_mut().unwrap();
                match invalid {
                    0 => proof.version = 2,
                    1 => proof.generation = if protocol.is_head() { None } else { Some(7) },
                    2 => proof.proofs.clear(),
                    3 => {
                        proof.proofs.insert("device-1".into(), "wrong".into());
                    }
                    _ => bad.state = UploadState::Failed,
                }
                assert!(catalog.apply(Op::SetUploadIntent { id: "import-1".into(), intent: Some(Box::new(bad)) }).is_err());
                assert_eq!(catalog.upload_intents()["import-1"], complete);
            }
        }
        catalog.apply(inverse).unwrap();
        assert!(catalog.upload_intents().is_empty() && catalog.assets().is_empty() && catalog.photo(PhotoId(1)).is_none());
    }

    #[test]
    fn blocked_reasons_are_bounded_and_legacy_intents_default_to_unblocked() {
        let reason = format!("{}\0", "あ".repeat(2000));
        let block = UploadBlock::new(&reason, true);
        assert!(block.reason.len() <= 4096 && !block.reason.contains('\0'));
        assert!(!UploadBlock::new("", false).reason.is_empty());
        let intent: UploadIntent = serde_json::from_value(serde_json::json!({
            "assetId":"staged","photoId":1,"state":"savedLocally","verified":[]
        }))
        .unwrap();
        assert!(intent.blocked.is_none());
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
            finalization: None,
            blocked: None,
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
