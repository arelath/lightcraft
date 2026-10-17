use super::*;
use crate::{Session, library::LibraryStores};
use lightcraft_catalog::Store;
use lightcraft_catalog::assets::{Asset, AssetId, Blob, BlobId, RemoteLocation};
use lightcraft_catalog::{Album, AlbumId, Journal, MemStore, Photo, PhotoId};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
struct Interrupted {
    files: MemStore,
    fail: Arc<AtomicBool>,
}
impl Store for Interrupted {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.files.write_atomic(name, bytes)
    }
    fn append(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            self.files.append(name, &bytes[..bytes.len() / 2])?;
            return Err(std::io::Error::other("simulated disk full during record"));
        }
        self.files.append(name, bytes)
    }
    fn truncate(&mut self, name: &str, length: u64) -> std::io::Result<()> {
        self.files.truncate(name, length)
    }
    fn describe(&self) -> String {
        "interrupted".into()
    }
}

fn session() -> (Session, MemStore) {
    let store = MemStore::new();
    let mut session = Session::new();
    session
        .open_library_in(
            LibraryStores { dir: "replication".into(), catalog: Box::new(store.clone()), files: Box::new(MemStore::new()), on_disk: false },
            false,
        )
        .unwrap();
    let asset_id = AssetId("asset".into());
    let asset = Asset {
        original: Blob {
            id: BlobId("a".repeat(64)),
            size: 12,
            remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
        },
        smart_preview: None,
        thumbnail: None,
        associated: Default::default(),
    };
    session
        .commit(
            "fixture",
            Op::Batch {
                ops: vec![
                    Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) },
                    Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), Source::Managed { asset_id }, "raw.dng", "DNG", 32, 24, "2026-10-13")) },
                ],
            },
        )
        .unwrap();
    let op = enable(&session.catalog, "library".into(), "device-a".into(), "personal".into()).unwrap();
    session.apply_system(op).unwrap();
    session.persist().unwrap();
    session.selection = crate::Selection::single(PhotoId(1));
    (session, store)
}

#[test]
fn adopting_existing_local_photos_reprojects_album_cover_and_stack_membership() {
    use lightcraft_catalog::{Stack, StackId};
    let mut s = Session::new();
    for id in [1, 2] {
        s.commit(
            "Local original",
            Op::AddPhoto {
                photo: Box::new(Photo::new(PhotoId(id), Source::File { path: format!("{id}.png") }, &format!("{id}.png"), "PNG", 10, 10, "")),
            },
        )
        .unwrap();
    }
    let mut album = Album::new(AlbumId(1), "Before cloud");
    album.photos = vec![PhotoId(1), PhotoId(2)];
    album.cover = Some(PhotoId(2));
    s.commit(
        "containers",
        Op::Batch {
            ops: vec![
                Op::AddAlbum { album },
                Op::AddStack { stack: Stack { id: StackId(1), photos: vec![PhotoId(1), PhotoId(2)], collapsed: true } },
            ],
        },
    )
    .unwrap();
    let op = enable(&s.catalog, "library".into(), "device".into(), "personal".into()).unwrap();
    s.apply_system(op).unwrap();
    for id in [1, 2] {
        let asset_id = AssetId(format!("asset-{id}"));
        let asset = Asset {
            original: Blob {
                id: BlobId(format!("{id:064x}")),
                size: 12,
                remote: RemoteLocation { connection: "personal".into(), key: format!("originals/{id}"), version: None },
            },
            smart_preview: None,
            thumbnail: None,
            associated: Default::default(),
        };
        s.commit(
            "adopt",
            Op::Batch {
                ops: vec![
                    Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) },
                    Op::Relink { id: PhotoId(id), source: Source::Managed { asset_id }, file_name: format!("{id}.png"), format: None },
                ],
            },
        )
        .unwrap();
    }
    let state = s.catalog.sync_state().unwrap();
    let album = format!("album:{}", s.catalog.cloud_identity("album:1").unwrap());
    for id in [1, 2] {
        let global = s.catalog.cloud_identity(&format!("photo:{id}")).unwrap();
        assert_eq!(state.winning(&album, &format!("member:{global}")).unwrap().1, &json!(true));
    }
    assert_eq!(state.winning(&album, "cover").unwrap().1, &json!(s.catalog.cloud_identity("photo:2").unwrap()));
    let stack = format!("stack:{}", s.catalog.cloud_identity("stack:1").unwrap());
    assert_eq!(state.winning(&stack, "photos").unwrap().1.as_array().unwrap().len(), 2);
}
#[test]
fn unavailable_writer_blocks_new_local_mutations_but_allows_verified_remote_receive() {
    let (mut s, _) = session();
    s.cloud_writer_error = Some("persistent installation identity unavailable".into());
    let before = s.catalog.to_snapshot();
    assert!(s.execute("photo.rate", &json!({"ids":[1],"rating":4})).unwrap_err().to_string().contains("claiming"));
    assert_eq!(s.catalog.to_snapshot(), before);
    let mut authored = s.catalog.sync_state().unwrap().clone();
    authored.enqueue(vec![Change { entity: "library:library".into(), field: "label.Green".into(), base: None, value: json!("local") }]).unwrap();
    assert!(s.apply_received(Op::SetSyncState { state: Some(Box::new(authored)) }).is_err());
    assert_eq!(s.catalog.to_snapshot(), before);
    let mut peer = SyncState::new("library".into(), "peer".into(), "personal".into()).unwrap();
    let change = peer
        .enqueue(vec![Change { entity: "library:library".into(), field: "label.Green".into(), base: None, value: json!("remote") }])
        .unwrap()
        .unwrap();
    let mut received = s.catalog.sync_state().unwrap().clone();
    let pending = received.pending.clone();
    received.receive(&change).unwrap();
    s.apply_received(Op::SetSyncState { state: Some(Box::new(received)) }).unwrap();
    assert_eq!(s.catalog.sync_state().unwrap().pending, pending);
}
#[test]
fn publication_schedules_historical_authors_before_dependent_new_epochs() {
    let mut state = SyncState::new("library".into(), "old".into(), "personal".into()).unwrap();
    for value in 1..=3 {
        state.enqueue(vec![Change { entity: "library:library".into(), field: "label.Green".into(), base: None, value: json!(value) }]).unwrap();
    }
    let mut observed = state.clock.clone();
    observed.insert("old".into(), 2);
    let old = state.pending["old-3"].clone();
    state.local_owner = "owner".into();
    state.begin_writing_epoch("owner".into(), "new".into()).unwrap();
    let new = state
        .enqueue(vec![Change { entity: "library:library".into(), field: "label.Red".into(), base: None, value: json!("new") }])
        .unwrap()
        .unwrap();
    let ordered = publication_order(vec![new.clone(), old.clone()], &observed).unwrap();
    assert_eq!(ordered, vec![old.clone(), new.clone()]);
    assert_eq!(publication_order(vec![old, new.clone()], &observed).unwrap(), ordered);
    assert!(publication_order(vec![new], &observed).unwrap_err().to_string().contains("missing retained causal history"));
}
#[test]
fn commands_undo_redo_system_changes_and_final_slider_values_have_durable_fresh_identities() {
    let (mut s, store) = session();
    let first = s.catalog.sync_state().unwrap().pending.len();
    s.execute("photo.rate", &json!({"ids":[1],"rating":4})).unwrap();
    s.undo_step().unwrap();
    s.redo_step().unwrap();
    s.apply_system(Op::SetCaptured { id: PhotoId(1), captured: Some("2026-10-13T12:00:00".into()) }).unwrap();
    s.selection = crate::Selection::single(PhotoId(1));
    s.begin_interaction("Exposure").unwrap();
    for exposure in [0.1, 0.2, 0.3] {
        let mut settings = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        settings.light.exposure = exposure;
        s.set_develop(PhotoId(1), settings, "Exposure").unwrap();
    }
    assert_eq!(s.catalog.sync_state().unwrap().pending.len(), first + 4);
    s.end_interaction().unwrap();
    assert_eq!(s.catalog.sync_state().unwrap().pending.len(), first + 5);
    s.persist().unwrap();
    let (_, loaded, _) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(loaded.sync_state(), s.catalog.sync_state());
    let entity = format!("photo:{}", loaded.cloud_identity("photo:1").unwrap());
    let state = loaded.sync_state().unwrap();
    assert_eq!(state.winning(&entity, "rating").unwrap().1, &json!(4));
    assert_eq!(state.winning(&entity, "develop").unwrap().1.get("light").unwrap().get("exposure"), Some(&json!(0.3)));
}
#[test]
fn each_durable_record_contains_the_change_and_outbox_and_compaction_keeps_pending_work() {
    let (mut s, store) = session();
    s.execute("photo.rate", &json!({"ids":[1],"rating":5})).unwrap();
    let bytes = store.get(lightcraft_catalog::journal::LOG).unwrap();
    let last = String::from_utf8(bytes).unwrap().lines().last().unwrap().to_owned();
    assert!(last.contains("setRating") && last.contains("patchSyncState"));
    let value: Value = serde_json::from_str(&last).unwrap();
    assert_eq!(value["op"]["op"], json!("batch"));
    let (mut journal, loaded, _) = Journal::open(Box::new(store.clone())).unwrap();
    journal.snapshot(&loaded).unwrap();
    let (_, compacted, _) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(compacted.sync_state(), s.catalog.sync_state());
    assert_eq!(compacted.photo(PhotoId(1)).unwrap().rating, 5);
}
#[test]
fn conflict_status_cache_reuses_views_and_rejects_stale_revision_or_library_token() {
    let (mut s, _) = session();
    let initial = s.execute("cloud.syncStatus", &json!({})).unwrap();
    assert_eq!(initial["utilization"]["stateBytes"], serde_json::to_vec(s.catalog.sync_state().unwrap()).unwrap().len());
    let overview = s.execute("cloud.overview", &json!({})).unwrap();
    assert_eq!(overview["utilization"], initial["utilization"]);
    assert_eq!(overview["capacityWarnings"], initial["capacityWarnings"]);
    s.cloud_conflicts_cache.as_mut().unwrap().2["reuseMarker"] = json!(true);
    s.execute("cloud.syncStatus", &json!({})).unwrap();
    assert_eq!(s.cloud_conflicts_cache.as_ref().unwrap().2["reuseMarker"], true);
    s.commit("rate", Op::SetRating { id: PhotoId(1), rating: 4 }).unwrap();
    let changed = s.execute("cloud.syncStatus", &json!({})).unwrap();
    assert!(changed["utilization"]["stateBytes"].as_u64().unwrap() > initial["utilization"]["stateBytes"].as_u64().unwrap());
    assert!(s.cloud_conflicts_cache.as_ref().unwrap().2.get("reuseMarker").is_none());
    s.cloud_conflicts_cache.as_mut().unwrap().2["reuseMarker"] = json!(true);
    s.library_identity = Arc::new(());
    s.execute("cloud.syncStatus", &json!({})).unwrap();
    assert!(s.cloud_conflicts_cache.as_ref().unwrap().2.get("reuseMarker").is_none());
}

