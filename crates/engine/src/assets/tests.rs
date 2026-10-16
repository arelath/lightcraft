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
fn a_duplicate_waiter_can_cancel_without_waiting_for_the_shared_transfer() {
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let b = blob(b"original");
    let entry = resolver.entry(&b.id).unwrap();
    let gate = entry.gate.lock().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let flag = cancel.clone();
    let worker = std::thread::spawn(move || {
        tx.send(resolver.acquire(&b, &flag).err()).unwrap();
    });
    cancel.store(true, Ordering::Relaxed);
    let result = rx.recv_timeout(std::time::Duration::from_secs(1));
    drop(gate);
    worker.join().unwrap();
    assert_eq!(result.unwrap(), Some(AssetError::Cancelled));
}

#[test]
fn a_cached_preview_does_not_wait_for_an_original_being_downloaded() {
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let asset = Asset { associated: Default::default(), original: blob(b"original"), smart_preview: Some(blob(b"preview")), thumbnail: None };
    std::fs::write(scratch.0.join(&asset.smart_preview.as_ref().unwrap().id.0), b"preview").unwrap();
    let entry = resolver.entry(&asset.original.id).unwrap();
    let gate = entry.gate.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(resolver.acquire_asset(&asset, SourceRequirement::PreviewAllowed, &AtomicBool::new(false))).unwrap();
    });
    let result = rx.recv_timeout(std::time::Duration::from_secs(1));
    drop(gate);
    worker.join().unwrap();
    let (lease, proxy) = result.unwrap().unwrap();
    assert!(proxy);
    assert_eq!(lease.read_bytes().unwrap(), b"preview");
}

#[test]
fn a_known_cached_original_is_not_replaced_by_a_proxy_during_verification() {
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let asset = Asset { associated: Default::default(), original: blob(b"original"), smart_preview: Some(blob(b"preview")), thumbnail: None };
    std::fs::write(scratch.0.join(&asset.original.id.0), b"original").unwrap();
    std::fs::write(scratch.0.join(&asset.smart_preview.as_ref().unwrap().id.0), b"preview").unwrap();
    drop(resolver.acquire_cached(&asset.original, &AtomicBool::new(false)).unwrap().unwrap());
    let entry = resolver.entry(&asset.original.id).unwrap();
    let gate = entry.gate.lock().unwrap();
    {
        let _ui = CachedOnly::enter(true);
        assert!(matches!(resolver.acquire_asset(&asset, SourceRequirement::PreviewAllowed, &AtomicBool::new(false)), Err(AssetError::Pending)));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (worker_resolver, worker_asset) = (resolver.clone(), asset.clone());
    let worker = std::thread::spawn(move || {
        tx.send(worker_resolver.acquire_asset(&worker_asset, SourceRequirement::PreviewAllowed, &AtomicBool::new(false))).unwrap();
    });
    assert!(rx.recv_timeout(std::time::Duration::from_millis(50)).is_err());
    drop(gate);
    let (lease, proxy) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap();
    worker.join().unwrap();
    assert!(!proxy);
    assert_eq!(lease.read_bytes().unwrap(), b"original");
}

#[test]
fn a_direct_render_replaces_cached_proxy_pixels_after_original_arrival() {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let scratch = Scratch::new();
    let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |_, _| [20, 40, 60, 255]);
    let original = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let preview = crate::smart::encode(&lightcraft_raster::Rgb32f::from_fn(16, 12, |_, _| [0.6, 0.2, 0.1]), None).unwrap();
    let asset = Asset { associated: Default::default(), original: blob(&original), smart_preview: Some(blob(&preview)), thumbnail: None };
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    resolver.connect("memory".into(), store(&preview)).unwrap();
    let mut session = crate::Session::new().with_fs();
    session.media.asset_resolver = Some(resolver.clone());
    let id = PhotoId(1);
    let asset_id = lightcraft_catalog::assets::AssetId("late-original".into());
    session.catalog.apply(Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset.clone())) }).unwrap();
    session
        .catalog
        .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Managed { asset_id }, "cloud.png", "PNG", 32, 24, "2026-10-12")) })
        .unwrap();
    // Acquire the proxy first so subsequent jobs have a stable generation.
    drop(resolver.acquire(asset.smart_preview.as_ref().unwrap(), &AtomicBool::new(false)).unwrap());
    let proxy = session.render_job(id, 16, 12, false, true).unwrap().run();
    assert!(proxy.loaded.as_ref().unwrap().proxy);
    session.accept(&proxy);
    assert!(session.media.get(id, crate::media::SourceLevel::Thumb).is_some());
    let late = session.render_job(id, 16, 12, false, true).unwrap().run();
    std::fs::write(scratch.0.join(&asset.original.id.0), &original).unwrap();
    drop(resolver.acquire_cached(&asset.original, &AtomicBool::new(false)).unwrap().unwrap());
    session.media.clear_sources();
    session.accept(&late);
    assert!(session.media.get(id, crate::media::SourceLevel::Thumb).is_none(), "late proxy jobs cannot repopulate a new generation");
    // An already accepted proxy is also bypassed by direct/headless callers.
    let mut stale = proxy;
    stale.source_key = None;
    session.accept(&stale);
    let full = session.render_job(id, 32, 24, false, true).unwrap().run();
    assert!(!full.loaded.as_ref().unwrap().proxy);
    assert_eq!((full.rendered.as_ref().unwrap().image.width, full.rendered.as_ref().unwrap().image.height), (32, 24));
}

