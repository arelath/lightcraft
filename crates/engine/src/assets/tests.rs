use super::*;
use lightcraft_catalog::assets::RemoteLocation;
use std::sync::atomic::AtomicU64;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("lightcraft-cloud-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct MemoryStore {
    bytes: Vec<u8>,
    reads: AtomicUsize,
    error: Option<AssetError>,
}
impl BlobStore for MemoryStore {
    fn download(&self, _: &Blob, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        if let Some(error) = &self.error {
            out.write_all(b"partial").map_err(disk)?;
            return Err(error.clone());
        }
        out.write_all(&self.bytes).map_err(disk)?;
        progress(self.bytes.len() as u64);
        Ok(())
    }
}

fn blob(bytes: &[u8]) -> Blob {
    Blob {
        id: sha256(bytes),
        size: bytes.len() as u64,
        remote: RemoteLocation { connection: "memory".into(), key: "library/original".into(), version: None },
    }
}
fn store(bytes: &[u8]) -> Arc<MemoryStore> {
    Arc::new(MemoryStore { bytes: bytes.to_vec(), reads: AtomicUsize::new(0), error: None })
}

#[test]
fn reopen_verifies_and_reuses_cache_without_a_connection() {
    let scratch = Scratch::new();
    let bytes = b"a safely downloaded original";
    let b = blob(bytes);
    let provider = store(bytes);
    let resolver = AssetResolver::new(scratch.0.clone());
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let lease = resolver.acquire(&b, &AtomicBool::new(false)).unwrap();
    assert_eq!(lease.read_bytes().unwrap(), bytes);
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
    assert!(resolver.evict(&b).is_err(), "active leases prevent eviction");
    drop(lease);
    let reopened = AssetResolver::new(scratch.0.clone());
    assert_eq!(reopened.acquire(&b, &AtomicBool::new(false)).unwrap().read_bytes().unwrap(), bytes);
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
}

#[test]
fn simultaneous_requests_download_once_and_keep_independent_leases() {
    let scratch = Scratch::new();
    let bytes = b"shared original";
    let b = blob(bytes);
    let provider = store(bytes);
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (r, b, start) = (resolver.clone(), b.clone(), barrier.clone());
            std::thread::spawn(move || {
                start.wait();
                r.acquire(&b, &AtomicBool::new(false)).unwrap()
            })
        })
        .collect();
    let leases: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
    assert!(resolver.evict(&b).is_err());
    drop(leases);
    resolver.evict(&b).unwrap();
    assert_eq!(resolver.availability(&b.id), AssetAvailability::Remote);
}

#[test]
fn partial_corrupt_cancelled_and_unauthorized_downloads_never_publish() {
    for error in [AssetError::Network("interrupted".into()), AssetError::Unauthorized, AssetError::Missing, AssetError::Cancelled] {
        let scratch = Scratch::new();
        let b = blob(b"expected original");
        let resolver = AssetResolver::new(scratch.0.clone());
        resolver
            .connect("memory".into(), Arc::new(MemoryStore { bytes: Vec::new(), reads: AtomicUsize::new(0), error: Some(error.clone()) }))
            .unwrap();
        assert!(matches!(resolver.acquire(&b, &AtomicBool::new(false)), Err(e) if e == error));
        assert!(!scratch.0.join(&b.id.0).exists());
        assert!(std::fs::read_dir(&scratch.0).unwrap().next().is_none(), "partial temp files removed");
        if error == AssetError::Missing {
            assert_eq!(resolver.availability(&b.id), AssetAvailability::Missing);
        }
    }
    let scratch = Scratch::new();
    let b = blob(b"expected original");
    let resolver = AssetResolver::new(scratch.0.clone());
    resolver.connect("memory".into(), store(b"corrupted content")).unwrap();
    assert!(matches!(resolver.acquire(&b, &AtomicBool::new(false)), Err(AssetError::Integrity)));
    assert!(!scratch.0.join(&b.id.0).exists());
}

