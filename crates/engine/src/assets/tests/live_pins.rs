use super::*;
use crate::assets::cache::{OfflinePin, OfflineQuality};
use lightcraft_catalog::assets::AssetId;
use lightcraft_catalog::{AlbumId, Op, PhotoId, Source};
use serde_json::json;
use std::collections::BTreeSet;

struct FailingAppend {
    inner: lightcraft_catalog::FsStore,
    fail: Arc<AtomicBool>,
}
impl lightcraft_catalog::Store for FailingAppend {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.read(name)
    }
    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.inner.write_atomic(name, bytes)
    }
    fn append(&mut self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        if self.fail.load(Ordering::Acquire) {
            return Err(std::io::Error::other("test disk full"));
        }
        self.inner.append(name, bytes)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.inner.truncate(name, len)
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

fn session(scratch: &Scratch) -> crate::Session {
    let mut s = crate::Session::new().with_fs();
    s.open_library(&scratch.0, false).unwrap();
    s
}

fn attach(s: &mut crate::Session, name: &str, bytes: &[u8]) -> PhotoId {
    PhotoId(
        s.execute(
            "cloud.attach",
            &json!({
                "assetId": name, "asset": Asset { original: blob(bytes), smart_preview: None, thumbnail: None, associated: Default::default() },
                "fileName": format!("{name}.raw"), "width": 32, "height": 24, "kind": "raw"
            }),
        )
        .unwrap()["id"]
            .as_u64()
            .unwrap(),
    )
}

fn idle(s: &crate::Session) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.media.asset_resolver.as_ref().unwrap().keep_all_running.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!s.media.asset_resolver.as_ref().unwrap().keep_all_running.load(Ordering::Acquire));
}

