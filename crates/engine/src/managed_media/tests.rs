use super::*;

fn prepared(name: &str, digest: char) -> PreparedMedia {
    let asset_id = AssetId(name.into());
    let blob = Blob {
        id: lightcraft_catalog::assets::BlobId(digest.to_string().repeat(64)),
        size: 7,
        remote: RemoteLocation { connection: "personal".into(), key: format!("library/originals/{name}"), version: None },
    };
    let mut photo = Photo::new(PhotoId(1), Source::Managed { asset_id: asset_id.clone() }, "example.png", "png", 4, 4, "now");
    photo.file_size = 7;
    PreparedMedia {
        asset_id,
        asset: Asset { original: blob.clone(), smart_preview: Some(blob.clone()), thumbnail: Some(blob), associated: Default::default() },
        file_name: "example.png".into(),
        photo: Some(Box::new(photo)),
        prepared: true,
        target: None,
        working: None,
        stack_with: None,
        origin_asset: None,
    }
}

#[test]
fn media_plans_commit_local_revisions_without_cloud_enrollment_policy() {
    let mut catalog = Catalog::new();
    let first = prepared("initial", 'a');
    let initial_asset = first.asset_id.clone();
    let initial = plan(&mut catalog, None, first).unwrap();
    assert!(!initial.ops.iter().any(|op| matches!(op, Op::SetCloudIdentity { .. } | Op::SetUploadIntent { .. } | Op::SetSyncState { .. })));
    let id = PhotoId(*initial.ids.first().unwrap());
    catalog.apply(Op::Batch { ops: initial.ops }).unwrap();
    catalog.apply(Op::SetRating { id, rating: 4 }).unwrap();
    let mut copy = (**catalog.photo(id).unwrap()).clone();
    copy.id = catalog.alloc_photo_id();
    copy.copy_of = Some(id);
    let copy_id = copy.id;
    catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    let mut next = prepared("revised", 'b');
    next.photo.as_mut().unwrap().width = 8;
    next.target = Some(Revision { photo_id: id, base_asset: initial_asset });
    let revised = plan(&mut catalog, None, next).unwrap();
    assert_eq!(revised.ids, vec![id.0]);
    assert!(!revised.ops.iter().any(|op| matches!(op, Op::SetCloudIdentity { .. } | Op::SetUploadIntent { .. } | Op::SetSyncState { .. })));
    catalog.apply(Op::Batch { ops: revised.ops }).unwrap();
    for photo_id in [id, copy_id] {
        let photo = catalog.photo(photo_id).unwrap();
        assert_eq!(photo.width, 8);
        assert_eq!(photo.rating, 4);
        assert_eq!(photo.source, Source::Managed { asset_id: AssetId("revised".into()) });
        assert!(catalog.cloud_identity(&format!("photo:{}", photo_id.0)).is_none());
    }
    assert_eq!(catalog.photo(copy_id).unwrap().copy_of, Some(id));
    assert!(catalog.sync_state().is_none());
    assert!(catalog.upload_intents().is_empty());
}

#[test]
fn invalid_or_stale_media_plans_preserve_the_owner_catalog() {
    let mut catalog = Catalog::new();
    let mut media = prepared("initial", 'a');
    media.working = Some("../outside".into());
    assert!(plan(&mut catalog, None, media).is_err());
    assert!(catalog.is_empty());
    assert!(catalog.asset(&AssetId("initial".into())).is_none());
    assert!(catalog.asset(&AssetId("revised".into())).is_none());
    let mut revision = prepared("revised", 'b');
    revision.target = Some(Revision { photo_id: PhotoId(55), base_asset: AssetId("missing".into()) });
    assert!(plan(&mut catalog, None, revision).is_err());
    assert!(catalog.is_empty());
    assert!(catalog.asset(&AssetId("initial".into())).is_none());
    assert!(catalog.asset(&AssetId("revised".into())).is_none());
}