#[test]
fn unchanged_head_requires_all_durable_metadata_and_preserves_ack_progress_and_pending_work() {
    let (mut s, _) = session();
    let mut local = s.catalog.sync_state().unwrap().clone();
    let ids = local.pending.keys().cloned().collect();
    local.acknowledge(&ids, "a".repeat(64), 1).unwrap();
    s.apply_system(Op::SetSyncState { state: Some(Box::new(local)) }).unwrap();
    let remote = s.catalog.sync_state().unwrap().portable();
    let check = |remote: &lightcraft_catalog::replication::Portable, ids: &BTreeSet<String>, cursor: &str, generation| {
        unchanged_head(&s.catalog, remote, ids, cursor, generation)
    };
    assert!(check(&remote, &BTreeSet::new(), &"a".repeat(64), 1));
    assert!(!check(&remote, &BTreeSet::from(["device-a-1".into()]), &"a".repeat(64), 1));
    assert!(!check(&remote, &BTreeSet::new(), &"b".repeat(64), 1));
    assert!(!check(&remote, &BTreeSet::new(), &"a".repeat(64), 2));
    let mut versions = Vec::new();
    let mut changed = remote.clone();
    changed.version = 1;
    versions.push(changed);
    let mut changed = remote.clone();
    changed.library = "other".into();
    versions.push(changed);
    let mut changed = remote.clone();
    changed.clock.insert("remote".into(), 1);
    versions.push(changed);
    let mut changed = remote.clone();
    changed.documents.clear();
    versions.push(changed);
    let mut changed = remote.clone();
    changed.received.clear();
    versions.push(changed);
    let mut changed = remote.clone();
    changed.archives.insert("remote".into(), lightcraft_catalog::replication::archive::Prefix { through: 1, roots: vec!["b".repeat(64)] });
    versions.push(changed);
    for changed in versions {
        assert!(!check(&changed, &BTreeSet::new(), &"a".repeat(64), 1));
    }
    s.execute("photo.rate", &json!({"ids":[1],"rating":5})).unwrap();
    let pending = s.catalog.sync_state().unwrap().portable();
    assert!(!unchanged_head(&s.catalog, &pending, &BTreeSet::new(), &"a".repeat(64), 1));
}

#[test]
fn folder_rearrangement_detaches_before_attaching_and_concurrent_cycles_converge() {
    let (mut s, _) = session();
    for (id, global) in [(AlbumId(1), "zzz-A"), (AlbumId(2), "aaa-B")] {
        s.apply_system(Op::SetCloudIdentity { entity: format!("album:{}", id.0), id: Some(global.into()) }).unwrap();
        let mut album = Album::new(id, global);
        album.folder = true;
        s.commit("folder", Op::AddAlbum { album }).unwrap();
    }
    s.commit("initial nesting", Op::MoveAlbum { id: AlbumId(1), parent: Some(AlbumId(2)) }).unwrap();
    let mut remote = SyncState::new("library".into(), "remote".into(), "personal".into()).unwrap();
    remote.merge(&s.catalog.sync_state().unwrap().portable()).unwrap();
    let changes = [("album:zzz-A", Value::Null), ("album:aaa-B", json!("zzz-A"))]
        .into_iter()
        .map(|(entity, value)| Change {
            entity: entity.into(),
            field: "parent".into(),
            base: remote.winning(entity, "parent").map(|(stamp, _)| stamp),
            value,
        })
        .collect();
    let mutation = remote.enqueue(changes).unwrap().unwrap();
    let op = incoming(&s.catalog, &[mutation], &BTreeSet::new(), "a".repeat(64), 1).unwrap();
    s.apply_received(op).unwrap();
    assert_eq!(s.catalog.album(AlbumId(1)).unwrap().parent, None);
    assert_eq!(s.catalog.album(AlbumId(2)).unwrap().parent, Some(AlbumId(1)));
    s.commit("root", Op::MoveAlbum { id: AlbumId(2), parent: None }).unwrap();
    let initial = s.catalog.clone();
    let moves: Vec<_> = [("peer-a", "album:zzz-A", "aaa-B"), ("peer-b", "album:aaa-B", "zzz-A")]
        .into_iter()
        .map(|(device, entity, parent)| {
            let mut peer = SyncState::new("library".into(), device.into(), "personal".into()).unwrap();
            peer.merge(&initial.sync_state().unwrap().portable()).unwrap();
            peer.enqueue(vec![Change {
                entity: entity.into(),
                field: "parent".into(),
                base: peer.winning(entity, "parent").map(|(stamp, _)| stamp),
                value: json!(parent),
            }])
            .unwrap()
            .unwrap()
        })
        .collect();
    let mut a = initial.clone();
    let mut b = initial;
    a.apply(incoming(&a, &moves, &BTreeSet::new(), "b".repeat(64), 2).unwrap()).unwrap();
    b.apply(incoming(&b, &moves.into_iter().rev().collect::<Vec<_>>(), &BTreeSet::new(), "b".repeat(64), 2).unwrap()).unwrap();
    for id in [AlbumId(1), AlbumId(2)] {
        assert_eq!(a.album(id), b.album(id));
    }
    assert_eq!(a.album(AlbumId(1)).unwrap().parent, None);
    assert_eq!(a.album(AlbumId(2)).unwrap().parent, Some(AlbumId(1)));
    assert_eq!(a.sync_state().unwrap().winning("album:zzz-A", "parent").unwrap().1, &json!("aaa-B"), "conflicting proposed edge remains retained");
    let conflict = structural_conflicts(&a);
    assert_eq!(conflict.len(), 1);
    let mut status_session = Session::new();
    status_session.catalog = a.clone();
    let overview = crate::cloud_status::overview(&mut status_session, &Value::Null).unwrap();
    assert_eq!(overview["summary"]["issues"], 0, "the conflict has no photo fields");
    assert_eq!(overview["health"]["conflicts"]["structural"], 1);
    assert_eq!(overview["structuralConflicts"], serde_json::to_value(&conflict).unwrap());
    a.apply(accept_structure(&a, conflict[0]["entity"].as_str().unwrap()).unwrap()).unwrap();
    assert!(structural_conflicts(&a).is_empty());
    b.apply(incoming_checkpoint(&b, &a.sync_state().unwrap().portable(), &BTreeSet::new(), "c".repeat(64), 3).unwrap()).unwrap();
    assert!(structural_conflicts(&b).is_empty());
}

