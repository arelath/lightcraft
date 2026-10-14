//! Durable managed import: prepare on workers, commit photo + upload intent on the owner.
//! Prepared manifests and immutable bytes survive cancellation, abandoned replies and failed saves.

use crate::assets::{AssetResolver, TransferManager, keys, sha256};
use crate::{EngineError, Result, Selection, Session};
use lightcraft_catalog::assets::{Asset, AssetId, Blob, RemoteLocation};
use lightcraft_catalog::uploads::{UploadIntent, UploadState};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

const MAX_BYTES: u64 = 512 << 20;
const MAX_MANIFEST_BYTES: u64 = 2 << 20;

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn io(error: std::io::Error) -> EngineError {
    EngineError::Other(error.to_string())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    version: u32,
    pub id: String,
    pub asset_id: AssetId,
    pub asset: Asset,
    pub file_name: String,
    pub photo: Option<Box<Photo>>,
    pub prepared: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<RevisionTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_with: Option<PhotoId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_asset: Option<AssetId>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisionTarget {
    pub photo_id: PhotoId,
    pub base_asset: AssetId,
    pub global_id: String,
}

impl Manifest {
    fn validate(&self) -> Result<()> {
        if !matches!(self.version, 1 | 2)
            || (self.version == 1 && (self.target.is_some() || self.working.is_some() || self.stack_with.is_some() || self.origin_asset.is_some()))
            || !lightcraft_catalog::assets::identifier(&self.id)
            || !lightcraft_catalog::assets::identifier(&self.asset_id.0)
            || self.file_name.is_empty()
            || self.file_name.len() > 1024
            || self.file_name.contains(['/', '\\'])
            || self.file_name.chars().any(char::is_control)
        {
            return Err(error("invalid managed import manifest; preserved for recovery"));
        }
        self.asset.validate().map_err(error)?;
        if self.working.as_ref().is_some_and(|path| !working_name(path))
            || self.stack_with.is_some_and(|id| id.0 == 0 || id.0 == u64::MAX)
            || self.origin_asset.as_ref().is_some_and(|id| !lightcraft_catalog::assets::identifier(&id.0))
            || (self.stack_with.is_some() != self.origin_asset.is_some())
        {
            return Err(error("invalid external working-file record; preserved"));
        }
        if let Some(target) = &self.target
            && (target.photo_id.0 == 0
                || target.photo_id.0 == u64::MAX
                || !lightcraft_catalog::assets::identifier(&target.base_asset.0)
                || !lightcraft_catalog::assets::identifier(&target.global_id))
        {
            return Err(error("invalid managed revision target; preserved for recovery"));
        }
        if let Some(photo) = &self.photo
            && (photo.source != (Source::Managed { asset_id: self.asset_id.clone() })
                || photo.file_name != self.file_name
                || photo.width == 0
                || photo.height == 0
                || photo.width > 65_535
                || photo.height > 65_535
                || u64::from(photo.width).saturating_mul(u64::from(photo.height)) > 100_000_000)
        {
            return Err(error("invalid staged photo description; manifest preserved"));
        }
        if self.prepared && (self.photo.is_none() || self.asset.smart_preview.is_none() || self.asset.thumbnail.is_none()) {
            return Err(error("prepared import is missing its photo or renditions"));
        }
        Ok(())
    }
    fn write(&self, root: &Path) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|e| error(e.to_string()))?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(error("managed import manifest exceeds 2 MiB"));
        }
        std::fs::create_dir_all(root.join("imports")).map_err(io)?;
        lightcraft_catalog::safe_file::write_atomic(&root.join("imports").join(format!("{}.json", self.id)), &bytes).map_err(io)
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(io)?;
    let size = file.metadata().map_err(io)?.len();
    if size == 0 || size > limit {
        return Err(error(format!("{}: empty file or larger than the {} MiB safety limit", path.display(), limit >> 20)));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(size).map_err(|_| error("file too large"))?)
        .map_err(|_| error("not enough memory to stage the original"))?;
    file.take(size.saturating_add(1)).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 != size {
        return Err(error("file changed while being read; retry the import"));
    }
    Ok(bytes)
}

/// Maintenance reads outside the durable guard and rechecks the namespace epoch
/// before changing bytes. Every malformed ownership record conservatively stops it.
pub(crate) fn visit_staging(root: &Path, mut visit: impl FnMut(&Manifest) -> Result<()>) -> Result<()> {
    let dir = root.join("imports");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(error("staging manifest directory is redirected or invalid; data preserved")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    }
    for (index, entry) in std::fs::read_dir(&dir).map_err(io)?.enumerate() {
        if index >= 100_000 {
            return Err(error("too many staging records to maintain safely; data preserved"));
        }
        let entry = entry.map_err(io)?;
        if entry.path().extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        if !entry.file_type().map_err(io)?.is_file() {
            return Err(error("staging ownership record is not a regular file; preserved"));
        }
        let manifest: Manifest = serde_json::from_slice(&read_bounded(&entry.path(), MAX_MANIFEST_BYTES)?)
            .map_err(|_| error("invalid or future staging ownership record; data preserved"))?;
        manifest.validate()?;
        if entry.file_name() != std::ffi::OsStr::new(&format!("{}.json", manifest.id)) {
            return Err(error("staging ownership identity differs from filename; preserved"));
        }
        visit(&manifest)?;
    }
    Ok(())
}

pub(crate) fn revision_target(root: &Path, id: &str) -> Result<Option<RevisionTarget>> {
    Ok(read_manifest(root, id)?.target)
}

pub(crate) fn read_manifest(root: &Path, id: &str) -> Result<Manifest> {
    if !lightcraft_catalog::assets::identifier(id) {
        return Err(error("invalid staging identity"));
    }
    let path = root.join("imports").join(format!("{id}.json"));
    let manifest: Manifest =
        serde_json::from_slice(&read_bounded(&path, MAX_MANIFEST_BYTES)?).map_err(|_| error("invalid revision manifest; original preserved"))?;
    manifest.validate()?;
    if manifest.id != id {
        return Err(error("revision identity differs from manifest"));
    }
    Ok(manifest)
}

fn stage(root: &Path, blob: &Blob, bytes: &[u8]) -> Result<()> {
    blob.validate().map_err(error)?;
    if bytes.len() as u64 != blob.size || sha256(bytes) != blob.id {
        return Err(error("staged bytes failed verification"));
    }
    std::fs::create_dir_all(root.join("blobs")).map_err(io)?;
    let path = root.join("blobs").join(&blob.id.0);
    match lightcraft_catalog::safe_file::write_new(&path, bytes) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let previous = read_bounded(&path, MAX_BYTES)?;
            if sha256(&previous) == blob.id && previous.len() as u64 == blob.size {
                Ok(())
            } else {
                Err(error("an existing durable blob failed verification; it was preserved"))
            }
        }
        Err(e) => Err(io(e)),
    }
}

fn make_blob(bytes: &[u8], connection: &str, key: String) -> Blob {
    Blob { id: sha256(bytes), size: bytes.len() as u64, remote: RemoteLocation { connection: connection.into(), key, version: None } }
}

struct Job {
    paths: Vec<PathBuf>,
    root: PathBuf,
    connection: crate::assets::s3::S3Connection,
    resolver: Arc<AssetResolver>,
    now: String,
    defaults: crate::import::ImportDefaults,
    presets: Vec<lightcraft_develop::Preset>,
    metadata_presets: Vec<crate::cmd::metadata::MetadataPreset>,
    recover: bool,
    known: std::collections::BTreeSet<String>,
    selected: Option<std::collections::BTreeSet<String>>,
    revisions: Vec<(Photo, Asset, RevisionTarget, RevisionWork)>,
    edits: Vec<std::sync::Mutex<Option<EditPlan>>>,
}

struct EditPlan {
    export: crate::export::PreparedExport,
    parent: PhotoId,
    base_asset: AssetId,
}

