//! Worker-only durable copies for leaving cloud mode. These files are independent
//! of both the disposable cache and reclaimable managed-upload staging.
use crate::assets::{AssetResolver, TransferManager};
use crate::{EngineError, Result};
use lightcraft_catalog::assets::{Asset, Blob, BlobId};
use lightcraft_catalog::{Catalog, Source};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
const BUFFER_BYTES: usize = 256 << 10;

fn error(message: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("Keep cloud library locally: {message}"))
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) { Err(error("cancelled; completed local copies retained")) } else { Ok(()) }
}
fn insert(blobs: &mut BTreeMap<BlobId, Blob>, asset: &Asset) -> Result<()> {
    asset.validate().map_err(error)?;
    for blob in asset.blobs() {
        if blobs.get(&blob.id).is_some_and(|old| old.size != blob.size) {
            return Err(error("one immutable blob has conflicting lengths; library retained"));
        }
        blobs.entry(blob.id.clone()).or_insert_with(|| blob.clone());
    }
    Ok(())
}

/// Include every rendition/supporting document and every retained content choice
/// belonging to a live managed photo. Identity deduplication never merges photos.
pub(crate) fn plan(catalog: &Catalog) -> Result<Vec<Blob>> {
    let mut blobs = BTreeMap::new();
    for photo in catalog.photos().filter(|photo| !photo.deleted) {
        let Source::Managed { asset_id } = &photo.source else { continue };
        let asset = catalog.asset(asset_id).ok_or_else(|| error(format!("photo {} has no asset manifest", photo.id.0)))?;
        insert(&mut blobs, asset)?;
        if let Some(state) = catalog.sync_state()
            && let Some(global) = catalog.cloud_identity(&format!("photo:{}", photo.id.0))
            && let Some(register) = state.documents.get(&format!("photo:{global}")).and_then(|fields| fields.get("content"))
        {
            for value in register.0.values() {
                let asset = crate::cloud_sync::retained_content_asset(value.clone(), &state.connection)?;
                insert(&mut blobs, &asset)?;
            }
        }
    }
    Ok(blobs.into_values().collect())
}

/// Additional unique bytes required by Recently Deleted are counted separately
/// from the union of live/deleted immutable files. Photos remain separate identities.
pub(crate) struct ConversionPlan {
    pub blobs: Vec<Blob>,
    pub deleted_photos: u64,
    pub deleted_bytes: u64,
}
pub(crate) fn conversion_plan(catalog: &Catalog) -> Result<ConversionPlan> {
    let mut catalog = catalog.clone();
    catalog.apply(lightcraft_catalog::Op::Batch { ops: crate::cloud_sync::materialize_deleted_for_local(&catalog)? })?;
    let mut live = BTreeMap::new();
    let mut deleted = BTreeMap::new();
    let deleted_photos = catalog.photos().filter(|photo| photo.deleted && !photo.local).fold(0u64, |count, _| count.saturating_add(1));
    for photo in catalog.photos() {
        let Source::Managed { asset_id } = &photo.source else { continue };
        let blobs = if photo.deleted { &mut deleted } else { &mut live };
        let asset = catalog.asset(asset_id).ok_or_else(|| error(format!("photo {} has no asset manifest", photo.id.0)))?;
        insert(blobs, asset)?;
        if let Some(state) = catalog.sync_state()
            && let Some(global) = catalog.cloud_identity(&format!("photo:{}", photo.id.0))
            && let Some(register) = state.documents.get(&format!("photo:{global}")).and_then(|fields| fields.get("content"))
        {
            for value in register.0.values() {
                insert(blobs, &crate::cloud_sync::retained_content_asset(value.clone(), &state.connection)?)?;
            }
        }
    }
    let deleted_bytes = deleted.iter().filter(|(id, _)| !live.contains_key(*id)).fold(0u64, |n, (_, blob)| n.saturating_add(blob.size));
    for (id, blob) in deleted {
        if live.get(&id).is_some_and(|old| old.size != blob.size) {
            return Err(error("one immutable blob has conflicting lengths; library retained"));
        }
        live.entry(id).or_insert(blob);
    }
    Ok(ConversionPlan { blobs: live.into_values().collect(), deleted_photos, deleted_bytes })
}

