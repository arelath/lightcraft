use super::*;
use lightcraft_catalog::assets::{Blob, BlobId, RemoteLocation};
use lightcraft_catalog::{AlbumId, Filter, Match, Rule, RuleSet};
use serde_json::json;

fn fixture() -> (Catalog, Photo, AssetId, Asset) {
    let asset_id = AssetId("asset".into());
    let original = Blob {
        id: BlobId("a".repeat(64)),
        size: 12,
        remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: Some("v1".into()) },
    };
    let asset = Asset { original, smart_preview: None, thumbnail: None, associated: BTreeMap::new() };
    let mut catalog = Catalog::new();
    catalog.apply(Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset.clone())) }).unwrap();
    let photo = Photo::new(PhotoId(7), Source::Managed { asset_id: asset_id.clone() }, "photo.raw", "RAW", 32, 24, "today");
    catalog.apply(Op::AddPhoto { photo: Box::new(photo.clone()) }).unwrap();
    catalog.apply(Op::SetCloudIdentity { entity: "photo:7".into(), id: Some("shared-photo".into()) }).unwrap();
    catalog.apply(Op::SetCloudIdentity { entity: "album:4".into(), id: Some("shared-album".into()) }).unwrap();
    (catalog, photo, asset_id, asset)
}

// Frozen pre-CR12 encoding, used only as a compatibility oracle. Production must
// select its schema explicitly rather than serializing a catalog object.
fn legacy_photo(photo: &Photo, asset_id: &AssetId, asset: &Asset) -> BTreeMap<String, Value> {
    let mut value = serde_json::to_value(photo).unwrap();
    let map = value.as_object_mut().unwrap();
    for key in ["id", "source", "local", "local_baseline"] {
        map.remove(key);
    }
    let mut content = serde_json::Map::from_iter([("assetId".into(), json!(asset_id)), ("asset".into(), json!(asset.rebound("library").unwrap()))]);
    for key in ["format", "kind", "width", "height", "file_size", "content_hash", "preview_only", "embedded_lens", "as_shot_wb", "duration"] {
        if let Some(value) = map.remove(key) {
            content.insert(key.into(), value);
        }
    }
    map.insert("content".into(), Value::Object(content));
    if photo.copy_of.is_some() {
        map.insert("copy_of".into(), Value::Null);
    }
    let mut meta = map.remove("meta").unwrap().as_object().unwrap().clone();
    for (key, value) in [("copyright_status", json!("unknown")), ("usage_terms", json!("")), ("copyright_url", json!("")), ("regions", json!([]))] {
        meta.entry(key).or_insert(value);
    }
    let mut fields: BTreeMap<_, _> = map.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
    for (key, value) in meta {
        fields.insert(format!("meta.{key}"), value);
    }
    fields
}

#[test]
fn explicit_photo_schema_keeps_the_current_wire_and_excludes_private_fields() {
    let (catalog, mut photo, id, asset) = fixture();
    for enriched in [false, true] {
        if enriched {
            photo.local = true;
            photo.local_baseline = Some(123);
            photo.source = Source::File { path: "private/device/original.raw".into() };
            photo.copy_of = Some(PhotoId(999));
            photo.copy_name = Some("Independent recipe".into());
            photo.import_look = Some(photo.develop.clone());
            photo.content_hash = Some("b".repeat(64));
            photo.preview_only = Some("Unsupported raw variant".into());
            photo.as_shot_wb = Some((5200.0, 4.0));
            photo.duration = Some(2.0);
            photo.meta.title = "Portable title".into();
            photo.meta.usage_terms = "Attribution".into();
        }
        let fields = PortablePhoto::from_local(&photo, &id, &asset, &catalog, &catalog, "personal").unwrap().fields().unwrap();
        assert_eq!(fields, legacy_photo(&photo, &id, &asset));
        assert_eq!(serde_json::to_vec(&fields).unwrap(), serde_json::to_vec(&legacy_photo(&photo, &id, &asset)).unwrap());
        for private in ["id", "source", "local", "local_baseline"] {
            assert!(!fields.contains_key(private));
        }
        assert!(!serde_json::to_string(&fields).unwrap().contains("private/device"));
        for default in ["copyright_status", "usage_terms", "copyright_url", "regions"] {
            assert!(fields.contains_key(&format!("meta.{default}")));
        }
    }
}

