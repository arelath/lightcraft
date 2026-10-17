//! Managed original preparation and media catalog plans, independent of cloud enrollment.
//! Immutable bytes and checkpoints are supplied by the durable owner. This layer
//! neither chooses cloud identities nor creates upload intents or settles requests.
use crate::assets::{AssetResolver, keys, sha256};
use crate::{EngineError, Result, Session};
use lightcraft_catalog::assets::{Asset, AssetId, Blob, RemoteLocation};
use lightcraft_catalog::{Catalog, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_BYTES: u64 = crate::input_limits::ORIGINAL_BYTES;
const MAX_DOCUMENT_BYTES: u64 = 16 << 20;
fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn io(e: std::io::Error) -> EngineError {
    error(e.to_string())
}

#[derive(Clone)]
pub(crate) struct Revision {
    pub photo_id: PhotoId,
    pub base_asset: AssetId,
}
#[derive(Clone)]
pub(crate) struct PreparedMedia {
    pub asset_id: AssetId,
    pub asset: Asset,
    pub file_name: String,
    pub photo: Option<Box<Photo>>,
    pub prepared: bool,
    pub target: Option<Revision>,
    pub working: Option<String>,
    pub stack_with: Option<PhotoId>,
    pub origin_asset: Option<AssetId>,
}
impl PreparedMedia {
    fn validate(&self) -> Result<()> {
        if !lightcraft_catalog::assets::identifier(&self.asset_id.0)
            || self.file_name.is_empty()
            || self.file_name.len() > 1024
            || self.file_name.contains(['/', '\\'])
            || self.file_name.chars().any(char::is_control)
            || self.working.as_ref().is_some_and(|path| !working_name(path))
            || self.stack_with.is_some_and(|id| id.0 == 0 || id.0 == u64::MAX)
            || self.origin_asset.as_ref().is_some_and(|id| !lightcraft_catalog::assets::identifier(&id.0))
            || self.stack_with.is_some() != self.origin_asset.is_some()
            || self.target.as_ref().is_some_and(|target| {
                target.photo_id.0 == 0 || target.photo_id.0 == u64::MAX || !lightcraft_catalog::assets::identifier(&target.base_asset.0)
            })
        {
            return Err(error("invalid managed media description; staged data preserved"));
        }
        self.asset.validate().map_err(error)?;
        if self.photo.as_ref().is_some_and(|photo| {
            photo.source != (Source::Managed { asset_id: self.asset_id.clone() })
                || photo.file_name != self.file_name
                || !crate::input_limits::shared_dimensions(photo.width, photo.height)
        }) {
            return Err(error("invalid managed photo description; staged data preserved"));
        }
        if self.prepared && (self.photo.is_none() || self.asset.smart_preview.is_none() || self.asset.thumbnail.is_none()) {
            return Err(error("prepared media is missing its photo or renditions"));
        }
        Ok(())
    }
}

pub(crate) struct Preparation<'a> {
    pub root: &'a Path,
    pub resolver: Arc<AssetResolver>,
    pub connection: &'a str,
    pub prefix: &'a str,
    pub now: &'a str,
    pub defaults: &'a crate::import::ImportDefaults,
    pub presets: &'a [lightcraft_develop::Preset],
    pub metadata_presets: &'a [crate::cmd::metadata::MetadataPreset],
    pub labels: &'a Catalog,
}
/// The completion context is frozen when preparation starts. Managed local
/// revisions use their originating operation, independently of cloud enrollment.
#[derive(Clone, Copy, Default)]
pub(crate) enum PreparationKind {
    #[default]
    NewOriginal,
    Dng,
    Xmp,
    Returned,
    ExternalCopy,
}
pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
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

