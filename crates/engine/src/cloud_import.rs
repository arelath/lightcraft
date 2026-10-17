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

mod adopt;
pub(crate) use adopt::start as adopt;
mod automatic;
pub(crate) use automatic::resume as resume_library;
pub(crate) use automatic::retry as retry_library;
mod issues;
mod requests;
mod settled;
use issues::Issue;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Recipe {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    adoption: Vec<Photo>,
    now: String,
    defaults: crate::import::ImportDefaults,
    presets: Vec<lightcraft_develop::Preset>,
    metadata_presets: Vec<crate::cmd::metadata::MetadataPreset>,
    naming: crate::sidecar::SidecarNaming,
    labels: Vec<(lightcraft_catalog::ColorLabel, String)>,
}

struct Completed {
    value: Value,
    skipped: Vec<settled::Skipped>,
}

struct Prepared {
    automatic: bool,
    order: Vec<String>,
    skipped: Vec<settled::Skipped>,
    manifests: Vec<Manifest>,
    failed: Vec<Issue>,
    request: Option<String>,
    duplicate_policy: requests::DuplicatePolicy,
}

const MAX_BYTES: u64 = crate::input_limits::ORIGINAL_BYTES;
const MAX_DOCUMENT_BYTES: u64 = 16 << 20;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duplicate_policy: Option<requests::DuplicatePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adoption: Option<adopt::Target>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisionTarget {
    pub photo_id: PhotoId,
    pub base_asset: AssetId,
    pub global_id: String,
}

impl Manifest {
    pub(crate) fn adopted_photo(&self) -> Option<PhotoId> {
        self.adoption.as_ref().map(|target| target.photo_id())
    }
    fn validate(&self) -> Result<()> {
        if !matches!(self.version, 1..=5)
            || (self.version < 4 && self.adoption.is_some())
            || (self.version < 5 && self.adoption.as_ref().is_some_and(adopt::Target::generated))
            || (self.adoption.is_some()
                && (self.target.is_some() || self.working.is_some() || self.stack_with.is_some() || self.origin_asset.is_some()))
            || self.adoption.as_ref().is_some_and(|target| !target.valid())
            || (self.version < 3 && self.duplicate_policy.is_some())
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
                || !crate::input_limits::shared_dimensions(photo.width, photo.height))
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
    if limit == MAX_BYTES {
        crate::input_limits::bytes(size).map_err(|e| error(format!("{}: {e}", path.display())))?;
    }
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

struct RequestOptions {
    requested: Option<String>,
    fresh: bool,
    policy: Option<requests::DuplicatePolicy>,
}
/// The completion context is frozen when preparation starts. Managed local
/// revisions use their originating operation, independently of cloud enrollment.
#[derive(Clone, Copy, Default)]
enum PreparationKind {
    #[default]
    Enrollment,
    Dng,
    Xmp,
    Returned,
    ExternalCopy,
}
impl PreparationKind {
    fn command(self) -> &'static str {
        match self {
            Self::Enrollment => "cloud.import",
            Self::Dng => "photo.convertToDng",
            Self::Xmp => "photo.saveMetadataToFile",
            Self::Returned => "photo.reload",
            Self::ExternalCopy => "photo.editExternal",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Enrollment => "Stage Cloud Originals",
            Self::Dng => "Convert to DNG",
            Self::Xmp => "Save Metadata to File",
            Self::Returned => "Reload External Edit",
            Self::ExternalCopy => "Prepare External Edit",
        }
    }
}
struct Job {
    kind: PreparationKind,
    automatic: bool,
    paths: Vec<PathBuf>,
    adoption: Option<Vec<Photo>>,
    adoption_key: Option<String>,
    root: PathBuf,
    connection: crate::assets::s3::S3Connection,
    resolver: Arc<AssetResolver>,
    now: String,
    defaults: crate::import::ImportDefaults,
    presets: Vec<lightcraft_develop::Preset>,
    metadata_presets: Vec<crate::cmd::metadata::MetadataPreset>,
    naming: crate::sidecar::SidecarNaming,
    labels: lightcraft_catalog::Catalog,
    recover: bool,
    known: std::collections::BTreeSet<String>,
    selected: Option<std::collections::BTreeSet<String>>,
    revisions: Vec<(Photo, Asset, RevisionTarget, RevisionWork)>,
    edits: Vec<std::sync::Mutex<Option<EditPlan>>>,
    request: Option<requests::Request>,
    request_options: Option<RequestOptions>,
    failed: std::sync::Mutex<Vec<Issue>>,
    skipped: std::sync::Mutex<Vec<settled::Skipped>>,
}