#[test]
fn oversized_original_pins_protect_required_preview_and_missing_preview_reports_unready() {
    let original = blob(b"original");
    let preview = blob(b"preview");
    let fixture = |with_preview: bool| {
        let mut s = crate::Session::new();
        s.commit(
            "oversized offline fixture",
            Op::Batch {
                ops: vec![
                    Op::SetAsset {
                        id: AssetId("large".into()),
                        asset: Some(Box::new(Asset {
                            original: original.clone(),
                            smart_preview: with_preview.then(|| preview.clone()),
                            thumbnail: None,
                            associated: Default::default(),
                        })),
                    },
                    Op::AddPhoto {
                        photo: Box::new(lightcraft_catalog::Photo::new(
                            PhotoId(1),
                            Source::Managed { asset_id: AssetId("large".into()) },
                            "large.raw",
                            "RAW",
                            20000,
                            10000,
                            "",
                        )),
                    },
                ],
            },
        )
        .unwrap();
        s
    };
    let owners = BTreeMap::from([
        ("photo:1".into(), OfflinePin { quality: OfflineQuality::Original, blobs: Vec::new() }),
        ("all:originals".into(), OfflinePin { quality: OfflineQuality::Original, blobs: Vec::new() }),
        ("all:previews".into(), OfflinePin { quality: OfflineQuality::Preview, blobs: Vec::new() }),
    ]);
    let s = fixture(true);
    let result = super::super::offline::resolve(&s.catalog, &owners);
    assert!(result.errors.is_empty());
    for owner in ["photo:1", "all:originals"] {
        assert_eq!(result.pins[owner].blobs.iter().cloned().collect::<BTreeSet<_>>(), [original.id.clone(), preview.id.clone()].into());
    }
    assert_eq!(result.pins["all:previews"].blobs, vec![preview.id.clone()]);
    let s = fixture(false);
    let result = super::super::offline::resolve(&s.catalog, &owners);
    assert_eq!(result.errors.len(), 3);
    assert_eq!(result.pins["photo:1"].blobs, vec![original.id.clone()], "a missing preview cannot relax original protection");
    assert_eq!(result.pins["all:originals"].blobs, vec![original.id]);
    assert!(result.errors["all:originals"].contains("publish one from desktop"));
}
#[test]
fn empty_album_policy_follows_membership_undo_and_reopen_without_saved_blob_lists() {
    let scratch = Scratch::new();
    let mut s = session(&scratch);
    let resolver = s.media.asset_resolver.as_ref().unwrap().clone();
    resolver.connect("memory".into(), map_store(&[b"first", b"second"])).unwrap();
    let album = s.execute("album.create", &json!({"name": "Offline", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    let owner = format!("album:{album}");
    s.execute("cloud.pin", &json!({"albumId": album})).unwrap();
    assert!(resolver.offline_pin(&owner).unwrap().blobs.is_empty());
    let first = attach(&mut s, "first", b"first");
    s.execute("cloud.cache", &json!({"budgetMb": 0})).unwrap();
    s.execute("album.addPhotos", &json!({"id": album, "ids": [first.0]})).unwrap();
    idle(&s);
    assert_eq!(resolver.offline_pin(&owner).unwrap().blobs, vec![sha256(b"first")]);
    assert!(resolver.root().join(sha256(b"first").0).is_file());
    assert!(resolver.evict(&blob(b"first")).is_err());
    s.execute("album.removePhotos", &json!({"id": album, "ids": [first.0]})).unwrap();
    idle(&s);
    assert!(resolver.offline_pin(&owner).unwrap().blobs.is_empty());
    resolver.clear_cache().unwrap();
    assert!(!resolver.root().join(sha256(b"first").0).exists());
    s.execute("edit.undo", &json!({})).unwrap();
    idle(&s);
    assert!(resolver.root().join(sha256(b"first").0).is_file());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(resolver.root().join("retention.json")).unwrap()).unwrap();
    assert_eq!(saved["version"], 3);
    assert_eq!(saved["pins"][&owner]["quality"], "original");
    assert_eq!(saved["pins"][&owner]["blobs"], json!([]));
    s.close_library().unwrap();
    drop(s);
    let mut reopened = session(&scratch);
    let resolver = reopened.media.asset_resolver.as_ref().unwrap().clone();
    idle(&reopened);
    assert_eq!(resolver.offline_pin(&owner).unwrap().blobs, vec![sha256(b"first")]);
    assert_eq!(resolver.clear_cache().unwrap(), 0);
    assert_eq!(resolver.acquire(&blob(b"first"), &AtomicBool::new(false)).unwrap().read_bytes().unwrap(), b"first");
    resolver.connect("memory".into(), map_store(&[b"second"])).unwrap();
    let second = attach(&mut reopened, "second", b"second");
    reopened.execute("album.addPhotos", &json!({"id": album, "ids": [second.0]})).unwrap();
    idle(&reopened);
    assert_eq!(resolver.offline_pin(&owner).unwrap().blobs.len(), 2);
    assert_eq!(reopened.catalog.album(AlbumId(album)).unwrap().photos, vec![first, second]);
    assert!(resolver.root().join(sha256(b"second").0).is_file());
}

#[test]
fn photo_revision_keeps_previous_dependency_until_catalog_persistence_then_follows_undo() {
    let scratch = Scratch::new();
    let fail = Arc::new(AtomicBool::new(false));
    let mut s = crate::Session::new().with_fs();
    let stores = crate::library::LibraryStores {
        dir: scratch.0.clone(),
        catalog: Box::new(FailingAppend { inner: lightcraft_catalog::FsStore::open(&scratch.0).unwrap(), fail: fail.clone() }),
        files: Box::new(lightcraft_catalog::FsStore::open(&scratch.0).unwrap()),
        on_disk: true,
    };
    s.open_library_in_with_cache(stores, false, scratch.0.join("assets-cache")).unwrap();
    let resolver = s.media.asset_resolver.as_ref().unwrap().clone();
    resolver.connect("memory".into(), map_store(&[b"old original", b"new original"])).unwrap();
    let id = attach(&mut s, "old", b"old original");
    s.execute("cloud.pin", &json!({"ids": [id.0], "wait": true})).unwrap();
    idle(&s);
    let asset_id = lightcraft_catalog::assets::AssetId("revision".into());
    let old = blob(b"old original");
    let new = blob(b"new original");
    s.commit(
        "Revision",
        Op::Batch {
            ops: vec![
                Op::SetAsset {
                    id: asset_id.clone(),
                    asset: Some(Box::new(Asset { original: new.clone(), smart_preview: None, thumbnail: None, associated: Default::default() })),
                },
                Op::Relink { id, file_name: "new.raw".into(), source: Source::Managed { asset_id }, format: None },
            ],
        },
    )
    .unwrap();
    assert_eq!(resolver.offline_pin(&format!("photo:{}", id.0)).unwrap().blobs, vec![new.id.clone()]);
    assert!(resolver.evict(&old).is_err(), "old dependency remains protected until the catalog append succeeds");
    fail.store(true, Ordering::Release);
    assert!(s.persist().is_err());
    assert!(resolver.evict(&old).is_err(), "a failed save cannot release prior offline coverage");
    fail.store(false, Ordering::Release);
    s.persist().unwrap();
    idle(&s);
    assert_eq!(resolver.acquire(&new, &AtomicBool::new(false)).unwrap().read_bytes().unwrap(), b"new original");
    resolver.evict(&old).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    idle(&s);
    assert_eq!(resolver.offline_pin(&format!("photo:{}", id.0)).unwrap().blobs, vec![old.id.clone()]);
    assert!(resolver.evict(&old).is_err());
    resolver.evict(&new).unwrap();
}

#[test]
fn smart_album_policy_follows_metadata_rules_and_received_changes() {
    let scratch = Scratch::new();
    let mut s = session(&scratch);
    let resolver = s.media.asset_resolver.as_ref().unwrap().clone();
    resolver.connect("memory".into(), map_store(&[b"smart member"])).unwrap();
    let id = attach(&mut s, "smart", b"smart member");
    let album = s.execute("album.createSmart", &json!({"name": "Five stars", "rules": {"rating": 5}})).unwrap()["id"].as_u64().unwrap();
    let owner = format!("album:{album}");
    s.execute("cloud.pin", &json!({"albumId": album})).unwrap();
    assert!(resolver.offline_pin(&owner).unwrap().blobs.is_empty());
    s.apply_received(Op::SetRating { id, rating: 5 }).unwrap();
    s.persist().unwrap();
    idle(&s);
    assert_eq!(resolver.offline_pin(&owner).unwrap().blobs, vec![sha256(b"smart member")]);
    s.execute("album.setRules", &json!({"id": album, "rules": {"flag": "pick"}})).unwrap();
    assert!(resolver.offline_pin(&owner).unwrap().blobs.is_empty());
    s.execute("edit.undo", &json!({})).unwrap();
    idle(&s);
    assert_eq!(resolver.offline_pin(&owner).unwrap().blobs.len(), 1);
    s.execute("photo.delete", &json!({"ids": [id.0]})).unwrap();
    assert!(resolver.offline_pin(&owner).unwrap().blobs.is_empty());
    s.execute("edit.undo", &json!({})).unwrap();
    idle(&s);
    assert_eq!(resolver.offline_pin(&owner).unwrap().blobs.len(), 1);
}

#[test]
fn preview_policy_reports_missing_new_rendition_and_retains_owner_for_repair() {
    let scratch = Scratch::new();
    let mut s = session(&scratch);
    let resolver = s.media.asset_resolver.as_ref().unwrap().clone();
    let id = attach(&mut s, "no-preview", b"original");
    let owner = format!("photo:{}", id.0);
    // A saved preview policy may outlive a source revision's rendition. It stays explicit,
    // rather than downloading an unexpected full original or forgetting the owner.
    resolver.set_offline_pins(&[(owner.clone(), Some(cache::OfflinePin { quality: cache::OfflineQuality::Preview, blobs: Vec::new() }))]).unwrap();
    crate::cmd::cloud::refresh_offline_pins(&s, true);
    assert!(resolver.offline_pin(&owner).unwrap().blobs.is_empty());
    assert!(resolver.cache_status().offline_errors[&owner].contains("Preview"));
    assert!(s.execute("cloud.storage", &json!({})).unwrap()["offlineErrors"][&owner].as_str().unwrap().contains("repair"));
    assert!(resolver.offline_owners().contains_key(&owner));
    s.execute("cloud.unpin", &json!({"owner": owner})).unwrap();
    assert!(resolver.cache_status().offline_errors.is_empty());
}

#[test]
fn preview_owner_follows_new_rendition_without_fetching_the_original() {
    let scratch = Scratch::new();
    let mut s = session(&scratch);
    let resolver = s.media.asset_resolver.as_ref().unwrap().clone();
    let provider = map_store(&[b"first preview", b"new preview"]);
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let old = Asset { original: blob(b"original"), smart_preview: Some(blob(b"first preview")), thumbnail: None, associated: Default::default() };
    let id = PhotoId(
        s.execute("cloud.attach", &json!({"assetId": "preview-original", "asset": old, "fileName": "preview.raw", "width": 32, "height": 24}))
            .unwrap()["id"]
            .as_u64()
            .unwrap(),
    );
    s.execute("cloud.pin", &json!({"ids": [id.0], "quality": "preview", "wait": true})).unwrap();
    let new =
        Asset { original: blob(b"revised original"), smart_preview: Some(blob(b"new preview")), thumbnail: None, associated: Default::default() };
    let asset_id = lightcraft_catalog::assets::AssetId("preview-revision".into());
    s.commit(
        "New preview",
        Op::Batch {
            ops: vec![
                Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(new)) },
                Op::Relink { id, file_name: "revised.raw".into(), source: Source::Managed { asset_id }, format: None },
            ],
        },
    )
    .unwrap();
    s.persist().unwrap();
    idle(&s);
    assert_eq!(provider.1.load(Ordering::Acquire), 2, "each preview downloaded once, no original requests");
    assert_eq!(resolver.offline_pin(&format!("photo:{}", id.0)).unwrap().blobs, vec![sha256(b"new preview")]);
    assert!(resolver.cache_status().offline_errors.is_empty());
    assert!(resolver.evict(&blob(b"new preview")).is_err());
    s.close_library().unwrap();
    drop(s);
    let reopened = session(&scratch);
    idle(&reopened);
    let resolver = reopened.media.asset_resolver.as_ref().unwrap();
    assert_eq!(resolver.offline_pin(&format!("photo:{}", id.0)).unwrap().quality, cache::OfflineQuality::Preview);
    assert_eq!(resolver.acquire(&blob(b"new preview"), &AtomicBool::new(false)).unwrap().read_bytes().unwrap(), b"new preview");
}