pub(crate) fn stage(root: &Path, blob: &Blob, bytes: &[u8]) -> Result<()> {
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

pub(crate) fn make_blob(bytes: &[u8], connection: &str, key: String) -> Blob {
    Blob { id: sha256(bytes), size: bytes.len() as u64, remote: RemoteLocation { connection: connection.into(), key, version: None } }
}

pub(crate) fn working_name(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    parts.len() == 3
        && parts.first() == Some(&"working")
        && parts.get(1).is_some_and(|id| lightcraft_catalog::assets::identifier(id))
        && parts.get(2).is_some_and(|name| {
            !name.is_empty() && name.len() <= 1024 && *name != "." && *name != ".." && !name.contains('\\') && !name.chars().any(char::is_control)
        })
}

impl Preparation<'_> {
    pub(crate) fn describe(&self, manifest: &PreparedMedia, info: crate::media::ProbeInfo, packet: Option<&[u8]>) -> Result<Photo> {
        if info.kind == lightcraft_catalog::MediaKind::Video || info.preview_only.is_some() {
            return Err(error("managed import requires a decodable photo original"));
        }
        let sidecar = packet
            .map(|packet| {
                let packet = std::str::from_utf8(packet).map_err(|e| error(format!("supporting XMP is unreadable; source preserved: {e}")))?;
                let target = crate::crs::Target::for_file(info.kind, &info.format, info.preview_only.is_some(), &info.meta.camera);
                crate::sidecar::parse_sidecar(packet, target)
                    .map(|data| data.resolve_label(self.labels))
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
            self.now,
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
            defaults.import_defaults = (*self.defaults).clone();
            defaults.presets = self.presets.to_vec();
            defaults.metadata_presets = self.metadata_presets.to_vec();
            crate::import::apply_import_defaults(&defaults, &mut photo);
        }
        if let Some(sidecar) = sidecar {
            crate::sidecar::merge_into(&mut photo, &sidecar, self.now);
        }
        Ok(photo)
    }

    pub(crate) fn prepare(
        &self,
        manifest: &mut PreparedMedia,
        cancel: &AtomicBool,
        verify_source: &dyn Fn(&[u8]) -> Result<()>,
        checkpoint: &mut dyn FnMut(&PreparedMedia, &AtomicBool) -> Result<()>,
        stage_blob: &dyn Fn(&Blob, &[u8], &AtomicBool) -> Result<()>,
    ) -> Result<()> {
        manifest.validate()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(error("cloud import cancelled; durable original retained"));
        }
        let path = self.root.join("blobs").join(&manifest.asset.original.id.0);
        let bytes = read_bounded(&path, MAX_BYTES)?;
        if sha256(&bytes) != manifest.asset.original.id || bytes.len() as u64 != manifest.asset.original.size {
            return Err(error("durable original failed verification"));
        }
        verify_source(&bytes)?;
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
        shadow.import_defaults = (*self.defaults).clone();
        shadow.presets = self.presets.to_vec();
        shadow.metadata_presets = self.metadata_presets.to_vec();
        let mut photo = if let Some(photo) = &manifest.photo {
            (**photo).clone()
        } else {
            let info = crate::files::probe_bytes(&manifest.file_name, &bytes).map_err(error)?;
            self.describe(manifest, info, packet.as_deref())?
        };
        photo.id = PhotoId(1);
        manifest.photo = Some(Box::new(photo.clone()));
        manifest.prepared = false;
        checkpoint(manifest, cancel)?;
        photo.copy_of = None; // the render seed does not need its catalog family
        shadow.media.asset_resolver = Some(self.resolver.clone());
        shadow.commit(
            "Prepare managed media",
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
            self.connection,
            keys::in_library(self.prefix, &keys::smart_preview(&manifest.asset.original.id, &sha256(&preview)).map_err(|e| error(e.to_string()))?)
                .map_err(|e| error(e.to_string()))?,
        );
        manifest.asset.smart_preview = Some(preview_blob.clone());
        checkpoint(manifest, cancel)?;
        stage_blob(&preview_blob, &preview, cancel)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(error("cloud import cancelled; durable original and prepared data retained"));
        }
        let rendered =
            shadow.thumb_job(photo.id, 512).ok_or_else(|| error("could not prepare the managed thumbnail"))?.run().rendered.map_err(error)?;
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
        let thumbnail = make_blob(&png, self.connection, keys::in_library(self.prefix, &key).map_err(|e| error(e.to_string()))?);
        manifest.asset.thumbnail = Some(thumbnail.clone());
        checkpoint(manifest, cancel)?;
        stage_blob(&thumbnail, &png, cancel)?;
        manifest.prepared = true;
        if let Some(relative) = &manifest.working {
            let path = self.root.join(relative);
            let parent = path.parent().ok_or_else(|| error("invalid working-file parent"))?;
            if !crate::originals::prospective_path(&path).map_err(error)?.starts_with(crate::originals::prospective_path(self.root).map_err(error)?) {
                return Err(error("working directory is redirected outside managed storage"));
            }
            std::fs::create_dir_all(parent).map_err(io)?;
            match lightcraft_catalog::safe_file::write_new(&path, &bytes) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {} // editor revisions are never replaced
                Err(e) => return Err(io(e)),
            }
        }
        checkpoint(manifest, cancel)
    }
}