#[test]
fn stack_membership_swaps_and_cross_entity_overlap_materialize_without_uniqueness_failure() {
    use lightcraft_catalog::{Stack, StackId};
    let (mut s, _) = session();
    for id in 2..=4 {
        s.commit(
            "photo",
            Op::AddPhoto {
                photo: Box::new(Photo::new(PhotoId(id), Source::Managed { asset_id: AssetId("asset".into()) }, "copy.dng", "DNG", 32, 24, "")),
            },
        )
        .unwrap();
    }
    for (id, global, photos) in [(StackId(1), "aaa", vec![PhotoId(1), PhotoId(2)]), (StackId(2), "zzz", vec![PhotoId(3), PhotoId(4)])] {
        s.apply_system(Op::SetCloudIdentity { entity: format!("stack:{}", id.0), id: Some(global.into()) }).unwrap();
        s.commit("stack", Op::AddStack { stack: Stack { id, photos, collapsed: false } }).unwrap();
    }
    let mut remote = SyncState::new("library".into(), "remote".into(), "personal".into()).unwrap();
    remote.merge(&s.catalog.sync_state().unwrap().portable()).unwrap();
    for memberships in [[[1, 3], [2, 4]], [[1, 2], [2, 3]]] {
        let changes = ["stack:aaa", "stack:zzz"]
            .into_iter()
            .zip(memberships)
            .map(|(entity, ids)| Change {
                entity: entity.into(),
                field: "photos".into(),
                base: remote.winning(entity, "photos").map(|(stamp, _)| stamp),
                value: json!(ids.map(|id| s.catalog.cloud_identity(&format!("photo:{id}")).unwrap())),
            })
            .collect();
        let mutation = remote.enqueue(changes).unwrap().unwrap();
        s.apply_received(incoming(&s.catalog, &[mutation], &BTreeSet::new(), "a".repeat(64), 1).unwrap()).unwrap();
        if memberships[0] == [1, 3] {
            assert_eq!(s.catalog.stack(StackId(1)).unwrap().photos, vec![PhotoId(1), PhotoId(3)]);
            assert_eq!(s.catalog.stack(StackId(2)).unwrap().photos, vec![PhotoId(2), PhotoId(4)]);
        } else {
            assert_eq!(s.catalog.stack(StackId(1)).unwrap().photos, vec![PhotoId(1), PhotoId(2)]);
            assert!(s.catalog.stack(StackId(2)).is_none());
            assert_eq!(s.catalog.sync_state().unwrap().winning("stack:zzz", "photos").unwrap().1.as_array().unwrap().len(), 2);
            let status = s.execute("cloud.acceptStructure", &json!({"entity":"stack:zzz"})).unwrap();
            assert!(status["structuralConflicts"].as_array().unwrap().is_empty());
            assert_eq!(s.catalog.sync_state().unwrap().winning("stack:zzz", "photos").unwrap().1, &json!([]));
            let before = s.catalog.to_snapshot();
            assert!(s.execute("cloud.acceptStructure", &json!({"entity":"stack:unknown"})).is_err());
            assert_eq!(s.catalog.to_snapshot(), before);
        }
    }
}

#[test]
fn shared_stack_ownership_is_independent_of_local_members_and_ids() {
    use lightcraft_catalog::{Catalog, Stack, StackId};
    let (mut seed, _) = session();
    for id in 2..=4 {
        seed.commit(
            "photo",
            Op::AddPhoto {
                photo: Box::new(Photo::new(PhotoId(id), Source::Managed { asset_id: AssetId("asset".into()) }, "copy.dng", "DNG", 32, 24, "")),
            },
        )
        .unwrap();
    }
    let mut remote = seed.catalog.sync_state().unwrap().clone();
    let globals: Vec<_> = (1..=4).map(|id| seed.catalog.cloud_identity(&format!("photo:{id}")).unwrap().to_owned()).collect();
    let before = remote.portable();
    let proposals =
        [vec![globals[0].clone(), globals[1].clone()], vec![globals[1].clone(), globals[2].clone()], vec![globals[2].clone(), globals[3].clone()]];
    let changes = ["aaa", "bbb", "ccc"]
        .into_iter()
        .zip(proposals)
        .flat_map(|(global, photos)| {
            [
                Change { entity: format!("stack:{global}"), field: "photos".into(), base: None, value: json!(photos) },
                Change { entity: format!("stack:{global}"), field: "collapsed".into(), base: None, value: json!(true) },
            ]
        })
        .collect();
    remote.enqueue(changes).unwrap();
    let shared = remote.portable();
    let expected =
        BTreeMap::from([("stack:aaa".to_owned(), json!([globals[0], globals[1]])), ("stack:ccc".to_owned(), json!([globals[2], globals[3]]))]);
    for (local_count, known_stack) in [(0, Some("bbb")), (1, Some("bbb")), (2, Some("bbb")), (2, None), (2, Some("aaa"))] {
        let mut catalog = Catalog::new();
        let local_ids: Vec<_> = (0..local_count).map(|i| PhotoId(40 + local_count * 7 + i)).collect();
        for id in &local_ids {
            catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(Photo::new(*id, Source::File { path: format!("private/{}.jpg", id.0) }, "local.jpg", "JPEG", 32, 24, "")),
                })
                .unwrap();
        }
        let state = SyncState::new("library".into(), "peer".into(), "personal".into()).unwrap();
        catalog.apply(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
        catalog.apply(incoming_checkpoint(&catalog, &before, &BTreeSet::new(), "a".repeat(64), 1).unwrap()).unwrap();
        let photo = |global: &str| {
            PhotoId(
                catalog
                    .cloud_identities()
                    .iter()
                    .find(|(key, value)| key.starts_with("photo:") && value.as_str() == global)
                    .unwrap()
                    .0
                    .strip_prefix("photo:")
                    .unwrap()
                    .parse()
                    .unwrap(),
            )
        };
        if !local_ids.is_empty() {
            let mut photos = vec![photo(&globals[2])];
            photos.extend(&local_ids);
            catalog.apply(Op::AddStack { stack: Stack { id: StackId(31), photos, collapsed: false } }).unwrap();
            if let Some(global) = known_stack {
                catalog.apply(Op::SetCloudIdentity { entity: "stack:31".into(), id: Some(global.into()) }).unwrap();
            }
        }
        catalog.apply(incoming_checkpoint(&catalog, &shared, &BTreeSet::new(), "b".repeat(64), 2).unwrap()).unwrap();
        let projection = project(&catalog, &catalog, catalog.sync_state().unwrap(), None).unwrap();
        let stacks: BTreeMap<_, _> = projection
            .into_iter()
            .filter(|(entity, _)| entity.starts_with("stack:"))
            .map(|(entity, fields)| (entity, fields["photos"].clone()))
            .collect();
        assert_eq!(stacks, expected, "local count {local_count}, synchronized overlay {known_stack:?}");
        assert_eq!(catalog.sync_state().unwrap().portable(), shared, "materialization cannot echo local overlays");
        if known_stack == Some("aaa") {
            let local_stack = catalog.stack(StackId(31)).unwrap();
            let shared_members: Vec<_> = local_stack.photos.iter().filter(|id| !local_ids.contains(id)).copied().collect();
            assert_eq!(shared_members.len(), 2);
            assert!(local_ids.iter().all(|id| local_stack.photos.contains(id)), "attach local overlay to a surviving shared stack");
            assert!(local_stack.collapsed, "shared stack keeps its shared presentation");
        } else if local_ids.len() >= 2 {
            let local_stack = catalog.stack(StackId(31)).unwrap();
            assert_eq!(local_stack.photos, local_ids, "preserve the local-only group after shared dissolution or reassignment");
            assert!(!local_stack.collapsed, "local group keeps its own presentation");
        } else {
            assert!(catalog.stack(StackId(31)).is_none());
        }
        let reopened = Catalog::from_snapshot(&catalog.to_snapshot()).unwrap();
        assert_eq!(structural_conflicts(&reopened), structural_conflicts(&catalog));
        assert!(structural_conflicts(&catalog).iter().any(|conflict| conflict["entity"] == "stack:bbb" && conflict["projected"] == json!([])));
        let before = catalog.to_snapshot();
        catalog.apply(incoming_checkpoint(&catalog, &shared, &BTreeSet::new(), "b".repeat(64), 2).unwrap()).unwrap();
        assert_eq!(catalog.to_snapshot(), before, "repeated materialization is stable");

        // Existing devices retain deleted photo objects; a fresh bootstrap does
        // not. Neither those objects nor unresolved references may affect winners.
        let mut deleted = remote.clone();
        let entity = format!("photo:{}", globals[0]);
        deleted
            .enqueue(vec![
                Change {
                    entity: entity.clone(),
                    field: "deleted".into(),
                    base: deleted.winning(&entity, "deleted").map(|(stamp, _)| stamp),
                    value: json!(true),
                },
                Change {
                    entity: "stack:bbb".into(),
                    field: "photos".into(),
                    base: deleted.winning("stack:bbb", "photos").map(|(stamp, _)| stamp),
                    value: json!([globals[1], "unresolved", globals[2]]),
                },
            ])
            .unwrap();
        let deleted = deleted.portable();
        let mut fresh = Catalog::new();
        fresh
            .apply(Op::SetSyncState { state: Some(Box::new(SyncState::new("library".into(), "fresh".into(), "personal".into()).unwrap())) })
            .unwrap();
        for device in [&mut catalog, &mut fresh] {
            device.apply(incoming_checkpoint(device, &deleted, &BTreeSet::new(), "c".repeat(64), 3).unwrap()).unwrap();
            let projected = project(device, device, device.sync_state().unwrap(), None).unwrap();
            let stacks: Vec<_> = projected.iter().filter(|(entity, _)| entity.starts_with("stack:")).collect();
            assert_eq!(stacks.len(), 1);
            assert_eq!(stacks[0].0, "stack:bbb");
            assert_eq!(stacks[0].1["photos"], json!([globals[1], globals[2]]));
        }
    }
}