enum RevisionWork {
    Dng(String),
    Xmp { packet: String, working: Option<String> },
    Returned(String),
}

fn working_name(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    parts.len() == 3
        && parts.first() == Some(&"working")
        && parts.get(1).is_some_and(|id| lightcraft_catalog::assets::identifier(id))
        && parts.get(2).is_some_and(|name| {
            !name.is_empty() && name.len() <= 1024 && *name != "." && *name != ".." && !name.contains('\\') && !name.chars().any(char::is_control)
        })
}

impl Job {
    fn new(s: &mut Session, p: &Value, recover: bool) -> Result<Self> {
        let connection_id = p.get("connection").and_then(Value::as_str).ok_or_else(|| error("choose a configured storage connection"))?;
        let connection = s
            .library
            .as_mut()
            .ok_or_else(|| error("open a persistent native library first"))?
            .cloud_connections()?
            .into_iter()
            .find(|connection| connection.id == connection_id)
            .ok_or_else(|| error("storage connection is not configured"))?;
        let paths = if recover {
            Vec::new()
        } else {
            p.get("paths")
                .and_then(Value::as_array)
                .filter(|paths| !paths.is_empty() && paths.len() <= 1000)
                .ok_or_else(|| error("choose between 1 and 1000 original files"))?
                .iter()
                .map(|path| {
                    path.as_str()
                        .filter(|path| !path.is_empty() && path.len() <= 32_768 && !path.contains('\0'))
                        .map(PathBuf::from)
                        .ok_or_else(|| error("invalid import path"))
                })
                .collect::<Result<Vec<_>>>()?
        };
        let selected = p
            .get("imports")
            .map(|value| {
                let ids = value
                    .as_array()
                    .filter(|ids| !ids.is_empty() && ids.len() <= 1000)
                    .ok_or_else(|| error("imports must contain between 1 and 1000 import identities"))?;
                ids.iter()
                    .map(|id| {
                        id.as_str()
                            .filter(|id| lightcraft_catalog::assets::identifier(id))
                            .map(str::to_owned)
                            .ok_or_else(|| error("invalid recovery import identity"))
                    })
                    .collect::<Result<std::collections::BTreeSet<_>>>()
            })
            .transpose()?;
        let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("managed storage is unavailable"))?;
        resolver.validate_storage().map_err(|e| error(e.to_string()))?;
        let root = resolver.durable_root().ok_or_else(|| error("this library has no durable native storage; choose a persistent library"))?;
        if crate::originals::paths_overlap(&root, resolver.root()).map_err(error)? {
            return Err(error("managed staging overlaps the disposable cache"));
        }
        Ok(Self {
            paths,
            root,
            connection,
            resolver,
            now: (s.clock)(),
            defaults: s.import_defaults.clone(),
            presets: s.presets.clone(),
            metadata_presets: s.metadata_presets.clone(),
            recover,
            known: s.catalog.upload_intents().keys().cloned().collect(),
            selected,
            revisions: Vec::new(),
            edits: Vec::new(),
        })
    }

    fn prepare(&self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Vec<Manifest>> {
        if self.recover {
            return self.recover_prepared(cancel, progress);
        }
        if !self.revisions.is_empty() {
            return self.prepare_revisions(cancel, progress);
        }
        if !self.edits.is_empty() {
            return self.prepare_edits(cancel, progress);
        }
        let mut prepared = Vec::new();
        for (index, path) in self.paths.iter().enumerate() {
            self.resolver.validate_storage().map_err(|e| error(e.to_string()))?;
            if cancel.load(Ordering::Relaxed) {
                return Err(error("cloud import cancelled; staged bytes remain recoverable"));
            }
            progress.detail(&path.to_string_lossy());
            let bytes = read_bounded(path, MAX_BYTES)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(error("cloud import cancelled; original source preserved"));
            }
            let file_name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| error("original filename must be valid UTF-8"))?.to_owned();
            let source = sha256(&bytes);
            let mut random = [0u8; 32];
            getrandom::fill(&mut random).map_err(|e| error(format!("could not create an import identity: {e}")))?;
            let id = sha256(&random).0;
            let original = make_blob(
                &bytes,
                &self.connection.id,
                keys::in_library(&self.connection.prefix, &keys::original(&source).map_err(|e| error(e.to_string()))?)
                    .map_err(|e| error(e.to_string()))?,
            );
            let mut manifest = Manifest {
                version: 1,
                id: id.clone(),
                asset_id: AssetId(id),
                asset: Asset { associated: Default::default(), original, smart_preview: None, thumbnail: None },
                file_name,
                photo: None,
                prepared: false,
                target: None,
                working: None,
                stack_with: None,
                origin_asset: None,
            };
            // The identity and intended original precede its immutable copy. A crash here
            // preserves the user's source; recovery reports a missing staged copy.
            self.write_manifest(&manifest, cancel)?;
            self.stage(&manifest.asset.original, &bytes, cancel)?;
            drop(bytes);
            self.finish(&mut manifest, cancel)?;
            prepared.push(manifest);
            progress.progress(index as u64 + 1, self.paths.len() as u64);
        }
        Ok(prepared)
    }

    fn prepare_revisions(&self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Vec<Manifest>> {
        let mut prepared = Vec::new();
        for (index, (photo, asset, target, work)) in self.revisions.iter().enumerate() {
            progress.detail(&photo.file_name);
            let bytes = if let RevisionWork::Returned(path) = work {
                if !crate::originals::prospective_path(&self.root.join(path))
                    .map_err(error)?
                    .starts_with(crate::originals::prospective_path(&self.root).map_err(error)?)
                {
                    return Err(error("working file is redirected outside durable storage; file was retained"));
                }
                read_bounded(&self.root.join(path), MAX_BYTES)?
            } else {
                let lease = self.resolver.acquire(&asset.original, cancel).map_err(|e| error(e.to_string()))?;
                read_bounded(lease.path(), MAX_BYTES)?
            };
            if matches!(work, RevisionWork::Returned(_)) && sha256(&bytes) == asset.original.id {
                continue;
            }
            let (bytes, name, xmp) = match work {
                RevisionWork::Dng(packet) => (
                    crate::cmd::convert::encode_dng(&bytes, packet.clone()).map_err(error)?,
                    Path::new(&photo.file_name).with_extension("dng").to_string_lossy().to_string(),
                    None,
                ),
                RevisionWork::Xmp { packet, .. } => {
                    let merged = if let Some(previous) = asset.associated.get("xmp") {
                        let lease = self.resolver.acquire(previous, cancel).map_err(|e| error(e.to_string()))?;
                        let old = read_bounded(lease.path(), 16 << 20)?;
                        crate::sidecar::merge_managed_packet(&old, packet).map_err(error)?
                    } else {
                        packet.clone()
                    };
                    (bytes, photo.file_name.clone(), Some(merged.into_bytes()))
                }
                RevisionWork::Returned(_) => (bytes, photo.file_name.clone(), None),
            };
            let id = new_identity()?;
            let original = make_blob(
                &bytes,
                &self.connection.id,
                keys::in_library(&self.connection.prefix, &keys::original(&sha256(&bytes)).map_err(|e| error(e.to_string()))?)
                    .map_err(|e| error(e.to_string()))?,
            );
            let mut updated = photo.clone();
            updated.source = Source::Managed { asset_id: AssetId(id.clone()) };
            updated.file_name = name.clone();
            let info = crate::files::probe_bytes(&name, &bytes).map_err(error)?;
            updated.format = info.format.clone();
            updated.content_hash = info.content_hash;
            updated.file_size = info.file_size;
            updated.width = info.width;
            updated.height = info.height;
            let mut manifest = Manifest {
                version: 2,
                id: id.clone(),
                asset_id: AssetId(id),
                asset: Asset { original, smart_preview: None, thumbnail: None, associated: Default::default() },
                file_name: name,
                photo: Some(Box::new(updated)),
                prepared: false,
                target: Some(target.clone()),
                working: match work {
                    RevisionWork::Returned(path) => Some(path.clone()),
                    RevisionWork::Xmp { working, .. } => working.clone(),
                    _ => None,
                },
                stack_with: None,
                origin_asset: None,
            };
            if let Some(xmp) = &xmp {
                let key =
                    keys::in_library(&self.connection.prefix, &format!("associated/xmp/{}.xmp", sha256(xmp).0)).map_err(|e| error(e.to_string()))?;
                manifest.asset.associated.insert("xmp".into(), make_blob(xmp, &self.connection.id, key));
            }
            self.write_manifest(&manifest, cancel)?;
            self.stage(&manifest.asset.original, &bytes, cancel)?;
            if let Some(xmp) = &xmp
                && let Some(blob) = manifest.asset.associated.get("xmp")
            {
                self.stage(blob, xmp, cancel)?;
            }
            self.finish(&mut manifest, cancel)?;
            prepared.push(manifest);
            progress.progress(index as u64 + 1, self.revisions.len() as u64);
        }
        Ok(prepared)
    }

    fn prepare_edits(&self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Vec<Manifest>> {
        let mut prepared = Vec::new();
        for (index, edit) in self.edits.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(error("external edit cancelled; durable data preserved"));
            }
            let EditPlan { export, parent, base_asset } = edit
                .lock()
                .map_err(|_| error("external export state unavailable"))?
                .take()
                .ok_or_else(|| error("external export was already prepared"))?;
            let name = export.file_name.clone();
            progress.detail(&name);
            let output = export.run().map_err(error)?;
            if output.bytes.len() as u64 > MAX_BYTES {
                return Err(error("external edit exceeds 512 MiB staging limit"));
            }
            let id = new_identity()?;
            let original = make_blob(
                &output.bytes,
                &self.connection.id,
                keys::in_library(&self.connection.prefix, &keys::original(&sha256(&output.bytes)).map_err(|e| error(e.to_string()))?)
                    .map_err(|e| error(e.to_string()))?,
            );
            let mut manifest = Manifest {
                version: 2,
                id: id.clone(),
                asset_id: AssetId(id.clone()),
                asset: Asset { original, smart_preview: None, thumbnail: None, associated: Default::default() },
                file_name: name.clone(),
                photo: None,
                prepared: false,
                target: None,
                working: Some(format!("working/{id}/{name}")),
                stack_with: Some(parent),
                origin_asset: Some(base_asset),
            };
            self.write_manifest(&manifest, cancel)?;
            self.stage(&manifest.asset.original, &output.bytes, cancel)?;
            self.finish(&mut manifest, cancel)?;
            prepared.push(manifest);
            progress.progress(index as u64 + 1, self.edits.len() as u64);
        }
        Ok(prepared)
    }

    fn recover_prepared(&self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Vec<Manifest>> {
        let dir = self.root.join("imports");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io(error)),
        };
        let mut manifests = Vec::new();
        for (count, entry) in entries.enumerate() {
            if count >= 100_000 {
                return Err(error("too many staging manifests to recover safely"));
            }
            let entry = entry.map_err(io)?;
            if !entry.file_type().map_err(io)?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(id) = name.strip_suffix(".json").filter(|id| lightcraft_catalog::assets::identifier(id)) else { continue };
            if self.known.contains(id) || self.selected.as_ref().is_some_and(|selected| !selected.contains(id)) {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(error("import recovery cancelled; staged data retained"));
            }
            let bytes = read_bounded(&entry.path(), MAX_MANIFEST_BYTES)?;
            let mut manifest: Manifest =
                serde_json::from_slice(&bytes).map_err(|e| error(format!("{}: {e}; manifest preserved", entry.path().display())))?;
            manifest.validate()?;
            if manifest.id != id {
                return Err(error("staged import identity does not match its filename"));
            }
            if manifest.asset.original.remote.connection != self.connection.id {
                continue;
            }
            progress.detail(&manifest.file_name);
            self.finish(&mut manifest, cancel)?;
            manifests.push(manifest);
            if manifests.len() > 1000 {
                return Err(error("recover at most 1000 imports at once; select their import identities"));
            }
            progress.progress(manifests.len() as u64, 0);
        }
        Ok(manifests)
    }

    fn finish(&self, manifest: &mut Manifest, cancel: &AtomicBool) -> Result<()> {
        manifest.validate()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(error("cloud import cancelled; durable original retained"));
        }
        let path = self.root.join("blobs").join(&manifest.asset.original.id.0);
        let bytes = read_bounded(&path, MAX_BYTES)?;
        if sha256(&bytes) != manifest.asset.original.id || bytes.len() as u64 != manifest.asset.original.size {
            return Err(error("durable original failed verification"));
        }
        for blob in manifest.asset.associated.values() {
            let document = read_bounded(&self.root.join("blobs").join(&blob.id.0), 16 << 20)?;
            if sha256(&document) != blob.id || document.len() as u64 != blob.size {
                return Err(error("associated document failed verification"));
            }
        }
        let mut shadow = Session::new().with_fs();
        shadow.import_defaults = self.defaults.clone();
        shadow.presets = self.presets.clone();
        shadow.metadata_presets = self.metadata_presets.clone();
        let mut photo = if let Some(photo) = &manifest.photo {
            (**photo).clone()
        } else {
            let info = crate::files::probe_bytes(&manifest.file_name, &bytes).map_err(error)?;
            if info.kind == lightcraft_catalog::MediaKind::Video || info.preview_only.is_some() {
                return Err(error("managed import requires a decodable photo original"));
            }
            let mut photo = Photo::new(
                PhotoId(1),
                Source::Managed { asset_id: manifest.asset_id.clone() },
                &manifest.file_name,
                &info.format,
                info.width,
                info.height,
                &self.now,
            );
            photo.kind = info.kind;
            photo.file_size = info.file_size;
            photo.captured = info.captured;
            photo.meta = info.meta;
            photo.as_shot_wb = info.as_shot_wb;
            photo.content_hash = info.content_hash;
            photo.embedded_lens = info.embedded_lens;
            if manifest.working.is_none() {
                crate::import::apply_import_defaults(&shadow, &mut photo);
            }
            photo
        };
        photo.id = PhotoId(1);
        manifest.photo = Some(Box::new(photo.clone()));
        manifest.prepared = false;
        self.write_manifest(manifest, cancel)?;
        shadow.media.asset_resolver = Some(self.resolver.clone());
        shadow.commit(
            "Prepare cloud import",
            Op::Batch {
                ops: vec![
                    // Recovery always regenerates from the original, even when a
                    // partial manifest already names a prepared proxy.
                    Op::SetAsset {
                        id: manifest.asset_id.clone(),
                        asset: Some(Box::new(Asset {
                            associated: Default::default(),
                            original: manifest.asset.original.clone(),
                            smart_preview: None,
                            thumbnail: None,
                        })),
                    },
                    Op::AddPhoto { photo: Box::new(photo.clone()) },
                ],
            },
        )?;
        let source = shadow.media.source_ref(&photo, crate::SourceLevel::Preview).load_source().map_err(error)?;
        let tone = source.info.as_ref().and_then(|info| info.camera_tone.as_ref()).or(source.camera_tone.as_ref());
        let preview = crate::smart::encode(&source.image, tone).map_err(error)?;
        let preview_blob = make_blob(
            &preview,
            &self.connection.id,
            keys::in_library(
                &self.connection.prefix,
                &keys::smart_preview(&manifest.asset.original.id, &sha256(&preview)).map_err(|e| error(e.to_string()))?,
            )
            .map_err(|e| error(e.to_string()))?,
        );
        manifest.asset.smart_preview = Some(preview_blob.clone());
        self.write_manifest(manifest, cancel)?;
        self.stage(&preview_blob, &preview, cancel)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(error("cloud import cancelled; durable original and prepared data retained"));
        }
        let rendered =
            shadow.thumb_job(photo.id, 512).ok_or_else(|| error("could not prepare the cloud thumbnail"))?.run().rendered.map_err(error)?;
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&rendered.image), &Default::default())
            .map_err(|e| error(e.to_string()))?;
        let key = keys::thumbnail(
            &manifest.asset.original.id,
            photo.develop.hash64(),
            Session::cloud_thumbnail_parameters(&photo, true).0,
            512,
            &sha256(&png),
        )
        .map_err(|e| error(e.to_string()))?;
        let thumbnail = make_blob(&png, &self.connection.id, keys::in_library(&self.connection.prefix, &key).map_err(|e| error(e.to_string()))?);
        manifest.asset.thumbnail = Some(thumbnail.clone());
        self.write_manifest(manifest, cancel)?;
        self.stage(&thumbnail, &png, cancel)?;
        manifest.prepared = true;
        if let Some(relative) = &manifest.working {
            let path = self.root.join(relative);
            let parent = path.parent().ok_or_else(|| error("invalid working-file parent"))?;
            if !crate::originals::prospective_path(&path).map_err(error)?.starts_with(crate::originals::prospective_path(&self.root).map_err(error)?)
            {
                return Err(error("working directory is redirected outside managed storage"));
            }
            std::fs::create_dir_all(parent).map_err(io)?;
            match lightcraft_catalog::safe_file::write_new(&path, &bytes) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {} // editor revisions are never replaced
                Err(e) => return Err(io(e)),
            }
        }
        self.write_manifest(manifest, cancel)
    }

    fn write_manifest(&self, manifest: &Manifest, cancel: &AtomicBool) -> Result<()> {
        let _guard = self.resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
        manifest.write(&self.root)
    }

    fn stage(&self, blob: &Blob, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
        let _guard = self.resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
        stage(&self.root, blob, bytes)
    }
}