#[test]
fn render_source_activity_reports_bytes_cancels_and_recovers_after_reconnect() {
    struct Slow;
    impl BlobStore for Slow {
        fn download(&self, _: &Blob, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
            out.write_all(b"o").map_err(disk)?;
            progress(1);
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(AssetError::Cancelled)
        }
    }
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let activity = crate::activity::Activity::default();
    resolver.set_activity(activity.clone());
    resolver.connect("memory".into(), Arc::new(Slow)).unwrap();
    let b = blob(b"original");
    let (r, original) = (resolver.clone(), b.clone());
    let worker = std::thread::spawn(move || r.source_transfer(|cancel| r.acquire(&original, cancel)).err());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let task = loop {
        if let Some(task) = activity.list().into_iter().find(|task| task.done == 1) {
            break task;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    assert_eq!(task.total, 8);
    assert_eq!(task.unit, crate::activity::Unit::Bytes);
    activity.cancel(task.id).unwrap();
    assert_eq!(worker.join().unwrap(), Some(AssetError::Cancelled));
    assert!(activity.list().is_empty());
    assert!(!scratch.0.join(&b.id.0).exists());
    assert!(matches!(resolver.availability(&b.id), AssetAvailability::Failed { .. }));
    let before = resolver.generation();
    resolver.connect("memory".into(), store(b"original")).unwrap();
    assert!(resolver.generation() > before);
    assert_eq!(resolver.source_transfer(|cancel| resolver.acquire(&b, cancel)).unwrap().read_bytes().unwrap(), b"original");
    assert!(activity.list().is_empty());
}

#[test]
fn resolver_bounds_network_io_and_cancels_queued_work_without_downloading() {
    struct HeldStore {
        released: AtomicBool,
        reads: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
    }
    impl BlobStore for HeldStore {
        fn download(&self, blob: &Blob, out: &mut dyn Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
            self.peak.fetch_max(active, Ordering::Relaxed);
            while !self.released.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            self.active.fetch_sub(1, Ordering::Relaxed);
            if cancel.load(Ordering::Relaxed) {
                return Err(AssetError::Cancelled);
            }
            out.write_all(blob.remote.key.as_bytes()).map_err(disk)?;
            progress(blob.size);
            Ok(())
        }
    }
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let provider =
        Arc::new(HeldStore { released: AtomicBool::new(false), reads: AtomicUsize::new(0), active: AtomicUsize::new(0), peak: AtomicUsize::new(0) });
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut workers = Vec::new();
    let mut flags = Vec::new();
    for i in 0..6 {
        let key = format!("source-{i}");
        let mut b = blob(key.as_bytes());
        b.remote.key = key;
        let (r, tx, cancel) = (resolver.clone(), tx.clone(), Arc::new(AtomicBool::new(false)));
        flags.push(cancel.clone());
        workers.push(std::thread::spawn(move || {
            tx.send((i, r.acquire(&b, &cancel))).unwrap();
        }));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = resolver.transfer_status();
        if status.active.len() == 4 && status.queued.len() == 2 {
            // Identify a queued request regardless of thread start ordering.
            let index = (0..6).find(|i| status.queued.iter().any(|q| q.blob == sha256(format!("source-{i}").as_bytes()))).unwrap();
            flags[index].store(true, Ordering::Relaxed);
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let (_, cancelled) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(matches!(cancelled, Err(AssetError::Cancelled)));
    assert_eq!(provider.reads.load(Ordering::Relaxed), 4);
    provider.released.store(true, Ordering::Relaxed);
    let leases: Vec<_> = (0..5).map(|_| rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().1.unwrap()).collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(provider.reads.load(Ordering::Relaxed), 5);
    assert_eq!(provider.peak.load(Ordering::Relaxed), 4);
    assert_eq!(leases.len(), 5);
}

#[test]
fn a_foreground_duplicate_promotes_the_existing_queued_blob() {
    let scratch = Scratch::new();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    let provider = store(b"original");
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let b = blob(b"original");
    let held: Vec<_> = (0..4)
        .map(|i| resolver.transfers.enter(&BlobId(format!("held-{i}")), Arc::new(AtomicU32::new(0)), &AtomicBool::new(false)).unwrap())
        .collect();
    let (r, original) = (resolver.clone(), b.clone());
    let background = std::thread::spawn(move || r.acquire(&original, &AtomicBool::new(false)).unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while resolver.transfer_status().queued.is_empty() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(resolver.transfer_status().queued[0].priority, transfers::Priority::Background);
    let (r, original) = (resolver.clone(), b);
    let foreground = std::thread::spawn(move || {
        transfers::with_priority(transfers::Priority::ActivePhoto, || r.acquire(&original, &AtomicBool::new(false)).unwrap())
    });
    while resolver.transfer_status().queued[0].priority != transfers::Priority::ActivePhoto {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(resolver.transfer_status().queued.len(), 1);
    drop(held);
    let leases = [background.join().unwrap(), foreground.join().unwrap()];
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
    assert_eq!(leases[0].path(), leases[1].path());
}

#[test]
fn headless_wait_fetch_reports_errors_and_finishes_before_returning() {
    let scratch = Scratch::new();
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    let bytes = b"a downloaded source";
    let asset = Asset { associated: Default::default(), original: blob(bytes), smart_preview: None, thumbnail: None };
    let attached = session
        .execute("cloud.attach", &serde_json::json!({"assetId": "waited", "asset": asset, "fileName": "waited.jpg", "width": 4, "height": 3}))
        .unwrap();
    let id = attached["id"].as_u64().unwrap();
    assert!(session.execute("cloud.fetch", &serde_json::json!({"ids": [id], "wait": true})).is_err());
    let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
    let provider = store(bytes);
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let downloaded = session.execute("cloud.fetch", &serde_json::json!({"ids": [id], "wait": true})).unwrap();
    assert_eq!(downloaded["completed"], 1);
    assert_eq!(resolver.availability(&blob(bytes).id), AssetAvailability::Cached);
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
    assert!(session.activity.list().is_empty());
    let _ui = CachedOnly::enter(true);
    assert!(session.execute("cloud.fetch", &serde_json::json!({"ids": [id], "wait": true})).is_err());
    assert!(session.execute("cloud.pin", &serde_json::json!({"ids": [id], "wait": true})).is_err());
    assert!(resolver.offline_pin(&format!("photo:{id}")).is_none(), "desktop wait rejection precedes persistent changes");
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
    let asset = Asset { associated: Default::default(), original: blob(bytes), smart_preview: None, thumbnail: None };
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
    let asset = Asset { associated: Default::default(), original: blob(&bytes), smart_preview: None, thumbnail: None };
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
    let asset =
        Asset { associated: Default::default(), original: blob(b"unavailable original"), smart_preview: Some(blob(&preview)), thumbnail: None };
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
    assert_eq!(reopened.offline_pin("album:1").unwrap().quality, OfflineQuality::Original);
    assert!(reopened.cache_status().resolving_pins);
    assert!(reopened.offline_pin("album:1").unwrap().blobs.is_empty(), "membership is resolved from the catalog, not saved");
    assert_eq!(reopened.clear_cache().unwrap(), 0, "reopen defers eviction until owner resolution");
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
    let asset = Asset { associated: Default::default(), original: blob(b"original"), smart_preview: None, thumbnail: None };
    resolver.connect("memory".into(), store(b"original")).unwrap();
    let source = crate::media::SourceRef::Managed {
        asset: Some(Box::new(asset.clone())),
        resolver: Some(resolver.clone()),
        reader: None,
        peek: None,
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
    for bytes in [b"broken".as_slice(), br#"{"version":4,"budget_bytes":0,"pins":{},"removed":[]}"#] {
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
    let asset = Asset { associated: Default::default(), original: b.clone(), smart_preview: None, thumbnail: None };
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
    let asset = Asset { associated: Default::default(), original: blob(b"remote original"), smart_preview: Some(blob(&preview)), thumbnail: None };
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

#[test]
fn managed_thumbnail_browses_without_original_and_is_not_reused_after_an_edit() {
    let scratch = Scratch::new();
    let pixels = lightcraft_raster::Rgb32f::from_fn(32, 24, |_, _| [0.2, 0.3, 0.4]).to_srgb8();
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
    let original = lightcraft_codecs::encode_png(
        &lightcraft_codecs::EncodeImage::rgba8(&pixels),
        &lightcraft_codecs::EncodeMeta { ppi: Some(300), ..Default::default() },
    )
    .unwrap();
    let provider = store(&png);
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
    resolver.connect("memory".into(), provider.clone()).unwrap();
    let mut asset = Asset { associated: Default::default(), original: blob(&original), smart_preview: None, thumbnail: None };
    let attached = session
        .execute(
            "cloud.attach",
            &serde_json::json!({"assetId": "thumbnail", "asset": asset, "fileName": "cloud.png", "format": "PNG", "width": 32, "height": 24}),
        )
        .unwrap();
    let id = lightcraft_catalog::PhotoId(attached["id"].as_u64().unwrap());
    let photo = session.catalog.photo(id).unwrap();
    let mut thumbnail = blob(&png);
    thumbnail.remote.key = keys::in_library(
        "library",
        &keys::thumbnail(&asset.original.id, photo.develop.hash64(), crate::Session::cloud_thumbnail_parameters(photo, true).0, 512, &thumbnail.id)
            .unwrap(),
    )
    .unwrap();
    asset.thumbnail = Some(thumbnail.clone());
    if let Some(path) = std::env::var_os("LIGHTCRAFT_CLOUD_PREVIEW_ACCEPTANCE_DIR") {
        let path = PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        let working = lightcraft_codecs::decode(&original, Default::default()).unwrap().to_working();
        let preview = crate::smart::encode(&working, None).unwrap();
        let mut preview_blob = blob(&preview);
        preview_blob.remote.key = keys::in_library("library", &keys::smart_preview(&asset.original.id, &preview_blob.id).unwrap()).unwrap();
        let mut fixture_asset = asset.clone();
        fixture_asset.smart_preview = Some(preview_blob);
        let attach =
            serde_json::json!({"assetId": "thumbnail", "asset": fixture_asset, "fileName": "cloud.png", "format": "PNG", "width": 32, "height": 24});
        std::fs::write(path.join("preview-original.png"), &original).unwrap();
        std::fs::write(path.join("preview-thumbnail.png"), &png).unwrap();
        std::fs::write(path.join("preview-smart.lcsp"), &preview).unwrap();
        std::fs::write(path.join("preview-attach.json"), serde_json::to_vec_pretty(&attach).unwrap()).unwrap();
    }
    session
        .commit(
            "Publish thumbnail",
            lightcraft_catalog::Op::Batch {
                ops: vec![
                    lightcraft_catalog::Op::SetAsset {
                        id: lightcraft_catalog::assets::AssetId("thumbnail-complete".into()),
                        asset: Some(Box::new(asset.clone())),
                    },
                    lightcraft_catalog::Op::Relink {
                        id,
                        file_name: "cloud.png".into(),
                        source: lightcraft_catalog::Source::Managed { asset_id: lightcraft_catalog::assets::AssetId("thumbnail-complete".into()) },
                        format: None,
                    },
                ],
            },
        )
        .unwrap();
    let job = session.thumb_job(id, 384).unwrap();
    assert!(session.quick_thumb_is_final(session.catalog.photo(id).unwrap(), 384));
    assert!(!session.quick_thumb_is_final(session.catalog.photo(id).unwrap(), 1024));
    let quick = session.quick_thumb_job(&job).unwrap().run();
    assert_eq!(quick.quick, Some(crate::media::QuickSource::ManagedThumbnail));
    assert_eq!(quick.rendered.unwrap().image.data, pixels.data);
    assert_eq!(provider.reads.load(Ordering::Relaxed), 1);
    assert_eq!(resolver.availability(&asset.original.id), AssetAvailability::Remote);
    assert!(!resolver.root().join(&asset.original.id.0).exists());
    assert!(session.quick_view_job(id, 1600, true).unwrap().managed.is_some());
    assert!(session.quick_view_job(id, 1600, false).unwrap().managed.is_none(), "a cropped rendition cannot stand in for an uncropped view");
    let old_variant = session.variant_job(id, &session.catalog.photo(id).unwrap().develop.clone(), 256).unwrap().key;
    resolver.evict(&thumbnail).unwrap();
    let new_variant = session.variant_job(id, &session.catalog.photo(id).unwrap().develop.clone(), 256).unwrap().key;
    assert_ne!(old_variant, new_variant, "managed variant renders invalidate with asset availability");
    session.execute("develop.set", &serde_json::json!({"id": id.0, "control": "light.exposure", "value": 1.0})).unwrap();
    let edited = session.thumb_job(id, 384).unwrap();
    assert!(session.quick_thumb_job(&edited).is_none(), "another recipe never reuses the shared asset thumbnail");
    assert!(!session.quick_thumb_is_final(session.catalog.photo(id).unwrap(), 384));
}

#[test]
fn replacing_a_manifest_invalidates_queued_sources_and_quick_render_keys() {
    let scratch = Scratch::new();
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    let image = lightcraft_raster::Rgb32f::from_fn(16, 12, |_, _| [0.2, 0.3, 0.4]);
    let preview = crate::smart::encode(&image, None).unwrap();
    let mut asset =
        Asset { associated: Default::default(), original: blob(b"unavailable original"), smart_preview: Some(blob(&preview)), thumbnail: None };
    let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
    resolver.connect("memory".into(), store(&preview)).unwrap();
    let attached = session
        .execute(
            "cloud.attach",
            &serde_json::json!({"assetId": "updated", "asset": asset, "fileName": "cloud.raw", "width": 32, "height": 24, "kind": "raw"}),
        )
        .unwrap();
    let id = lightcraft_catalog::PhotoId(attached["id"].as_u64().unwrap());
    let old = session.render_job(id, 16, 12, false, true).unwrap().run();
    assert!(old.loaded.is_some());
    let quick_before = session.quick_view_job(id, 512, true).unwrap().key;
    let generation = resolver.generation();
    asset.thumbnail = Some(blob(b"new rendition"));
    session
        .commit(
            "Add rendition",
            lightcraft_catalog::Op::Batch {
                ops: vec![
                    lightcraft_catalog::Op::SetAsset {
                        id: lightcraft_catalog::assets::AssetId("updated-complete".into()),
                        asset: Some(Box::new(asset)),
                    },
                    lightcraft_catalog::Op::Relink {
                        id,
                        file_name: "cloud.raw".into(),
                        source: lightcraft_catalog::Source::Managed { asset_id: lightcraft_catalog::assets::AssetId("updated-complete".into()) },
                        format: None,
                    },
                ],
            },
        )
        .unwrap();
    assert!(resolver.generation() > generation);
    assert_ne!(quick_before, session.quick_view_job(id, 512, true).unwrap().key);
    session.accept(&old);
    assert!(
        session.media.get(id, crate::media::SourceLevel::Thumb).is_none(),
        "a result captured before a rendition update cannot repopulate the decoded cache"
    );
}

/// A store holding several blobs, by identity.
struct MapStore(BTreeMap<BlobId, Vec<u8>>, AtomicUsize);
impl BlobStore for MapStore {
    fn download(&self, blob: &Blob, out: &mut dyn Write, _: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<()> {
        self.1.fetch_add(1, Ordering::Relaxed);
        let bytes = self.0.get(&blob.id).ok_or(AssetError::Missing)?;
        out.write_all(bytes).map_err(disk)?;
        progress(bytes.len() as u64);
        Ok(())
    }
}
fn map_store(contents: &[&[u8]]) -> Arc<MapStore> {
    Arc::new(MapStore(contents.iter().map(|b| (sha256(b), b.to_vec())).collect(), AtomicUsize::new(0)))
}

#[test]
fn clear_cache_evicts_every_eligible_download_but_keeps_pinned_and_leased_bytes() {
    use super::cache::{OfflinePin, OfflineQuality};
    let scratch = Scratch::new();
    let resolver = AssetResolver::new(scratch.0.clone());
    let (plain, pinned, leased) = (blob(b"plain download"), blob(b"kept offline"), blob(b"being decoded"));
    resolver.connect("memory".into(), map_store(&[b"plain download", b"kept offline", b"being decoded"])).unwrap();
    for b in [&plain, &pinned] {
        drop(resolver.acquire(b, &AtomicBool::new(false)).unwrap());
    }
    let lease = resolver.acquire(&leased, &AtomicBool::new(false)).unwrap();
    resolver.set_offline_pins(&[("photo:1".into(), Some(OfflinePin { quality: OfflineQuality::Original, blobs: vec![pinned.id.clone()] }))]).unwrap();
    let budget = resolver.cache_status().budget_bytes;
    assert_eq!(resolver.clear_cache().unwrap(), plain.size);
    assert!(!scratch.0.join(&plain.id.0).exists());
    assert!(scratch.0.join(&pinned.id.0).exists() && scratch.0.join(&leased.id.0).exists());
    assert_eq!(resolver.cache_status().budget_bytes, budget, "clearing does not change the size limit");
    assert_eq!(resolver.availability(&plain.id), AssetAvailability::Remote);
    drop(lease);
    assert_eq!(resolver.clear_cache().unwrap(), leased.size);
    assert_eq!(resolver.cache_status().used_bytes, pinned.size);
}

#[test]
fn keep_all_protects_library_blobs_downloads_new_arrivals_and_survives_restart() {
    use super::cache::KeepAll;
    let scratch = Scratch::new();
    let (a, b) = (blob(b"first original"), blob(b"second original"));
    let provider = map_store(&[b"first original", b"second original"]);
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    resolver.connect("memory".into(), provider.clone()).unwrap();
    resolver.set_cache_budget(0).unwrap();
    let keep = KeepAll { originals: true, previews: false };
    resolver.set_keep_all(keep, BTreeMap::from([(a.id.clone(), a.clone())])).unwrap();
    let wait = |path: &Path| {
        for _ in 0..500 {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{} was not downloaded", path.display());
    };
    wait(&scratch.0.join(&a.id.0));
    // an import arrives: covered and fetched too
    resolver.set_library_blobs(BTreeMap::from([(a.id.clone(), a.clone()), (b.id.clone(), b.clone())]));
    wait(&scratch.0.join(&b.id.0));
    for _ in 0..500 {
        if !resolver.keep_all_running.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(resolver.clear_cache().unwrap(), 0, "a zero budget and Clear cache keep library-wide offline copies");
    assert_eq!(resolver.cache_status().keep_all_blobs, 2);
    drop(resolver);
    let reopened = Arc::new(AssetResolver::new(scratch.0.clone()));
    assert_eq!(reopened.keep_all(), keep);
    reopened.set_library_blobs(BTreeMap::from([(a.id.clone(), a.clone()), (b.id.clone(), b.clone())]));
    assert_eq!(reopened.clear_cache().unwrap(), 0);
    // turning it off deletes nothing at once; the copies are ordinary cache entries again
    reopened.set_cache_budget(1 << 30).unwrap();
    reopened.set_keep_all(KeepAll::default(), BTreeMap::new()).unwrap();

    assert!(scratch.0.join(&a.id.0).exists());
    assert_eq!(reopened.clear_cache().unwrap(), a.size + b.size);
}

#[test]
fn version_1_retention_settings_load_and_are_saved_as_version_3() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("retention.json"), br#"{"version":1,"budget_bytes":1048576,"pins":{},"removed":[]}"#).unwrap();
    let resolver = Arc::new(AssetResolver::new(scratch.0.clone()));
    assert_eq!(resolver.cache_status().error, None);
    assert_eq!(resolver.cache_status().budget_bytes, 1 << 20);
    assert_eq!(resolver.keep_all(), super::cache::KeepAll::default());
    resolver.set_keep_all(super::cache::KeepAll { originals: false, previews: true }, BTreeMap::new()).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(scratch.0.join("retention.json")).unwrap()).unwrap();
    assert_eq!(saved["version"], 3);
    assert_eq!(saved["budget_bytes"], 1 << 20);
    assert_eq!(saved["keep_all"], serde_json::json!({"originals": false, "previews": true}));
}

#[test]
fn cache_settings_commands_report_the_drive_keep_new_cloud_photos_offline_and_clear() {
    let scratch = Scratch::new();
    let wait_idle = |session: &crate::Session| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !session.activity.list().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(session.activity.list().is_empty(), "background work finished");
    };
    let attach = |session: &mut crate::Session, name: &str, bytes: &[u8]| {
        let asset = Asset { associated: Default::default(), original: blob(bytes), smart_preview: None, thumbnail: None };
        let r = session
            .execute(
                "cloud.attach",
                &serde_json::json!({"assetId": name, "asset": asset, "fileName": format!("{name}.raw"), "width": 32, "height": 24, "kind": "raw"}),
            )
            .unwrap();
        r["id"].as_u64().unwrap()
    };
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    let provider = map_store(&[b"first cloud original", b"second cloud original"]);
    session.media.asset_resolver.as_ref().unwrap().connect("memory".into(), provider.clone()).unwrap();
    attach(&mut session, "first", b"first cloud original");

    let storage = session.execute("cloud.storage", &serde_json::json!({"wait": true})).unwrap();
    assert!(storage["volume"]["totalBytes"].as_u64().unwrap() > 0, "{storage}");
    assert!(storage["volume"]["label"].as_str().is_some_and(|l| !l.is_empty()));
    assert!(storage["appDataBytes"].as_u64().unwrap() > 0, "the catalog counts as app data");
    assert_eq!(storage["keepAll"], serde_json::json!({"originals": false, "previews": false}));
    assert!(session.execute("cloud.cache", &serde_json::json!({"keepAllOriginals": "yes"})).is_err());

    let status = session.execute("cloud.cache", &serde_json::json!({"keepAllOriginals": true, "budgetMb": 0})).unwrap();
    assert_eq!(status["keepAll"]["originals"], true);
    assert_eq!(status["keepAllBlobs"], 1);
    wait_idle(&session);
    // a photo arriving later is kept offline too
    attach(&mut session, "second", b"second cloud original");
    wait_idle(&session);
    let root = session.media.asset_resolver.as_ref().unwrap().root().to_path_buf();
    assert!(root.join(blob(b"second cloud original").id.0).exists());
    let cleared = session.execute("cloud.clearCache", &serde_json::json!({"wait": true})).unwrap();
    assert_eq!(cleared["freedBytes"], 0, "offline copies survive Clear cache");
    let storage = session.execute("cloud.storage", &serde_json::json!({"wait": true})).unwrap();
    assert_eq!(storage["offlineBytes"].as_u64().unwrap(), (b"first cloud original".len() + b"second cloud original".len()) as u64);
    assert_eq!(storage["photoCacheBytes"], 0);

    session.execute("cloud.cache", &serde_json::json!({"keepAllOriginals": false, "budgetMb": 1024})).unwrap();
    let cleared = session.execute("cloud.clearCache", &serde_json::json!({})).unwrap();
    assert_eq!(cleared["background"], true);
    wait_idle(&session);
    assert_eq!(session.execute("cloud.cache", &serde_json::json!({})).unwrap()["usedBytes"], 0);
    assert_eq!(provider.1.load(Ordering::Relaxed), 2, "each original was downloaded once");
}

#[test]
fn moving_the_cloud_cache_copies_verified_files_reopens_on_the_new_folder_and_survives_restart() {
    let (scratch, elsewhere) = (Scratch::new(), Scratch::new());
    let target = elsewhere.0.join("Cloud cache");
    let bytes = b"a cached cloud original";
    let b = blob(bytes);
    let asset = Asset { associated: Default::default(), original: b.clone(), smart_preview: None, thumbnail: None };
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    let resolver = session.media.asset_resolver.clone().unwrap();
    resolver.connect("memory".into(), store(bytes)).unwrap();
    session
        .execute(
            "cloud.attach",
            &serde_json::json!({"assetId": "moved", "asset": asset, "fileName": "moved.raw", "width": 32, "height": 24, "kind": "raw"}),
        )
        .unwrap();
    drop(resolver.acquire(&b, &AtomicBool::new(false)).unwrap());
    let id = session.selection.active.unwrap();
    session.execute("cloud.pin", &serde_json::json!({"ids": [id.0], "wait": true})).unwrap();
    let album = session.execute("album.create", &serde_json::json!({"name": "Offline move", "addSelected": true})).unwrap()["id"].as_u64().unwrap();
    session.execute("cloud.pin", &serde_json::json!({"albumId": album, "wait": true})).unwrap();
    session.execute("cloud.cache", &serde_json::json!({"budgetMb": 0})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.activity.list().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let old_root = resolver.root().to_path_buf();
    drop(resolver);

    for bad in [
        serde_json::json!({"path": "relative/folder"}),
        serde_json::json!({"path": scratch.0.join("Managed/cache")}),
        serde_json::json!({"path": old_root.join("inner")}),
    ] {
        assert!(session.execute("cloud.cacheLocation", &bad).is_err(), "{bad}");
    }
    let used = elsewhere.0.join("used");
    std::fs::create_dir_all(&used).unwrap();
    std::fs::write(used.join("retention.json"), b"{}").unwrap();
    assert!(session.execute("cloud.cacheLocation", &serde_json::json!({"path": used})).is_err(), "another cache's folder is refused");

    let moved = session.execute("cloud.cacheLocation", &serde_json::json!({"path": target, "wait": true})).unwrap();
    assert_eq!(moved["result"]["ok"], true, "{moved}");
    let resolver = session.media.asset_resolver.clone().unwrap();
    assert_eq!(resolver.root(), target.as_path());
    assert_eq!(std::fs::read(target.join(&b.id.0)).unwrap(), bytes);
    assert_eq!(resolver.cache_status().budget_bytes, 0, "retention settings moved too");
    assert_eq!(resolver.offline_pin(&format!("photo:{}", id.0)).unwrap().blobs, vec![b.id.clone()]);
    assert_eq!(resolver.offline_pin(&format!("album:{album}")).unwrap().blobs, vec![b.id.clone()]);
    assert_eq!(resolver.clear_cache().unwrap(), 0, "live owners resolve before eviction at the new root");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while old_root.join(&b.id.0).exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!old_root.join(&b.id.0).exists(), "the old copy is deleted after the switch");
    let location = session.execute("cloud.cacheLocation", &serde_json::json!({})).unwrap();
    assert_eq!(location["custom"], true);
    drop(resolver);

    // restart: still there; a disconnected folder falls back to the library folder with a warning
    session.close_library().unwrap();
    session.open_library(&scratch.0, false).unwrap();
    assert_eq!(session.media.asset_resolver.as_ref().unwrap().root(), target.as_path());
    session.close_library().unwrap();
    std::fs::rename(&target, elsewhere.0.join("unplugged")).unwrap();
    session.open_library(&scratch.0, false).unwrap();
    assert_eq!(session.media.asset_resolver.as_ref().unwrap().root(), old_root.as_path());
    assert!(session.library.as_ref().unwrap().settings_warnings.iter().any(|w| w.contains("not available")));
    session.close_library().unwrap();
    std::fs::rename(elsewhere.0.join("unplugged"), &target).unwrap();
    session.open_library(&scratch.0, false).unwrap();

    // back to the library folder, as a background task finished by the owner
    let started = session.execute("cloud.cacheLocation", &serde_json::json!({"reset": true})).unwrap();
    assert_eq!(started["background"], true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.poll_cache_move() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(session.media.asset_resolver.as_ref().unwrap().root(), old_root.as_path());
    assert_eq!(std::fs::read(old_root.join(&b.id.0)).unwrap(), bytes);
    assert!(!scratch.0.join(crate::cloud_cache_location::POINTER).exists());
    assert_eq!(session.cache_move_report.as_ref().unwrap()["ok"], true);
}

#[test]
fn a_damaged_cache_location_pointer_falls_back_with_a_warning() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join(crate::cloud_cache_location::POINTER), b"not json").unwrap();
    let mut session = crate::Session::new().with_fs();
    session.open_library(&scratch.0, false).unwrap();
    assert_eq!(session.media.asset_resolver.as_ref().unwrap().root(), scratch.0.join("assets-cache").as_path());
    assert!(session.library.as_ref().unwrap().settings_warnings.iter().any(|w| w.contains("damaged")));
}

#[test]
fn bound_destinations_reject_retargeting_before_settings_or_resolver_changes_in_both_protocols() {
    use serde_json::json;
    for protocol in [None, Some("conditional-head-v1"), Some("append-log-v1")] {
        let scratch = Scratch::new();
        let root = scratch.0.join("library");
        let mut session = crate::Session::new().with_fs();
        session.open_library(&root, false).unwrap();
        let config =
            json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9","profile":"initial"});
        session.execute("cloud.configure", &config).unwrap();
        if let Some(protocol) = protocol {
            session.execute("cloud.syncEnable", &json!({"connection":"personal","protocol":protocol})).unwrap();
        } else {
            session
                .apply_system(lightcraft_catalog::Op::SetAsset {
                    id: lightcraft_catalog::assets::AssetId("existing".into()),
                    asset: Some(Box::new(lightcraft_catalog::assets::Asset {
                        original: Blob {
                            id: sha256(b"original"),
                            size: 8,
                            remote: RemoteLocation { connection: "personal".into(), key: "library/originals/test".into(), version: None },
                        },
                        smart_preview: None,
                        thumbnail: None,
                        associated: Default::default(),
                    })),
                })
                .unwrap();
        }
        let resolver = session.media.asset_resolver.clone().unwrap();
        let saved = std::fs::read(root.join("cloud-connections.json")).unwrap();
        let revision = resolver.connection_revision();
        let active = resolver.cache.staging.lock().unwrap().connections.get("personal").unwrap().clone();
        for (field, value) in [
            ("bucket", json!("other-photos")),
            ("region", json!("us-west-2")),
            ("prefix", json!("other")),
            ("endpoint", json!("http://127.0.0.1:10")),
        ] {
            let mut changed = config.clone();
            changed[field] = value;
            assert!(crate::cloud_setup::validate_destination(&mut session, &changed).unwrap_err().contains("migration"));
            assert!(session.execute("cloud.configure", &changed).unwrap_err().to_string().contains("cannot be retargeted"));
            assert_eq!(std::fs::read(root.join("cloud-connections.json")).unwrap(), saved);
            assert_eq!(resolver.connection_revision(), revision);
            assert_eq!(resolver.cache.staging.lock().unwrap().connections.get("personal"), Some(&active));
        }
        std::fs::remove_file(root.join("cloud-connections.json")).unwrap();
        assert!(session.execute("cloud.configure", &config).unwrap_err().to_string().contains("original storage settings are missing"));
        assert!(!root.join("cloud-connections.json").exists());
        assert_eq!(resolver.connection_revision(), revision);
        std::fs::write(root.join("cloud-connections.json"), &saved).unwrap();
        let mut rotated = config.clone();
        rotated["profile"] = json!("rotated");
        rotated["prefix"] = json!("library/");
        rotated["endpoint"] = json!("http://127.0.0.1:9/");
        crate::cloud_setup::validate_destination(&mut session, &rotated).unwrap();
        session.execute("cloud.configure", &rotated).unwrap();
        assert!(resolver.connection_revision() > revision);
        let connections = session.library.as_mut().unwrap().cloud_connections().unwrap();
        assert_eq!(connections[0].profile.as_deref(), Some("rotated"));
        assert_eq!(connections[0].prefix, "library");
        assert_eq!(connections[0].endpoint.as_deref(), Some("http://127.0.0.1:9"));
        assert!(active.same_destination(&connections[0]));
        assert_eq!(resolver.cache.staging.lock().unwrap().connections.get("personal"), Some(&active));
        // Older settings may already contain accepted slash aliases. Loading
        // normalizes the live transport without rewriting that retained file.
        let legacy = serde_json::to_vec(&vec![rotated.clone()]).unwrap();
        std::fs::write(root.join("cloud-connections.json"), &legacy).unwrap();
        let loaded = session.library.as_mut().unwrap().cloud_connections().unwrap();
        assert_eq!(loaded[0].prefix, "library");
        assert_eq!(loaded[0].endpoint.as_deref(), Some("http://127.0.0.1:9"));
        assert_eq!(std::fs::read(root.join("cloud-connections.json")).unwrap(), legacy);
        session.close_library().unwrap();
    }
}

mod live_pins;