#[test]
fn local_sources_and_working_paths_are_excluded_and_membership_changes_are_individual() {
    let (mut s, _) = session();
    let first = s.catalog.sync_state().unwrap().pending.len();
    s.commit(
        "local",
        Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(2), Source::File { path: "private/path.jpg".into() }, "path.jpg", "JPEG", 32, 24, "")) },
    )
    .unwrap();
    s.commit("local rating", Op::SetRating { id: PhotoId(2), rating: 3 }).unwrap();
    assert_eq!(s.catalog.sync_state().unwrap().pending.len(), first);
    s.commit("album", Op::AddAlbum { album: Album::new(AlbumId(1), "Trip") }).unwrap();
    s.commit("membership", Op::SetAlbumPhotos { id: AlbumId(1), photos: vec![PhotoId(1), PhotoId(2)] }).unwrap();
    let state = s.catalog.sync_state().unwrap();
    let serialized = serde_json::to_string(state).unwrap();
    assert!(!serialized.contains("private/path.jpg"));
    let entity = format!("album:{}", s.catalog.cloud_identity("album:1").unwrap());
    assert_eq!(state.documents[&entity].keys().filter(|k| k.starts_with("member:")).count(), 1);
    let before = state.pending.len();
    s.commit("remove", s.catalog.delete_permanently_ops(PhotoId(1))).unwrap();
    let state = s.catalog.sync_state().unwrap();
    assert_eq!(state.pending.len(), before + 1);
    let entity = format!("photo:{}", s.catalog.cloud_identity("photo:1").unwrap());
    assert_eq!(state.winning(&entity, "deleted").unwrap().1, &json!(true));
    assert!(state.winning(&entity, "content").is_some());
}

#[test]
fn torn_append_keeps_catalog_and_intent_together_and_retry_retains_the_mutation_id() {
    let (initial, files) = session();
    let before = initial.catalog.clone();
    drop(initial);
    let interrupted = Interrupted { files: files.clone(), fail: Arc::new(AtomicBool::new(false)) };
    let mut session = Session::new();
    session
        .open_library_in(
            LibraryStores { dir: "interrupted".into(), catalog: Box::new(interrupted.clone()), files: Box::new(MemStore::new()), on_disk: false },
            false,
        )
        .unwrap();
    session.selection = crate::Selection::single(PhotoId(1));
    interrupted.fail.store(true, Ordering::SeqCst);
    assert!(matches!(session.execute("photo.rate", &json!({"rating":5})), Err(EngineError::NotSaved(_))));
    let queued = session.catalog.sync_state().unwrap().pending.clone();
    let (_, crash, _) = Journal::open(Box::new(files.clone())).unwrap();
    assert_eq!(crash.photo(PhotoId(1)).unwrap().rating, before.photo(PhotoId(1)).unwrap().rating);
    assert_eq!(crash.sync_state(), before.sync_state());
    interrupted.fail.store(false, Ordering::SeqCst);
    session.persist().unwrap();
    let (_, recovered, _) = Journal::open(Box::new(files)).unwrap();
    assert_eq!(recovered.photo(PhotoId(1)).unwrap().rating, 5);
    assert_eq!(recovered.sync_state().unwrap().pending, queued);
}

#[test]
fn import_undo_keeps_its_global_identity_and_creates_a_tombstone_then_a_causal_restore() {
    let (mut s, _) = session();
    let mut photo = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    photo.id = PhotoId(2);
    photo.copy_of = Some(PhotoId(1));
    s.commit(
        "copy",
        Op::Batch {
            ops: vec![Op::AddPhoto { photo: Box::new(photo) }, Op::SetCloudIdentity { entity: "photo:2".into(), id: Some("imported-copy".into()) }],
        },
    )
    .unwrap();
    s.undo_step().unwrap();
    assert_eq!(s.catalog.cloud_identity("photo:2"), Some("imported-copy"));
    assert_eq!(s.catalog.sync_state().unwrap().winning("photo:imported-copy", "deleted").unwrap().1, &json!(true));
    s.redo_step().unwrap();
    assert_eq!(s.catalog.cloud_identity("photo:2"), Some("imported-copy"));
    assert_eq!(s.catalog.sync_state().unwrap().winning("photo:imported-copy", "deleted").unwrap().1, &json!(false));
    assert_eq!(s.catalog.sync_state().unwrap().winning("photo:imported-copy", "copy_of").unwrap().1, &json!(s.catalog.cloud_identity("photo:1")));
}

fn peer(device: &str) -> (Session, MemStore) {
    let files = MemStore::new();
    let mut s = Session::new();
    s.open_library_in(
        LibraryStores { dir: device.into(), catalog: Box::new(files.clone()), files: Box::new(MemStore::new()), on_disk: false },
        false,
    )
    .unwrap();
    s.apply_system(enable(&s.catalog, "library".into(), device.into(), "personal".into()).unwrap()).unwrap();
    (s, files)
}
fn deliver(from: &Session, to: &mut Session, generation: u64) {
    let mutations: Vec<_> = from.catalog.sync_state().unwrap().pending.values().cloned().collect();
    let pending = to.catalog.sync_state().unwrap().pending.clone();
    let op = incoming(&to.catalog, &mutations, &BTreeSet::new(), format!("{generation:064x}"), generation).unwrap();
    to.apply_received(op).unwrap();
    to.persist().unwrap();
    assert_eq!(to.catalog.sync_state().unwrap().pending, pending, "remote changes are never echoed");
}

#[test]
fn two_device_metadata_memberships_and_recipe_conflicts_converge_and_resolve_through_shared_commands() {
    let (mut a, _) = session();
    let (mut b, b_files) = peer("device-b");
    let mut second = (**a.catalog.photo(PhotoId(1)).unwrap()).clone();
    second.id = PhotoId(2);
    a.commit("another", Op::AddPhoto { photo: Box::new(second) }).unwrap();
    a.commit("album", Op::AddAlbum { album: Album::new(AlbumId(1), "Trip") }).unwrap();
    deliver(&a, &mut b, 1);
    let one = PhotoId(local_id(&b.catalog, "photo", a.catalog.cloud_identity("photo:1").unwrap()));
    let two = PhotoId(local_id(&b.catalog, "photo", a.catalog.cloud_identity("photo:2").unwrap()));
    let album = AlbumId(local_id(&b.catalog, "album", a.catalog.cloud_identity("album:1").unwrap()));
    a.commit("rating", Op::SetRating { id: PhotoId(1), rating: 4 }).unwrap();
    let mut meta = b.catalog.photo(one).unwrap().meta.clone();
    meta.caption = "Offline caption".into();
    b.commit("caption", Op::SetMeta { id: one, meta: Box::new(meta) }).unwrap();
    a.commit("add first", Op::SetAlbumPhotos { id: AlbumId(1), photos: vec![PhotoId(1)] }).unwrap();
    b.commit("add second", Op::SetAlbumPhotos { id: album, photos: vec![two] }).unwrap();
    let mut first_recipe = (*a.catalog.photo(PhotoId(1)).unwrap().develop).clone();
    first_recipe.light.exposure = 1.0;
    let mut second_recipe = (*b.catalog.photo(one).unwrap().develop).clone();
    second_recipe.light.exposure = 2.0;
    a.set_develop(PhotoId(1), first_recipe, "Exposure").unwrap();
    b.set_develop(one, second_recipe, "Exposure").unwrap();
    deliver(&a, &mut b, 2);
    deliver(&b, &mut a, 2);
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().rating, 4);
    assert_eq!(b.catalog.photo(one).unwrap().rating, 4);
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().meta.caption, "Offline caption");
    assert_eq!(a.catalog.album(AlbumId(1)).unwrap().photos.len(), 2);
    assert_eq!(b.catalog.album(album).unwrap().photos.len(), 2);
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().develop, b.catalog.photo(one).unwrap().develop);
    assert!(a.catalog.photo(PhotoId(1)).unwrap().versions.iter().any(|v| v.name.starts_with("Cloud conflict")));
    let entity = format!("photo:{}", a.catalog.cloud_identity("photo:1").unwrap());
    let revision = a.catalog.sync_state().unwrap().documents[&entity]["develop"].0.keys().next().unwrap().clone();
    a.execute("cloud.resolveConflict", &json!({"entity":entity,"field":"develop","revision":revision})).unwrap();
    deliver(&a, &mut b, 3);
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().develop, b.catalog.photo(one).unwrap().develop);
    assert_eq!(b.catalog.sync_state().unwrap().documents[&entity]["develop"].0.len(), 1);
    assert!(
        b.catalog.photo(one).unwrap().versions.iter().any(|v| v.name.starts_with("Cloud conflict")),
        "resolving does not delete the archived alternative"
    );
    let (mut fresh, _) = peer("fresh-device");
    let op = incoming_checkpoint(&fresh.catalog, &a.catalog.sync_state().unwrap().portable(), &BTreeSet::new(), "d".repeat(64), 3).unwrap();
    fresh.apply_received(op).unwrap();
    assert!(
        fresh.catalog.photos().any(|p| p.versions.iter().any(|v| v.name.starts_with("Cloud conflict"))),
        "checkpoint bootstrap retains resolved conflict versions"
    );
    let (_, loaded, _) = Journal::open(Box::new(b_files)).unwrap();
    assert_eq!(loaded.sync_state(), b.catalog.sync_state());
    assert_eq!(loaded.photo(one).unwrap().meta.caption, "Offline caption");
}
fn local_id(catalog: &Catalog, kind: &str, global: &str) -> u64 {
    catalog
        .cloud_identities()
        .iter()
        .find_map(|(key, id)| {
            let (prefix, local) = key.split_once(':')?;
            (prefix == kind && id == global).then(|| local.parse().unwrap())
        })
        .unwrap()
}

