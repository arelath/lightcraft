//! Durable identities and device-local external-edit bindings for managed photos.
use crate::assets::{AssetId, BlobId, identifier};
use serde::{Deserialize, Serialize};

pub fn entity_key(value: &str) -> bool {
    if value == "library" {
        return true;
    }
    let Some((kind, local)) = value.split_once(':') else { return false };
    matches!(kind, "photo" | "album" | "stack") && local.parse::<u64>().is_ok_and(|id| id > 0 && id < u64::MAX)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkingCopy {
    pub connection: String,
    pub path: String,
    pub asset_id: AssetId,
    /// Last file bytes ingested durably into the catalog, not a remote acknowledgement.
    pub ingested: BlobId,
}
impl WorkingCopy {
    pub fn validate(&self) -> Result<(), String> {
        if !identifier(&self.connection)
            || !identifier(&self.asset_id.0)
            || self.path.is_empty()
            || self.path.len() > 32_768
            || self.path.contains('\0')
            || self.ingested.0.len() != 64
            || !self.ingested.0.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err("invalid managed working-copy binding".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{Asset, Blob, RemoteLocation};
    use crate::{Catalog, Journal, MemStore, Op};

    #[test]
    fn identities_and_working_bindings_survive_replay_and_compaction_and_reject_retargeting() {
        let mut catalog = Catalog::new();
        let blob = Blob {
            id: BlobId("a".repeat(64)),
            size: 12,
            remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
        };
        let asset_id = AssetId("first".into());
        let working = WorkingCopy {
            connection: "personal".into(),
            path: "Managed/working/edit.tif".into(),
            asset_id: asset_id.clone(),
            ingested: blob.id.clone(),
        };
        let op = Op::Batch {
            ops: vec![
                Op::SetAsset {
                    id: asset_id,
                    asset: Some(Box::new(Asset { original: blob, smart_preview: None, thumbnail: None, associated: Default::default() })),
                },
                Op::SetCloudIdentity { entity: "photo:1".into(), id: Some("global-photo".into()) },
                Op::SetWorkingCopy { id: crate::PhotoId(1), working: Some(Box::new(working.clone())) },
            ],
        };
        catalog.apply(op.clone()).unwrap();
        let before = catalog.to_snapshot();
        for entity in ["photo:0", "photo:18446744073709551615", "unknown:1", "photo:2"] {
            assert!(catalog.apply(Op::SetCloudIdentity { entity: entity.into(), id: Some("global-photo".into()) }).is_err());
            assert_eq!(catalog.to_snapshot(), before);
        }
        assert!(catalog.apply(Op::SetCloudIdentity { entity: "photo:1".into(), id: Some("replacement".into()) }).is_err());
        let mut bad = working.clone();
        bad.ingested = BlobId("b".repeat(64));
        assert!(catalog.apply(Op::SetWorkingCopy { id: crate::PhotoId(1), working: Some(Box::new(bad)) }).is_err());
        assert_eq!(catalog.to_snapshot(), before);
        let store = MemStore::default();
        let (mut journal, _, _) = Journal::open(Box::new(store.clone())).unwrap();
        journal.append(&[op]).unwrap();
        let (_, loaded, _) = Journal::open(Box::new(store.clone())).unwrap();
        assert_eq!(loaded.cloud_identity("photo:1"), Some("global-photo"));
        assert_eq!(loaded.working_copy(crate::PhotoId(1)), Some(&working));
        journal.snapshot(&loaded).unwrap();
        let (_, compacted, _) = Journal::open(Box::new(store)).unwrap();
        assert_eq!(compacted.to_snapshot(), loaded.to_snapshot());
    }

    #[test]
    fn associated_documents_migrate_from_legacy_assets_and_reject_other_connections() {
        let blob = Blob {
            id: BlobId("a".repeat(64)),
            size: 12,
            remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
        };
        let mut asset: Asset = serde_json::from_value(serde_json::json!({"original":blob})).unwrap();
        assert!(asset.associated.is_empty());
        asset.associated.insert("xmp".into(), blob.clone());
        assert!(asset.validate().is_ok());
        let mut other = blob;
        other.remote.connection = "another".into();
        asset.associated.insert("xmp".into(), other);
        assert!(asset.validate().is_err());
    }

    #[test]
    fn exhausted_album_and_stack_ids_return_errors_instead_of_overflowing() {
        let mut value = serde_json::to_value(Catalog::new()).unwrap();
        value["next_album"] = serde_json::json!(u64::MAX);
        value["next_stack"] = serde_json::json!(u64::MAX);
        let mut catalog: Catalog = serde_json::from_value(value).unwrap();
        let album = catalog.alloc_album_id();
        let stack = catalog.alloc_stack_id();
        assert_eq!(album.0, u64::MAX);
        assert_eq!(stack.0, u64::MAX);
        assert!(catalog.apply(Op::AddAlbum { album: crate::Album::new(album, "overflow") }).is_err());
        for id in [1, 2] {
            catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(crate::Photo::new(
                        crate::PhotoId(id),
                        crate::Source::File { path: format!("{id}.png") },
                        "photo.png",
                        "PNG",
                        2,
                        2,
                        "",
                    )),
                })
                .unwrap();
        }
        assert!(
            catalog
                .apply(Op::AddStack { stack: crate::Stack { id: stack, photos: vec![crate::PhotoId(1), crate::PhotoId(2)], collapsed: false } })
                .is_err()
        );
        assert!(catalog.albums().next().is_none() && catalog.stacks().next().is_none());
    }
}