fn commit(s: &mut Session, manifests: Vec<Manifest>) -> Result<Value> {
    let mut ids = Vec::new();
    let mut ops = Vec::new();
    let mut copies = Vec::new();
    let mut stacks = Vec::new();
    for manifest in manifests {
        manifest.validate()?;
        if !manifest.prepared {
            return Err(error("import has not finished staging its renditions"));
        }
        if let Some(existing) = s.catalog.upload_intents().get(&manifest.id) {
            ids.push(existing.photo_id.0);
            continue;
        }
        if let (Some(parent), Some(base)) = (manifest.stack_with, &manifest.origin_asset)
            && s.catalog.photo(parent).is_none_or(|photo| photo.deleted || photo.source != (Source::Managed { asset_id: base.clone() }))
        {
            return Err(error("external-edit source changed or was deleted; durable working data was retained"));
        }
        let mut photo = *manifest.photo.ok_or_else(|| error("staged photo is missing"))?;
        let global_id = if let Some(target) = &manifest.target {
            let current = s.catalog.photo(target.photo_id).ok_or_else(|| error("revision photo no longer exists; staged data preserved"))?;
            if current.deleted || current.source != (Source::Managed { asset_id: target.base_asset.clone() }) {
                return Err(error("photo changed while preparing its revision; staged data preserved"));
            }
            photo.id = target.photo_id;
            target.global_id.clone()
        } else {
            photo.id = s.catalog.alloc_photo_id();
            manifest.id.clone()
        };
        let intent = UploadIntent {
            asset_id: manifest.asset_id.clone(),
            photo_id: photo.id,
            state: UploadState::SavedLocally,
            verified: Default::default(),
            error: None,
            finalization: None,
        };
        if let Some(relative) = &manifest.working {
            let root = s
                .media
                .asset_resolver
                .as_ref()
                .and_then(|resolver| resolver.durable_root())
                .ok_or_else(|| error("working storage is unavailable"))?;
            let path = root.join(relative).to_string_lossy().to_string();
            let binding = Op::SetWorkingCopy {
                id: photo.id,
                working: Some(Box::new(lightcraft_catalog::managed::WorkingCopy {
                    connection: manifest.asset.original.remote.connection.clone(),
                    path: path.clone(),
                    asset_id: manifest.asset_id.clone(),
                    ingested: manifest.asset.original.id.clone(),
                })),
            };
            copies.push(json!({"path":path,"id":photo.id.0,"original":manifest.stack_with.map(|id|id.0)}));
            stacks.push((photo.id, manifest.stack_with, binding));
        }
        ids.push(photo.id.0);
        ops.push(Op::SetAsset { id: manifest.asset_id, asset: Some(Box::new(manifest.asset)) });
        if manifest.target.is_some() {
            let copies: Vec<_> = s
                .catalog
                .photos()
                .filter(|copy| {
                    copy.id == photo.id
                        || (copy.copy_of == Some(photo.id)
                            && manifest
                                .target
                                .as_ref()
                                .is_some_and(|target| copy.source == (Source::Managed { asset_id: target.base_asset.clone() })))
                })
                .map(|copy| copy.id)
                .collect();
            for id in copies {
                ops.push(Op::Relink { id, file_name: photo.file_name.clone(), source: photo.source.clone(), format: Some(photo.format.clone()) });
                ops.push(Op::SetContent {
                    id,
                    width: photo.width,
                    height: photo.height,
                    file_size: photo.file_size,
                    content_hash: photo.content_hash.clone(),
                    preview_only: photo.preview_only.clone(),
                });
            }
        } else {
            ops.push(Op::AddPhoto { photo: Box::new(photo.clone()) });
        }
        ops.push(Op::SetCloudIdentity { entity: format!("photo:{}", photo.id.0), id: Some(global_id) });
        ops.push(Op::SetUploadIntent { id: manifest.id, intent: Some(Box::new(intent)) });
    }
    for (_, _, binding) in &stacks {
        ops.push(binding.clone());
    }
    if stacks.iter().any(|(_, parent, _)| parent.is_some()) {
        let mut staged = s.catalog.clone();
        staged.apply(Op::Batch { ops: ops.clone() })?;
        for (id, parent, _) in &stacks {
            if let Some(parent) = parent
                && staged.photo(*parent).is_some()
                && let Some(group) = staged.group_ops(*id, &[*parent], false)
            {
                staged.apply(group.clone())?;
                ops.push(group);
            }
        }
    }
    if !ops.is_empty() {
        s.skip_auto_write = true;
        s.commit("Import cloud photos", Op::Batch { ops })?;
        s.cloud_uploads.request_resume(false);
    }
    if let Some(id) = ids.first() {
        s.selection = Selection::single(PhotoId(*id));
    }
    let first = copies.first().cloned().unwrap_or(Value::Null);
    Ok(json!({"ids": ids, "savedLocally": true, "uploadPending": true, "copies":copies,
        "path":first["path"],"id":first["id"],"original":first["original"]}))
}

