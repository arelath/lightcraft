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