#[test]
fn revision_aware_undo_keeps_independent_remote_fields_and_refuses_competing_revisions() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    deliver(&a, &mut b, 1);
    a.commit("rating", Op::SetRating { id: PhotoId(1), rating: 4 }).unwrap();
    let mut meta = b.catalog.photo(PhotoId(1)).unwrap().meta.clone();
    meta.title = "Remote title".into();
    b.commit("title", Op::SetMeta { id: PhotoId(1), meta: Box::new(meta) }).unwrap();
    deliver(&b, &mut a, 2);
    a.undo_step().unwrap();
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().rating, 0);
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().meta.title, "Remote title");
    a.redo_step().unwrap();
    b.commit("competing rating", Op::SetRating { id: PhotoId(1), rating: 5 }).unwrap();
    deliver(&b, &mut a, 3);
    let before = a.catalog.to_snapshot();
    assert!(a.undo_step().unwrap_err().to_string().contains("received another revision"));
    assert_eq!(a.catalog.to_snapshot(), before);
}

#[test]
fn metadata_clear_virtual_copy_references_and_smart_filters_roundtrip_across_different_local_ids() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    b.commit(
        "local",
        Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), Source::File { path: "private.jpg".into() }, "private.jpg", "JPEG", 1, 1, "")) },
    )
    .unwrap();
    let mut copy = (**a.catalog.photo(PhotoId(1)).unwrap()).clone();
    copy.id = PhotoId(2);
    copy.copy_of = Some(PhotoId(1));
    a.commit("copy", Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    let mut meta = a.catalog.photo(PhotoId(1)).unwrap().meta.clone();
    meta.usage_terms = "Contact photographer".into();
    a.commit("rights", Op::SetMeta { id: PhotoId(1), meta: Box::new(meta) }).unwrap();
    let mut album = Album::new(AlbumId(1), "Smart");
    let filter = lightcraft_catalog::Filter { only: vec![PhotoId(2)], ..Default::default() };
    album.smart = Some(Box::new(filter));
    a.commit("smart", Op::AddAlbum { album }).unwrap();
    deliver(&a, &mut b, 1);
    let original = PhotoId(local_id(&b.catalog, "photo", a.catalog.cloud_identity("photo:1").unwrap()));
    let copy = PhotoId(local_id(&b.catalog, "photo", a.catalog.cloud_identity("photo:2").unwrap()));
    assert_eq!(b.catalog.photo(copy).unwrap().copy_of, Some(original));
    assert_ne!(original, PhotoId(1));
    let album = AlbumId(local_id(&b.catalog, "album", a.catalog.cloud_identity("album:1").unwrap()));
    assert_eq!(b.catalog.album(album).unwrap().smart.as_ref().unwrap().only, vec![copy]);
    let mut meta = a.catalog.photo(PhotoId(1)).unwrap().meta.clone();
    meta.usage_terms.clear();
    a.commit("clear rights", Op::SetMeta { id: PhotoId(1), meta: Box::new(meta) }).unwrap();
    deliver(&a, &mut b, 2);
    assert!(b.catalog.photo(original).unwrap().meta.usage_terms.is_empty());
}

#[test]
fn deleted_album_references_bootstrap_fresh_peers_without_live_targets_or_broader_rules() {
    let (mut a, _) = session();
    a.execute("album.create", &json!({"name":"Target","addSelected":true})).unwrap();
    let target = a.catalog.cloud_identity("album:1").unwrap().to_string();
    let mut globals = Vec::new();
    for rules in [
        json!({"album":1}),
        json!({"ruleSet":{"rules":[{"field":"album","op":"is","value":1}]}}),
        json!({"ruleSet":{"rules":[{"field":"album","op":"isNot","value":1}]}}),
        json!({"ruleSet":{"rules":[{"field":"album","op":"isNot","value":"1"}]}}),
        json!({"ruleSet":{"match":"none","rules":[{"group":{"rules":[{"field":"album","op":"is","value":1}]}}]}}),
    ] {
        let id = a.execute("album.createSmart", &json!({"name":"Smart","rules":rules})).unwrap()["id"].as_u64().unwrap();
        globals.push(a.catalog.cloud_identity(&format!("album:{id}")).unwrap().to_string());
    }
    a.execute("album.delete", &json!({"id":1})).unwrap();
    let (mut b, files) = peer("fresh");
    b.commit("private album", Op::AddAlbum { album: Album::new(AlbumId(1), "Private") }).unwrap();
    deliver(&a, &mut b, 1);
    let retained = AlbumId(local_id(&b.catalog, "album", &target));
    assert!(b.catalog.album(retained).is_none());
    for global in &globals {
        let id = AlbumId(local_id(&b.catalog, "album", global));
        assert_eq!(b.catalog.album_count(id), 0);
        assert_eq!(b.catalog.smart_album_problems(id)[0].issue, lightcraft_catalog::rules::Issue::NoSuchAlbum);
    }
    let mut restarted = Session::new();
    restarted
        .open_library_in(LibraryStores { dir: "fresh".into(), catalog: Box::new(files), files: Box::new(MemStore::new()), on_disk: false }, false)
        .unwrap();
    let next = restarted.catalog.alloc_album_id();
    assert_ne!(next, retained);
    assert!(restarted.catalog.cloud_identity(&format!("album:{}", next.0)).is_none());
    a.undo_step().unwrap();
    deliver(&a, &mut restarted, 2);
    assert_eq!(local_id(&restarted.catalog, "album", &target), retained.0);
    assert!(restarted.catalog.album(retained).is_some());
    for global in &globals {
        let id = AlbumId(local_id(&restarted.catalog, "album", global));
        assert!(restarted.catalog.smart_album_problems(id).is_empty());
    }
}

#[test]
fn unresolved_album_reference_reserves_identity_until_target_delivery() {
    let (mut a, _) = session();
    let photo = a.catalog.cloud_identity("photo:1").unwrap().to_string();
    let mut state = a.catalog.sync_state().unwrap().clone();
    state
        .enqueue(vec![
            Change { entity: "album:waiting-smart".into(), field: "name".into(), base: None, value: json!("Waiting") },
            Change { entity: "album:waiting-smart".into(), field: "smart".into(), base: None, value: json!({"album":"future-album"}) },
        ])
        .unwrap();
    a.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    let (mut b, _) = peer("receiver");
    deliver(&a, &mut b, 1);
    let target = AlbumId(local_id(&b.catalog, "album", "future-album"));
    let smart = AlbumId(local_id(&b.catalog, "album", "waiting-smart"));
    assert!(b.catalog.album(target).is_none());
    assert_eq!(b.catalog.album_count(smart), 0);
    let mut state = a.catalog.sync_state().unwrap().clone();
    state
        .enqueue(vec![
            Change { entity: "album:future-album".into(), field: "name".into(), base: None, value: json!("Arrived") },
            Change { entity: "album:future-album".into(), field: format!("member:{photo}"), base: None, value: json!(true) },
        ])
        .unwrap();
    a.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    deliver(&a, &mut b, 2);
    assert_eq!(local_id(&b.catalog, "album", "future-album"), target.0);
    assert_eq!(b.catalog.album_count(smart), 1);
}