#[test]
fn a_failed_cache_write_keeps_previous_bytes_and_can_retry() {
    let scratch = Scratch::new();
    let b = blob(b"expected original");
    let resolver = AssetResolver::new(scratch.0.clone());
    let provider = store(b"expected original");
    resolver.connect("memory".into(), provider).unwrap();
    std::fs::write(scratch.0.join(&b.id.0), b"corrupt old file").unwrap();
    let fault = lightcraft_catalog::safe_file::fail_writes_after(3);
    assert!(matches!(resolver.acquire(&b, &AtomicBool::new(false)), Err(AssetError::Disk(_))));
    assert_eq!(std::fs::read(scratch.0.join(&b.id.0)).unwrap(), b"corrupt old file");
    drop(fault);
    assert_eq!(resolver.acquire(&b, &AtomicBool::new(false)).unwrap().read_bytes().unwrap(), b"expected original");
}

#[test]
fn hostile_identity_cannot_escape_cache_or_start_a_request() {
    let scratch = Scratch::new();
    let mut b = blob(b"expected original");
    b.id = BlobId("../../outside".into());
    let provider = store(b"expected original");
    let resolver = AssetResolver::new(scratch.0.clone());
    resolver.connect("memory".into(), provider.clone()).unwrap();
    assert!(resolver.acquire(&b, &AtomicBool::new(false)).is_err());
    assert_eq!(provider.reads.load(Ordering::Relaxed), 0);
    assert!(std::fs::read_dir(&scratch.0).unwrap().next().is_none());
}

#[test]
fn cached_acquisition_never_starts_a_network_request() {
    let scratch = Scratch::new();
    let resolver = AssetResolver::new(scratch.0.clone());
    let provider = store(b"original");
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let b = blob(b"original");
    assert!(resolver.acquire_cached(&b, &AtomicBool::new(false)).unwrap().is_none());
    assert_eq!(provider.reads.load(Ordering::Relaxed), 0);
    std::fs::write(scratch.0.join(&b.id.0), b"corrupt!").unwrap();
    assert!(resolver.acquire_cached(&b, &AtomicBool::new(false)).unwrap().is_none());
    assert_eq!(provider.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn ui_scope_uses_cached_bytes_without_starting_a_transfer() {
    let scratch = Scratch::new();
    let resolver = AssetResolver::new(scratch.0.clone());
    let provider = store(b"original");
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let b = blob(b"original");
    {
        let _scope = CachedOnly::enter(true);
        let _nested = CachedOnly::enter(false);
        assert!(matches!(resolver.acquire(&b, &AtomicBool::new(false)), Err(AssetError::Pending)));
        assert_eq!(provider.reads.load(Ordering::Relaxed), 0);
        assert_eq!(resolver.availability(&b.id), AssetAvailability::Remote);
    }
    drop(resolver.acquire(&b, &AtomicBool::new(false)).unwrap());
    let _scope = CachedOnly::enter(true);
    assert_eq!(resolver.acquire(&b, &AtomicBool::new(false)).unwrap().read_bytes().unwrap(), b"original");
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
}

#[test]
fn ui_scope_does_not_wait_for_an_in_progress_transfer() {
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let b = blob(b"original");
    let entry = resolver.entry(&b.id).unwrap();
    let gate = entry.gate.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _scope = CachedOnly::enter(true);
        tx.send(resolver.acquire(&b, &AtomicBool::new(false)).err()).unwrap();
    });
    let result = rx.recv_timeout(std::time::Duration::from_secs(1));
    drop(gate); // Always unblock a regressed worker before the assertion/join.
    worker.join().unwrap();
    assert_eq!(result.unwrap(), Some(AssetError::Pending));
}