#[test]
fn sparse_content_preserves_legacy_fallbacks_but_rejects_malformed_or_local_fields() {
    let (_, mut photo, id, asset) = fixture();
    photo.file_size = 99;
    photo.kind = lightcraft_catalog::MediaKind::Raw;
    photo.local = true;
    photo.local_baseline = Some(42);
    photo.content_hash = Some("obsolete".into());
    photo.as_shot_wb = Some((5000.0, 0.0));
    let value = json!({"assetId":id,"asset":asset,"format":"DNG","width":16,"height":12});
    let content: Content = decode(value.clone()).unwrap();
    content.apply(&mut photo);
    assert_eq!((photo.width, photo.height, photo.file_size), (16, 12, 99));
    assert_eq!(photo.kind, lightcraft_catalog::MediaKind::Raw);
    assert!(!photo.local);
    assert_eq!(photo.local_baseline, None);
    assert_eq!(photo.content_hash, None);
    assert_eq!(photo.as_shot_wb, None);
    for field in ["path", "source", "local", "cache", "futureField"] {
        let mut malformed = value.clone();
        malformed[field] = json!("private");
        assert!(decode::<Content>(malformed).is_err(), "{field}");
    }
    for field in ["format", "width", "height", "kind", "file_size"] {
        let mut malformed = value.clone();
        malformed[field] = Value::Null;
        assert!(decode::<Content>(malformed).is_err(), "{field}");
    }
}

#[test]
fn typed_album_stack_and_smart_references_keep_the_wire_shape() {
    let (mut catalog, _, _, _) = fixture();
    let target = AlbumId(4);
    catalog.apply(Op::AddAlbum { album: Album::new(target, "Target") }).unwrap();
    let filter = Filter {
        only: vec![PhotoId(7)],
        album: Some(target),
        rule_set: Some(RuleSet {
            mode: Match::None,
            rules: vec![Rule::Group {
                group: RuleSet { mode: Match::Any, rules: vec![Rule::Field { field: "album".into(), op: "isNot".into(), value: json!(4) }] },
            }],
        }),
        ..Default::default()
    };
    let portable = PortableFilter::from_local(&filter, &catalog, &catalog, "personal").unwrap();
    let mut expected = json!(filter);
    let map = expected.as_object_mut().unwrap();
    map.remove("folder");
    map.remove("libraryFolder");
    expected["only"] = json!(["shared-photo"]);
    expected["onlyRestricted"] = json!(true);
    expected["album"] = json!("shared-album");
    expected["ruleSet"]["rules"][0]["group"]["rules"][0]["value"] = json!("shared-album");
    assert_eq!(json!(portable), expected);
    let mut ops = Vec::new();
    let restored = decode::<PortableFilter>(expected).unwrap().to_local(&mut catalog, &mut ops).unwrap();
    assert_eq!(restored, filter);
    let mut album = Album::new(AlbumId(5), "Shared");
    album.parent = Some(target);
    album.cover = Some(PhotoId(7));
    album.smart = Some(Box::new(filter));
    album.quick = true;
    album.order = Some(2);
    let document = PortableAlbum::from_local(&album, &catalog, &catalog, "personal").unwrap();
    let mut expected = json!(album);
    expected.as_object_mut().unwrap().remove("id");
    expected.as_object_mut().unwrap().remove("photos");
    expected["parent"] = json!("shared-album");
    expected["cover"] = json!("shared-photo");
    expected["smart"] = json!(portable);
    expected["deleted"] = json!(false);
    assert_eq!(json!(document), expected);
    let plain = Album::new(AlbumId(6), "Plain");
    assert_eq!(
        json!(PortableAlbum::from_local(&plain, &catalog, &catalog, "personal").unwrap()),
        json!({"name":"Plain","parent":null,"folder":false,"cover":null,"deleted":false})
    );
    assert_eq!(
        json!(PortableStack { photos: vec![PhotoRef::try_from("shared-photo".to_owned()).unwrap()], collapsed: true, deleted: false }),
        json!({"photos":["shared-photo"],"collapsed":true,"deleted":false})
    );
}