fn new_identity() -> Result<String> {
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|e| error(format!("could not create a managed identity: {e}")))?;
    Ok(sha256(&random).0)
}

/// Managed conversions prepare and verify new immutable bytes on the import worker.
pub(crate) fn convert_dng(s: &mut Session, ids: &[PhotoId], params: &Value) -> Result<Value> {
    revise(s, ids, params, RevisionKind::Dng)
}

pub(crate) fn save_xmp(s: &mut Session, ids: &[PhotoId], params: &Value) -> Result<Value> {
    revise(s, ids, params, RevisionKind::Xmp)
}

pub(crate) fn reload_working(s: &mut Session, ids: &[PhotoId], params: &Value) -> Result<Value> {
    revise(s, ids, params, RevisionKind::Returned)
}

enum RevisionKind {
    Dng,
    Xmp,
    Returned,
}

fn revise(s: &mut Session, ids: &[PhotoId], params: &Value, kind: RevisionKind) -> Result<Value> {
    if ids.len() > 50 {
        return Err(error("convert at most 50 managed photos at once"));
    }
    let mut job = None;
    for id in ids {
        let photo = s.catalog.photo(*id).ok_or_else(|| error("photo no longer exists"))?;
        if photo.deleted || photo.copy_of.is_some() {
            return Err(error("choose an original photo; virtual-copy settings live in the catalog"));
        }
        let Source::Managed { asset_id } = &photo.source else {
            return Err(error("expected a managed photo"));
        };
        let asset = s.catalog.asset(asset_id).ok_or_else(|| error("managed asset is missing"))?.clone();
        if job.as_ref().is_some_and(|job: &Job| job.connection.id != asset.original.remote.connection) {
            return Err(error("convert photos from one storage connection at a time"));
        }
        let global_id = s
            .catalog
            .cloud_identity(&format!("photo:{}", id.0))
            .map(str::to_owned)
            .or_else(|| s.catalog.upload_intents().iter().find(|(_, intent)| intent.photo_id == *id).map(|(key, _)| key.clone()))
            .map(Ok)
            .unwrap_or_else(new_identity)?;
        let revision = (
            (**photo).clone(),
            asset.clone(),
            RevisionTarget { photo_id: *id, base_asset: asset_id.clone(), global_id },
            match kind {
                RevisionKind::Dng => RevisionWork::Dng(crate::sidecar::sidecar_packet(photo, &s.catalog)),
                RevisionKind::Xmp => RevisionWork::Xmp {
                    packet: crate::sidecar::sidecar_packet(photo, &s.catalog),
                    working: working_relative(s, *id, asset_id, &asset)?,
                },
                RevisionKind::Returned => {
                    let relative = working_relative(s, *id, asset_id, &asset)?.ok_or_else(|| error("photo has no external working copy"))?;
                    RevisionWork::Returned(relative)
                }
            },
        );
        if job.is_none() {
            job = Some(Job::new(s, &json!({"connection":asset.original.remote.connection}), true)?);
        }
        if let Some(job) = &mut job {
            job.revisions.push(revision);
            job.recover = false;
        }
    }
    launch_job(s, params, job.ok_or_else(|| error("no managed photos selected"))?)
}