#[test]
fn private_managed_handles_do_not_capture_valid_local_filenames() {
    let mut session = crate::Session::new();
    session.media.file_bytes = Some(Arc::new(|path| {
        assert_eq!(path, "lcasset:local-original");
        Ok(b"local bytes".to_vec())
    }));
    let source = lightcraft_catalog::Source::File { path: "lcasset:local-original".into() };
    let reader = session.original_reader_for(std::slice::from_ref(&source));
    assert_eq!(reader(&source_path(&source).unwrap()).unwrap(), b"local bytes");
}

#[test]
fn user_outputs_cannot_replace_managed_cache_or_create_future_blob_files() {
    let scratch = Scratch::new();
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    let bytes = b"original bytes";
    let asset = Asset { original: blob(bytes), smart_preview: None, thumbnail: None };
    session
        .execute(
            "cloud.attach",
            &serde_json::json!({"assetId": "protected", "asset": asset,
        "fileName": "original.jpg", "width": 4, "height": 3}),
        )
        .unwrap();
    let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
    resolver.connect("memory".into(), store(bytes)).unwrap();
    let lease = resolver.acquire(&blob(bytes), &AtomicBool::new(false)).unwrap();
    assert!(session.check_write_target(&lease.path().to_string_lossy()).is_err());
    let future = resolver.root().join("new-folder").join("a".repeat(64));
    assert!(session.check_write_target(&future.to_string_lossy()).is_err());
    assert!(session.original_guard().check(&future).is_err(), "prepared exports protect prospective paths too");
    let alias = resolver.root().join("future").join("..").join("b".repeat(64));
    assert!(session.check_write_target(&alias.to_string_lossy()).is_err());
    assert!(session.check_write_target(&scratch.0.join("export.png").to_string_lossy()).is_ok());
    assert_eq!(lease.read_bytes().unwrap(), bytes);
    #[cfg(unix)]
    {
        let linked = scratch.0.join("cache-link");
        std::os::unix::fs::symlink(resolver.root(), &linked).unwrap();
        assert!(session.check_write_target(&linked.join("future.png").to_string_lossy()).is_err());
    }
}

#[test]
fn catalog_storage_and_local_caches_are_independent() {
    let scratch = Scratch::new();
    let mut session = crate::Session::new().with_fs();
    let store = lightcraft_catalog::MemStore::new();
    session
        .open_library_in_with_cache(
            crate::library::LibraryStores { dir: "shared-library".into(), catalog: Box::new(store.clone()), files: Box::new(store), on_disk: false },
            false,
            scratch.0.clone(),
        )
        .unwrap();
    let library = session.library.as_ref().unwrap();
    assert!(!library.on_disk, "filesystem original management remains disabled");
    assert_eq!(library.cache_dir.as_ref(), Some(&scratch.0));
    assert_eq!(library.thumbs_dir(), scratch.0.join("thumbs"));
    assert!(session.media.asset_resolver.is_some());
    assert_eq!(session.media.smart_dir, Some(crate::smart::dir(&scratch.0)));
}

#[test]
fn cloud_photo_opens_develops_exports_and_reopens() {
    let scratch = Scratch::new();
    let bytes = crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default());
    let asset = Asset { original: blob(&bytes), smart_preview: None, thumbnail: None };
    let provider = store(&bytes);
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    session.media.asset_resolver.as_ref().unwrap().connect("memory".into(), provider.clone()).unwrap();
    let attach = serde_json::json!({"assetId": "shared-image", "asset": asset, "fileName": "cloud.dng", "width": 32, "height": 24, "format": "DNG", "kind": "raw"});
    let id = lightcraft_catalog::PhotoId(session.execute("cloud.attach", &attach).unwrap()["id"].as_u64().unwrap());
    assert_eq!(session.execute("library.missing", &serde_json::json!({})).unwrap(), serde_json::json!([]));
    let render = session.render_now(id, 32, 24).unwrap();
    assert_eq!(render.image.width, 32);
    session.execute("develop.set", &serde_json::json!({"id": id.0, "control": "light.exposure", "value": 1.0})).unwrap();
    let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Png, resize: None, ..Default::default() };
    let exported = crate::export::prepare_export(&mut session, id, &options, 1).unwrap().run().unwrap();
    assert!(!exported.bytes.is_empty());
    let original = crate::export::ExportOptions { format: crate::export::ExportFormat::Original, ..Default::default() };
    assert_eq!(crate::export::prepare_export(&mut session, id, &original, 1).unwrap().run().unwrap().bytes, bytes);
    session.close_library().unwrap();
    drop(session);
    let mut reopened = crate::Session::new().with_fs();
    reopened.open_library(&scratch.0, false).unwrap();
    assert_eq!(reopened.catalog.photo(id).unwrap().develop.light.exposure, 1.0);
    assert!(reopened.render_now(id, 32, 24).is_ok());
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
}