fn self_publication(s: &mut Session, generation: u64) {
    let state = s.catalog.sync_state().unwrap();
    let remote = state.portable();
    let ids = state.pending.keys().cloned().collect();
    let op = if state.protocol.is_head() {
        incoming_checkpoint(&s.catalog, &remote, &ids, format!("{generation:064x}"), generation).unwrap()
    } else {
        let progress = lightcraft_catalog::replication::AppendProgress {
            version: 1,
            manifest: "a".repeat(64),
            frontiers: lightcraft_catalog::replication::publication_prefixes(&remote).unwrap(),
            remote,
            scan: Default::default(),
            gaps: BTreeSet::new(),
        };
        incoming_append(&s.catalog, progress, &ids).unwrap()
    };
    s.apply_received(op).unwrap();
    s.persist().unwrap();
}
#[test]
fn nonportable_smart_albums_keep_exact_local_rules_through_enablement_and_self_sync() {
    for protocol in [lightcraft_catalog::replication::SyncProtocol::ConditionalHead, lightcraft_catalog::replication::SyncProtocol::AppendLog] {
        let (mut s, _) = session();
        s.apply_received(Op::SetSyncState { state: None }).unwrap();
        let mut local = Photo::new(PhotoId(2), Source::File { path: "/local/a.jpg".into() }, "a.jpg", "JPEG", 32, 24, "");
        local.rating = 4;
        s.commit("local photo", Op::AddPhoto { photo: Box::new(local) }).unwrap();
        s.commit("cloud rating", Op::SetRating { id: PhotoId(1), rating: 4 }).unwrap();
        for (id, filter) in [
            (1, lightcraft_catalog::Filter { folder: Some("/local".into()), rating: 4, ..Default::default() }),
            (2, lightcraft_catalog::Filter { library_folder: Some("/local".into()), rating: 4, ..Default::default() }),
            (3, lightcraft_catalog::Filter { only: vec![PhotoId(2)], ..Default::default() }),
            (4, lightcraft_catalog::Filter { only: vec![PhotoId(1), PhotoId(2)], ..Default::default() }),
            (5, serde_json::from_value(json!({"ruleSet":{"rules":[{"field":"album","op":"is","value":1}]}})).unwrap()),
        ] {
            s.commit("local smart", Op::AddAlbum { album: Album { smart: Some(Box::new(filter)), ..Album::new(AlbumId(id), "Private smart") } })
                .unwrap();
        }
        let before: Vec<_> = s.catalog.albums().map(|a| (a.id, a.smart.clone(), s.catalog.album_photos(a.id))).collect();
        assert_eq!(before[0].2, vec![PhotoId(2)]);
        let op = enable_mode(&s.catalog, "library".into(), "new-author".into(), "personal".into(), protocol, String::new()).unwrap();
        s.apply_system(op).unwrap();
        assert!(s.catalog.sync_state().unwrap().documents.keys().all(|e| !e.starts_with("album:")));
        self_publication(&mut s, 1);
        for (id, filter, matches) in before {
            assert_eq!(s.catalog.album(id).unwrap().smart, filter);
            assert_eq!(s.catalog.album_photos(id), matches);
        }
        let listed = s.execute("albums.list", &json!({})).unwrap();
        assert!(listed.as_array().unwrap().iter().all(|a| a["syncScope"] == "deviceLocal"));
    }
}
#[test]
fn shared_to_local_smart_transition_withdraws_dependents_without_overwriting_origin_rules() {
    let (mut a, _) = session();
    let mut local = Photo::new(PhotoId(2), Source::File { path: "/local/a.jpg".into() }, "a.jpg", "JPEG", 1, 1, "");
    local.rating = 4;
    a.commit("local", Op::AddPhoto { photo: Box::new(local) }).unwrap();
    a.execute("album.createSmart", &json!({"name":"First","rules":{"rating":4}})).unwrap();
    a.execute("album.createSmart", &json!({"name":"Dependent","rules":{"ruleSet":{"rules":[{"field":"album","op":"is","value":1}]}}})).unwrap();
    self_publication(&mut a, 1);
    a.execute("album.setRules", &json!({"id":1,"rules":{"folder":"/local"}})).unwrap();
    let rule = a.catalog.album(AlbumId(1)).unwrap().smart.clone();
    for id in [1, 2] {
        let global = a.catalog.cloud_identity(&format!("album:{id}")).unwrap();
        assert_eq!(a.catalog.sync_state().unwrap().winning(&format!("album:{global}"), "deleted").unwrap().1, &json!(true));
    }
    self_publication(&mut a, 2);
    assert_eq!(a.catalog.album(AlbumId(1)).unwrap().smart, rule);
    assert_eq!(a.catalog.album_photos(AlbumId(1)), vec![PhotoId(2)]);
    assert!(a.catalog.album(AlbumId(2)).is_some());
    a.execute("album.setRules", &json!({"id":1,"rules":{"folder":null}})).unwrap();
    for id in [1, 2] {
        let global = a.catalog.cloud_identity(&format!("album:{id}")).unwrap();
        assert_eq!(a.catalog.sync_state().unwrap().winning(&format!("album:{global}"), "deleted").unwrap().1, &json!(false));
    }
    self_publication(&mut a, 3);
    assert!(device_local_smart_albums(&a.catalog, "personal").is_empty());
}
#[test]
fn missing_cloud_selection_matches_nothing_then_resolves_after_photo_recovery() {
    let (mut a, _) = session();
    a.commit(
        "selected smart",
        Op::AddAlbum {
            album: Album {
                smart: Some(Box::new(lightcraft_catalog::Filter { only: vec![PhotoId(1)], ..Default::default() })),
                ..Album::new(AlbumId(1), "Selected")
            },
        },
    )
    .unwrap();
    let photo = a.catalog.cloud_identity("photo:1").unwrap().to_string();
    let album = a.catalog.cloud_identity("album:1").unwrap().to_string();
    a.commit("delete", Op::RemovePhoto { id: PhotoId(1) }).unwrap();
    assert!(device_local_smart_albums(&a.catalog, "personal").is_empty());
    let (mut b, _) = peer("fresh-selection");
    b.commit(
        "private photo",
        Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), Source::File { path: "private.jpg".into() }, "private.jpg", "JPEG", 1, 1, "")) },
    )
    .unwrap();
    deliver(&a, &mut b, 1);
    let smart = AlbumId(local_id(&b.catalog, "album", &album));
    let filter = b.catalog.album(smart).unwrap().smart.as_ref().unwrap();
    assert!(filter.only.is_empty() && filter.only_restricted);
    assert_eq!(b.catalog.album_count(smart), 0);
    a.execute("cloud.restoreDeleted", &json!({"entity":format!("photo:{photo}")})).unwrap();
    deliver(&a, &mut b, 2);
    assert_eq!(b.catalog.album_count(smart), 1);
    let filter = b.catalog.album(smart).unwrap().smart.as_ref().unwrap();
    assert_eq!(filter.only.len(), 1);
    assert_ne!(filter.only[0], PhotoId(1));
}
#[test]
fn empty_selection_has_a_removable_chip_and_no_unrestricted_fallback() {
    let (mut s, _) = session();
    s.execute("library.filter", &json!({"onlyRestricted":true,"only":[]})).unwrap();
    assert!(s.visible().is_empty());
    let chips = crate::view::filter_chips(&s.filter, &s.catalog);
    let clear = &chips.iter().find(|c| c.clear.get("only").is_some()).unwrap().clear;
    assert_eq!(clear["onlyRestricted"], false);
    s.execute("library.filter", clear).unwrap();
    assert_eq!(s.visible(), vec![PhotoId(1)]);
    s.execute("library.filter", &json!({"onlyRestricted":true,"only":[]})).unwrap();
    s.execute("library.filter", &json!({"only":[]})).unwrap();
    assert_eq!(s.visible(), vec![PhotoId(1)], "legacy clearing must still work after an explicit empty selection");
}

#[test]
fn deletion_wins_offline_edit_without_resurrection_and_explicit_recovery_preserves_the_edit() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    deliver(&a, &mut b, 1);
    a.commit("delete permanently", Op::RemovePhoto { id: PhotoId(1) }).unwrap();
    b.commit("offline rating", Op::SetRating { id: PhotoId(1), rating: 5 }).unwrap();
    deliver(&b, &mut a, 2);
    assert!(a.catalog.photo(PhotoId(1)).is_none(), "offline edit cannot recreate deleted photo");
    deliver(&a, &mut b, 2);
    assert!(b.catalog.photo(PhotoId(1)).unwrap().deleted);
    let entity = format!("photo:{}", a.catalog.cloud_identity("photo:1").unwrap());
    a.execute("cloud.restoreDeleted", &json!({"entity":entity})).unwrap();
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().rating, 5);
    assert!(!a.catalog.photo(PhotoId(1)).unwrap().deleted);
    deliver(&a, &mut b, 3);
    assert_eq!(a.catalog.photo(PhotoId(1)), b.catalog.photo(PhotoId(1)));
    assert!(a.execute("cloud.restoreDeleted", &json!({"entity":entity})).is_err());
}

#[test]
fn independent_offline_imports_keep_distinct_ids_and_replaying_them_is_idempotent() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    let asset = a.catalog.asset(&AssetId("asset".into())).unwrap().clone();
    let photo = (**a.catalog.photo(PhotoId(1)).unwrap()).clone();
    b.commit(
        "offline import",
        Op::Batch { ops: vec![Op::SetAsset { id: AssetId("asset".into()), asset: Some(Box::new(asset)) }, Op::AddPhoto { photo: Box::new(photo) }] },
    )
    .unwrap();
    assert_ne!(a.catalog.cloud_identity("photo:1"), b.catalog.cloud_identity("photo:1"));
    deliver(&a, &mut b, 1);
    deliver(&b, &mut a, 1);
    let before = a.catalog.sync_state().unwrap().received.clone();
    deliver(&b, &mut a, 2);
    assert_eq!(a.catalog.photos().count(), 2);
    assert_eq!(b.catalog.photos().count(), 2);
    assert_eq!(a.catalog.sync_state().unwrap().received, before);
}

