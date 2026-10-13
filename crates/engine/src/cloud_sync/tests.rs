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
    assert!(last.contains("setRating") && last.contains("setSyncState"));
    let value: Value = serde_json::from_str(&last).unwrap();
    assert_eq!(value["op"]["op"], json!("batch"));
    let (mut journal, loaded, _) = Journal::open(Box::new(store.clone())).unwrap();
    journal.snapshot(&loaded).unwrap();
    let (_, compacted, _) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(compacted.sync_state(), s.catalog.sync_state());
    assert_eq!(compacted.photo(PhotoId(1)).unwrap().rating, 5);
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