#[test]
fn export_requires_original_even_if_a_preview_is_cached() {
    let scratch = Scratch::new();
    let img = lightcraft_raster::Rgb32f::from_fn(16, 12, |_, _| [0.2, 0.3, 0.4]);
    let preview = crate::smart::encode(&img, None).unwrap();
    let asset = Asset { original: blob(b"unavailable original"), smart_preview: Some(blob(&preview)), thumbnail: None };
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    resolver.connect("memory".into(), store(&preview)).unwrap();
    let mut session = crate::Session::new().with_fs();
    session.media.asset_resolver = Some(resolver);
    let id = lightcraft_catalog::PhotoId(1);
    let asset_id = lightcraft_catalog::assets::AssetId("photo".into());
    session.catalog.apply(lightcraft_catalog::Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) }).unwrap();
    let photo = lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::Managed { asset_id }, "photo.raw", "RAW", 32, 24, "2026-10-12");
    session.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    assert!(session.render_now(id, 16, 12).is_ok());
    let job = session.export_job(id, 16, 12, lightcraft_pipeline::OutputSpace::Srgb, lightcraft_pipeline::OutputDepth::U8).unwrap();
    assert!(job.run().rendered.is_err(), "even a small export must not silently use a proxy");
    let options =
        crate::export::ExportOptions { allow_smart_preview: true, format: crate::export::ExportFormat::Png, resize: None, ..Default::default() };
    let exported = crate::export::prepare_export(&mut session, id, &options, 1).unwrap().run().unwrap();
    assert_eq!((exported.width, exported.height), (16, 12), "explicit proxy export must not upscale to catalog dimensions");
    let original = crate::export::ExportOptions { allow_smart_preview: true, format: crate::export::ExportFormat::Original, ..Default::default() };
    assert!(crate::export::prepare_export(&mut session, id, &original, 1).is_err());
}

#[test]
fn budget_keeps_active_leases_and_independent_persistent_pin_owners() {
    use super::cache::{OfflinePin, OfflineQuality};
    let scratch = Scratch::new();
    let b = blob(b"offline original");
    let other = blob(b"disposable bytes");
    let resolver = AssetResolver::new(scratch.0.clone());
    resolver.connect("memory".into(), store(b"offline original")).unwrap();
    let a = resolver.acquire(&b, &AtomicBool::new(false)).unwrap();
    resolver.connect("memory".into(), store(b"disposable bytes")).unwrap();
    let active = resolver.acquire(&other, &AtomicBool::new(false)).unwrap();
    let pin = OfflinePin { quality: OfflineQuality::Original, blobs: vec![b.id.clone()] };
    resolver.set_offline_pins(&[("photo:1".into(), Some(pin.clone())), ("album:1".into(), Some(pin))]).unwrap();
    resolver.set_cache_budget(0).unwrap();
    assert_eq!(resolver.trim_cache().unwrap().used_bytes, b.size + other.size);
    drop(a);
    resolver.set_offline_pins(&[("photo:1".into(), None)]).unwrap();
    assert!(resolver.evict(&b).unwrap_err().to_string().contains("pinned"));
    drop(active);
    let status = resolver.trim_cache().unwrap();
    assert_eq!((status.used_bytes, status.pinned_bytes), (b.size, b.size));
    assert!(scratch.0.join(&b.id.0).exists());
    assert!(!scratch.0.join(&other.id.0).exists());
    drop(resolver);
    let reopened = AssetResolver::new(scratch.0.clone());
    assert_eq!(reopened.cache_status().budget_bytes, 0);
    assert_eq!(reopened.offline_pin("album:1").unwrap().blobs, vec![b.id.clone()]);
    let offline = reopened.acquire(&b, &AtomicBool::new(false)).unwrap();
    assert_eq!(offline.read_bytes().unwrap(), b"offline original");
    reopened.set_offline_pins(&[("album:1".into(), None)]).unwrap();
    assert!(reopened.evict(&b).is_err(), "the last active lease still protects an unpinned blob");
    drop(offline);
    assert_eq!(reopened.trim_cache().unwrap().used_bytes, 0);
}