fn working_relative(s: &Session, id: PhotoId, asset_id: &AssetId, asset: &Asset) -> Result<Option<String>> {
    let Some(working) = s.catalog.working_copy(id) else { return Ok(None) };
    if working.asset_id != *asset_id || working.ingested != asset.original.id {
        return Err(error("working-copy binding changed; preserve the editor file and reopen external editing"));
    }
    let root = s.media.asset_resolver.as_ref().and_then(|resolver| resolver.durable_root()).ok_or_else(|| error("managed storage is unavailable"))?;
    let relative = Path::new(&working.path)
        .strip_prefix(root)
        .map_err(|_| error("working file is outside durable storage"))?
        .to_string_lossy()
        .replace('\\', "/");
    if !working_name(&relative) {
        return Err(error("invalid working-file path"));
    }
    Ok(Some(relative))
}

pub(crate) fn edit_external(s: &mut Session, ids: &[PhotoId], options: &crate::export::ExportOptions, params: &Value) -> Result<Value> {
    let mut job = None;
    for (index, id) in ids.iter().enumerate() {
        let base_asset = match s.catalog.photo(*id).map(|photo| &photo.source) {
            Some(Source::Managed { asset_id }) => asset_id.clone(),
            _ => return Err(error("expected a managed photo")),
        };
        let connection = s
            .catalog
            .photo(*id)
            .and_then(|photo| match &photo.source {
                Source::Managed { asset_id } => s.catalog.asset(asset_id).map(|asset| asset.original.remote.connection.clone()),
                _ => None,
            })
            .ok_or_else(|| error("expected a managed photo"))?;
        if job.as_ref().is_some_and(|job: &Job| job.connection.id != connection) {
            return Err(error("edit photos from one storage connection at a time"));
        }
        if job.is_none() {
            job = Some(Job::new(s, &json!({"connection":connection}), true)?);
        }
        let export = crate::export::prepare_export(s, *id, options, index + 1).map_err(error)?;
        if let Some(job) = &mut job {
            job.edits.push(std::sync::Mutex::new(Some(EditPlan { export, parent: *id, base_asset })));
            job.recover = false;
        }
    }
    launch_job(s, params, job.ok_or_else(|| error("no managed photos selected"))?)
}