struct EditPlan {
    export: crate::export::PreparedExport,
    parent: PhotoId,
    base_asset: AssetId,
    asset: Asset,
    packet: String,
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
    fn document(&self, name: &str, bytes: &[u8]) -> Result<Blob> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(error("supporting document is empty or exceeds the 16 MiB safety limit; source preserved"));
        }
        let key =
            keys::in_library(&self.connection.prefix, &format!("associated/{name}/{}.xmp", sha256(bytes).0)).map_err(|e| error(e.to_string()))?;
        Ok(make_blob(bytes, &self.connection.id, key))
    }

    fn describe(&self, manifest: &Manifest, info: crate::media::ProbeInfo, packet: Option<&[u8]>) -> Result<Photo> {
        if info.kind == lightcraft_catalog::MediaKind::Video || info.preview_only.is_some() {
            return Err(error("managed import requires a decodable photo original"));
        }
        let sidecar = packet
            .map(|packet| {
                let packet = std::str::from_utf8(packet).map_err(|e| error(format!("supporting XMP is unreadable; source preserved: {e}")))?;
                let target = crate::crs::Target::for_file(info.kind, &info.format, info.preview_only.is_some(), &info.meta.camera);
                crate::sidecar::parse_sidecar(packet, target)
                    .map(|data| data.resolve_label(&self.labels))
                    .map_err(|e| error(format!("supporting XMP could not be applied; source preserved: {e}")))
            })
            .transpose()?;
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
            let mut defaults = Session::new();
            defaults.import_defaults = self.defaults.clone();
            defaults.presets = self.presets.clone();
            defaults.metadata_presets = self.metadata_presets.clone();
            crate::import::apply_import_defaults(&defaults, &mut photo);
        }
        if let Some(sidecar) = sidecar {
            crate::sidecar::merge_into(&mut photo, &sidecar, &self.now);
        }
        Ok(photo)
    }

    /// Read all supporting documents while the previous asset is still reachable.
    /// New revisions carry them into staging even on a freshly joined device.
    fn supporting(&self, asset: &Asset, cancel: &AtomicBool) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
        asset
            .associated
            .iter()
            .map(|(name, blob)| {
                let lease = self.resolver.acquire(blob, cancel).map_err(|e| error(e.to_string()))?;
                let bytes = read_bounded(lease.path(), MAX_DOCUMENT_BYTES)?;
                Ok((name.clone(), bytes))
            })
            .collect()
    }

    fn merge_packet(documents: &std::collections::BTreeMap<String, Vec<u8>>, fresh: &str) -> Result<String> {
        match documents.get("xmp") {
            Some(old) => crate::sidecar::merge_managed_packet(old, fresh).map_err(error),
            None => Ok(fresh.into()),
        }
    }

    fn stage_documents(&self, manifest: &Manifest, documents: &std::collections::BTreeMap<String, Vec<u8>>, cancel: &AtomicBool) -> Result<()> {
        for (name, bytes) in documents {
            let blob = manifest.asset.associated.get(name).ok_or_else(|| error("supporting document description is missing"))?;
            self.stage(blob, bytes, cancel)?;
        }
        Ok(())
    }

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
        let paths = if recover || (p.get("requestId").is_some() && p.get("paths").is_none()) {
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
        // Sidecar interpretation needs only the five label names, not a clone of
        // the library's photos/documents. Freeze them with the import defaults.
        let mut labels = lightcraft_catalog::Catalog::new();
        for label in lightcraft_catalog::ColorLabel::ALL {
            if let Some(name) = s.catalog.custom_label_name(label) {
                labels.apply(Op::SetLabelName { label, name: Some(name.into()) })?;
            }
        }
        let mut job = Self {
            kind: PreparationKind::Enrollment,
            paths,
            adoption: None,
            adoption_key: None,
            automatic: false,
            root,
            connection,
            resolver,
            now: (s.clock)(),
            defaults: s.import_defaults.clone(),
            presets: s.presets.clone(),
            metadata_presets: s.metadata_presets.clone(),
            naming: s.xmp.naming,
            labels,
            recover,
            known: s.catalog.upload_intents().keys().cloned().collect(),
            selected,
            revisions: Vec::new(),
            edits: Vec::new(),
            request: None,
            request_options: None,
            failed: Default::default(),
            skipped: Default::default(),
        };
        if !recover {
            let requested = p.get("requestId").map(|v| v.as_str().ok_or_else(|| error("requestId must be a string"))).transpose()?;
            let fresh =
                p.get("newRequest").map(|v| v.as_bool().ok_or_else(|| error("newRequest must be true or false"))).transpose()?.unwrap_or(false);
            let policy = p
                .get("duplicatePolicy")
                .map(|v| serde_json::from_value(v.clone()).map_err(|_| error("duplicatePolicy must be skip or allow")))
                .transpose()?;
            job.request_options = Some(RequestOptions { requested: requested.map(str::to_owned), fresh, policy });
        }
        Ok(job)
    }

    fn initialize_request(&mut self, cancel: &AtomicBool) -> Result<()> {
        if self.request.is_some() {
            return Ok(());
        }
        let Some(options) = &self.request_options else {
            return Ok(());
        };
        let requested = options.requested.as_deref();
        let fresh = options.fresh;
        let policy = options.policy;
        let recipe = Recipe {
            adoption: self.adoption.clone().unwrap_or_default(),
            now: self.now.clone(),
            defaults: self.defaults.clone(),
            presets: self.presets.clone(),
            metadata_presets: self.metadata_presets.clone(),
            naming: self.naming,
            labels: lightcraft_catalog::ColorLabel::ALL
                .into_iter()
                .filter_map(|label| self.labels.custom_label_name(label).map(|name| (label, name.to_owned())))
                .collect(),
        };
        let request = {
            let _guard = self.resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
            let request_root = if self.adoption.is_some() { self.root.join("adoptions") } else { self.root.clone() };
            requests::ensure_directory(&self.root)?;
            requests::register_keyed(
                &request_root,
                &self.connection,
                &self.paths,
                requested,
                fresh,
                policy,
                serde_json::to_value(recipe).map_err(|e| error(e.to_string()))?,
                self.adoption_key.clone(),
            )?
        };
        let recipe: Recipe = serde_json::from_value(request.recipe.clone()).map_err(|_| error("invalid saved import recipe; request preserved"))?;
        self.paths = request.candidates.iter().map(|candidate| PathBuf::from(&candidate.path)).collect();
        if self.adoption.is_some() {
            self.adoption = Some(recipe.adoption);
        } else if !recipe.adoption.is_empty() {
            return Err(error("an adoption request cannot be used as a new import"));
        }
        self.now = recipe.now;
        self.defaults = recipe.defaults;
        self.presets = recipe.presets;
        self.metadata_presets = recipe.metadata_presets;
        self.naming = recipe.naming;
        self.labels = lightcraft_catalog::Catalog::new();
        for (label, name) in recipe.labels {
            self.labels.apply(Op::SetLabelName { label, name: Some(name) })?;
        }
        self.request = Some(request);
        Ok(())
    }

    fn failure(&self, id: &str, name: &str, reason: String, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("import cancelled; staged data and request retained"));
        }
        let (command, retry) = match &self.request {
            Some(request) => (
                if self.adoption.is_some() { "cloud.adopt" } else { "cloud.import" },
                if self.adoption.is_some() {
                    json!({"connection":self.connection.id,"requestId":request.id,"wait":false,"automatic":self.automatic,
                        "adoptionIds":self.adoption.as_ref().into_iter().flatten().map(|photo| photo.id.0).collect::<Vec<_>>()})
                } else {
                    json!({"connection":self.connection.id,"requestId":request.id,"wait":false})
                },
            ),
            None => ("cloud.recoverImports", json!({"connection":self.connection.id,"imports":[id],"wait":false})),
        };
        let issue = Issue { id: id.into(), name: name.into(), reason, command: command.into(), retry };
        let _guard = self.resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
        issues::update(&self.root, std::slice::from_ref(&issue), &[])?;
        self.failed.lock().map_err(|_| error("import failure state unavailable"))?.push(issue);
        Ok(())
    }

    fn persist_completion(&self, result: Result<Completed>) -> Result<Value> {
        let Completed { value, skipped } = result?;
        let failed: Vec<Issue> = serde_json::from_value(value["failed"].clone()).map_err(|e| error(e.to_string()))?;
        let completed: Vec<String> = serde_json::from_value(value["completedImports"].clone()).map_err(|e| error(e.to_string()))?;
        let _guard = self.resolver.durable_guard(&AtomicBool::new(false)).map_err(|e| error(e.to_string()))?;
        for record in &skipped {
            settled::save(&self.root, record)?;
        }
        issues::update(&self.root, &failed, &completed)?;
        Ok(value)
    }

    fn run(&mut self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Prepared> {
        let manifests = self.prepare(cancel, progress)?;
        Ok(Prepared {
            automatic: self.automatic,
            order: self.request.as_ref().map_or_else(Vec::new, |request| request.candidates.iter().map(|candidate| candidate.id.clone()).collect()),
            skipped: self.skipped.lock().map_err(|_| error("settled import state unavailable"))?.clone(),
            manifests,
            failed: self.failed.lock().map_err(|_| error("import failure state unavailable"))?.clone(),
            request: self.request.as_ref().map(|r| r.id.clone()),
            duplicate_policy: self.request.as_ref().map_or(requests::DuplicatePolicy::Allow, |r| r.duplicate_policy),
        })
    }

    fn prepare(&mut self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Vec<Manifest>> {
        self.initialize_request(cancel)?;
        if self.adoption.is_some() {
            return self.prepare_adoption(cancel, progress);
        }
        if self.recover {
            return self.recover_prepared(cancel, progress);
        }
        if !self.revisions.is_empty() {
            return self.prepare_revisions(cancel, progress);
        }
        if !self.edits.is_empty() {
            return self.prepare_edits(cancel, progress);
        }
        let request = self.request.as_ref().ok_or_else(|| error("import request is missing"))?;
        let mut prepared = Vec::new();
        for (index, candidate) in request.candidates.iter().enumerate() {
            self.resolver.validate_storage().map_err(|e| error(e.to_string()))?;
            if cancel.load(Ordering::Relaxed) {
                return Err(error("cloud import cancelled; request and staged bytes remain recoverable"));
            }
            progress.detail(&candidate.path);
            let result = (|| -> Result<Option<Manifest>> {
                if let Some(record) = settled::read(&self.root, &candidate.id)? {
                    self.connection.validate_blob(&record.manifest.asset.original).map_err(|e| error(e.to_string()))?;
                    self.skipped.lock().map_err(|_| error("settled import state unavailable"))?.push(record);
                    return Ok(None);
                }
                self.prepare_candidate(candidate, cancel).map(Some)
            })();
            match result {
                Ok(Some(manifest)) => prepared.push(manifest),
                Ok(None) => {}
                Err(e) => self.failure(&candidate.id, &candidate.path, e.to_string(), cancel)?,
            }
            progress.progress(index as u64 + 1, request.candidates.len() as u64);
        }
        Ok(prepared)
    }

    fn prepare_candidate(&self, candidate: &requests::Candidate, cancel: &AtomicBool) -> Result<Manifest> {
        let path = Path::new(&candidate.path);
        let record = self.root.join("imports").join(format!("{}.json", candidate.id));
        match std::fs::symlink_metadata(&record) {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(error("staged manifest is redirected; preserved"));
                }
                let mut manifest = read_manifest(&self.root, &candidate.id)?;
                if self.known.contains(&candidate.id) {
                    return Ok(manifest);
                }
                // A crash may precede any of the immutable writes. Only bytes
                // matching this frozen manifest may fill those missing copies.
                let source_path = self.root.join("blobs").join(&manifest.asset.original.id.0);
                if !source_path.try_exists().map_err(io)? {
                    self.stage(&manifest.asset.original, &read_bounded(path, MAX_BYTES)?, cancel)?;
                }
                for (name, blob) in &manifest.asset.associated {
                    if !self.root.join("blobs").join(&blob.id.0).try_exists().map_err(io)? {
                        if name != "xmp" {
                            return Err(error("supporting document is not staged; restore its matching immutable copy and retry"));
                        }
                        let bytes = if let Some(source) = crate::sidecar::find_sidecar(&candidate.path, self.naming) {
                            read_bounded(&source, MAX_DOCUMENT_BYTES)?
                        } else {
                            let original = read_bounded(&source_path, MAX_BYTES)?;
                            crate::files::probe_bytes(&manifest.file_name, &original)
                                .map_err(error)?
                                .xmp
                                .ok_or_else(|| {
                                    error("supporting document is not staged; restore the original matching sidecar and retry this request")
                                })?
                                .into_bytes()
                        };
                        self.stage(blob, &bytes, cancel)?;
                    }
                }
                self.finish(&mut manifest, cancel)?;
                return Ok(manifest);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        let bytes = read_bounded(path, MAX_BYTES)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(error("cloud import cancelled; original source preserved"));
        }
        let file_name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| error("original filename must be valid UTF-8"))?.to_owned();
        let info = crate::files::probe_bytes(&file_name, &bytes).map_err(error)?;
        let packet = if let Some(sidecar) = crate::sidecar::find_sidecar(&candidate.path, self.naming) {
            Some(read_bounded(&sidecar, MAX_DOCUMENT_BYTES)?)
        } else {
            info.xmp.clone().filter(|_| info.kind == lightcraft_catalog::MediaKind::Raw).map(String::into_bytes)
        };
        let source = sha256(&bytes);
        let original = make_blob(
            &bytes,
            &self.connection.id,
            keys::in_library(&self.connection.prefix, &keys::original(&source).map_err(|e| error(e.to_string()))?)
                .map_err(|e| error(e.to_string()))?,
        );
        let mut manifest = Manifest {
            version: 3,
            id: candidate.id.clone(),
            asset_id: AssetId(candidate.id.clone()),
            asset: Asset { associated: Default::default(), original, smart_preview: None, thumbnail: None },
            file_name,
            photo: None,
            prepared: false,
            target: None,
            adoption: None,
            working: None,
            stack_with: None,
            origin_asset: None,
            duplicate_policy: self.request.as_ref().map(|r| r.duplicate_policy),
        };
        if let Some(packet) = &packet {
            manifest.asset.associated.insert("xmp".into(), self.document("xmp", packet)?);
        }
        manifest.photo = Some(Box::new(self.describe(&manifest, info, packet.as_deref())?));
        self.write_manifest(&manifest, cancel)?;
        self.stage(&manifest.asset.original, &bytes, cancel)?;
        if let Some(packet) = &packet
            && let Some(blob) = manifest.asset.associated.get("xmp")
        {
            self.stage(blob, packet, cancel)?;
        }
        drop(bytes);
        self.finish(&mut manifest, cancel)?;
        Ok(manifest)
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
            let mut documents = self.supporting(asset, cancel)?;
            let (bytes, name, packet) = match work {
                RevisionWork::Dng(packet) => {
                    let merged = Self::merge_packet(&documents, packet)?;
                    (
                        crate::cmd::convert::encode_dng(&bytes, merged.clone()).map_err(error)?,
                        Path::new(&photo.file_name).with_extension("dng").to_string_lossy().to_string(),
                        merged,
                    )
                }
                RevisionWork::Xmp { packet, .. } => (bytes, photo.file_name.clone(), Self::merge_packet(&documents, packet)?),
                RevisionWork::Returned(_) => {
                    (bytes, photo.file_name.clone(), Self::merge_packet(&documents, &crate::sidecar::sidecar_packet(photo, &self.labels))?)
                }
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
                asset: Asset { original, smart_preview: None, thumbnail: None, associated: asset.associated.clone() },
                file_name: name,
                photo: Some(Box::new(updated)),
                prepared: false,
                target: Some(target.clone()),
                adoption: None,
                working: match work {
                    RevisionWork::Returned(path) => Some(path.clone()),
                    RevisionWork::Xmp { working, .. } => working.clone(),
                    _ => None,
                },
                stack_with: None,
                origin_asset: None,
                duplicate_policy: None,
            };
            let xmp = packet.into_bytes();
            manifest.asset.associated.insert("xmp".into(), self.document("xmp", &xmp)?);
            documents.insert("xmp".into(), xmp);
            self.write_manifest(&manifest, cancel)?;
            self.stage(&manifest.asset.original, &bytes, cancel)?;
            self.stage_documents(&manifest, &documents, cancel)?;
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
            let EditPlan { export, parent, base_asset, asset, packet } = edit
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
            let mut documents = self.supporting(&asset, cancel)?;
            let packet = Self::merge_packet(&documents, &packet)?.into_bytes();
            let mut manifest = Manifest {
                version: 2,
                id: id.clone(),
                asset_id: AssetId(id.clone()),
                asset: Asset { original, smart_preview: None, thumbnail: None, associated: asset.associated },
                file_name: name.clone(),
                photo: None,
                prepared: false,
                target: None,
                adoption: None,
                working: Some(format!("working/{id}/{name}")),
                stack_with: Some(parent),
                origin_asset: Some(base_asset),
                duplicate_policy: None,
            };
            manifest.asset.associated.insert("xmp".into(), self.document("xmp", &packet)?);
            documents.insert("xmp".into(), packet);
            self.write_manifest(&manifest, cancel)?;
            self.stage(&manifest.asset.original, &output.bytes, cancel)?;
            self.stage_documents(&manifest, &documents, cancel)?;
            self.finish(&mut manifest, cancel)?;
            prepared.push(manifest);
            progress.progress(index as u64 + 1, self.edits.len() as u64);
        }
        Ok(prepared)
    }

    fn recover_prepared(&self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Vec<Manifest>> {
        let dir = self.root.join("imports");
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err(error("staging manifest directory is redirected; data preserved")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
        }
        let mut manifests = Vec::new();
        for (count, entry) in std::fs::read_dir(&dir).map_err(io)?.enumerate() {
            if count >= 100_000 {
                return Err(error("too many staging manifests to recover safely"));
            }
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(id) = name.strip_suffix(".json").filter(|id| lightcraft_catalog::assets::identifier(id)) else { continue };
            if self.known.contains(id) || self.selected.as_ref().is_some_and(|selected| !selected.contains(id)) {
                continue;
            }
            if manifests.len() >= 1000 {
                return Err(error("recover at most 1000 imports at once; select their import identities"));
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(error("import recovery cancelled; staged data retained"));
            }
            let result = (|| -> Result<Option<Manifest>> {
                if !entry.file_type().map_err(io)?.is_file() {
                    return Err(error("staged manifest is not a regular file; preserved"));
                }
                let mut manifest = read_manifest(&self.root, id)?;
                if manifest.asset.original.remote.connection != self.connection.id {
                    return Ok(None);
                }
                for blob in std::iter::once(&manifest.asset.original).chain(manifest.asset.associated.values()) {
                    self.connection.validate_blob(blob).map_err(|e| error(e.to_string()))?;
                }
                if let Some(record) = settled::read(&self.root, id)? {
                    self.skipped.lock().map_err(|_| error("settled import state unavailable"))?.push(record);
                    return Ok(None);
                }
                progress.detail(&manifest.file_name);
                self.finish(&mut manifest, cancel)?;
                Ok(Some(manifest))
            })();
            match result {
                Ok(Some(manifest)) => manifests.push(manifest),
                Ok(None) => {}
                Err(e) => self.failure(id, &name, e.to_string(), cancel)?,
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
        if self::adopt::original_mismatch(manifest, &bytes) {
            return Err(error("staged adoption original differs from the saved source identity; retained for repair"));
        }
        let mut packet = None;
        for (name, blob) in &manifest.asset.associated {
            let document = read_bounded(&self.root.join("blobs").join(&blob.id.0), MAX_DOCUMENT_BYTES)?;
            if sha256(&document) != blob.id || document.len() as u64 != blob.size {
                return Err(error("associated document failed verification"));
            }
            if name == "xmp" {
                packet = Some(document);
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
            self.describe(manifest, info, packet.as_deref())?
        };
        photo.id = PhotoId(1);
        manifest.photo = Some(Box::new(photo.clone()));
        manifest.prepared = false;
        self.write_manifest(manifest, cancel)?;
        photo.copy_of = None; // the render seed does not need its catalog family
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

#[derive(Default)]
struct Plan {
    ids: Vec<u64>,
    ops: Vec<Op>,
    copies: Vec<Value>,
    stacks: Vec<(PhotoId, Option<PhotoId>, Op)>,
    skipped: Option<settled::Skipped>,
}
fn plan_candidate(catalog: &mut lightcraft_catalog::Catalog, root: Option<&Path>, manifest: Manifest, duplicate: Option<PhotoId>) -> Result<Plan> {
    let mut plan = Plan::default();
    manifest.validate()?;
    if !manifest.prepared {
        return Err(error("import has not finished staging its renditions"));
    }
    if manifest.adoption.is_some() {
        return adopt::plan(catalog, manifest);
    }
    if let Some(existing) = catalog.upload_intents().get(&manifest.id) {
        plan.ids.push(existing.photo_id.0);
        return Ok(plan);
    }
    if let Some(id) = duplicate {
        plan.ids.push(id.0);
        plan.skipped = Some(settled::Skipped::new(manifest, id));
        return Ok(plan);
    }
    if let (Some(parent), Some(base)) = (manifest.stack_with, &manifest.origin_asset)
        && catalog.photo(parent).is_none_or(|photo| photo.deleted || photo.source != (Source::Managed { asset_id: base.clone() }))
    {
        return Err(error("external-edit source changed or was deleted; durable working data was retained"));
    }
    let mut photo = *manifest.photo.ok_or_else(|| error("staged photo is missing"))?;
    let global_id = if let Some(target) = &manifest.target {
        let current = catalog.photo(target.photo_id).ok_or_else(|| error("revision photo no longer exists; staged data preserved"))?;
        if current.deleted || current.source != (Source::Managed { asset_id: target.base_asset.clone() }) {
            return Err(error("photo changed while preparing its revision; staged data preserved"));
        }
        photo.id = target.photo_id;
        target.global_id.clone()
    } else {
        photo.id = catalog.alloc_photo_id();
        manifest.id.clone()
    };
    let intent = UploadIntent {
        asset_id: manifest.asset_id.clone(),
        photo_id: photo.id,
        state: UploadState::SavedLocally,
        verified: Default::default(),
        error: None,
        finalization: None,
        blocked: None,
    };
    if let Some(relative) = &manifest.working {
        let root = root.ok_or_else(|| error("working storage is unavailable"))?;
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
        plan.copies.push(json!({"path":path,"id":photo.id.0,"original":manifest.stack_with.map(|id|id.0)}));
        plan.stacks.push((photo.id, manifest.stack_with, binding));
    }
    plan.ids.push(photo.id.0);
    plan.ops.push(Op::SetAsset { id: manifest.asset_id, asset: Some(Box::new(manifest.asset)) });
    if manifest.target.is_some() {
        let copies: Vec<_> = catalog
            .photos()
            .filter(|copy| {
                copy.id == photo.id
                    || (copy.copy_of == Some(photo.id)
                        && manifest.target.as_ref().is_some_and(|target| copy.source == (Source::Managed { asset_id: target.base_asset.clone() })))
            })
            .map(|copy| copy.id)
            .collect();
        for id in copies {
            plan.ops.push(Op::Relink { id, file_name: photo.file_name.clone(), source: photo.source.clone(), format: Some(photo.format.clone()) });
            plan.ops.push(Op::SetContent {
                id,
                width: photo.width,
                height: photo.height,
                file_size: photo.file_size,
                content_hash: photo.content_hash.clone(),
                preview_only: photo.preview_only.clone(),
            });
        }
    } else {
        plan.ops.push(Op::AddPhoto { photo: Box::new(photo.clone()) });
    }
    plan.ops.push(Op::SetCloudIdentity { entity: format!("photo:{}", photo.id.0), id: Some(global_id) });
    plan.ops.push(Op::SetUploadIntent { id: manifest.id, intent: Some(Box::new(intent)) });
    catalog.apply(Op::Batch { ops: plan.ops.clone() })?;
    Ok(plan)
}

#[cfg(test)]
fn commit(s: &mut Session, manifests: Vec<Manifest>) -> Result<Value> {
    complete(
        s,
        Prepared {
            automatic: false,
            order: Vec::new(),
            skipped: Vec::new(),
            manifests,
            failed: Vec::new(),
            request: None,
            duplicate_policy: requests::DuplicatePolicy::Allow,
        },
    )
    .map(|done| done.value)
}

fn complete(s: &mut Session, mut prepared: Prepared) -> Result<Completed> {
    // A different worker may already have committed this candidate. Preparation
    // failures concern uncommitted records; upload integrity has its own status.
    let already_complete: Vec<_> =
        prepared.failed.iter().filter(|issue| s.catalog.upload_intents().contains_key(&issue.id)).map(|issue| issue.id.clone()).collect();
    prepared.failed.retain(|issue| !s.catalog.upload_intents().contains_key(&issue.id));
    let mut skipped = prepared.skipped;
    let mut ids: Vec<u64> = skipped.iter().filter(|record| record.target_matches(&s.catalog)).map(|record| record.photo_id.0).collect();
    ids.extend(already_complete.iter().filter_map(|id| s.catalog.upload_intents().get(id).map(|intent| intent.photo_id.0)));
    let mut resolved: std::collections::BTreeMap<String, u64> =
        skipped.iter().filter(|record| record.target_matches(&s.catalog)).map(|record| (record.manifest.id.clone(), record.photo_id.0)).collect();
    for id in &already_complete {
        if let Some(intent) = s.catalog.upload_intents().get(id) {
            resolved.insert(id.clone(), intent.photo_id.0);
        }
    }
    let mut completed = already_complete;
    completed.extend(skipped.iter().map(|record| record.manifest.id.clone()));
    let adopting = prepared.manifests.iter().any(|m| m.adoption.is_some());
    let selection = s.selection.clone();
    if prepared.manifests.is_empty() {
        let ids = ordered_ids(&prepared.order, &resolved, ids);
        return Ok(completed_result(s, ids, Vec::new(), prepared.failed, prepared.request, skipped, completed));
    }
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("managed storage is unavailable"))?;
    let root = resolver.durable_root().ok_or_else(|| error("working storage is unavailable"))?;
    let mut staged = s.catalog.clone();
    let mut hashes = std::collections::BTreeMap::new();
    for photo in staged.photos().filter(|photo| !photo.deleted) {
        if let Source::Managed { asset_id } = &photo.source
            && let Some(asset) = staged.asset(asset_id)
        {
            hashes.entry((asset.original.remote.connection.clone(), asset.original.id.clone())).or_insert(photo.id);
        }
    }
    let mut ops = Vec::new();
    let mut copies = Vec::new();
    let mut stacks = Vec::new();
    for manifest in prepared.manifests {
        let id = manifest.id.clone();
        let name = manifest.file_name.clone();
        let connection = manifest.asset.original.remote.connection.clone();
        let hash = manifest.asset.original.id.clone();
        let duplicate = if manifest.duplicate_policy.unwrap_or(prepared.duplicate_policy) == requests::DuplicatePolicy::Skip
            && manifest.target.is_none()
            && manifest.stack_with.is_none()
        {
            hashes.get(&(connection.clone(), hash.clone())).copied()
        } else {
            None
        };
        match plan_candidate(&mut staged, Some(&root), manifest, duplicate) {
            Ok(plan) => {
                if let Some(photo) = plan.ids.first() {
                    resolved.insert(id.clone(), *photo);
                    hashes.entry((connection, hash)).or_insert(PhotoId(*photo));
                }
                if let Some(record) = plan.skipped {
                    skipped.push(record);
                }
                ids.extend(plan.ids);
                ops.extend(plan.ops);
                copies.extend(plan.copies);
                stacks.extend(plan.stacks);
                completed.push(id);
            }
            Err(e) => prepared.failed.push(Issue {
                id: id.clone(),
                name,
                reason: e.to_string(),
                command: "cloud.recoverImports".into(),
                retry: json!({"connection":connection,"imports":[id],"wait":false}),
            }),
        }
    }
    for (_, _, binding) in &stacks {
        ops.push(binding.clone());
    }
    if stacks.iter().any(|(_, parent, _)| parent.is_some()) {
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
        if adopting && prepared.automatic {
            s.apply_system(Op::Batch { ops })?;
        } else {
            s.commit(if adopting { "Move photos to cloud storage" } else { "Import cloud photos" }, Op::Batch { ops })?;
        }
        if adopting
            && !prepared.automatic
            && let Some(entry) = s.undo.last_mut()
        {
            // Publication may already reference these durable resources. Undo
            // withdraws the photos; it must not strand the frozen outbox.
            adopt::retain_resources(&mut entry.op);
        }
        s.cloud_uploads.request_resume(false);
    }
    let ids = if adopting {
        let mut ids = ids;
        ids.sort();
        ids.dedup();
        ids
    } else {
        ordered_ids(&prepared.order, &resolved, ids)
    };
    let done = completed_result(s, ids, copies, prepared.failed, prepared.request, skipped, completed);
    if adopting {
        s.selection = selection;
    }
    Ok(done)
}
fn ordered_ids(order: &[String], resolved: &std::collections::BTreeMap<String, u64>, ids: Vec<u64>) -> Vec<u64> {
    if order.is_empty() { ids } else { order.iter().filter_map(|id| resolved.get(id).copied()).collect() }
}

fn completed_result(
    s: &mut Session,
    ids: Vec<u64>,
    copies: Vec<Value>,
    failed: Vec<Issue>,
    request: Option<String>,
    skipped: Vec<settled::Skipped>,
    completed: Vec<String>,
) -> Completed {
    if let Some(id) = ids.first().filter(|id| s.catalog.photo(PhotoId(**id)).is_some()) {
        s.selection = Selection::single(PhotoId(*id));
    }
    let selected: std::collections::BTreeSet<_> = ids.iter().copied().collect();
    let pending = s
        .catalog
        .upload_intents()
        .values()
        .any(|intent| selected.contains(&intent.photo_id.0) && !matches!(intent.state, UploadState::Synchronized | UploadState::Superseded));
    let first = copies.first().cloned().unwrap_or(Value::Null);
    let summaries: Vec<_> = skipped
        .iter()
        .map(|record| {
            let available = record.target_matches(&s.catalog);
            json!({"id":record.manifest.id,"name":record.manifest.file_name,
                "reason":if available {"duplicate original"} else {"previously skipped target is unavailable or changed; start a new request for another photo"},
                "photoId":available.then_some(record.photo_id.0),"previousPhotoId":record.photo_id.0})
        })
        .collect();
    Completed {
        value: json!({"ids":ids,"savedLocally":!ids.is_empty(),"uploadPending":pending,"copies":copies,
        "path":first["path"],"id":first["id"],"original":first["original"],"requestId":request,"failed":failed,"skipped":summaries,"completedImports":completed}),
        skipped,
    }
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

#[derive(Clone, Copy)]
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
            job.kind = match kind {
                RevisionKind::Dng => PreparationKind::Dng,
                RevisionKind::Xmp => PreparationKind::Xmp,
                RevisionKind::Returned => PreparationKind::Returned,
            };
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
        let source = s.catalog.photo(*id).ok_or_else(|| error("external edit source disappeared"))?;
        let mut rendered = (**source).clone();
        // Develop edits are already baked into the exported pixels. Fresh owned
        // XMP must describe the rendered derivative, while foreign data survives.
        rendered.develop = Arc::new(Default::default());
        let packet = crate::sidecar::sidecar_packet(&rendered, &s.catalog);
        let asset = s.catalog.asset(&base_asset).ok_or_else(|| error("external edit asset disappeared"))?.clone();
        if let Some(job) = &mut job {
            job.kind = PreparationKind::ExternalCopy;
            job.edits.push(std::sync::Mutex::new(Some(EditPlan { export, parent: *id, base_asset, asset, packet })));
            job.recover = false;
        }
    }
    launch_job(s, params, job.ok_or_else(|| error("no managed photos selected"))?)
}

struct Pending {
    kind: PreparationKind,
    photos: usize,
    receiver: mpsc::Receiver<Result<Prepared>>,
    applied: bool,
    acknowledge: mpsc::SyncSender<Result<Completed>>,
    finalized: mpsc::Receiver<Result<Value>>,
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
    automatic: automatic::Queue,
    pending: Vec<Pending>,
    coordinator: Arc<std::sync::Mutex<()>>,
    pub reports: Vec<Value>,
    pub failures: Vec<Value>,
}
impl Imports {
    pub(crate) fn reload(&mut self, resolver: Option<&AssetResolver>, catalog: &lightcraft_catalog::Catalog) {
        self.failures = resolver.and_then(|r| r.durable_root()).map_or_else(Vec::new, |root| match issues::load(&root) {
            Ok(issues) => issues
                .into_values()
                .filter(|issue| !catalog.upload_intents().contains_key(&issue.id))
                .filter_map(|issue| serde_json::to_value(issue).ok())
                .collect(),
            Err(e) => vec![json!({"id":"recovery-status","name":"Import recovery status","reason":e.to_string()})],
        });
    }
    pub fn preparing(&self) -> usize {
        self.pending.iter().filter(|pending| !pending.done.load(Ordering::Relaxed)).count()
    }
    pub fn preparing_photos(&self) -> usize {
        self.pending.iter().filter(|pending| !pending.applied).map(|pending| pending.photos).sum()
    }
    pub(crate) fn running(&self) -> bool {
        !self.pending.is_empty()
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

fn preparation_guard<'a>(coordinator: &'a std::sync::Mutex<()>, cancel: &AtomicBool) -> Result<std::sync::MutexGuard<'a, ()>> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("import cancelled; retained requests remain recoverable"));
        }
        match coordinator.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(e)) => return Ok(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

fn launch_job(s: &mut Session, p: &Value, mut job: Job) -> Result<Value> {
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
    if wait && !s.cloud_imports.pending.is_empty() {
        return Err(error("wait for background imports to finish before a blocking import"));
    }
    let coordinator = s.cloud_imports.coordinator.clone();
    let kind = job.kind;
    let task = s.activity.start("import", kind.label(), crate::activity::Cancel::Yes);
    let photos = job.paths.len();
    task.progress(0, photos as u64);
    let cancel = task.cancel_flag().ok_or_else(|| error("could not create import cancellation flag"))?;
    let id = task.id();
    let progress = task.handle();
    if wait {
        let _coordinator = preparation_guard(&coordinator, &cancel)?;
        let result =
            crate::assets::transfers::with_progress(progress.clone(), || job.run(&cancel, &progress)).and_then(|prepared| complete(s, prepared));
        let result = job.persist_completion(result);
        s.cloud_imports.reload(s.media.asset_resolver.as_deref(), &s.catalog);
        return result;
    }
    let request_id = job.request_options.as_ref().and_then(|options| options.requested.clone());
    let (sender, receiver) = mpsc::sync_channel(1);
    let (acknowledge, accepted) = mpsc::sync_channel(1);
    let (finished, finalized) = mpsc::sync_channel(1);
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
            let _coordinator = match preparation_guard(&coordinator, &flag) {
                Ok(guard) => guard,
                Err(e) => {
                    let _ = sender.send(Err(e));
                    return;
                }
            };
            let result = crate::assets::transfers::with_progress(progress.clone(), || job.run(&flag, &progress));
            if sender.send(result).is_ok() {
                loop {
                    match accepted.recv_timeout(std::time::Duration::from_millis(20)) {
                        Ok(result) => {
                            let _ = finished.send(job.persist_completion(result));
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) if flag.load(Ordering::Relaxed) => break,
                        Err(_) => {}
                    }
                }
            }
            job.resolver.assets_changed();
        })
        .map_err(io)?;
    s.cloud_imports.pending.push(Pending {
        kind,
        photos,
        receiver,
        applied: false,
        acknowledge,
        finalized,
        library: s.library_token(),
        guard: task,
        cancel,
        done,
    });
    Ok(json!({"background": true, "task": id,"requestId":request_id}))
}

impl Session {
    pub fn poll_cloud_imports(&mut self) {
        let mut imports = std::mem::take(&mut self.cloud_imports);
        let mut changed = false;
        imports.pending.retain_mut(|pending| {
            if !pending.applied {
                let result = match pending.receiver.try_recv() {
                    Ok(result) => result,
                    Err(mpsc::TryRecvError::Empty) => return true,
                    Err(mpsc::TryRecvError::Disconnected) => Err(error("import worker stopped; staged originals remain recoverable")),
                };
                let result = if !Arc::ptr_eq(&pending.library, &self.library_token()) || pending.cancel.load(Ordering::Relaxed) {
                    Err(error("library changed or import cancelled; staged originals remain recoverable"))
                } else {
                    result.and_then(|prepared| {
                        let mut skipped = Vec::new();
                        self.execute_fn(pending.kind.command(), |s| {
                            let completed = complete(s, prepared)?;
                            skipped = completed.skipped;
                            Ok(completed.value)
                        })
                        .map(|value| Completed { value, skipped })
                    })
                };
                pending.applied = true;
                // The worker owns request/status IO and waits without the staging
                // lock while the library owner performs the single bulk commit.
                if pending.acknowledge.send(result).is_ok() {
                    return true;
                }
            }
            let result = match pending.finalized.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) if !pending.done.load(Ordering::Relaxed) => return true,
                Err(_) => Err(error("import finalization stopped; retained requests can be retried safely")),
            };
            changed = true;
            imports.reports.push(match result {
                Ok(mut value) => {
                    if let Some(failed) = value["failed"].as_array_mut() {
                        failed.retain(|issue| issue["id"].as_str().is_none_or(|id| !self.catalog.upload_intents().contains_key(id)));
                    }
                    json!({"task":pending.guard.id(),"ok":true,"result":value})
                }
                Err(e) => json!({"task":pending.guard.id(),"ok":false,"error":e.to_string()}),
            });
            false
        });
        if imports.reports.len() > 32 {
            imports.reports.drain(..imports.reports.len() - 32);
        }
        if changed {
            imports.reload(self.media.asset_resolver.as_deref(), &self.catalog);
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

    fn supporting_packet(catalog: &lightcraft_catalog::Catalog) -> String {
        let mut photo = Photo::new(PhotoId(1), Source::File { path: "source.raw".into() }, "source.raw", "RAW", 32, 24, "");
        photo.rating = 4;
        photo.flag = lightcraft_catalog::Flag::Pick;
        photo.label = Some(lightcraft_catalog::ColorLabel::Red);
        photo.meta.title = "Harbour".into();
        photo.meta.caption = "Supporting packet".into();
        photo.meta.keywords = vec!["boats".into()];
        photo.captured = Some("2020-02-03T04:05:06".into());
        Arc::make_mut(&mut photo.develop).light.exposure = 0.75;
        let foreign = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:foreign="urn:lightcraft:test:foreign" xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/" foreign:note="keep-exact-value"><xmpMM:History><rdf:Seq><rdf:li rdf:parseType="Resource"><foreign:event>before-import</foreign:event></rdf:li></rdf:Seq></xmpMM:History></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        crate::sidecar::merge_managed_packet(foreign.as_bytes(), &crate::sidecar::sidecar_packet(&photo, catalog)).unwrap()
    }

    fn packet_for(session: &Session, id: PhotoId) -> Vec<u8> {
        let Source::Managed { asset_id } = &session.catalog.photo(id).unwrap().source else { panic!("managed") };
        let blob = session.catalog.asset(asset_id).unwrap().associated.get("xmp").unwrap();
        let root = session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap();
        let bytes = std::fs::read(root.join("blobs").join(&blob.id.0)).unwrap();
        assert_eq!(sha256(&bytes), blob.id);
        bytes
    }

    fn assert_foreign(packet: &[u8]) {
        let text = std::str::from_utf8(packet).unwrap();
        assert!(text.contains("keep-exact-value"), "{text}");
        assert!(text.contains("before-import"), "{text}");
        assert!(text.contains("xmpMM:History"), "{text}");
    }

    #[test]
    fn managed_raw_import_matches_ordinary_sidecar_application_and_preserves_exact_packet() {
        for naming in [crate::sidecar::SidecarNaming::Stem, crate::sidecar::SidecarNaming::Full] {
            let fixture = Fixture::new();
            let raw = fixture.root.join("source.raw");
            std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
            let mut managed = fixture.session();
            managed.xmp.naming = naming;
            managed.import_defaults.creator = "Import default".into();
            managed.commit("Label names", Op::SetLabelName { label: lightcraft_catalog::ColorLabel::Red, name: Some("Client".into()) }).unwrap();
            let packet = supporting_packet(&managed.catalog);
            let preferred = crate::sidecar::sidecar_path(raw.to_str().unwrap(), naming).with_extension("XMP");
            std::fs::write(&preferred, &packet).unwrap();
            let other =
                if naming == crate::sidecar::SidecarNaming::Stem { crate::sidecar::SidecarNaming::Full } else { crate::sidecar::SidecarNaming::Stem };
            std::fs::write(
                crate::sidecar::sidecar_path(raw.to_str().unwrap(), other),
                crate::sidecar::sidecar_packet(
                    &Photo::new(PhotoId(1), Source::File { path: "other".into() }, "other", "RAW", 32, 24, ""),
                    &managed.catalog,
                ),
            )
            .unwrap();
            let mut ordinary = Session::new().with_fs();
            ordinary.xmp.naming = naming;
            ordinary.import_defaults = managed.import_defaults.clone();
            ordinary.commit("Label names", Op::SetLabelName { label: lightcraft_catalog::ColorLabel::Red, name: Some("Client".into()) }).unwrap();
            ordinary.execute("library.import", &json!({"paths":[raw]})).unwrap();
            let id = PhotoId(managed.execute("cloud.import", &json!({"connection":"personal","paths":[raw]})).unwrap()["ids"][0].as_u64().unwrap());
            let a = ordinary.catalog.photos().next().unwrap();
            let b = managed.catalog.photo(id).unwrap();
            assert_eq!((b.rating, b.flag, b.label), (4, lightcraft_catalog::Flag::Pick, Some(lightcraft_catalog::ColorLabel::Red)));
            assert_eq!((a.rating, a.flag, a.label), (b.rating, b.flag, b.label));
            assert_eq!(a.meta, b.meta);
            assert_eq!(a.captured, b.captured);
            assert_eq!(a.develop, b.develop);
            assert_eq!(b.develop.light.exposure, 0.75);
            assert_eq!(packet_for(&managed, id), packet.as_bytes());
            std::fs::remove_file(&preferred).unwrap();
            std::fs::remove_file(&raw).unwrap();
            managed.close_library().unwrap();
            drop(managed);
            assert_eq!(packet_for(&fixture.session(), id), packet.as_bytes());
        }
    }

    #[test]
    fn managed_raw_embedded_xmp_is_retained_when_no_sidecar_exists() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let packet = supporting_packet(&session.catalog);
        let raw = fixture.root.join("embedded.dng");
        std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(Some(&packet), Default::default())).unwrap();
        let id = PhotoId(session.execute("cloud.import", &json!({"connection":"personal","paths":[raw]})).unwrap()["ids"][0].as_u64().unwrap());
        assert_eq!(session.catalog.photo(id).unwrap().rating, 4);
        assert_eq!(session.catalog.photo(id).unwrap().develop.light.exposure, 0.75);
        assert_eq!(packet_for(&session, id), packet.as_bytes());
    }

    #[test]
    fn local_only_background_media_revisions_commit_without_enrolling_cloud_imports() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let raw = fixture.root.join("source.raw");
        std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        let id = PhotoId(session.execute("cloud.import", &json!({"connection":"personal","paths":[raw]})).unwrap()["ids"][0].as_u64().unwrap());
        let source = session.catalog.photo(id).unwrap().source.clone();
        session.commit("Raw source", Op::Relink { id, source, file_name: "source.raw".into(), format: Some("RAW".into()) }).unwrap();
        session.execute("cloud.syncEnable", &json!({"connection":"personal"})).unwrap();
        let resolver = session.media.asset_resolver.clone().unwrap();
        let destination = fixture.root.join("library/Local Originals");
        let blobs = crate::cloud_local_transfer::plan(&session.catalog).unwrap();
        crate::cloud_local_transfer::materialize(resolver, &destination, &blobs, &AtomicBool::new(false), |_| {}).unwrap();
        session.close_library().unwrap();
        drop(session);
        std::fs::write(fixture.root.join("library/cloud-mode.json"), br#"{"version":1,"localOnly":true,"transition":null}"#).unwrap();
        let mut session = Session::new().with_fs();
        session.open_library(fixture.root.join("library"), false).unwrap();
        assert!(session.cloud_transition.local_only());
        assert!(session.execute("cloud.import", &fixture.params()).is_err(), "explicit enrollment stays prohibited");

        fn background(session: &mut Session, command: &str, id: PhotoId) -> Value {
            session.execute(command, &json!({"ids":[id.0],"wait":false})).unwrap();
            assert_eq!(session.cloud_imports.pending.len(), 1, "{command} must exercise asynchronous completion");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !session.cloud_imports.pending.is_empty() {
                session.poll_cloud_imports();
                assert!(std::time::Instant::now() < deadline, "{command}: preparation timed out");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let report = session.cloud_imports.reports.last().unwrap();
            assert_eq!(report["ok"], true, "{command}: {report}");
            assert_eq!(report["result"]["failed"], json!([]), "{command}: {report}");
            let result = report["result"].clone();
            session.resume_cloud_uploads();
            session.resume_cloud_replication();
            assert_eq!(session.cloud_uploads.running(), 0);
            assert!(!session.cloud_replication_jobs.running());
            result
        }
        session.execute("photo.rate", &json!({"ids":[id.0],"rating":4})).unwrap();
        background(&mut session, "photo.saveMetadataToFile", id);
        assert_eq!(lightcraft_meta::parse_xmp(std::str::from_utf8(&packet_for(&session, id)).unwrap()).unwrap().metadata.rating, Some(4));
        background(&mut session, "photo.convertToDng", id);
        assert_eq!(session.catalog.photo(id).unwrap().format, "DNG");
        let edit = background(&mut session, "photo.editExternal", id);
        let derived = PhotoId(edit["id"].as_u64().unwrap());
        std::fs::copy(&fixture.original, edit["path"].as_str().unwrap()).unwrap();
        background(&mut session, "photo.reload", derived);
        assert_eq!((session.catalog.photo(derived).unwrap().width, session.catalog.photo(derived).unwrap().height), (32, 24));
        session.close_library().unwrap();
        drop(session);
        let mut reopened = Session::new().with_fs();
        reopened.open_library(fixture.root.join("library"), false).unwrap();
        assert!(reopened.cloud_transition.local_only());
        assert_eq!(reopened.catalog.photo(id).unwrap().rating, 4);
        assert!(reopened.catalog.photo(derived).is_some());
        crate::cloud_transition::enable(&mut reopened).unwrap();
        let root = reopened.media.asset_resolver.as_ref().unwrap().durable_root().unwrap();
        let superseded: Vec<_> = reopened.catalog.upload_intents().iter().filter(|(_, intent)| intent.state == UploadState::Superseded).collect();
        assert!(!superseded.is_empty(), "intermediate disconnected revisions are retained without endless publication waiting");
        for (id, intent) in superseded {
            let manifest = read_manifest(&root, id).unwrap();
            assert_eq!(manifest.asset_id, intent.asset_id);
            assert_eq!(std::fs::read(root.join("blobs").join(manifest.asset.original.id.0)).unwrap().len() as u64, manifest.asset.original.size);
        }
        assert_eq!(reopened.catalog.sync_overview().failed_uploads, 0);
        assert!(reopened.catalog.local_sync_baseline().is_none());
    }
    #[test]
    fn supporting_xmp_survives_dng_xmp_write_external_render_and_returned_original_revisions() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let raw = fixture.root.join("source.raw");
        let packet = supporting_packet(&session.catalog);
        std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        std::fs::write(raw.with_extension("xmp"), &packet).unwrap();
        let id = PhotoId(session.execute("cloud.import", &json!({"connection":"personal","paths":[raw]})).unwrap()["ids"][0].as_u64().unwrap());
        let original_packet = packet_for(&session, id);
        let source = session.catalog.photo(id).unwrap().source.clone();
        session.commit("Synthetic raw", Op::Relink { id, source, file_name: "source.raw".into(), format: Some("RAW".into()) }).unwrap();
        std::fs::remove_file(raw.with_extension("xmp")).unwrap();
        std::fs::remove_file(raw).unwrap();
        session.execute("photo.convertToDng", &json!({"ids":[id.0]})).unwrap();
        let converted = packet_for(&session, id);
        assert_foreign(&converted);
        let Source::Managed { asset_id } = &session.catalog.photo(id).unwrap().source else { panic!("managed") };
        let blob = &session.catalog.asset(asset_id).unwrap().original;
        let root = session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap();
        let dng = std::fs::read(root.join("blobs").join(&blob.id.0)).unwrap();
        assert_foreign(lightcraft_meta::embedded(&dng).xmp.unwrap().as_bytes());
        session.execute("photo.rate", &json!({"id":id.0,"rating":2})).unwrap();
        session.execute("photo.saveMetadataToFile", &json!({"ids":[id.0]})).unwrap();
        let saved = packet_for(&session, id);
        assert_foreign(&saved);
        assert_eq!(lightcraft_meta::parse_xmp(std::str::from_utf8(&saved).unwrap()).unwrap().metadata.rating, Some(2));
        let edit = session.execute("photo.editExternal", &json!({"ids":[id.0]})).unwrap();
        let derived = PhotoId(edit["id"].as_u64().unwrap());
        let working = PathBuf::from(edit["path"].as_str().unwrap());
        assert_eq!(session.catalog.photo(derived).unwrap().develop.light.exposure, 0.0, "rendered pixels already contain source exposure");
        let derived_packet = packet_for(&session, derived);
        assert_foreign(&derived_packet);
        session.execute("photo.readMetadataFromFile", &json!({"ids":[derived.0]})).unwrap();
        assert_eq!(session.catalog.photo(derived).unwrap().develop.light.exposure, 0.0);
        let returned = lightcraft_codecs::encode_png(
            &lightcraft_codecs::EncodeImage::rgba8(&lightcraft_raster::Rgba8::from_fn(11, 9, |_, _| [60, 100, 180, 255])),
            &Default::default(),
        )
        .unwrap();
        std::fs::write(&working, returned).unwrap();
        session.execute("photo.reload", &json!({"ids":[derived.0]})).unwrap();
        let returned_packet = packet_for(&session, derived);
        assert_foreign(&returned_packet);
        assert_eq!(session.catalog.photo(derived).unwrap().develop.light.exposure, 0.0);
        // Earlier immutable supporting bytes remain available to old revisions.
        let root = session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap();
        assert_eq!(std::fs::read(root.join("blobs").join(sha256(&original_packet).0)).unwrap(), original_packet);
        session.close_library().unwrap();
        drop(session);
        let mut reopened = fixture.session();
        assert_eq!(packet_for(&reopened, derived), returned_packet);
        reopened.execute("photo.readMetadataFromFile", &json!({"ids":[derived.0]})).unwrap();
        assert_eq!(reopened.catalog.photo(derived).unwrap().develop.light.exposure, 0.0);
    }

    #[test]
    fn malformed_supporting_xmp_reports_an_error_without_losing_source_files() {
        let fixture = Fixture::new();
        let sidecar = fixture.original.with_extension("xmp");
        std::fs::write(&sidecar, b"\xffinvalid packet").unwrap();
        let original = std::fs::read(&fixture.original).unwrap();
        let mut session = fixture.session();
        let result = session.execute("cloud.import", &fixture.params()).unwrap();
        assert!(result["failed"][0]["reason"].as_str().unwrap().contains("supporting XMP"));
        assert_eq!(std::fs::read(&fixture.original).unwrap(), original);
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"\xffinvalid packet");
        assert_eq!(session.catalog.photos().count(), 0);
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
            let mut job = Job::new(&mut session, &fixture.params(), false).unwrap();
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
        let asset = session.catalog.asset(&asset_id).unwrap().clone();
        let mut rendered = (**session.catalog.photo(id).unwrap()).clone();
        rendered.develop = Arc::new(Default::default());
        let packet = crate::sidecar::sidecar_packet(&rendered, &session.catalog);
        job.edits.push(std::sync::Mutex::new(Some(EditPlan { export, parent: id, base_asset: asset_id, asset, packet })));
        let task = session.activity.start("import", "External test", crate::activity::Cancel::Yes);
        let manifests = job.prepare(&AtomicBool::new(false), &task.handle()).unwrap();
        let working = job.root.join(manifests[0].working.as_ref().unwrap());
        let before = std::fs::read(&working).unwrap();
        session.execute("photo.delete", &json!({"ids":[id.0]})).unwrap();
        assert!(commit(&mut session, manifests).unwrap()["failed"][0]["reason"].as_str().unwrap().contains("source changed or was deleted"));
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
        let mut job = Job::new(&mut session, &fixture.params(), false).unwrap();
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
        let mut job = Job::new(&mut session, &fixture.params(), false).unwrap();
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
    fn original_at(fixture: &Fixture, name: &str, color: u8) -> PathBuf {
        let path = fixture.root.join(name);
        let image = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [color, x as u8, y as u8, 255]);
        std::fs::write(&path, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap()).unwrap();
        path
    }

    #[test]
    fn interrupted_bulk_request_reuses_frozen_candidates_after_restart_and_recovery() {
        let fixture = Fixture::new();
        let b = original_at(&fixture, "B.png", 90);
        let c = fixture.root.join("C.png");
        let params = json!({"connection":"personal","paths":[fixture.original,b,c]});
        let mut session = fixture.session();
        session.import_defaults.creator = "Frozen recipe".into();
        let mut job = Job::new(&mut session, &params, false).unwrap();
        job.initialize_request(&AtomicBool::new(false)).unwrap();
        let request = job.request.as_ref().unwrap().id.clone();
        let task = session.activity.start("import", "Interrupted request", crate::activity::Cancel::Yes);
        let abandoned = job.run(&AtomicBool::new(false), &task.handle()).unwrap();
        assert_eq!(abandoned.manifests.len(), 2);
        assert_eq!(abandoned.failed.len(), 1);
        let identities: Vec<_> = abandoned.manifests.iter().map(|m| m.id.clone()).collect();
        assert!(session.catalog.upload_intents().is_empty());
        drop(abandoned);
        drop(task);
        drop(job);
        std::fs::remove_file(&fixture.original).unwrap();
        std::fs::remove_file(&b).unwrap();
        original_at(&fixture, "C.png", 150);
        session.close_library().unwrap();
        drop(session);
        let mut session = fixture.session();
        assert_eq!(session.cloud_imports.failures.len(), 1, "preparation failures survive the abandoned reply");
        session.import_defaults.creator = "Changed after interruption".into();
        let result = session.execute("cloud.import", &params).unwrap();
        assert_eq!(result["requestId"], request);
        assert_eq!(result["failed"], json!([]));
        assert_eq!(session.catalog.photos().count(), 3);
        assert_eq!(session.catalog.upload_intents().len(), 3);
        for id in identities {
            assert!(session.catalog.upload_intents().contains_key(&id));
        }
        assert!(session.catalog.photos().all(|photo| photo.meta.creator == "Frozen recipe"));
        assert!(session.cloud_imports.failures.is_empty());
        let repeat = session.execute("cloud.import", &json!({"connection":"personal","requestId":request})).unwrap();
        assert_eq!(repeat["ids"], result["ids"]);
        session.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
        assert_eq!(session.catalog.photos().count(), 3);
        assert_eq!(std::fs::read_dir(fixture.root.join("library/Managed/imports")).unwrap().count(), 3);
    }

    #[test]
    fn corrupt_manifest_does_not_block_healthy_recovery_and_stays_visible_until_repaired() {
        let fixture = Fixture::new();
        let b = original_at(&fixture, "B.png", 90);
        let mut session = fixture.session();
        let mut job = Job::new(&mut session, &json!({"connection":"personal","paths":[fixture.original,b]}), false).unwrap();
        let task = session.activity.start("import", "Prepare recovery", crate::activity::Cancel::Yes);
        let prepared = job.prepare(&AtomicBool::new(false), &task.handle()).unwrap();
        let path = job.root.join("imports").join(format!("{}.json", prepared[0].id));
        let saved = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"damaged manifest").unwrap();
        let root = job.root.clone();
        drop(task);
        drop(job);
        let result = session.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
        assert_eq!(result["ids"].as_array().unwrap().len(), 1);
        assert_eq!(result["failed"].as_array().unwrap().len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged manifest");
        assert!(visit_staging(&root, |_| Ok(())).is_err(), "cache staging protection remains strict");
        session.close_library().unwrap();
        drop(session);
        let mut session = fixture.session();
        assert_eq!(session.execute("cloud.importStatus", &json!({})).unwrap()["failed"].as_array().unwrap().len(), 1);
        let overview = crate::cloud_status::overview(&mut session, &Value::Null).unwrap();
        assert!(overview["error"].is_string());
        assert_eq!(overview["health"]["imports"]["failed"][0]["command"], "cloud.recoverImports");
        std::fs::write(&path, saved).unwrap();
        let result = session.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
        assert_eq!(result["failed"], json!([]));
        assert_eq!(session.catalog.photos().count(), 2);
        assert!(session.cloud_imports.failures.is_empty());
    }

    #[test]
    fn duplicate_policy_preserves_retry_identity_and_allows_explicit_new_photo_records() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let original = session.execute("cloud.import", &fixture.params()).unwrap();
        assert_eq!(session.execute("cloud.import", &fixture.params()).unwrap()["ids"], original["ids"]);
        let copy = fixture.root.join("same-bytes.png");
        std::fs::copy(&fixture.original, &copy).unwrap();
        let skipped = session.execute("cloud.import", &json!({"connection":"personal","paths":[copy]})).unwrap();
        assert_eq!(skipped["ids"], original["ids"]);
        assert_eq!(skipped["skipped"].as_array().unwrap().len(), 1);
        session.execute("cloud.recoverImports", &json!({"connection":"personal"})).unwrap();
        assert_eq!(session.catalog.photos().count(), 1, "recovery retains the saved skip policy");
        let params = json!({"connection":"personal","paths":[fixture.original],"newRequest":true,"duplicatePolicy":"allow"});
        let duplicate = session.execute("cloud.import", &params).unwrap();
        assert_ne!(duplicate["requestId"], original["requestId"]);
        assert_ne!(duplicate["ids"], original["ids"]);
        assert_eq!(session.catalog.photos().count(), 2);
        let resume = json!({"connection":"personal","requestId":duplicate["requestId"]});
        assert_eq!(session.execute("cloud.import", &resume).unwrap()["ids"], duplicate["ids"]);
        assert_eq!(session.catalog.photos().count(), 2);
        assert!(
            session.execute("cloud.import", &json!({"connection":"personal","requestId":duplicate["requestId"],"duplicatePolicy":"skip"})).is_err()
        );
    }

    #[test]
    fn request_pointer_survives_interruption_before_request_file_and_candidate_ids_are_stable() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let mut job = Job::new(&mut session, &fixture.params(), false).unwrap();
        job.initialize_request(&AtomicBool::new(false)).unwrap();
        let request = job.request.as_ref().unwrap();
        let group = request.id.split('-').next().unwrap();
        let record = job.root.join("requests").join(group).join(format!("{}.json", request.id));
        std::fs::remove_file(&record).unwrap();
        let mut retry = Job::new(&mut session, &fixture.params(), false).unwrap();
        retry.initialize_request(&AtomicBool::new(false)).unwrap();
        assert_eq!(request.id, retry.request.as_ref().unwrap().id);
        assert_eq!(request.candidates[0].id, retry.request.as_ref().unwrap().candidates[0].id);
        assert!(record.is_file());
    }
    #[test]
    fn stale_revision_is_reported_without_blocking_an_independent_candidate() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let initial = session.execute("cloud.import", &fixture.params()).unwrap();
        let id = PhotoId(initial["ids"][0].as_u64().unwrap());
        let before = session.catalog.photo(id).unwrap().source.clone();
        let b = original_at(&fixture, "B.png", 90);
        let mut job = Job::new(&mut session, &json!({"connection":"personal","paths":[b]}), false).unwrap();
        let task = session.activity.start("import", "Prepare independent candidate", crate::activity::Cancel::Yes);
        let mut candidates = job.prepare(&AtomicBool::new(false), &task.handle()).unwrap();
        let mut stale = candidates[0].clone();
        stale.id = new_identity().unwrap();
        stale.asset_id = AssetId(stale.id.clone());
        stale.photo.as_mut().unwrap().source = Source::Managed { asset_id: stale.asset_id.clone() };
        stale.target = Some(RevisionTarget {
            photo_id: id,
            base_asset: AssetId("f".repeat(64)),
            global_id: session.catalog.cloud_identity(&format!("photo:{}", id.0)).unwrap().into(),
        });
        candidates.insert(0, stale);
        let result = commit(&mut session, candidates).unwrap();
        assert_eq!(result["ids"].as_array().unwrap().len(), 1);
        assert_eq!(result["failed"].as_array().unwrap().len(), 1);
        assert!(result["failed"][0]["reason"].as_str().unwrap().contains("photo changed"));
        assert_eq!(session.catalog.photo(id).unwrap().source, before);
        assert_eq!(session.catalog.photos().count(), 2);
    }
    #[test]
    fn late_preparation_failure_cannot_undo_a_committed_import_or_hide_upload_integrity_errors() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.execute("cloud.import", &fixture.params()).unwrap();
        let (id, intent) = session.catalog.upload_intents().iter().next().map(|(id, intent)| (id.clone(), intent.clone())).unwrap();
        let resolver = session.media.asset_resolver.as_ref().unwrap().clone();
        let root = resolver.durable_root().unwrap();
        let late = Issue {
            id: id.clone(),
            name: "old attempt".into(),
            reason: "obsolete missing source".into(),
            command: "cloud.recoverImports".into(),
            retry: json!({"connection":"personal","imports":[id]}),
        };
        // Worker A's local write arrives after worker B has durably committed.
        {
            let _guard = resolver.durable_guard(&AtomicBool::new(false)).unwrap();
            issues::update(&root, std::slice::from_ref(&late), &[]).unwrap();
        }
        session.close_library().unwrap();
        drop(session);
        let mut session = fixture.session();
        assert_eq!(session.execute("cloud.importStatus", &json!({})).unwrap()["failed"], json!([]));
        let result = complete(
            &mut session,
            Prepared {
                automatic: false,
                order: Vec::new(),
                skipped: Vec::new(),
                manifests: Vec::new(),
                failed: vec![late],
                request: None,
                duplicate_policy: requests::DuplicatePolicy::Skip,
            },
        )
        .unwrap();
        assert_eq!(result.value["failed"], json!([]));
        let job = Job::new(&mut session, &json!({"connection":"personal"}), true).unwrap();
        job.persist_completion(Ok(result)).unwrap();
        assert!(issues::load(&root).unwrap().is_empty());
        let mut failed = intent;
        failed.state = UploadState::Failed;
        failed.error = Some("Later verification found missing remote bytes".into());
        session.apply_system(Op::SetUploadIntent { id, intent: Some(Box::new(failed)) }).unwrap();
        let overview = crate::cloud_status::overview(&mut session, &Value::Null).unwrap();
        assert!(overview["error"].as_str().unwrap().contains("missing remote bytes"));
    }
    #[test]
    fn overlapping_request_aliases_wait_for_owner_ack_and_commit_one_photo() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let mut job = Job::new(&mut session, &fixture.params(), false).unwrap();
        job.initialize_request(&AtomicBool::new(false)).unwrap();
        let request = job.request.as_ref().unwrap().id.clone();
        drop(job);
        session.execute("cloud.import", &json!({"connection":"personal","paths":[fixture.original],"wait":false})).unwrap();
        session.execute("cloud.import", &json!({"connection":"personal","requestId":request,"wait":false})).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (index, prepared) = loop {
            if let Some(result) = session
                .cloud_imports
                .pending
                .iter()
                .enumerate()
                .find_map(|(index, pending)| pending.receiver.try_recv().ok().map(|result| (index, result)))
            {
                break (result.0, result.1.unwrap());
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let other = if index == 0 { 1 } else { 0 };
        let held = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < held {
            assert!(
                matches!(session.cloud_imports.pending.get(other).unwrap().receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "another preparation cannot race the held owner acknowledgement"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            session
                .execute("cloud.import", &json!({"connection":"personal","requestId":request,"wait":true}))
                .unwrap_err()
                .to_string()
                .contains("background imports")
        );
        let pending = session.cloud_imports.pending.get_mut(index).unwrap();
        pending.applied = true;
        let acknowledge = pending.acknowledge.clone();
        assert!(acknowledge.send(complete(&mut session, prepared)).is_ok());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !session.cloud_imports.pending.is_empty() && std::time::Instant::now() < deadline {
            session.poll_cloud_imports();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(session.cloud_imports.pending.is_empty());
        assert_eq!(session.cloud_imports.reports.len(), 2);
        assert!(session.cloud_imports.reports.iter().all(|report| report["ok"] == true && report["result"]["ids"] == json!([1])));
        assert_eq!(session.catalog.photos().count(), 1);
        assert_eq!(session.catalog.upload_intents().len(), 1);
        assert!(session.cloud_imports.failures.is_empty());
    }
    #[test]
    fn a_settled_skip_never_selects_a_reused_photo_id_with_different_original_bytes() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.execute("cloud.import", &fixture.params()).unwrap();
        let skipped = session.execute("cloud.import", &json!({"connection":"personal","paths":[fixture.original],"newRequest":true})).unwrap();
        let other = original_at(&fixture, "other.png", 180);
        let added = session.execute("cloud.import", &json!({"connection":"personal","paths":[other]})).unwrap();
        let source = session.catalog.photo(PhotoId(added["ids"][0].as_u64().unwrap())).unwrap().source.clone();
        session.apply_system(Op::Relink { id: PhotoId(1), source, file_name: "restored-other.png".into(), format: None }).unwrap();
        let retry = session.execute("cloud.import", &json!({"connection":"personal","requestId":skipped["requestId"]})).unwrap();
        assert_eq!(retry["ids"], json!([]));
        assert!(retry["skipped"][0]["photoId"].is_null());
        assert!(retry["skipped"][0]["reason"].as_str().unwrap().contains("unavailable or changed"));
        assert_eq!(session.catalog.photos().count(), 2, "settled retry neither resurrects nor creates a new record");
    }
    #[test]
    fn mixed_new_and_settled_candidates_keep_request_order_on_retry() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        session.execute("cloud.import", &fixture.params()).unwrap();
        let different = original_at(&fixture, "different.png", 190);
        let params = json!({"connection":"personal","paths":[different,fixture.original],"newRequest":true});
        let first = session.execute("cloud.import", &params).unwrap();
        assert_eq!(first["ids"], json!([2, 1]));
        assert_eq!(first["skipped"].as_array().unwrap().len(), 1);
        let retry = session.execute("cloud.import", &json!({"connection":"personal","requestId":first["requestId"]})).unwrap();
        assert_eq!(retry["ids"], first["ids"]);
        assert_eq!(session.catalog.photos().count(), 2);
    }
    fn import_local(fixture: &Fixture, session: &mut Session) -> PhotoId {
        session.execute("library.import", &json!({"paths":[fixture.original]})).unwrap();
        session.catalog.photos().next().unwrap().id
    }

    fn pump_library_adoption(session: &mut Session, ready: impl Fn(&Session) -> bool) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !ready(session) || !session.cloud_imports.pending.is_empty() {
            assert!(std::time::Instant::now() < until, "{:?}", session.cloud_imports.failures);
            session.poll_cloud_imports();
            resume_library(session);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn automatic_enrollment_reports_twenty_photos_before_preparation_finishes() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        for n in 0..20 {
            let id = session.catalog.alloc_photo_id();
            let photo = Photo::new(id, Source::Demo { scene: 1 }, &format!("photo-{n}.JPG"), "JPEG", 60, 40, "2026-10-17T09:00:00");
            session.commit("Add photo", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.execute("cloud.syncEnable", &json!({"connection":"personal"})).unwrap();
        let coordinator = session.cloud_imports.coordinator.clone();
        let guard = coordinator.lock().unwrap();
        resume_library(&mut session);
        assert_eq!(session.cloud_imports.pending.len(), 1, "one bounded preparation worker");
        assert_eq!(session.cloud_imports.preparing_photos(), 20);
        let task = session.activity.list().into_iter().find(|task| task.kind == "import").unwrap();
        assert_eq!((task.done, task.total), (0, 20));
        assert_eq!(session.execute("cloud.overview", &json!({})).unwrap()["preparingPhotos"], 20);
        drop(guard);
        pump_library_adoption(&mut session, |s| s.catalog.upload_intents().len() == 20);
        assert_eq!(session.cloud_imports.preparing_photos(), 0);
    }
    #[test]
    fn cloud_mode_stages_generated_originals_preserving_edits_copies_and_restart() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = session.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::Demo { scene: 1 }, "generated.JPG", "JPEG", 60, 40, "2026-10-16T09:00:00");
        session.commit("Add generated photo", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        session.selection = Selection::single(id);
        session.execute("photo.rate", &json!({"ids":[id.0],"rating":4})).unwrap();
        session.execute("develop.set", &json!({"ids":[id.0],"control":"light.exposure","value":0.7})).unwrap();
        let edits = session.catalog.photo(id).unwrap().develop.clone();
        session.selection = Selection::single(id);
        let copy = PhotoId(session.execute("photo.virtualCopy", &json!({})).unwrap()["ids"][0].as_u64().unwrap());
        session.execute("cloud.syncEnable", &json!({"connection":"personal"})).unwrap();
        // The reported library is already enabled: reopening must discover its
        // previously skipped generated photos without another setup action.
        session.close_library().unwrap();
        drop(session);
        let mut session = fixture.session();
        let undo_steps = session.undo.len();
        pump_library_adoption(&mut session, |s| s.catalog.upload_intents().len() == 1);
        assert_eq!(session.undo.len(), undo_steps);
        assert_eq!(session.catalog.photos().count(), 2);
        for id in [id, copy] {
            let photo = session.catalog.photo(id).unwrap();
            assert!(matches!(photo.source, Source::Managed { .. }));
            assert_eq!(photo.format, "DNG");
            assert!(photo.content_hash.is_some());
            assert_eq!(photo.develop, edits);
        }
        let asset = session.catalog.assets().values().next().unwrap();
        let root = session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap();
        let bytes = std::fs::read(root.join("blobs").join(&asset.original.id.0)).unwrap();
        let decoded = lightcraft_raw::decode(&bytes).unwrap();
        let scene = lightcraft_scenes::demo_library().remove(0);
        let expected = crate::demo::scene_pixels(&scene, 60).unwrap();
        let gain = 2f32.powf(decoded.color.baseline_exposure as f32);
        let pixels = decoded.develop(lightcraft_raw::Method::Bilinear).unwrap();
        for (actual, expected) in pixels.data.iter().zip(&expected.data) {
            for (actual, expected) in actual.iter().zip(expected) {
                assert!((actual * gain - expected).abs() < expected.abs() * 1e-5 + 1e-6, "original preserves linear pixels without edits");
            }
        }
        assert!(session.source_info(id).raw, "generated original must retain scene-referred tone mapping");
        let (working, info) = crate::files::load_bytes(&bytes, 60).unwrap();
        assert!(info.raw);
        let request = lightcraft_pipeline::RenderRequest::fit(60, 40);
        let before = lightcraft_pipeline::render(&expected, &lightcraft_pipeline::SourceInfo { raw: true, ..Default::default() }, &edits, &request);
        let after = lightcraft_pipeline::render(&working, &info, &edits, &request);
        let error: u64 =
            before.image.data.iter().zip(&after.image.data).flat_map(|(a, b)| a.iter().zip(b)).map(|(a, b)| u64::from(a.abs_diff(*b))).sum();
        assert!(error < (60 * 40 * 4 * 2), "materialization changed appearance: total channel error {error}");
        session.close_library().unwrap();
        drop(session);
        let mut reopened = fixture.session();
        resume_library(&mut reopened);
        assert!(reopened.cloud_imports.pending.is_empty());
        assert_eq!(reopened.catalog.upload_intents().len(), 1);
        assert_eq!(reopened.catalog.photo(copy).unwrap().copy_of, Some(id));
    }

    #[test]
    fn generated_adoption_recovers_staged_original_and_rejects_changed_scene() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = session.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::Demo { scene: 1 }, "generated.JPG", "JPEG", 60, 40, "2026-10-16T09:00:00");
        session.commit("Add generated photo", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        session.execute("cloud.adopt", &json!({"connection":"personal","ids":[id.0],"wait":false})).unwrap();
        let prepared = session.cloud_imports.pending[0].receiver.recv_timeout(std::time::Duration::from_secs(20)).unwrap().unwrap();
        let import = prepared.manifests[0].id.clone();
        session.close_library().unwrap();
        drop(session);
        let mut session = fixture.session();
        session.commit("Change scene", Op::Relink { id, file_name: "changed.JPG".into(), source: Source::Demo { scene: 2 }, format: None }).unwrap();
        let failed = session.execute("cloud.recoverImports", &json!({"connection":"personal","imports":[import]})).unwrap();
        assert!(!failed["failed"].as_array().unwrap().is_empty());
        assert!(session.catalog.upload_intents().is_empty());
        session.undo_step().unwrap();
        let done = session.execute("cloud.recoverImports", &json!({"connection":"personal","imports":[import]})).unwrap();
        assert_eq!(done["failed"], json!([]), "{done}");
        assert_eq!(session.catalog.photos().count(), 1);
        assert!(matches!(session.catalog.photo(id).unwrap().source, Source::Managed { .. }));
        assert_eq!(session.catalog.upload_intents().len(), 1);
    }

    #[test]
    fn cloud_mode_stages_the_added_library_and_later_imports_across_reopen() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let first = import_local(&fixture, &mut session);
        session.execute("photo.rate", &json!({"ids":[first.0],"rating":4})).unwrap();
        session.execute("photo.virtualCopy", &json!({"ids":[first.0]})).unwrap();
        let browsed = original_at(&fixture, "browsed.png", 150);
        session.execute("library.import", &json!({"paths":[browsed]})).unwrap();
        let browsed_id = session.catalog.photos().find(|p| p.file_name == "browsed.png").unwrap().id;
        session.apply_system(Op::SetLocal { id: browsed_id, local: true }).unwrap();
        session.execute("cloud.syncEnable", &json!({"connection":"personal"})).unwrap();
        session.execute("cloud.pause", &json!({})).unwrap();
        resume_library(&mut session);
        assert!(session.cloud_imports.pending.is_empty());
        session.execute("cloud.resume", &json!({})).unwrap();
        let undo_steps = session.undo.len();
        pump_library_adoption(&mut session, |s| matches!(s.catalog.photo(first).unwrap().source, Source::Managed { .. }));
        assert_eq!(session.undo.len(), undo_steps, "background sync must not insert user undo steps");
        assert_eq!(session.catalog.photo(first).unwrap().rating, 4);
        assert_eq!(session.catalog.upload_intents().len(), 1, "virtual copies share one original");
        assert!(session.catalog.photos().filter(|p| p.id == first || p.copy_of == Some(first)).all(|p| matches!(p.source, Source::Managed { .. })));
        assert!(matches!(session.catalog.photo(browsed_id).unwrap().source, Source::File { .. }));
        assert!(fixture.original.is_file());
        session.close_library().unwrap();
        drop(session);
        let mut session = fixture.session();
        let later = original_at(&fixture, "later.png", 190);
        session.execute("library.import", &json!({"paths":[later]})).unwrap();
        pump_library_adoption(&mut session, |s| s.catalog.upload_intents().len() == 2);
        assert_eq!(session.catalog.photos().count(), 4);
        assert_eq!(session.catalog.photo(first).unwrap().rating, 4);
    }

    #[test]
    fn cloud_mode_reports_a_missing_original_continues_and_retries_without_duplicates() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let first = import_local(&fixture, &mut session);
        let healthy = original_at(&fixture, "healthy.png", 190);
        session.execute("library.import", &json!({"paths":[healthy]})).unwrap();
        let bytes = std::fs::read(&fixture.original).unwrap();
        std::fs::remove_file(&fixture.original).unwrap();
        session.execute("cloud.syncEnable", &json!({"connection":"personal"})).unwrap();
        pump_library_adoption(&mut session, |s| s.catalog.upload_intents().len() == 1 && !s.cloud_imports.failures.is_empty());
        let failures = session.cloud_imports.failures.clone();
        for _ in 0..10 {
            resume_library(&mut session);
            session.poll_cloud_imports();
        }
        assert_eq!(session.cloud_imports.failures, failures, "no per-frame retry storm");
        assert!(matches!(session.catalog.photo(first).unwrap().source, Source::File { .. }));
        std::fs::write(&fixture.original, bytes).unwrap();
        retry_library(&mut session);
        pump_library_adoption(&mut session, |s| s.catalog.upload_intents().len() == 2);
        assert!(session.cloud_imports.failures.is_empty());
        assert_eq!(session.catalog.photos().count(), 2);
    }

    #[test]
    fn adoption_preserves_existing_ids_recipes_metadata_history_albums_copies_and_restart() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = import_local(&fixture, &mut session);
        session.selection = Selection::single(id);
        let copy = PhotoId(session.execute("photo.virtualCopy", &json!({})).unwrap()["ids"][0].as_u64().unwrap());
        session.execute("photo.rate", &json!({"ids":[id.0],"rating":4})).unwrap();
        session.execute("develop.set", &json!({"ids":[id.0],"control":"light.exposure","value":0.7})).unwrap();
        let album = session.execute("album.create", &json!({"name":"Keep"})).unwrap()["id"].as_u64().unwrap();
        session.commit("Membership", Op::SetAlbumPhotos { id: lightcraft_catalog::AlbumId(album), photos: vec![id, copy] }).unwrap();
        let before: Vec<_> = [id, copy].into_iter().map(|id| session.catalog.photo(id).unwrap().clone()).collect();
        let members = session.catalog.album(lightcraft_catalog::AlbumId(album)).unwrap().photos.clone();
        let selected = session.selection.clone();
        let params = json!({"connection":"personal","ids":[copy.0,id.0]});
        let done = session.execute("cloud.adopt", &params).unwrap();
        assert_eq!(done["failed"], json!([]), "{done}");
        assert_eq!(done["ids"], json!([id.0, copy.0]));
        assert_eq!(session.selection.ids, selected.ids);
        for old in &before {
            let mut expected = (**old).clone();
            expected.source = session.catalog.photo(old.id).unwrap().source.clone();
            assert!(matches!(expected.source, Source::Managed { .. }));
            assert_eq!(serde_json::to_value(&expected).unwrap(), serde_json::to_value(session.catalog.photo(old.id).unwrap()).unwrap());
        }
        assert_eq!(session.catalog.album(lightcraft_catalog::AlbumId(album)).unwrap().photos, members);
        assert!(fixture.original.is_file(), "adoption keeps original files");
        session.close_library().unwrap();
        drop(session);
        let mut reopened = fixture.session();
        let repeat = reopened.execute("cloud.adopt", &params).unwrap();
        assert_eq!(repeat["failed"], json!([]), "{repeat}");
        assert_eq!(repeat["requestId"], done["requestId"]);
        assert_eq!(reopened.catalog.photos().count(), 2);
        assert_eq!(reopened.catalog.upload_intents().len(), 1);
        assert_eq!(reopened.catalog.photo(id).unwrap().develop, before[0].develop);
        assert_eq!(reopened.catalog.photo(copy).unwrap().copy_of, Some(id));
    }

    #[test]
    fn adoption_preparation_preserves_edits_made_while_worker_runs_and_retains_resources_on_undo() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = import_local(&fixture, &mut session);
        let params = json!({"connection":"personal","ids":[id.0],"wait":false});
        let queued = session.execute("cloud.adopt", &params).unwrap();
        let index = session.cloud_imports.pending.iter().position(|p| p.guard.id() == queued["task"].as_u64().unwrap()).unwrap();
        let prepared = session.cloud_imports.pending[index].receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap();
        session.execute("photo.rate", &json!({"ids":[id.0],"rating":5})).unwrap();
        let done = complete(&mut session, prepared).unwrap();
        assert_eq!(done.value["failed"], json!([]));
        assert_eq!(session.catalog.photo(id).unwrap().rating, 5);
        session.undo_step().unwrap();
        assert!(matches!(session.catalog.photo(id).unwrap().source, Source::File { .. }));
        assert_eq!(session.catalog.photo(id).unwrap().rating, 5);
        assert_eq!(session.catalog.upload_intents().len(), 1);
        assert_eq!(session.catalog.assets().len(), 1);
        session.redo_step().unwrap();
        assert!(matches!(session.catalog.photo(id).unwrap().source, Source::Managed { .. }));
        assert_eq!(session.catalog.photo(id).unwrap().rating, 5);
    }

    #[test]
    fn adoption_recovery_uses_staged_bytes_without_original_and_refuses_undone_retry() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = import_local(&fixture, &mut session);
        let params = json!({"connection":"personal","ids":[id.0],"wait":false});
        session.execute("cloud.adopt", &params).unwrap();
        let prepared = session.cloud_imports.pending[0].receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap();
        let import = prepared.manifests[0].id.clone();
        std::fs::remove_file(&fixture.original).unwrap();
        session.close_library().unwrap();
        drop(session);
        let mut session = fixture.session();
        let done = session.execute("cloud.recoverImports", &json!({"connection":"personal","imports":[import]})).unwrap();
        assert_eq!(done["failed"], json!([]), "{done}");
        assert_eq!(session.catalog.photos().count(), 1);
        assert!(matches!(session.catalog.photo(id).unwrap().source, Source::Managed { .. }));
        session.execute("edit.undo", &json!({})).unwrap();
        session
            .commit(
                "Relink elsewhere",
                Op::Relink { id, source: Source::File { path: "elsewhere.png".into() }, file_name: "elsewhere.png".into(), format: None },
            )
            .unwrap();
        let root = session.media.asset_resolver.as_ref().unwrap().durable_root().unwrap();
        let manifest = read_manifest(&root, &import).unwrap();
        let failure = plan_candidate(&mut session.catalog, Some(&root), manifest, None).err().unwrap();
        assert!(failure.to_string().contains("undone"), "{failure}");
        assert!(matches!(session.catalog.photo(id).unwrap().source, Source::File { .. }));
    }

    #[test]
    fn adoption_reports_changed_original_without_replacing_it_or_blocking_healthy_sibling() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let first = import_local(&fixture, &mut session);
        let other = original_at(&fixture, "other.png", 190);
        session.execute("library.import", &json!({"paths":[other]})).unwrap();
        let second = session.catalog.photos().find(|p| p.id != first).unwrap().id;
        let original_bytes = std::fs::read(&fixture.original).unwrap();
        std::fs::write(&fixture.original, b"changed").unwrap();
        let params = json!({"connection":"personal","ids":[first.0,second.0]});
        let done = session.execute("cloud.adopt", &params).unwrap();
        assert_eq!(done["failed"].as_array().unwrap().len(), 1, "{done}");
        assert_eq!(done["failed"][0]["command"], "cloud.adopt");
        assert!(matches!(session.catalog.photo(first).unwrap().source, Source::File { .. }));
        assert!(matches!(session.catalog.photo(second).unwrap().source, Source::Managed { .. }));
        assert_eq!(std::fs::read(&fixture.original).unwrap(), b"changed");
        assert_eq!(session.catalog.photos().count(), 2);
        let sibling_source = session.catalog.photo(second).unwrap().source.clone();
        std::fs::write(&fixture.original, original_bytes).unwrap();
        let retry = session.execute("cloud.adopt", &params).unwrap();
        assert_eq!(retry["failed"], json!([]), "{retry}");
        assert_eq!(retry["requestId"], done["requestId"]);
        assert_eq!(session.catalog.photo(second).unwrap().source, sibling_source);
        assert!(matches!(session.catalog.photo(first).unwrap().source, Source::Managed { .. }));
        assert_eq!(session.catalog.upload_intents().len(), 2);
        assert!(session.cloud_imports.failures.is_empty());
    }

    #[test]
    fn adoption_promotes_browsed_photos_without_reapplying_sidecar_or_import_defaults() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = import_local(&fixture, &mut session);
        let packet = supporting_packet(&session.catalog);
        std::fs::write(fixture.original.with_extension("xmp"), packet).unwrap();
        session.commit("Browse marker", Op::SetLocal { id, local: true }).unwrap();
        session.execute("photo.rate", &json!({"ids":[id.0],"rating":2})).unwrap();
        session.import_defaults.creator = "Do not apply again".into();
        let done = session.execute("cloud.adopt", &json!({"connection":"personal","ids":[id.0]})).unwrap();
        assert_eq!(done["failed"], json!([]), "{done}");
        let photo = session.catalog.photo(id).unwrap();
        assert!(!photo.local);
        assert_eq!(photo.rating, 2);
        assert_ne!(photo.meta.creator, "Do not apply again");
        let packet = packet_for(&session, id);
        assert_foreign(&packet);
        let parsed = crate::sidecar::parse_sidecar(std::str::from_utf8(&packet).unwrap(), crate::crs::Target::for_photo(photo)).unwrap();
        assert_eq!(parsed.rating, Some(2));
        session.undo_step().unwrap();
        assert!(session.catalog.photo(id).unwrap().local);
    }

    #[test]
    fn adoption_undo_keeps_frozen_publication_dependencies_in_both_protocols() {
        for protocol in ["conditional-head-v1", "append-log-v1"] {
            let fixture = Fixture::new();
            let mut session = fixture.session();
            let id = import_local(&fixture, &mut session);
            session.execute("cloud.syncEnable", &json!({"connection":"personal","protocol":protocol})).unwrap();
            let done = session.execute("cloud.adopt", &json!({"connection":"personal","ids":[id.0]})).unwrap();
            assert_eq!(done["failed"], json!([]), "{done}");
            let managed = session.catalog.photo(id).unwrap().source.clone();
            let frozen = session.catalog.sync_state().unwrap().pending.clone();
            assert!(frozen.len() >= 2);
            session.undo_step().unwrap();
            let state = session.catalog.sync_state().unwrap();
            assert!(frozen.iter().all(|(id, mutation)| state.pending.get(id) == Some(mutation)));
            assert!(state.pending.len() > frozen.len());
            let Source::Managed { asset_id } = managed else { panic!("managed") };
            assert!(session.catalog.asset(&asset_id).is_some());
            assert_eq!(session.catalog.upload_intents().len(), 1);
            session.close_library().unwrap();
            drop(session);
            let reopened = fixture.session();
            assert!(reopened.catalog.asset(&asset_id).is_some());
            assert!(matches!(reopened.catalog.photo(id).unwrap().source, Source::File { .. }));
            assert!(frozen.iter().all(|(id, mutation)| reopened.catalog.sync_state().unwrap().pending.get(id) == Some(mutation)));
        }
    }
    #[test]
    fn adoption_rejects_owner_source_changes_after_worker_preparation() {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        let id = import_local(&fixture, &mut session);
        session.execute("cloud.adopt", &json!({"connection":"personal","ids":[id.0],"wait":false})).unwrap();
        let prepared = session.cloud_imports.pending[0].receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap();
        session
            .commit("Relink", Op::Relink { id, source: Source::File { path: "other.png".into() }, file_name: "other.png".into(), format: None })
            .unwrap();
        let done = complete(&mut session, prepared).unwrap().value;
        assert!(done["failed"][0]["reason"].as_str().unwrap().contains("source changed"), "{done}");
        assert!(session.catalog.upload_intents().is_empty());
        assert!(session.catalog.assets().is_empty());
        assert_eq!(session.catalog.photo(id).unwrap().source, Source::File { path: "other.png".into() });
    }
}