#[test]
fn decoded_source_clones_keep_the_managed_original_leased() {
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let asset = Asset { original: blob(b"original"), smart_preview: None, thumbnail: None };
    resolver.connect("memory".into(), store(b"original")).unwrap();
    let source = crate::media::SourceRef::Managed {
        asset: Some(Box::new(asset.clone())),
        resolver: Some(resolver.clone()),
        requirement: SourceRequirement::OriginalRequired,
        max_edge: 4,
        denoise: None,
        loader: Some(Arc::new(|_, _| Ok((lightcraft_raster::Rgb32f::from_fn(4, 3, |_, _| [0.1; 3]), Default::default())))),
    }
    .load_source()
    .unwrap();
    let retained = source.clone();
    drop(source);
    resolver.set_cache_budget(0).unwrap();
    assert_eq!(resolver.trim_cache().unwrap().used_bytes, asset.original.size);
    assert!(resolver.evict(&asset.original).is_err());
    drop(retained);
    assert_eq!(resolver.trim_cache().unwrap().used_bytes, 0);
}

#[test]
fn damaged_or_future_retention_settings_disable_eviction_and_preserve_bytes() {
    for bytes in [b"broken".as_slice(), br#"{"version":2,"budget_bytes":0,"pins":{},"removed":[]}"#] {
        let scratch = Scratch::new();
        let b = blob(b"protected");
        std::fs::write(scratch.0.join(&b.id.0), b"protected").unwrap();
        std::fs::write(scratch.0.join("retention.json"), bytes).unwrap();
        let resolver = AssetResolver::new(scratch.0.clone());
        assert!(resolver.cache_status().error.is_some());
        assert!(resolver.trim_cache().is_err());
        assert!(resolver.set_cache_budget(0).is_err());
        assert!(resolver.evict(&b).is_err());
        assert_eq!(std::fs::read(scratch.0.join("retention.json")).unwrap(), bytes);
        assert_eq!(resolver.acquire(&b, &AtomicBool::new(false)).unwrap().read_bytes().unwrap(), b"protected");
    }
}

#[test]
fn a_failed_pin_save_does_not_change_retention_and_remove_local_does_not_refetch_automatically() {
    use super::cache::{OfflinePin, OfflineQuality};
    let scratch = Scratch::new();
    let b = blob(b"original");
    let provider = store(b"original");
    let resolver = AssetResolver::new(scratch.0.clone());
    resolver.connect("memory".into(), provider.clone()).unwrap();
    drop(resolver.acquire(&b, &AtomicBool::new(false)).unwrap());
    let fault = lightcraft_catalog::safe_file::fail_writes_after(1);
    assert!(
        resolver.set_offline_pins(&[("photo:1".into(), Some(OfflinePin { quality: OfflineQuality::Original, blobs: vec![b.id.clone()] }))]).is_err()
    );
    assert!(resolver.offline_pin("photo:1").is_none());
    drop(fault);
    resolver.evict(&b).unwrap();
    let asset = Asset { original: b.clone(), smart_preview: None, thumbnail: None };
    assert!(matches!(resolver.acquire_asset(&asset, SourceRequirement::PreviewAllowed, &AtomicBool::new(false)), Err(AssetError::Pending)));
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
    drop(resolver.acquire_asset(&asset, SourceRequirement::OriginalRequired, &AtomicBool::new(false)).unwrap());
    assert_eq!(provider.reads.load(Ordering::Relaxed), 2, "explicit fetch/export can reacquire a removed original");
    assert!(resolver.acquire_asset(&asset, SourceRequirement::PreviewAllowed, &AtomicBool::new(false)).is_ok());
}