struct Pending {
    receiver: mpsc::Receiver<Result<Vec<Manifest>>>,
    library: Arc<()>,
    guard: crate::activity::TaskGuard,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Imports {
    pending: Vec<Pending>,
    pub reports: Vec<Value>,
}
impl Imports {
    pub fn preparing(&self) -> usize {
        self.pending.iter().filter(|pending| !pending.done.load(Ordering::Relaxed)).count()
    }
}

pub fn start(s: &mut Session, p: &Value) -> Result<Value> {
    launch(s, p, false)
}
pub fn recover(s: &mut Session, p: &Value) -> Result<Value> {
    launch(s, p, true)
}

fn launch(s: &mut Session, p: &Value, recover: bool) -> Result<Value> {
    let job = Job::new(s, p, recover)?;
    launch_job(s, p, job)
}

fn launch_job(s: &mut Session, p: &Value, job: Job) -> Result<Value> {
    let wait = p
        .get("wait")
        .map(|value| value.as_bool().ok_or_else(|| error("wait must be true or false")))
        .transpose()?
        .unwrap_or(!crate::assets::is_cached_only());
    if wait && crate::assets::is_cached_only() {
        return Err(error("desktop imports prepare in the background; omit wait or use headless CLI/MCP"));
    }
    if s.cloud_imports.pending.len() >= 4 {
        return Err(error("too many cloud imports; wait for or cancel an import first"));
    }
    let task = s.activity.start("import", "Stage Cloud Originals", crate::activity::Cancel::Yes);
    let cancel = task.cancel_flag().ok_or_else(|| error("could not create import cancellation flag"))?;
    let id = task.id();
    let progress = task.handle();
    if wait {
        let prepared = crate::assets::transfers::with_progress(progress.clone(), || job.prepare(&cancel, &progress))?;
        return commit(s, prepared);
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    let flag = cancel.clone();
    let done = Arc::new(AtomicBool::new(false));
    let worker_done = done.clone();
    std::thread::Builder::new()
        .name("cloud-import".into())
        .spawn(move || {
            struct Finished(Arc<AtomicBool>);
            impl Drop for Finished {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            let _finished = Finished(worker_done);
            let result = crate::assets::transfers::with_progress(progress.clone(), || job.prepare(&flag, &progress));
            let _ = sender.send(result); // Abandoned results retain their manifests and originals.
            job.resolver.assets_changed();
        })
        .map_err(io)?;
    s.cloud_imports.pending.push(Pending { receiver, library: s.library_token(), guard: task, cancel, done });
    Ok(json!({"background": true, "task": id}))
}

impl Session {
    pub fn poll_cloud_imports(&mut self) {
        let mut imports = std::mem::take(&mut self.cloud_imports);
        imports.pending.retain(|pending| {
            let result = match pending.receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => Err(error("import worker stopped; staged originals remain recoverable")),
            };
            let result = if !Arc::ptr_eq(&pending.library, &self.library_token()) || pending.cancel.load(Ordering::Relaxed) {
                Err(error("library changed or import cancelled; staged originals remain recoverable"))
            } else {
                result.and_then(|prepared| self.execute_fn("cloud.import", |s| commit(s, prepared)))
            };
            imports.reports.push(match result {
                Ok(value) => json!({"task": pending.guard.id(), "ok": true, "result": value}),
                Err(e) => json!({"task": pending.guard.id(), "ok": false, "error": e.to_string()}),
            });
            false
        });
        if imports.reports.len() > 32 {
            imports.reports.drain(..imports.reports.len() - 32);
        }
        self.cloud_imports = imports;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture {
        root: PathBuf,
        original: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("lightcraft-staged-import-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&root).unwrap();
            let original = root.join("source.png");
            let image = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [30 + x as u8 * 3, 40 + y as u8 * 4, 100, 255]);
            let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
            std::fs::write(&original, bytes).unwrap();
            Self { root, original }
        }
        fn session(&self) -> Session {
            let mut session = Session::new().with_fs();
            session.open_library(self.root.join("library"), false).unwrap();
            session.execute("cloud.configure", &json!({"id":"personal", "bucket":"photos", "region":"us-east-1", "prefix":"library", "endpoint":"http://127.0.0.1:9", "profile":"unavailable"})).unwrap();
            session
        }
        fn params(&self) -> Value {
            json!({"connection":"personal", "paths":[self.original]})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn managed_dng_revision_preserves_identity_edits_and_virtual_copies_across_restart() {
        let fixture = Fixture::new();
        let raw_path = fixture.root.join("source.raw");
        std::fs::write(&raw_path, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        let mut session = fixture.session();
        let id = PhotoId(session.execute("cloud.import", &json!({"connection":"personal","paths":[raw_path]})).unwrap()["ids"][0].as_u64().unwrap());
        let base = session.catalog.photo(id).unwrap().source.clone();
        let global = session.catalog.cloud_identity(&format!("photo:{}", id.0)).unwrap().to_owned();
        session
            .commit("Synthetic raw format", Op::Relink { id, source: base.clone(), file_name: "source.raw".into(), format: Some("RAW".into()) })
            .unwrap();
        session.execute("develop.set", &json!({"id":id.0,"control":"light.exposure","value":0.75})).unwrap();
        let copy = PhotoId(session.execute("photo.virtualCopy", &json!({"id":id.0})).unwrap()["ids"][0].as_u64().unwrap());
        std::fs::remove_file(raw_path).unwrap();
        let result = session.execute("photo.convertToDng", &json!({"ids":[id.0]})).unwrap();
        assert_eq!(result["managed"]["ids"], json!([id.0]));
        assert_ne!(session.catalog.photo(id).unwrap().source, base);
        assert_eq!(session.catalog.photo(id).unwrap().source, session.catalog.photo(copy).unwrap().source);
        assert_eq!(session.catalog.photo(id).unwrap().develop.light.exposure, 0.75);
        assert_eq!(session.catalog.photo(id).unwrap().format, "DNG");
        assert_eq!(session.catalog.cloud_identity(&format!("photo:{}", id.0)), Some(global.as_str()));
        assert_eq!(session.catalog.upload_intents().len(), 2);
        let revised = session.catalog.photo(id).unwrap().source.clone();
        session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(session.catalog.photo(id).unwrap().source, base);
        session.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(session.catalog.photo(id).unwrap().source, revised);
        session.close_library().unwrap();
        drop(session);
        let mut reopened = fixture.session();
        assert_eq!(reopened.catalog.photo(id).unwrap().source, revised);
        reopened.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
        assert_eq!(reopened.catalog.photos().count(), 2);
        assert_eq!(reopened.catalog.upload_intents().len(), 2);
    }

    #[test]
    fn managed_xmp_is_an_immutable_uploaded_dependency_and_restores_saved_metadata() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = PhotoId(session.execute("cloud.import", &fixture.params()).unwrap()["ids"][0].as_u64().unwrap());
        let Source::Managed { asset_id } = &session.catalog.photo(id).unwrap().source else {
            panic!("managed");
        };
        let original = session.catalog.asset(asset_id).unwrap().original.clone();
        session.execute("photo.rate", &json!({"id":id.0,"rating":4})).unwrap();
        session.execute("develop.set", &json!({"id":id.0,"control":"light.exposure","value":0.5})).unwrap();
        let saved = session.execute("photo.saveMetadataToFile", &json!({"ids":[id.0]})).unwrap();
        assert_eq!(saved["managed"]["ids"], json!([id.0]));
        let Source::Managed { asset_id } = &session.catalog.photo(id).unwrap().source else {
            panic!("managed");
        };
        let asset = session.catalog.asset(asset_id).unwrap();
        assert_eq!(asset.original, original);
        assert!(asset.associated.contains_key("xmp"));
        let xmp = asset.associated["xmp"].clone();
        let root = session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap();
        let packet = std::fs::read(root.join("blobs").join(&xmp.id.0)).unwrap();
        assert_eq!(sha256(&packet), xmp.id);
        assert!(!fixture.root.join("source.xmp").exists());
        session.execute("photo.rate", &json!({"id":id.0,"rating":1})).unwrap();
        session.execute("develop.set", &json!({"id":id.0,"control":"light.exposure","value":0.0})).unwrap();
        let read = session.execute("photo.readMetadataFromFile", &json!({"ids":[id.0]})).unwrap();
        assert_eq!(read["read"].as_array().unwrap().len(), 1);
        assert_eq!(session.catalog.photo(id).unwrap().rating, 4);
        assert_eq!(session.catalog.photo(id).unwrap().develop.light.exposure, 0.5);
        session.close_library().unwrap();
        drop(session);
        let reopened = fixture.session();
        let Source::Managed { asset_id } = &reopened.catalog.photo(id).unwrap().source else {
            panic!("managed");
        };
        assert_eq!(reopened.catalog.asset(asset_id).unwrap().associated["xmp"], xmp);
    }

    #[test]
    fn external_working_file_ingests_a_new_revision_once_and_survives_eviction_and_restart() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = PhotoId(session.execute("cloud.import", &fixture.params()).unwrap()["ids"][0].as_u64().unwrap());
        let edit = session.execute("photo.editExternal", &json!({"ids":[id.0]})).unwrap();
        let derived = PhotoId(edit["id"].as_u64().unwrap());
        let path = PathBuf::from(edit["path"].as_str().unwrap());
        assert!(path.is_file());
        assert!(matches!(session.catalog.photo(derived).unwrap().source, Source::Managed { .. }));
        assert_eq!(session.catalog.stack_of(derived).unwrap().photos, vec![derived, id]);
        let before = session.catalog.working_copy(derived).unwrap().clone();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(sha256(&bytes), before.ingested);
        assert!(session.check_write_target(&path.to_string_lossy()).is_err());
        let alias = fixture.root.join("output-alias.tif");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(session.check_write_target(&alias.to_string_lossy()).unwrap_err().contains("hard link"));
        let image = lightcraft_raster::Rgba8::from_fn(11, 9, |_, _| [60, 100, 180, 255]);
        let returned = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
        std::fs::write(&path, &returned).unwrap();
        session.execute("photo.reload", &json!({"ids":[derived.0]})).unwrap();
        let after = session.catalog.working_copy(derived).unwrap().clone();
        assert_ne!(after.asset_id, before.asset_id);
        assert_eq!(after.ingested, sha256(&returned));
        let photo = session.catalog.photo(derived).unwrap();
        assert_eq!((photo.width, photo.height), (11, 9));
        let pending = session.catalog.upload_intents().len();
        session.execute("photo.reload", &json!({"ids":[derived.0]})).unwrap();
        assert_eq!(session.catalog.upload_intents().len(), pending);
        session.execute("cloud.cache", &json!({"budgetMb":0})).unwrap();
        session.media.asset_resolver.as_ref().unwrap().trim_cache().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), returned);
        session.close_library().unwrap();
        drop(session);
        let mut reopened = fixture.session();
        assert_eq!(reopened.catalog.working_copy(derived), Some(&after));
        let output = crate::export::export_photo(
            &mut reopened,
            derived,
            &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() },
            1,
        )
        .unwrap();
        assert_eq!((output.width, output.height), (11, 9));
        assert_eq!(std::fs::read(path).unwrap(), returned);
    }

