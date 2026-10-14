use super::*;
use crate::cloud_sync::{enable, incoming_checkpoint, project};
use lightcraft_catalog::assets::{Asset, AssetId, Blob, BlobId, RemoteLocation};
use lightcraft_catalog::replication::{Change, Mutation, Portable};
use lightcraft_catalog::{Album, AlbumId, Catalog, Filter, Op, Photo, PhotoId, Source, Stack, StackId};
use proptest::prelude::*;
use serde_json::json;

type Parents = BTreeMap<String, Option<String>>;
type Stacks = BTreeMap<String, (Vec<String>, bool)>;
fn base() -> (Portable, Vec<String>, Vec<String>) {
    let mut catalog = Catalog::new();
    let asset_id = AssetId("asset".into());
    catalog
        .apply(Op::SetAsset {
            id: asset_id.clone(),
            asset: Some(Box::new(Asset {
                original: Blob {
                    id: BlobId("a".repeat(64)),
                    size: 12,
                    remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
                },
                smart_preview: None,
                thumbnail: None,
                associated: Default::default(),
            })),
        })
        .unwrap();
    for i in 1..=6 {
        catalog
            .apply(Op::AddPhoto {
                photo: Box::new(Photo::new(PhotoId(i), Source::Managed { asset_id: asset_id.clone() }, &format!("photo-{i}.dng"), "DNG", 32, 24, "")),
            })
            .unwrap();
        let mut album = Album::new(AlbumId(i), format!("Folder {i}"));
        album.folder = true;
        catalog.apply(Op::AddAlbum { album }).unwrap();
    }
    let op = enable(&catalog, "library".into(), "seed".into(), "personal".into()).unwrap();
    catalog.apply(op).unwrap();
    let photos = (1..=6).map(|i| catalog.cloud_identity(&format!("photo:{i}")).unwrap().to_owned()).collect();
    let albums = (1..=6).map(|i| catalog.cloud_identity(&format!("album:{i}")).unwrap().to_owned()).collect();
    (catalog.sync_state().unwrap().portable(), photos, albums)
}
fn proposals(base: &Portable, photos: &[String], albums: &[String], raw: &[u8]) -> Vec<Mutation> {
    (0..3)
        .map(|actor| {
            let mut state = SyncState::restore_durable(base, format!("writer-{actor}"), "personal".into()).unwrap();
            let mut changes = Vec::new();
            for stack in 0..5 {
                let count = usize::from(raw[(actor * 19 + stack * 7) % raw.len()] % 9);
                let members: Vec<_> = (0..count)
                    .map(|n| {
                        let index = usize::from(raw[(actor * 23 + stack * 11 + n + 1) % raw.len()] % 8);
                        photos.get(index).cloned().unwrap_or_else(|| "missing-photo".into())
                    })
                    .collect();
                changes.push(Change { entity: format!("stack:stack-{stack}"), field: "photos".into(), base: None, value: json!(members) });
                changes.push(Change {
                    entity: format!("stack:stack-{stack}"),
                    field: "collapsed".into(),
                    base: None,
                    value: json!(raw[(stack + actor) % raw.len()].is_multiple_of(2)),
                });
            }
            for (n, global) in albums.iter().enumerate() {
                let index = usize::from(raw[(actor * 17 + n * 3) % raw.len()] % 8);
                let parent = match index {
                    0..=5 => json!(albums[index]),
                    6 => json!("missing-album"),
                    _ => Value::Null,
                };
                changes.push(Change { entity: format!("album:{global}"), field: "parent".into(), base: None, value: parent });
                changes.push(Change {
                    entity: format!("album:{global}"),
                    field: "deleted".into(),
                    base: None,
                    value: json!(raw[(actor * 11 + n + 31) % raw.len()].is_multiple_of(11)),
                });
            }
            for (n, global) in photos.iter().enumerate() {
                changes.push(Change {
                    entity: format!("photo:{global}"),
                    field: "deleted".into(),
                    base: None,
                    value: json!(raw[(actor * 7 + n + 53) % raw.len()].is_multiple_of(13)),
                });
            }
            state.enqueue(changes).unwrap().unwrap()
        })
        .collect()
}
fn received(base: &Portable, mutations: &[Mutation], order: &[usize]) -> SyncState {
    let mut state = SyncState::restore_durable(base, "reader".into(), "personal".into()).unwrap();
    for index in order {
        state.receive(&mutations[*index]).unwrap();
    }
    state
}
fn expected(state: &SyncState) -> (Parents, Stacks) {
    let stacks = stacks(state)
        .unwrap()
        .into_iter()
        .filter(|(_, stack)| !stack.photos.is_empty())
        .map(|(entity, stack)| (entity, (stack.photos, stack.collapsed)))
        .collect();
    (album_parents(state).unwrap(), stacks)
}
fn projected(catalog: &Catalog) -> (Parents, Stacks) {
    let documents = project(catalog, catalog, catalog.sync_state().unwrap(), None).unwrap();
    let mut parents = BTreeMap::new();
    let mut stacks = BTreeMap::new();
    for (entity, fields) in documents {
        if entity.starts_with("album:") {
            parents.insert(entity, fields.get("parent").and_then(Value::as_str).map(|global| format!("album:{global}")));
        } else if entity.starts_with("stack:") {
            stacks.insert(
                entity,
                (
                    fields["photos"].as_array().unwrap().iter().map(|value| value.as_str().unwrap().to_owned()).collect(),
                    fields["collapsed"].as_bool().unwrap(),
                ),
            );
        }
    }
    (parents, stacks)
}
struct Device {
    catalog: Catalog,
    local: Vec<PhotoId>,
    private: AlbumId,
    group: Option<StackId>,
}
fn device(base: &Portable, photos: &[String], offset: u64, local_count: u64, known: bool) -> Device {
    let mut catalog = Catalog::new();
    let local: Vec<_> = (0..local_count).map(|n| PhotoId(offset + n + 1)).collect();
    for id in &local {
        catalog
            .apply(Op::AddPhoto {
                photo: Box::new(Photo::new(*id, Source::File { path: format!("private/{}.png", id.0) }, "private.png", "PNG", 32, 24, "")),
            })
            .unwrap();
    }
    let private = AlbumId(offset + 30);
    let mut album = Album::new(private, "Private folder selection");
    album.smart = Some(Box::new(Filter { folder: Some("private-only".into()), ..Default::default() }));
    catalog.apply(Op::AddAlbum { album }).unwrap();
    catalog
        .apply(Op::SetSyncState { state: Some(Box::new(SyncState::new("library".into(), format!("peer-{offset}"), "personal".into()).unwrap())) })
        .unwrap();
    let op = incoming_checkpoint(&catalog, base, &BTreeSet::new(), format!("{:064x}", 1), 1).unwrap();
    catalog.apply(op).unwrap();
    let group = if local.len() >= 2 {
        let shared = PhotoId(
            catalog
                .cloud_identities()
                .iter()
                .find(|(entity, global)| entity.starts_with("photo:") && global.as_str() == photos[2])
                .unwrap()
                .0
                .strip_prefix("photo:")
                .unwrap()
                .parse()
                .unwrap(),
        );
        let id = StackId(offset + 500);
        let mut members = vec![shared];
        members.extend(local.iter().take(2));
        catalog.apply(Op::AddStack { stack: Stack { id, photos: members, collapsed: false } }).unwrap();
        if known {
            catalog.apply(Op::SetCloudIdentity { entity: format!("stack:{}", id.0), id: Some("stack-1".into()) }).unwrap();
        }
        Some(id)
    } else {
        None
    };
    Device { catalog, local, private, group }
}
fn deliver(device: &mut Device, base: &Portable, mutations: &[Mutation], order: &[usize]) {
    let mut state = SyncState::restore_durable(base, "reader".into(), "personal".into()).unwrap();
    for (step, index) in order.iter().enumerate() {
        state.receive(&mutations[*index]).unwrap();
        let generation = step as u64 + 2;
        let op = incoming_checkpoint(&device.catalog, &state.portable(), &BTreeSet::new(), format!("{generation:064x}"), generation).unwrap();
        device.catalog.apply(op).unwrap();
    }
}
proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
    #[test]
    fn cr33_shared_projection_is_independent_of_delivery_and_insertion_order(raw in proptest::collection::vec(any::<u8>(), 96)) {
        let (base, photos, albums) = base();
        let mutations = proposals(&base, &photos, &albums, &raw);
        let mut order = vec![0, 1, 2];
        order.sort_by_key(|n| raw[*n]);
        let mut reverse = order.clone();
        reverse.reverse();
        let a = received(&base, &mutations, &order);
        let mut b = received(&base, &mutations, &reverse);
        b.documents = b.documents.into_iter().rev().map(|(entity, fields)| (entity, fields.into_iter().rev().collect())).collect();
        prop_assert_eq!(a.portable(), b.portable());
        let projection = expected(&a);
        prop_assert_eq!(&projection, &expected(&b));
        let mut claimed = BTreeSet::new();
        for (members, _) in projection.1.values() {
            prop_assert!(members.len() >= 2);
            for photo in members {
                prop_assert!(claimed.insert(photo));
            }
        }
        for start in projection.0.keys() {
            let mut seen = BTreeSet::new();
            let mut current = Some(start);
            while let Some(entity) = current {
                prop_assert!(seen.insert(entity));
                current = projection.0.get(entity).and_then(|parent| parent.as_ref());
            }
        }
    }
    #[test]
    fn cr33_catalog_shared_relationships_ignore_local_ids_and_overlay_members(raw in proptest::collection::vec(any::<u8>(), 96), known in any::<bool>()) {
        let (base, photos, albums) = base();
        let mutations = proposals(&base, &photos, &albums, &raw);
        let order = vec![0, 1, 2];
        let reverse = vec![2, 1, 0];
        let state = received(&base, &mutations, &order);
        let wanted = expected(&state);
        let mut a = device(&base, &photos, 0, 0, false);
        let mut b = device(&base, &photos, 40 + u64::from(raw[0] % 20), 2, known);
        deliver(&mut a, &base, &mutations, &order);
        deliver(&mut b, &base, &mutations, &reverse);
        prop_assert_eq!(projected(&a.catalog), wanted.clone());
        prop_assert_eq!(projected(&b.catalog), wanted);
        prop_assert_eq!(a.catalog.sync_state().unwrap().portable(), state.portable());
        prop_assert_eq!(b.catalog.sync_state().unwrap().portable(), state.portable());
        let reopened = Catalog::from_snapshot(&b.catalog.to_snapshot()).unwrap();
        prop_assert_eq!(projected(&reopened), projected(&b.catalog));
        let before = b.catalog.to_snapshot();
        let op = incoming_checkpoint(&b.catalog, &state.portable(), &BTreeSet::new(), format!("{:064x}", 4), 4).unwrap();
        b.catalog.apply(op).unwrap();
        prop_assert_eq!(b.catalog.to_snapshot(), before);
    }
    #[test]
    fn cr33_device_local_photos_filters_and_groups_are_preserved_separately(raw in proptest::collection::vec(any::<u8>(), 96), known in any::<bool>()) {
        let (base, photos, albums) = base();
        let mutations = proposals(&base, &photos, &albums, &raw);
        let mut device = device(&base, &photos, 80, 2, known);
        let private = device.catalog.album(device.private).unwrap().clone();
        deliver(&mut device, &base, &mutations, &[1, 0, 2]);
        for id in &device.local {
            prop_assert!(matches!(device.catalog.photo(*id).unwrap().source, Source::File { .. }), "private photo remains local");
        }
        prop_assert_eq!(device.catalog.album(device.private).unwrap(), &private);
        let stack = device.catalog.stack(device.group.unwrap()).unwrap();
        prop_assert!(device.local.iter().all(|id| stack.photos.contains(id)));
    }
}
#[test]
fn cr33_global_cycle_severs_greatest_entity_and_retains_the_proposals() {
    let mut state = SyncState::new("library".into(), "writer".into(), "personal".into()).unwrap();
    let mut changes = Vec::new();
    for (node, parent) in [("a", "b"), ("b", "c"), ("c", "a")] {
        changes.push(Change { entity: format!("album:{node}"), field: "folder".into(), base: None, value: json!(true) });
        changes.push(Change { entity: format!("album:{node}"), field: "parent".into(), base: None, value: json!(parent) });
    }
    state.enqueue(changes).unwrap();
    let before = state.portable();
    let parents = album_parents(&state).unwrap();
    assert_eq!(parents["album:c"], None);
    assert_eq!(parents["album:a"], Some("album:b".into()));
    assert_eq!(state.portable(), before);
}