#[test]
fn unresolved_references_remain_restricted_and_only_schema_slots_are_converted() {
    let (mut catalog, _, _, _) = fixture();
    let wire = json!({"only":["missing-photo"],"album":"deleted-album","ruleSet":{"match":"none","rules":[
        {"field":"album","op":"isNot","value":"deleted-album"},
        {"field":"title","op":"contains","value":{"album":"literal-value","only":["literal-selection"]}}
    ]}});
    let portable: PortableFilter = decode(wire).unwrap();
    let mut ops = Vec::new();
    let local = portable.to_local(&mut catalog, &mut ops).unwrap();
    assert!(local.only.is_empty() && local.only_restricted);
    let album = local.album.unwrap();
    assert!(catalog.album(album).is_none());
    assert_eq!(catalog.cloud_identity(&format!("album:{}", album.0)), Some("deleted-album"));
    let rules = local.rule_set.unwrap();
    assert_eq!(rules.rules[0], Rule::Field { field: "album".into(), op: "isNot".into(), value: json!(album) });
    assert_eq!(
        rules.rules[1],
        Rule::Field { field: "title".into(), op: "contains".into(), value: json!({"album":"literal-value","only":["literal-selection"]}) }
    );
    assert!(decode::<PhotoRef>(json!(7)).is_err());
    assert!(decode::<AlbumRef>(json!("../invalid")).is_err());
    for field in ["folder", "libraryFolder", "syncStatus", "futurePrivateField"] {
        let value = Value::Object(serde_json::Map::from_iter([(field.into(), json!("private"))]));
        assert!(decode::<PortableFilter>(value).is_err());
    }
    let mut nested = json!({"rules":[]});
    for _ in 0..66 {
        nested = json!({"rules":[{"group":nested}]});
    }
    assert!(decode::<PortableFilter>(json!({"ruleSet":nested})).is_err());
}

#[test]
fn path_and_device_status_rules_and_their_dependents_remain_local() {
    let (mut catalog, _, _, _) = fixture();
    let path = Filter {
        rule_set: Some(RuleSet {
            mode: Match::All,
            rules: vec![Rule::Group {
                group: RuleSet {
                    mode: Match::None,
                    rules: vec![Rule::Field { field: "filePath".into(), op: "contains".into(), value: json!("private/device") }],
                },
            }],
        }),
        ..Default::default()
    };
    let status = Filter { sync_status: Some(lightcraft_catalog::sync_status::SyncStatus::Synced), ..Default::default() };
    for (id, filter) in [(AlbumId(10), path), (AlbumId(11), status)] {
        assert!(PortableFilter::from_local(&filter, &catalog, &catalog, "personal").is_err());
        let mut album = Album::new(id, "Device-local rule");
        album.smart = Some(Box::new(filter));
        catalog.apply(Op::AddAlbum { album }).unwrap();
    }
    let mut dependent = Album::new(AlbumId(12), "Depends on local rule");
    dependent.smart = Some(Box::new(Filter {
        rule_set: Some(RuleSet { mode: Match::All, rules: vec![Rule::Field { field: "album".into(), op: "is".into(), value: json!(10) }] }),
        ..Default::default()
    }));
    catalog.apply(Op::AddAlbum { album: dependent }).unwrap();
    assert_eq!(super::super::device_local_smart_albums(&catalog, "personal"), [AlbumId(10), AlbumId(11), AlbumId(12)].into_iter().collect());
}