    #[test]
    fn recovery_regenerates_partial_renditions_without_replacing_the_durable_original() {
        for boundary in [0, 1, 2, 3] {
            let fixture = Fixture::new();
            let mut session = fixture.session();
            let job = Job::new(&mut session, &fixture.params(), false).unwrap();
            let task = session.activity.start("import", "Crash boundary", crate::activity::Cancel::Yes);
            let mut manifest = job.prepare(&AtomicBool::new(false), &task.handle()).unwrap().remove(0);
            let original = job.root.join("blobs").join(&manifest.asset.original.id.0);
            let before = std::fs::read(&original).unwrap();
            manifest.prepared = false;
            if boundary == 0 {
                manifest.photo = None;
            }
            if boundary <= 1 {
                manifest.asset.smart_preview = None;
            }
            if boundary <= 2 {
                manifest.asset.thumbnail = None;
            }
            manifest.write(&job.root).unwrap();
            std::fs::remove_file(&fixture.original).unwrap();
            drop(task);
            session.close_library().unwrap();
            drop(session);
            let mut reopened = fixture.session();
            let recovered = reopened.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
            assert_eq!(recovered["ids"], json!([1]));
            assert_eq!(std::fs::read(&original).unwrap(), before);
            reopened.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
            assert_eq!(reopened.catalog.photos().count(), 1);
        }
    }