#[test]
fn a_member_removed_evicted_and_restored_during_another_download_is_acquired_again() {
    struct Held {
        inner: Arc<MapStore>,
        held: BlobId,
        entered: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl BlobStore for Held {
        fn download(&self, blob: &Blob, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
            if blob.id == self.held {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv_timeout(std::time::Duration::from_secs(20)).unwrap();
            }
            self.inner.download(blob, out, cancel, progress)
        }
    }
    let scratch = Scratch::new();
    let mut s = session(&scratch);
    let resolver = s.media.asset_resolver.as_ref().unwrap().clone();
    let mut contents: Vec<&[u8]> = vec![b"first member", b"held member"];
    contents.sort_by_key(|bytes| sha256(bytes));
    let (entered_tx, entered) = std::sync::mpsc::sync_channel(1);
    let (release, release_rx) = std::sync::mpsc::sync_channel(1);
    let inner = map_store(&contents);
    resolver
        .connect(
            "memory".into(),
            Arc::new(Held { inner: inner.clone(), held: sha256(contents[1]), entered: entered_tx, release: Mutex::new(release_rx) }),
        )
        .unwrap();
    let a = attach(&mut s, "returning", contents[0]);
    let b = attach(&mut s, "held", contents[1]);
    let album = s.execute("album.create", &json!({"name": "Live", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    s.execute("cloud.pin", &json!({"albumId": album})).unwrap();
    s.execute("album.addPhotos", &json!({"id": album, "ids": [a.0, b.0]})).unwrap();
    entered.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    s.execute("album.removePhotos", &json!({"id": album, "ids": [a.0]})).unwrap();
    let evicted = resolver.evict(&blob(contents[0]));
    let absent = !resolver.root().join(sha256(contents[0]).0).exists();
    let undo = s.execute("edit.undo", &json!({}));
    release.send(()).unwrap();
    idle(&s);
    evicted.unwrap();
    assert!(absent);
    undo.unwrap();
    assert!(resolver.root().join(sha256(contents[0]).0).is_file(), "the running worker must revisit newly restored demand");
    assert_eq!(inner.1.load(Ordering::Acquire), 3, "A once, held B once, restored A once");
}