#[test]
fn more_than_device_limit_managed_imports_use_one_causal_author() {
    let (s, _) = session();
    let mut catalog = Catalog::new();
    catalog
        .apply(Op::SetAsset { id: AssetId("asset".into()), asset: Some(Box::new(s.catalog.asset(&AssetId("asset".into())).unwrap().clone())) })
        .unwrap();
    for id in 1..=4097 {
        let mut photo = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
        photo.id = PhotoId(id);
        catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    catalog.apply(enable(&catalog, "library".into(), "one-physical-device".into(), "personal".into()).unwrap()).unwrap();
    let state = catalog.sync_state().unwrap();
    assert_eq!(state.documents.keys().filter(|entity| entity.starts_with("photo:")).count(), 4097);
    assert_eq!(state.clock.len(), 1);
    assert!(state.pending.values().all(|mutation| mutation.revision.device == "one-physical-device"));
    let mut peer = SyncState::new("library".into(), "peer".into(), "personal".into()).unwrap();
    for mutation in state.pending.values() {
        peer.receive(mutation).unwrap();
    }
    assert_eq!(peer.documents, state.documents);
    assert_eq!(peer.clock.len(), 1);
    peer.portable().validate().unwrap();
}

#[test]
fn large_existing_library_bootstrap_is_split_into_bounded_durable_mutations() {
    let (s, _) = session();
    let mut catalog = Catalog::new();
    catalog
        .apply(Op::SetAsset { id: AssetId("asset".into()), asset: Some(Box::new(s.catalog.asset(&AssetId("asset".into())).unwrap().clone())) })
        .unwrap();
    for id in 1..=80 {
        let mut photo = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
        photo.id = PhotoId(id);
        photo.meta.caption = "x".repeat(20000);
        catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let op = enable(&catalog, "library".into(), "migrated-device".into(), "personal".into()).unwrap();
    catalog.apply(op).unwrap();
    let state = catalog.sync_state().unwrap();
    assert!(state.pending.len() > 1);
    assert_eq!(state.documents.keys().filter(|id| id.starts_with("photo:")).count(), 80);
    for mutation in state.pending.values() {
        mutation.validate().unwrap();
    }
}

#[test]
fn entity_group_limits_accept_exact_boundaries_and_reject_one_more_without_changes() {
    let state = || SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
    let mut fields = (0..MAX_ENTITY_CHANGES)
        .map(|n| Change { entity: "album:large".into(), field: format!("member:{n}"), base: None, value: json!(true) })
        .collect::<Vec<_>>();
    let mut fit = state();
    enqueue_changes(&mut fit, fields.clone()).unwrap();
    assert_eq!(fit.documents["album:large"].len(), MAX_ENTITY_CHANGES);
    fields.push(Change { entity: "album:large".into(), field: "name".into(), base: None, value: json!("Large album") });
    let mut rejected = state();
    let before = rejected.clone();
    let error = enqueue_changes(&mut rejected, fields).unwrap_err().to_string();
    assert!(error.contains("Large album") && error.contains("4001 changes (limit 4000)") && error.contains("split large albums"), "{error}");
    assert_eq!(rejected, before);

    let mut changes = vec![Change { entity: "photo:one".into(), field: "meta.caption".into(), base: None, value: json!("") }];
    let overhead = serde_json::to_vec(&changes).unwrap().len();
    changes[0].value = json!("x".repeat(MAX_ENTITY_BYTES - overhead));
    assert_eq!(serde_json::to_vec(&changes).unwrap().len(), MAX_ENTITY_BYTES);
    enqueue_changes(&mut fit, changes.clone()).unwrap();
    changes[0].value = json!("x".repeat(MAX_ENTITY_BYTES - overhead + 1));
    let before = fit.clone();
    let error = enqueue_changes(&mut fit, changes).unwrap_err().to_string();
    assert!(error.contains("1048577 bytes (limit 1048576") && error.contains("shorten large metadata"), "{error}");
    assert_eq!(fit, before);
}

#[test]
fn oversized_existing_album_setup_is_atomic_on_both_protocols_and_splitting_recovers() {
    let (s, _) = session();
    let mut catalog = Catalog::new();
    let asset_id = AssetId("asset".into());
    catalog.apply(Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(s.catalog.asset(&asset_id).unwrap().clone())) }).unwrap();
    for id in 1..=MAX_ENTITY_CHANGES as u64 + 1 {
        let mut photo = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
        photo.id = PhotoId(id);
        catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let mut album = Album::new(AlbumId(1), "Oversized trip");
    album.photos = catalog.photos().map(|p| p.id).collect();
    catalog.apply(Op::AddAlbum { album }).unwrap();
    let before = catalog.to_snapshot();
    for protocol in [lightcraft_catalog::replication::SyncProtocol::ConditionalHead, lightcraft_catalog::replication::SyncProtocol::AppendLog] {
        let error = enable_mode(&catalog, "library".into(), "device".into(), "personal".into(), protocol, String::new()).unwrap_err().to_string();
        assert!(error.contains("Oversized trip") && error.contains("limit 4000") && error.contains("split large albums"), "{error}");
        assert_eq!(catalog.to_snapshot(), before);
        assert!(catalog.sync_state().is_none());
        assert!(catalog.cloud_identities().is_empty());
    }
    let photos = catalog.album(AlbumId(1)).unwrap().photos.clone();
    catalog.apply(Op::SetAlbumPhotos { id: AlbumId(1), photos: photos.iter().copied().take(2000).collect() }).unwrap();
    let mut other = Album::new(AlbumId(2), "Trip part two");
    other.photos = photos.iter().copied().skip(2000).collect();
    catalog.apply(Op::AddAlbum { album: other }).unwrap();
    catalog.apply(enable(&catalog, "library".into(), "device".into(), "personal".into()).unwrap()).unwrap();
    let state = catalog.sync_state().unwrap();
    assert_eq!(state.documents.keys().filter(|id| id.starts_with("photo:")).count(), MAX_ENTITY_CHANGES + 1);
    assert_eq!(state.documents.values().flat_map(|f| f.keys()).filter(|k| k.starts_with("member:")).count(), MAX_ENTITY_CHANGES + 1);
    state.validate().unwrap();

    // Swapping disjoint memberships removes 2,000 fields and adds 2,001 in one
    // entity edit. The public commit path must preserve all durable work on failure.
    let replacement = catalog.album(AlbumId(2)).unwrap().photos.clone();
    let mut editing = Session::new();
    editing.catalog = catalog;
    let before = editing.catalog.to_snapshot();
    let revision = editing.catalog.revision;
    let journal = editing.pending_log.len();
    let error = editing.commit("oversized membership edit", Op::SetAlbumPhotos { id: AlbumId(1), photos: replacement }).unwrap_err().to_string();
    assert!(error.contains("4001 changes (limit 4000)") && error.contains("smaller commands"), "{error}");
    assert_eq!(editing.catalog.to_snapshot(), before);
    assert_eq!(editing.catalog.revision, revision);
    assert_eq!(editing.pending_log.len(), journal);
}

#[test]
fn logical_metadata_undo_redo_preserves_remote_caption_and_guards_the_title_revision() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    deliver(&a, &mut b, 1);
    b.selection = crate::Selection::single(PhotoId(1));
    a.execute("photo.setMeta", &json!({"ids":[1],"title":"Local title"})).unwrap();
    b.execute("photo.setMeta", &json!({"ids":[1],"caption":"Remote caption"})).unwrap();
    deliver(&b, &mut a, 2);
    a.undo_step().unwrap();
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().meta.title, "");
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().meta.caption, "Remote caption");
    b.execute("photo.setMeta", &json!({"ids":[1],"caption":"New remote caption"})).unwrap();
    deliver(&b, &mut a, 3);
    a.redo_step().unwrap();
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().meta.title, "Local title");
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().meta.caption, "New remote caption");
    b.execute("photo.setMeta", &json!({"ids":[1],"title":"Competing title"})).unwrap();
    deliver(&b, &mut a, 4);
    let before = a.catalog.to_snapshot();
    assert!(a.undo_step().unwrap_err().to_string().contains("received another revision"));
    assert_eq!(a.catalog.to_snapshot(), before);
}

#[test]
fn logical_membership_undo_redo_preserves_independent_received_members_and_local_members() {
    let (mut a, _) = session();
    let mut second = (**a.catalog.photo(PhotoId(1)).unwrap()).clone();
    second.id = PhotoId(2);
    a.commit("second", Op::AddPhoto { photo: Box::new(second) }).unwrap();
    a.commit(
        "local",
        Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(3), Source::File { path: "private.jpg".into() }, "private.jpg", "JPEG", 1, 1, "")) },
    )
    .unwrap();
    a.commit("album", Op::AddAlbum { album: Album::new(AlbumId(1), "Shared") }).unwrap();
    let (mut b, _) = peer("device-b");
    b.commit(
        "offset",
        Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), Source::File { path: "offset.jpg".into() }, "offset.jpg", "JPEG", 1, 1, "")) },
    )
    .unwrap();
    deliver(&a, &mut b, 1);
    b.selection = crate::Selection::single(PhotoId(1));
    let p = PhotoId(local_id(&b.catalog, "photo", a.catalog.cloud_identity("photo:1").unwrap()));
    let q = PhotoId(local_id(&b.catalog, "photo", a.catalog.cloud_identity("photo:2").unwrap()));
    let album = AlbumId(local_id(&b.catalog, "album", a.catalog.cloud_identity("album:1").unwrap()));
    assert_ne!(p, PhotoId(1));
    a.commit("local members", Op::SetAlbumPhotos { id: AlbumId(1), photos: vec![PhotoId(1), PhotoId(3)] }).unwrap();
    b.commit("remote member", Op::SetAlbumPhotos { id: album, photos: vec![q] }).unwrap();
    deliver(&b, &mut a, 2);
    a.undo_step().unwrap();
    assert_eq!(a.catalog.album(AlbumId(1)).unwrap().photos, vec![PhotoId(2)]);
    b.commit("remove independent member", Op::SetAlbumPhotos { id: album, photos: vec![] }).unwrap();
    deliver(&b, &mut a, 3);
    a.redo_step().unwrap();
    assert_eq!(a.catalog.album(AlbumId(1)).unwrap().photos, vec![PhotoId(1), PhotoId(3)]);
    deliver(&a, &mut b, 4);
    b.commit("remove same member", Op::SetAlbumPhotos { id: album, photos: vec![] }).unwrap();
    b.commit("replace same member", Op::SetAlbumPhotos { id: album, photos: vec![p] }).unwrap();
    deliver(&b, &mut a, 5);
    let before = a.catalog.to_snapshot();
    assert!(a.undo_step().unwrap_err().to_string().contains("received another revision"));
    assert_eq!(a.catalog.to_snapshot(), before);
}