#[derive(Default)]
pub(crate) struct Plan {
    pub ids: Vec<u64>,
    pub ops: Vec<Op>,
    pub copies: Vec<Value>,
    pub stacks: Vec<(PhotoId, Option<PhotoId>, Op)>,
}
pub(crate) fn plan(catalog: &mut Catalog, root: Option<&Path>, media: PreparedMedia) -> Result<Plan> {
    media.validate()?;
    if !media.prepared {
        return Err(error("media has not finished staging its renditions"));
    }
    let mut plan = Plan::default();
    if let (Some(parent), Some(base)) = (media.stack_with, &media.origin_asset)
        && catalog.photo(parent).is_none_or(|photo| photo.deleted || photo.source != (Source::Managed { asset_id: base.clone() }))
    {
        return Err(error("external-edit source changed or was deleted; durable working data was retained"));
    }
    let mut photo = *media.photo.ok_or_else(|| error("staged photo is missing"))?;
    if let Some(target) = &media.target {
        let current = catalog.photo(target.photo_id).ok_or_else(|| error("revision photo no longer exists; staged data preserved"))?;
        if current.deleted || current.source != (Source::Managed { asset_id: target.base_asset.clone() }) {
            return Err(error("photo changed while preparing its revision; staged data preserved"));
        }
        photo.id = target.photo_id;
    } else {
        photo.id = catalog.alloc_photo_id();
    };
    if let Some(relative) = &media.working {
        let root = root.ok_or_else(|| error("working storage is unavailable"))?;
        let path = root.join(relative).to_string_lossy().to_string();
        let binding = Op::SetWorkingCopy {
            id: photo.id,
            working: Some(Box::new(lightcraft_catalog::managed::WorkingCopy {
                connection: media.asset.original.remote.connection.clone(),
                path: path.clone(),
                asset_id: media.asset_id.clone(),
                ingested: media.asset.original.id.clone(),
            })),
        };
        plan.copies.push(json!({"path":path,"id":photo.id.0,"original":media.stack_with.map(|id|id.0)}));
        plan.stacks.push((photo.id, media.stack_with, binding));
    }
    plan.ids.push(photo.id.0);
    plan.ops.push(Op::SetAsset { id: media.asset_id, asset: Some(Box::new(media.asset)) });
    if media.target.is_some() {
        let copies: Vec<_> = catalog
            .photos()
            .filter(|copy| {
                copy.id == photo.id
                    || (copy.copy_of == Some(photo.id)
                        && media.target.as_ref().is_some_and(|target| copy.source == (Source::Managed { asset_id: target.base_asset.clone() })))
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
    Ok(plan)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