/// Existing file-backed library originals already have durable locations. Verify
/// those references too; browsed Local records are outside the added library.
pub(crate) fn verify_local_files(catalog: &Catalog, cancel: &AtomicBool) -> Result<()> {
    let mut checked = std::collections::BTreeSet::new();
    let mut buffer = vec![0; BUFFER_BYTES];
    for photo in catalog.photos().filter(|p| !p.local) {
        let Source::File { path } = &photo.source else { continue };
        if !checked.insert(path) {
            continue;
        }
        let metadata = fs::metadata(path).map_err(|e| error(format!("{}: {e}; locate or restore this original before retrying", photo.file_name)))?;
        if !metadata.is_file() {
            return Err(error(format!("{} is not a regular local original file", photo.file_name)));
        }
        let mut file = File::open(path).map_err(|e| error(format!("{}: {e}; locate or restore this original before retrying", photo.file_name)))?;
        let size = file.metadata().map_err(error)?.len();
        crate::input_limits::bytes(size).map_err(error)?;
        let mut hash = lightcraft_preview::Hasher128::new();
        let mut read = 0u64;
        loop {
            cancelled(cancel)?;
            let n = file.read(&mut buffer).map_err(error)?;
            if n == 0 {
                break;
            }
            read = read.checked_add(n as u64).filter(|n| *n <= size).ok_or_else(|| error("local original changed while verifying"))?;
            hash.update(buffer.get(..n).ok_or_else(|| error("invalid local original read length"))?);
        }
        if read != size
            || (photo.file_size > 0 && photo.file_size != size)
            || photo.content_hash.as_ref().is_some_and(|expected| expected != &hash.finish().to_string())
        {
            return Err(error(format!("{} changed or failed verification; reload or restore the original, then retry", photo.file_name)));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Progress {
    pub files: u64,
    pub total_files: u64,
    pub bytes: u64,
    pub total_bytes: u64,
    pub current_file: String,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Materialized {
    pub files: u64,
    pub bytes: u64,
    pub reused_files: u64,
}

fn redirected(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes junctions, whose FileType need not report is_symlink().
        meta.file_type().is_symlink() || meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
/// Validate every existing ancestor, including Windows junctions, before creating
/// missing directories. Rechecked at each file boundary.
fn directory(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(error("local originals directory must be absolute"));
    }
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.is_dir() && !redirected(&meta) => {}
            Ok(_) => return Err(error(format!("{} is redirected or is not a directory", ancestor.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(ancestor).map_err(error)?;
                lightcraft_catalog::safe_file::sync_dir(ancestor.parent().unwrap_or(ancestor));
            }
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}
pub(crate) fn blob_path(destination: &Path, blob: &Blob) -> Result<PathBuf> {
    blob.validate().map_err(error)?;
    Ok(destination.join("blobs").join(&blob.id.0))
}

fn verify_path(path: &Path, blob: &Blob, cancel: &AtomicBool) -> Result<bool> {
    cancelled(cancel)?;
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(error(e)),
    };
    if !meta.is_file() || redirected(&meta) || meta.len() != blob.size {
        return Err(error(format!("{} is redirected or has the wrong length; preserved for repair", path.display())));
    }
    let mut source = File::open(path).map_err(error)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        cancelled(cancel)?;
        let count = source.read(&mut buffer).map_err(error)?;
        if count == 0 {
            break;
        }
        let bytes = buffer.get(..count).ok_or_else(|| error("invalid read length"))?;
        size = size.checked_add(count as u64).ok_or_else(|| error("file size overflow"))?;
        if size > blob.size {
            return Err(error("local immutable file changed while verifying; preserved"));
        }
        digest.update(bytes);
    }
    if size != blob.size || format!("{:x}", digest.finalize()) != blob.id.0 {
        return Err(error(format!("{} failed SHA-256 verification; preserved for repair", path.display())));
    }
    Ok(true)
}
/// Missing files return false; existing invalid files return an error and are
/// never silently replaced. This also verifies an interrupted run's receipts.
pub(crate) fn verify(destination: &Path, blob: &Blob, cancel: &AtomicBool) -> Result<bool> {
    directory(destination)?;
    directory(&destination.join("blobs"))?;
    verify_path(&blob_path(destination, blob)?, blob, cancel)
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn temporary(root: &Path) -> Result<(Temporary, File)> {
    for _ in 0..128 {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(".local-copy-{}-{sequence}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((Temporary(path), file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(error(e)),
        }
    }
    Err(error("could not reserve a temporary local file; existing files retained"))
}
fn distinct_storage(resolver: &AssetResolver, destination: &Path) -> Result<()> {
    let destination = fs::canonicalize(destination).map_err(error)?;
    for root in std::iter::once(resolver.root().to_path_buf()).chain(resolver.durable_root()) {
        // Resolver roots may not have been created yet.
        let root = if root.exists() { fs::canonicalize(root).map_err(error)? } else { std::path::absolute(root).map_err(error)? };
        if destination.starts_with(&root) || root.starts_with(&destination) {
            return Err(error("local originals must be separate from cache and managed staging"));
        }
    }
    Ok(())
}

/// Copy verified leased bytes to an independent durable namespace. Call from a
/// worker; callback byte counts cover durable copying, with a file-start update
/// before network acquisition. Previously verified copies resume without network.
pub(crate) fn materialize(
    resolver: Arc<AssetResolver>,
    destination: &Path,
    blobs: &[Blob],
    cancel: &AtomicBool,
    mut callback: impl FnMut(Progress),
) -> Result<Materialized> {
    cancelled(cancel)?;
    directory(destination)?;
    distinct_storage(&resolver, destination)?;
    let root = destination.join("blobs");
    directory(&root)?;
    // Probe write permission before requesting any original from the network.
    let (probe, mut file) = temporary(&root)?;
    file.write_all(b"LightCraft local-originals write probe").map_err(error)?;
    file.sync_all().map_err(error)?;
    drop(file);
    drop(probe);
    let mut unique = BTreeMap::<BlobId, Blob>::new();
    for blob in blobs {
        blob.validate().map_err(error)?;
        if unique.get(&blob.id).is_some_and(|old| old.size != blob.size) {
            return Err(error("conflicting immutable blob lengths"));
        }
        unique.entry(blob.id.clone()).or_insert_with(|| blob.clone());
    }
    let mut progress = Progress { total_files: unique.len() as u64, ..Default::default() };
    let mut required = 0_u64;
    let mut existing = BTreeMap::new();
    for blob in unique.values() {
        cancelled(cancel)?;
        progress.total_bytes = progress.total_bytes.checked_add(blob.size).ok_or_else(|| error("library byte size overflow"))?;
        let exists = verify_path(&blob_path(destination, blob)?, blob, cancel)?;
        if !exists {
            required = required.checked_add(blob.size).ok_or_else(|| error("required disk space overflow"))?;
        }
        existing.insert(blob.id.clone(), exists);
    }
    let available = fs4::statvfs(&root).map_err(error)?.available_space();
    // Acquisition may need a cache download before its durable copy is published.
    let scratch = unique.values().filter(|b| existing.get(&b.id) != Some(&true)).map(|b| b.size).max().unwrap_or(0);
    let required = required.checked_add(scratch).and_then(|n| n.checked_add(16 << 20)).ok_or_else(|| error("required disk space overflow"))?;
    if required > available {
        return Err(error(format!("need {required} free bytes for remaining local copies; {available} available; cloud connection retained")));
    }
    let mut result = Materialized::default();
    for blob in unique.values() {
        cancelled(cancel)?;
        directory(&root)?;
        progress.current_file = blob.remote.key.clone();
        callback(progress.clone());
        if existing.get(&blob.id) == Some(&true) {
            result.reused_files += 1;
            progress.bytes += blob.size;
        } else {
            // Lease remains live through copy + publication, preventing eviction.
            let lease = resolver.acquire(blob, cancel).map_err(error)?;
            let mut source = File::open(lease.path()).map_err(error)?;
            let (temporary, mut output) = temporary(&root)?;
            let mut copied = 0_u64;
            let mut digest = Sha256::new();
            let mut buffer = vec![0; BUFFER_BYTES];
            loop {
                cancelled(cancel)?;
                let count = source.read(&mut buffer).map_err(error)?;
                if count == 0 {
                    break;
                }
                let bytes = buffer.get(..count).ok_or_else(|| error("invalid copy length"))?;
                copied = copied.checked_add(count as u64).ok_or_else(|| error("copy length overflow"))?;
                if copied > blob.size {
                    return Err(error("original changed during local copy; cloud connection retained"));
                }
                output.write_all(bytes).map_err(error)?;
                digest.update(bytes);
                progress.bytes += count as u64;
                callback(progress.clone());
            }
            if copied != blob.size || format!("{:x}", digest.finalize()) != blob.id.0 {
                return Err(error("original failed integrity verification; cloud connection retained"));
            }
            output.sync_all().map_err(error)?;
            drop(output);
            verify_path(&temporary.0, blob, cancel)?;
            directory(&root)?;
            let target = blob_path(destination, blob)?;
            if !verify_path(&target, blob, cancel)? {
                // Hard links publish without replacing a concurrent destination.
                // No check-then-rename fallback: an unrelated file must survive.
                if let Err(e) = fs::hard_link(&temporary.0, &target)
                    && (e.kind() != std::io::ErrorKind::AlreadyExists || !verify_path(&target, blob, cancel)?)
                {
                    return Err(error(format!("could not publish durable local original: {e}; use a local filesystem supporting hard links")));
                }
                lightcraft_catalog::safe_file::sync_dir(&root);
            }
            verify_path(&target, blob, cancel)?;
            drop(lease);
        }
        progress.files += 1;
        result.files = progress.files;
        result.bytes = progress.bytes;
        callback(progress.clone());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{AssetError, BlobStore, sha256};
    use lightcraft_catalog::assets::{AssetId, RemoteLocation};
    use lightcraft_catalog::{Op, Photo, PhotoId};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("lightcraft-local-transfer-{}-{}", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct Memory(Vec<u8>);
    impl BlobStore for Memory {
        fn download(&self, _: &Blob, out: &mut dyn Write, _: &AtomicBool, progress: &mut dyn FnMut(u64)) -> crate::assets::Result<()> {
            out.write_all(&self.0).map_err(|e| AssetError::Disk(e.to_string()))?;
            progress(self.0.len() as u64);
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
    #[test]
    fn local_copies_survive_cache_eviction_and_resume_without_network() {
        let scratch = Scratch::new();
        let resolver = Arc::new(AssetResolver::new(scratch.0.join("cache")));
        let bytes = b"an independent durable original";
        let blob = blob(bytes);
        resolver.connect("memory".into(), Arc::new(Memory(bytes.to_vec()))).unwrap();
        let destination = scratch.0.join("Local Originals");
        let cancel = AtomicBool::new(false);
        let mut updates = Vec::new();
        let report = materialize(resolver.clone(), &destination, &[blob.clone(), blob.clone()], &cancel, |p| updates.push(p)).unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(report.bytes, bytes.len() as u64);
        assert_eq!(report.reused_files, 0);
        resolver.evict(&blob).unwrap();
        assert!(verify(&destination, &blob, &cancel).unwrap());
        let disconnected = Arc::new(AssetResolver::new(scratch.0.join("empty-cache")));
        let resumed = materialize(disconnected, &destination, &[blob], &cancel, |_| {}).unwrap();
        assert_eq!(resumed.reused_files, 1);
        assert_eq!(updates.last().unwrap().files, 1);
    }
    #[test]
    fn insufficient_space_rejects_before_any_source_acquisition() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("Local Originals");
        let available = fs4::statvfs(&scratch.0).unwrap().available_space();
        let size = lightcraft_catalog::assets::MAX_BLOB_BYTES;
        let count = available / size + 1;
        assert!(count < 100_000, "test volume unexpectedly large");
        let blobs: Vec<_> = (0..count)
            .map(|n| Blob {
                id: BlobId(format!("{n:064x}")),
                size,
                remote: RemoteLocation { connection: "unconfigured".into(), key: format!("originals/{n}"), version: None },
            })
            .collect();
        let resolver = Arc::new(AssetResolver::new(scratch.0.join("cache")));
        let e = materialize(resolver, &destination, &blobs, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(e.to_string().contains("free bytes"), "{e}");
        assert_eq!(fs::read_dir(destination.join("blobs")).unwrap().count(), 0);
    }
    #[test]
    fn missing_or_changed_local_file_prevents_a_complete_local_library() {
        let scratch = Scratch::new();
        let path = scratch.0.join("original.png");
        let mut catalog = Catalog::new();
        let mut photo = Photo::new(PhotoId(1), Source::File { path: path.to_string_lossy().into_owned() }, "original.png", "PNG", 10, 10, "");
        photo.file_size = 8;
        photo.content_hash = Some(lightcraft_preview::hash_bytes(b"original").to_string());
        catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        assert!(verify_local_files(&catalog, &AtomicBool::new(false)).is_err());
        fs::write(&path, b"modified").unwrap();
        assert!(verify_local_files(&catalog, &AtomicBool::new(false)).is_err());
        fs::write(&path, b"original").unwrap();
        verify_local_files(&catalog, &AtomicBool::new(false)).unwrap();
        catalog.apply(Op::SetDeleted { id: PhotoId(1), deleted: true }).unwrap();
        assert_eq!(conversion_plan(&catalog).unwrap().deleted_photos, 1);
        fs::remove_file(&path).unwrap();
        assert!(verify_local_files(&catalog, &AtomicBool::new(false)).is_err(), "Recently Deleted local originals must remain recoverable too");
    }
    #[test]
    fn permanent_copies_are_protected_from_cache_overlap_and_outputs() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("Local Originals");
        let resolver = Arc::new(AssetResolver::new(scratch.0.join("cache")));
        let bytes = b"only permanent copy";
        let blob = blob(bytes);
        resolver.connect("memory".into(), Arc::new(Memory(bytes.to_vec()))).unwrap();
        materialize(resolver, &destination, std::slice::from_ref(&blob), &AtomicBool::new(false), |_| {}).unwrap();
        let overlap = Arc::new(AssetResolver::new(destination.join("blobs")));
        overlap.set_permanent_root(destination.clone());
        assert!(overlap.clear_cache().is_err());
        assert!(verify(&destination, &blob, &AtomicBool::new(false)).unwrap());
        let mut s = crate::Session::new();
        let safe = Arc::new(AssetResolver::new(scratch.0.join("other-cache")));
        safe.set_permanent_root(destination.clone());
        s.media.asset_resolver = Some(safe);
        assert!(s.check_write_target(blob_path(&destination, &blob).unwrap().to_str().unwrap()).is_err());
        assert!(s.check_write_target(destination.join("blobs").join("new-file").to_str().unwrap()).is_err());
    }
    #[test]
    fn bad_existing_copy_and_cancellation_preserve_files() {
        let scratch = Scratch::new();
        let resolver = Arc::new(AssetResolver::new(scratch.0.join("cache")));
        let blob = blob(b"original");
        let destination = scratch.0.join("Local Originals");
        directory(&destination.join("blobs")).unwrap();
        let target = blob_path(&destination, &blob).unwrap();
        fs::write(&target, b"damaged!").unwrap();
        assert!(materialize(resolver.clone(), &destination, std::slice::from_ref(&blob), &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"damaged!");
        assert!(materialize(resolver, &destination, &[blob], &AtomicBool::new(true), |_| {}).is_err());
        assert_eq!(fs::read(target).unwrap(), b"damaged!");
    }
    #[test]
    fn cancellation_during_copy_does_not_publish_a_partial_original() {
        let scratch = Scratch::new();
        let resolver = Arc::new(AssetResolver::new(scratch.0.join("cache")));
        let bytes = vec![37; BUFFER_BYTES * 3];
        let blob = blob(&bytes);
        resolver.connect("memory".into(), Arc::new(Memory(bytes))).unwrap();
        let destination = scratch.0.join("Local Originals");
        let cancel = AtomicBool::new(false);
        assert!(
            materialize(resolver.clone(), &destination, std::slice::from_ref(&blob), &cancel, |p| {
                if p.bytes > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .is_err()
        );
        assert!(!blob_path(&destination, &blob).unwrap().exists());
        assert!(fs::read_dir(destination.join("blobs")).unwrap().next().is_none());
        cancel.store(false, Ordering::Relaxed);
        materialize(resolver, &destination, &[blob], &cancel, |_| {}).unwrap();
    }
    #[test]
    fn plan_includes_associated_documents_and_deduplicates_virtual_copies() {
        let mut catalog = Catalog::new();
        let original = blob(b"original");
        let xmp = blob(b"supporting XMP");
        let asset_id = AssetId("asset".into());
        let asset = Asset { original: original.clone(), smart_preview: None, thumbnail: None, associated: [("xmp".into(), xmp.clone())].into() };
        catalog.apply(Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) }).unwrap();
        for id in [1, 2] {
            let mut photo = Photo::new(PhotoId(id), Source::Managed { asset_id: asset_id.clone() }, "a.raw", "RAW", 10, 10, "");
            if id == 2 {
                photo.copy_of = Some(PhotoId(1));
            }
            catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        let planned = plan(&catalog).unwrap();
        assert_eq!(planned.len(), 2);
        assert!(planned.contains(&original));
        assert!(planned.contains(&xmp));
        catalog.apply(Op::SetDeleted { id: PhotoId(2), deleted: true }).unwrap();
        let converted = conversion_plan(&catalog).unwrap();
        assert_eq!(converted.deleted_photos, 1);
        assert_eq!(converted.deleted_bytes, 0, "a live virtual copy already requires the same immutable files");
        assert_eq!(converted.blobs.len(), 2);
        catalog.apply(Op::SetDeleted { id: PhotoId(1), deleted: true }).unwrap();
        let converted = conversion_plan(&catalog).unwrap();
        assert_eq!(converted.deleted_photos, 2, "photo identities remain distinct");
        assert_eq!(converted.deleted_bytes, original.size + xmp.size);
        assert_eq!(converted.blobs.len(), 2, "shared originals and sidecars are copied once");
    }
    #[test]
    fn plan_keeps_concurrent_content_revisions_of_live_photos() {
        use lightcraft_catalog::replication::{Change, Mutation, Stamp};
        let mut catalog = Catalog::new();
        let original = blob(b"current original");
        let previous = blob(b"retained concurrent original");
        let asset_id = AssetId("asset".into());
        let asset = Asset { original: original.clone(), smart_preview: None, thumbnail: None, associated: Default::default() };
        catalog.apply(Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) }).unwrap();
        let photo = Photo::new(PhotoId(1), Source::Managed { asset_id }, "a.raw", "RAW", 10, 10, "");
        catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        catalog.apply(crate::cloud_sync::enable(&catalog, "library".into(), "device".into(), "memory".into()).unwrap()).unwrap();
        let global = catalog.cloud_identity("photo:1").unwrap();
        let entity = format!("photo:{global}");
        let mut state = catalog.sync_state().unwrap().clone();
        let mut content = state.winning(&entity, "content").unwrap().1.clone();
        content["asset"]["original"] = serde_json::to_value(&previous).unwrap();
        content["assetId"] = serde_json::json!("retained-asset");
        state
            .receive(&Mutation {
                version: 2,
                library: "library".into(),
                revision: Stamp { sequence: 1, device: "other".into() },
                context: Default::default(),
                changes: vec![Change { entity, field: "content".into(), base: None, value: content }],
            })
            .unwrap();
        catalog.apply(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
        let planned = plan(&catalog).unwrap();
        assert_eq!(planned.len(), 2);
        assert!(planned.contains(&original));
        assert!(planned.contains(&previous));
        catalog.apply(Op::SetDeleted { id: PhotoId(1), deleted: true }).unwrap();
        assert!(plan(&catalog).unwrap().is_empty(), "ordinary live-original protection keeps its existing scope");
        let converted = conversion_plan(&catalog).unwrap();
        assert_eq!(converted.deleted_photos, 1);
        assert_eq!(converted.deleted_bytes, original.size + previous.size);
        assert_eq!(converted.blobs.len(), 2);
        assert!(converted.blobs.contains(&original));
        assert!(converted.blobs.contains(&previous));
    }
}