#[test]
fn pinned_preview_restarts_offline_edits_and_explicit_proxy_export_preserves_membership() {
    let scratch = Scratch::new();
    let image = lightcraft_raster::Rgb32f::from_fn(16, 12, |_, _| [0.2, 0.3, 0.4]);
    let preview = crate::smart::encode(&image, None).unwrap();
    let asset = Asset { original: blob(b"remote original"), smart_preview: Some(blob(&preview)), thumbnail: None };
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    session.media.asset_resolver.as_ref().unwrap().connect("memory".into(), store(&preview)).unwrap();
    let result = session
        .execute(
            "cloud.attach",
            &serde_json::json!({"assetId": "offline-preview", "asset": asset, "fileName": "offline.raw", "width": 32, "height": 24, "kind": "raw"}),
        )
        .unwrap();
    let id = result["id"].as_u64().unwrap();
    session.execute("cloud.pin", &serde_json::json!({"ids": [id], "quality": "preview"})).unwrap();
    let album = session.execute("album.create", &serde_json::json!({"name": "Offline trip", "addSelected": true})).unwrap();
    let album_id = album["id"].as_u64().unwrap();
    session.execute("cloud.pin", &serde_json::json!({"albumId": album_id, "quality": "preview"})).unwrap();
    session.execute("cloud.unpin", &serde_json::json!({"ids": [id]})).unwrap();
    session.execute("cloud.cache", &serde_json::json!({"budgetMb": 0})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.activity.list().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(session.activity.list().is_empty(), "offline transfer finished");
    let resolver = session.media.asset_resolver.as_ref().unwrap();
    assert_eq!(resolver.trim_cache().unwrap().used_bytes, preview.len() as u64);
    assert!(resolver.offline_pin(&format!("photo:{id}")).is_none());
    assert!(resolver.offline_pin(&format!("album:{album_id}")).is_some());
    session.close_library().unwrap();
    drop(session);
    let mut reopened = crate::Session::new().with_fs();
    reopened.open_library(&scratch.0, false).unwrap();
    reopened.execute("develop.set", &serde_json::json!({"id": id, "control": "light.exposure", "value": 0.5})).unwrap();
    assert!(reopened.render_now(lightcraft_catalog::PhotoId(id), 16, 12).is_ok());
    let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Png, resize: None, ..Default::default() };
    assert!(
        crate::export::prepare_export(&mut reopened, lightcraft_catalog::PhotoId(id), &options, 1).unwrap().run().is_err(),
        "full-quality export reports unavailable original offline"
    );
    let proxy = crate::export::ExportOptions { allow_smart_preview: true, ..options };
    let exported = crate::export::prepare_export(&mut reopened, lightcraft_catalog::PhotoId(id), &proxy, 1).unwrap().run().unwrap();
    assert_eq!((exported.width, exported.height), (16, 12));
    let removed = reopened.execute("cloud.removeLocalCopy", &serde_json::json!({"ids": [id]})).unwrap();
    assert_eq!(removed["removed"].as_array().unwrap().len(), 1);
    assert!(reopened.catalog.photo(lightcraft_catalog::PhotoId(id)).is_some());
    assert_eq!(reopened.execute("library.missing", &serde_json::json!({})).unwrap(), serde_json::json!([]));
    assert!(reopened.render_now(lightcraft_catalog::PhotoId(id), 16, 12).is_ok(), "removal retains the pinned editable preview");
}