    #[test]
    fn managed_hdr_acquires_staged_originals_and_writes_a_durable_merge_without_source_paths() {
        let fixture = Fixture::new();
        let paths: Vec<_> = lightcraft_merge::synth::bracket_dngs(128, 96, &[-1.0, 1.0])
            .unwrap()
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| {
                let path = fixture.root.join(format!("bracket-{index}.dng"));
                std::fs::write(&path, bytes).unwrap();
                path
            })
            .collect();
        let mut session = fixture.session();
        let imported = session.execute("cloud.import", &json!({"connection":"personal","paths":paths})).unwrap();
        for path in paths {
            std::fs::remove_file(path).unwrap();
        }
        let merged = session.execute("merge.hdr", &json!({"ids":imported["ids"],"autoSettings":false,"align":false})).unwrap();
        let path = PathBuf::from(merged["path"].as_str().unwrap());
        assert!(path.starts_with(fixture.root.join("library/Managed/working/merges")), "{merged}");
        assert!(lightcraft_raw::decode(&std::fs::read(&path).unwrap()).is_ok());
        assert!(session.check_write_target(&path.to_string_lossy()).is_err());
        session.execute("cloud.cache", &json!({"budgetMb":0})).unwrap();
        session.media.asset_resolver.as_ref().unwrap().trim_cache().unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn deleting_the_external_edit_source_during_preparation_preserves_working_data_without_adding_a_photo() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = PhotoId(session.execute("cloud.import", &fixture.params()).unwrap()["ids"][0].as_u64().unwrap());
        let Source::Managed { asset_id } = session.catalog.photo(id).unwrap().source.clone() else { panic!("managed") };
        let mut job = Job::new(&mut session, &json!({"connection":"personal"}), true).unwrap();
        job.recover = false;
        let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Tiff, naming: "{name}-Edit".into(), ..Default::default() };
        let export = crate::export::prepare_export(&mut session, id, &options, 1).unwrap();
        job.edits.push(std::sync::Mutex::new(Some(EditPlan { export, parent: id, base_asset: asset_id })));
        let task = session.activity.start("import", "External test", crate::activity::Cancel::Yes);
        let manifests = job.prepare(&AtomicBool::new(false), &task.handle()).unwrap();
        let working = job.root.join(manifests[0].working.as_ref().unwrap());
        let before = std::fs::read(&working).unwrap();
        session.execute("photo.delete", &json!({"ids":[id.0]})).unwrap();
        assert!(commit(&mut session, manifests).unwrap_err().to_string().contains("source changed or was deleted"));
        assert_eq!(session.catalog.photos().count(), 1);
        assert_eq!(std::fs::read(working).unwrap(), before);
    }

    #[test]
    fn a_differently_named_hard_link_to_an_unuploaded_blob_cannot_be_an_export_target() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.execute("cloud.import", &fixture.params()).unwrap();
        let asset = session.catalog.assets().values().next().unwrap();
        let staged = session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap().join("blobs").join(&asset.original.id.0);
        let alias = fixture.root.join("user-output.png");
        std::fs::hard_link(&staged, &alias).unwrap();
        assert!(session.check_write_target(&alias.to_string_lossy()).unwrap_err().contains("hard link"));
        assert_eq!(std::fs::read(staged).unwrap(), std::fs::read(alias).unwrap());
    }

    #[test]
    fn managed_names_and_logical_folders_preserve_blobs_through_mixed_rename_undo_and_restart() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let imported = session.execute("cloud.import", &fixture.params()).unwrap();
        let managed = PhotoId(imported["ids"][0].as_u64().unwrap());
        let source = session.catalog.photo(managed).unwrap().source.clone();
        let assets = session.catalog.assets().clone();
        let original =
            session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap().join("blobs").join(&assets.values().next().unwrap().original.id.0);
        let bytes = std::fs::read(&original).unwrap();
        let local = session.catalog.alloc_photo_id();
        session
            .commit(
                "Local rename fixture",
                Op::AddPhoto {
                    photo: Box::new(Photo::new(
                        local,
                        Source::File { path: fixture.original.to_string_lossy().into() },
                        "source.png",
                        "PNG",
                        32,
                        24,
                        "",
                    )),
                },
            )
            .unwrap();
        let params = json!({"ids":[managed.0,local.0],"template":"Cloud-{seq:2}","start":3});
        let moved = fixture.root.join("Cloud-04.png");
        let preview = session.execute("photo.renamePreview", &params).unwrap();
        assert!(preview[0].get("from_path").is_none() && preview[0].get("to_path").is_none());
        assert_eq!(preview[1]["to_path"], moved.to_string_lossy().as_ref());
        let renamed = session.execute("photo.rename", &params).unwrap();
        assert_eq!(renamed["renamed"], 2);
        assert_eq!(session.catalog.photo(managed).unwrap().file_name, "Cloud-03.png");
        assert!(moved.is_file() && !fixture.original.exists());
        assert_eq!(session.catalog.photo(managed).unwrap().source, source);
        assert_eq!(session.catalog.assets(), &assets);
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(session.catalog.photo(managed).unwrap().file_name, "source.png");
        assert!(fixture.original.is_file() && !moved.exists());
        session.execute("edit.redo", &json!({})).unwrap();
        assert!(moved.is_file());
        let first = session.execute("album.create", &json!({"name":"Cloud photos","folder":true})).unwrap()["id"].as_u64().unwrap();
        let second = session.execute("album.create", &json!({"name":"Archive","folder":true})).unwrap()["id"].as_u64().unwrap();
        let album = session.execute("album.create", &json!({"name":"Trip","parent":first})).unwrap()["id"].as_u64().unwrap();
        session.execute("album.addPhotos", &json!({"id":album,"ids":[managed.0]})).unwrap();
        session.execute("album.move", &json!({"id":album,"parent":second})).unwrap();
        session.execute("album.rename", &json!({"id":second,"name":"Cloud archive"})).unwrap();
        session.execute("library.source", &json!({"kind":"album","id":album})).unwrap();
        assert_eq!(session.visible_cloned(), vec![managed]);
        let physical = fixture.root.join("physical");
        std::fs::create_dir(&physical).unwrap();
        assert!(
            session.execute("photo.moveToFolder", &json!({"ids":[managed.0],"folder":physical})).unwrap_err().to_string().contains("logical folders")
        );
        assert_eq!(session.catalog.assets(), &assets);
        session.close_library().unwrap();
        drop(session);
        let reopened = fixture.session();
        assert_eq!(reopened.catalog.photo(managed).unwrap().file_name, "Cloud-03.png");
        assert_eq!(reopened.catalog.photo(managed).unwrap().source, source);
        assert_eq!(reopened.catalog.assets(), &assets);
        let album = reopened.catalog.album(lightcraft_catalog::AlbumId(album)).unwrap();
        assert_eq!(album.parent, Some(lightcraft_catalog::AlbumId(second)));
        assert_eq!(album.photos, vec![managed]);
        assert_eq!(std::fs::read(original).unwrap(), bytes);
    }

    #[test]
    fn oversized_rename_sequence_is_rejected_before_any_name_or_path_changes() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.execute("cloud.import", &fixture.params()).unwrap();
        let copy = session.execute("photo.virtualCopy", &json!({})).unwrap();
        let copy = copy["ids"][0].as_u64().unwrap();
        let before = session.catalog.to_snapshot();
        for start in [json!(u64::MAX), json!(-1), json!("bad")] {
            for command in ["photo.renamePreview", "photo.rename"] {
                let error = session.execute(command, &json!({"ids":[1,copy],"template":"{seq}","start":start})).unwrap_err();
                assert!(!error.to_string().contains("panic"), "input is validated without reaching the panic guard: {error}");
                assert_eq!(session.catalog.to_snapshot(), before);
            }
        }
        // Direct planning also stays total at the integer boundary.
        let plans = session.plan_rename(&[PhotoId(1), PhotoId(copy)], "{seq}", usize::MAX);
        assert_eq!(plans.len(), 2);
        assert_ne!(plans[0].to, plans[1].to);
    }

    #[test]
    fn import_is_editable_after_source_removal_and_originals_survive_zero_cache_budget_and_restart() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let imported = session.execute("cloud.import", &fixture.params()).unwrap();
        let id = PhotoId(imported["ids"][0].as_u64().unwrap());
        assert_eq!(session.catalog.upload_intents().len(), 1);
        let photo = session.catalog.photo(id).unwrap();
        assert!(matches!(photo.source, Source::Managed { .. }));
        let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
        let intent = session.catalog.upload_intents().values().next().unwrap();
        let asset = session.catalog.asset(&intent.asset_id).unwrap().clone();
        assert!(asset.smart_preview.is_some() && asset.thumbnail.is_some());
        assert!(session.quick_thumb_is_final(photo, 512));
        std::fs::remove_file(&fixture.original).unwrap();
        session.execute("cloud.cache", &json!({"budgetMb":0})).unwrap();
        resolver.trim_cache().unwrap();
        assert!(resolver.evict(&asset.original).is_err());
        assert!(session.check_write_target(&resolver.durable_root().unwrap().join("blobs").join(&asset.original.id.0).to_string_lossy()).is_err());
        session.execute("develop.set", &json!({"id":id.0,"control":"light.exposure","value":0.5})).unwrap();
        let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Png, resize: None, ..Default::default() };
        let exported = crate::export::prepare_export(&mut session, id, &options, 1).unwrap().run().unwrap();
        assert_eq!((exported.width, exported.height), (32, 24));
        session.close_library().unwrap();
        drop(session);
        let mut reopened = fixture.session();
        assert_eq!(reopened.catalog.upload_intents().len(), 1);
        assert_eq!(reopened.execute("library.missing", &json!({})).unwrap(), json!([]));
        let exported = crate::export::prepare_export(&mut reopened, id, &options, 1).unwrap().run().unwrap();
        assert_eq!((exported.width, exported.height), (32, 24));
        let recovered = reopened.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
        assert_eq!(recovered["ids"], json!([]), "committed receipts prevent duplicate recovery");
        assert_eq!(reopened.catalog.photos().count(), 1);
    }

    #[test]
    fn abandoned_preparation_recovers_once_without_the_source_file() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let job = Job::new(&mut session, &fixture.params(), false).unwrap();
        let task = session.activity.start("import", "Prepare test", crate::activity::Cancel::Yes);
        let prepared = job.prepare(&AtomicBool::new(false), &task.handle()).unwrap();
        let import_id = prepared[0].id.clone();
        assert!(session.catalog.upload_intents().is_empty());
        std::fs::remove_file(&fixture.original).unwrap();
        drop(prepared);
        drop(task);
        session.close_library().unwrap();
        drop(session);
        let mut reopened = fixture.session();
        let recovered = reopened.execute("cloud.recoverImports", &json!({"connection":"personal","imports":[import_id]})).unwrap();
        assert_eq!(recovered["ids"].as_array().unwrap().len(), 1);
        assert_eq!(reopened.catalog.upload_intents().len(), 1);
        reopened.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
        assert_eq!(reopened.catalog.photos().count(), 1);
    }

    #[test]
    fn failed_journal_append_keeps_staged_data_and_upload_intent_for_save_retry() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let job = Job::new(&mut session, &fixture.params(), false).unwrap();
        let task = session.activity.start("import", "Prepare test", crate::activity::Cancel::Yes);
        let prepared = job.prepare(&AtomicBool::new(false), &task.handle()).unwrap();
        let stage_file = job.root.join("blobs").join(&prepared[0].asset.original.id.0);
        let blocker = fixture.root.join("library").join(lightcraft_catalog::journal::LOG);
        std::fs::create_dir(&blocker).unwrap();
        assert!(session.execute_fn("cloud.import", |s| commit(s, prepared)).is_err());
        assert!(stage_file.is_file());
        assert_eq!(session.catalog.upload_intents().len(), 1);
        std::fs::remove_dir(&blocker).unwrap();
        drop(task);
        session.persist().unwrap();
        session.close_library().unwrap();
        drop(session);
        assert_eq!(fixture.session().catalog.upload_intents().len(), 1);
    }

    #[test]
    fn durable_working_files_and_staging_are_outside_disposable_eviction() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.execute("cloud.import", &fixture.params()).unwrap();
        let resolver = session.media.asset_resolver.as_ref().unwrap();
        let root = resolver.durable_root().unwrap();
        let working = root.join("working");
        std::fs::create_dir_all(&working).unwrap();
        let unsynced = working.join("a".repeat(64));
        std::fs::write(&unsynced, b"externally edited unsynchronized working data").unwrap();
        resolver.set_cache_budget(0).unwrap();
        assert_eq!(resolver.trim_cache().unwrap().used_bytes, 0);
        assert!(unsynced.is_file());
        let asset = session.catalog.assets().values().next().unwrap();
        let blobs: std::collections::BTreeSet<_> =
            std::iter::once(&asset.original).chain(asset.smart_preview.iter()).chain(asset.thumbnail.iter()).map(|blob| &blob.id).collect();
        assert_eq!(std::fs::read_dir(root.join("blobs")).unwrap().count(), blobs.len());
    }

    #[test]
    fn overlapping_cache_and_durable_storage_disable_deletion_and_import() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
        std::fs::create_dir_all(resolver.root()).unwrap();
        let protected = resolver.root().join("a".repeat(64));
        std::fs::write(&protected, b"unuploaded data").unwrap();
        resolver.set_durable_root(resolver.root().to_path_buf());
        assert!(resolver.set_cache_budget(0).is_err());
        assert!(resolver.trim_cache().is_err());
        assert!(session.execute("cloud.import", &fixture.params()).is_err());
        assert_eq!(std::fs::read(&protected).unwrap(), b"unuploaded data");
    }

    #[test]
    fn desktop_preparation_is_background_and_library_switch_does_not_commit_into_another_library() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let _ui = crate::assets::CachedOnly::enter(true);
        assert!(session.execute("cloud.import", &json!({"connection":"personal","paths":[fixture.original],"wait":true})).is_err());
        let started = session.execute("cloud.import", &fixture.params()).unwrap();
        assert_eq!(started["background"], true);
        let done = session.cloud_imports.pending[0].done.clone();
        session.open_library(fixture.root.join("other"), false).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            session.poll_cloud_imports();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(done.load(Ordering::Relaxed));
        assert!(session.catalog.upload_intents().is_empty());
        assert_eq!(session.catalog.photos().count(), 0);
    }
}