#[test]
fn merged_logical_metadata_inverses_handle_omitted_defaults_without_overwriting_remote_fields() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    deliver(&a, &mut b, 1);
    b.selection = crate::Selection::single(PhotoId(1));
    a.execute("photo.setMeta", &json!({"ids":[1],"title":"Title"})).unwrap();
    a.execute("photo.setMeta", &json!({"ids":[1],"usageTerms":"Contact me","copyrightStatus":"copyrighted"})).unwrap();
    a.merge_undo(2, "Title and rights");
    b.execute("photo.setMeta", &json!({"ids":[1],"caption":"Remote caption"})).unwrap();
    deliver(&b, &mut a, 2);
    a.undo_step().unwrap();
    let meta = &a.catalog.photo(PhotoId(1)).unwrap().meta;
    assert_eq!(meta.title, "");
    assert_eq!(meta.usage_terms, "");
    assert_eq!(meta.copyright_status, lightcraft_catalog::CopyrightStatus::Unknown);
    assert_eq!(meta.caption, "Remote caption");
    a.redo_step().unwrap();
    let meta = &a.catalog.photo(PhotoId(1)).unwrap().meta;
    assert_eq!(meta.title, "Title");
    assert_eq!(meta.usage_terms, "Contact me");
    assert_eq!(meta.caption, "Remote caption");
}

#[test]
fn merged_logical_undo_does_not_forgive_a_remote_revision_between_local_actions() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    deliver(&a, &mut b, 1);
    b.selection = crate::Selection::single(PhotoId(1));
    a.execute("photo.setMeta", &json!({"ids":[1],"title":"Local title"})).unwrap();
    b.execute("photo.setMeta", &json!({"ids":[1],"title":"Remote title"})).unwrap();
    deliver(&b, &mut a, 2);
    a.execute("photo.setMeta", &json!({"ids":[1],"caption":"Local caption"})).unwrap();
    a.merge_undo(2, "Title and caption");
    let before = a.catalog.to_snapshot();
    assert!(a.undo_step().unwrap_err().to_string().contains("received another revision"));
    assert_eq!(a.catalog.to_snapshot(), before);
}

#[test]
fn merged_logical_undo_preserves_a_remote_caption_received_between_local_actions() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    deliver(&a, &mut b, 1);
    b.selection = crate::Selection::single(PhotoId(1));
    a.execute("photo.setMeta", &json!({"ids":[1],"title":"Local title"})).unwrap();
    b.execute("photo.setMeta", &json!({"ids":[1],"caption":"Remote caption"})).unwrap();
    deliver(&b, &mut a, 2);
    a.execute("photo.setMeta", &json!({"ids":[1],"copyright":"Local rights"})).unwrap();
    a.merge_undo(2, "Title and rights");
    a.undo_step().unwrap();
    let meta = &a.catalog.photo(PhotoId(1)).unwrap().meta;
    assert_eq!(meta.title, "");
    assert_eq!(meta.copyright, "");
    assert_eq!(meta.caption, "Remote caption");
    a.redo_step().unwrap();
    let meta = &a.catalog.photo(PhotoId(1)).unwrap().meta;
    assert_eq!(meta.title, "Local title");
    assert_eq!(meta.copyright, "Local rights");
    assert_eq!(meta.caption, "Remote caption");
}

#[test]
fn logical_restore_guards_different_retained_fields_instead_of_recertifying_an_old_payload() {
    let (mut a, _) = session();
    let mut photo = (**a.catalog.photo(PhotoId(1)).unwrap()).clone();
    photo.id = PhotoId(2);
    a.commit(
        "copy",
        Op::Batch { ops: vec![Op::AddPhoto { photo: Box::new(photo) }, Op::SetCloudIdentity { entity: "photo:2".into(), id: Some("copy".into()) }] },
    )
    .unwrap();
    a.undo_step().unwrap();
    let mut peer = SyncState::new("library".into(), "remote".into(), "personal".into()).unwrap();
    peer.merge(&a.catalog.sync_state().unwrap().portable()).unwrap();
    let mutation = peer
        .enqueue(vec![Change {
            entity: "photo:copy".into(),
            field: "meta.caption".into(),
            base: peer.winning("photo:copy", "meta.caption").map(|(stamp, _)| stamp),
            value: json!("Remote retained caption"),
        }])
        .unwrap()
        .unwrap();
    let mut received = a.catalog.sync_state().unwrap().clone();
    received.receive(&mutation).unwrap();
    a.apply_received(Op::SetSyncState { state: Some(Box::new(received)) }).unwrap();
    let before = a.catalog.to_snapshot();
    assert!(a.redo_step().unwrap_err().to_string().contains("received another revision"));
    assert_eq!(a.catalog.to_snapshot(), before);
}

#[test]
fn any_field_conflict_lists_named_choices_and_keeping_another_value_resolves_it_everywhere() {
    let (mut a, _) = session();
    let (mut b, _) = peer("device-b");
    deliver(&a, &mut b, 1);
    a.commit("rating", Op::SetRating { id: PhotoId(1), rating: 2 }).unwrap();
    b.commit("rating", Op::SetRating { id: PhotoId(1), rating: 5 }).unwrap();
    deliver(&a, &mut b, 2);
    deliver(&b, &mut a, 2);
    let status = a.execute("cloud.syncStatus", &json!({})).unwrap();
    let entity = format!("photo:{}", a.catalog.cloud_identity("photo:1").unwrap());
    let conflict = status["conflicts"].as_array().unwrap().iter().find(|c| c["entity"] == entity && c["field"] == "rating").unwrap().clone();
    assert_eq!(conflict["name"], a.catalog.photo(PhotoId(1)).unwrap().file_name.as_str());
    let choices = conflict["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0]["current"], true, "the displayed value is listed first");
    assert_eq!(choices.iter().filter(|c| c["thisDevice"] == true).count(), 1);
    let shown = a.catalog.photo(PhotoId(1)).unwrap().rating;
    let other = choices.iter().find(|c| c["current"] == false).unwrap();
    assert_eq!(other["kind"], "value");
    assert_ne!(other["preview"], shown.to_string());
    let overview = a.execute("cloud.overview", &json!({})).unwrap();
    assert_eq!(overview["health"]["conflicts"]["fields"], 1);
    a.execute("cloud.resolveConflict", &json!({"entity":entity,"field":"rating","revision":other["revision"]})).unwrap();
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().rating.to_string(), other["preview"].as_str().unwrap());
    assert!(a.execute("cloud.syncStatus", &json!({})).unwrap()["conflicts"].as_array().unwrap().is_empty());
    deliver(&a, &mut b, 3);
    assert!(b.execute("cloud.syncStatus", &json!({})).unwrap()["conflicts"].as_array().unwrap().is_empty());
    assert_eq!(a.catalog.photo(PhotoId(1)).unwrap().rating, b.catalog.photo(PhotoId(1)).unwrap().rating);
}

#[test]
fn deleted_photo_list_is_computed_once_per_revision_and_capped_with_a_full_count() {
    let (mut s, _) = session();
    let template = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    let extra = crate::cmd::cloud_sync::MAX_DELETED_LISTED + 4;
    for n in 2..=(extra as u64 + 1) {
        let mut photo = template.clone();
        photo.id = PhotoId(n);
        photo.file_name = format!("photo-{n}.jpg");
        s.commit("add", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    for n in 2..=(extra as u64 + 1) {
        s.commit("delete", Op::RemovePhoto { id: PhotoId(n) }).unwrap();
    }
    let status = s.execute("cloud.syncStatus", &json!({})).unwrap();
    assert_eq!(status["deletedPhotoCount"], extra);
    let listed = status["deletedPhotos"].as_array().unwrap();
    assert_eq!(listed.len(), crate::cmd::cloud_sync::MAX_DELETED_LISTED);
    assert!(listed[0]["name"].as_str().unwrap().starts_with("photo-"));
    s.cloud_conflicts_cache.as_mut().unwrap().2["reuseMarker"] = json!(true);
    s.execute("cloud.syncStatus", &json!({})).unwrap();
    assert_eq!(s.cloud_conflicts_cache.as_ref().unwrap().2["reuseMarker"], true, "unchanged revision reuses the cached lists");
    let entity = listed[0]["entity"].as_str().unwrap().to_owned();
    s.execute("cloud.restoreDeleted", &json!({"entity":entity})).unwrap();
    assert_eq!(s.execute("cloud.syncStatus", &json!({})).unwrap()["deletedPhotoCount"], extra - 1);
}
