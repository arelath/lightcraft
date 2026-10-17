//! Adoption changes source ownership, never replaces an existing photo record.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Target {
    photo_id: PhotoId,
    path: String,
    content_hash: String,
    size: u64,
    /// Generated source identity; encoded hash/size describe its staged DNG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    demo: Option<[u32; 3]>,
}
impl Target {
    pub(super) fn generated(&self) -> bool {
        self.demo.is_some()
    }
    pub(super) fn photo_id(&self) -> PhotoId {
        self.photo_id
    }
    pub(super) fn valid(&self) -> bool {
        self.photo_id.0 > 0
            && self.photo_id.0 < u64::MAX
            && !self.path.is_empty()
            && self.path.len() <= 32_768
            && !self.path.contains('\0')
            && self.content_hash.len() == 32
            && self.content_hash.bytes().all(|b| b.is_ascii_hexdigit())
            && self.size > 0
            && self.size <= MAX_BYTES
            && self.demo.is_none_or(|[scene, width, height]| scene > 0 && width > 0 && height > 0)
    }
    fn matches(&self, photo: &Photo) -> bool {
        !photo.deleted
            && match self.demo {
                Some([scene, width, height]) => photo.source == (Source::Demo { scene }) && (photo.width, photo.height) == (width, height),
                None => {
                    photo.source == (Source::File { path: self.path.clone() })
                        && photo.content_hash.as_deref() == Some(self.content_hash.as_str())
                        && photo.file_size == self.size
                }
            }
    }
}

pub(super) fn original_mismatch(manifest: &Manifest, bytes: &[u8]) -> bool {
    manifest.adoption.as_ref().is_some_and(|target| {
        target.size != bytes.len() as u64
            || target.content_hash != lightcraft_preview::hash_bytes(bytes).to_string()
            || manifest
                .photo
                .as_ref()
                .is_none_or(|photo| photo.content_hash.as_deref() != Some(target.content_hash.as_str()) || photo.file_size != target.size)
    })
}

pub(crate) fn start(s: &mut Session, p: &Value) -> Result<Value> {
    let mut ids = if p.get("requestId").is_some() && p.get("ids").is_none() && p.get("id").is_none() { Vec::new() } else { s.targets(p) };
    ids.sort();
    ids.dedup();
    if ids.len() > 1000 || (ids.is_empty() && p.get("requestId").is_none()) {
        return Err(error("choose between 1 and 1000 existing photos"));
    }
    let mut photos = Vec::new();
    let mut roots = BTreeSet::new();
    for id in &ids {
        let photo = s.catalog.photo(*id).ok_or_else(|| error("selected photo no longer exists"))?;
        if photo.deleted {
            return Err(error("restore deleted photos before moving them to cloud storage"));
        }
        let photo = photo
            .copy_of
            .and_then(|parent| s.catalog.photo(parent))
            .filter(|parent| !parent.deleted && parent.source == photo.source)
            .unwrap_or(photo);
        if roots.insert(photo.id) && matches!(photo.source, Source::File { .. } | Source::Demo { .. }) {
            photos.push((**photo).clone());
        }
    }
    let mut job = Job::new(s, p, true)?;
    job.automatic = p["automatic"] == true;
    job.recover = false;
    job.paths = photos
        .iter()
        .filter_map(|photo| match &photo.source {
            Source::File { path } => Some(PathBuf::from(path)),
            Source::Demo { .. } => Some(job.root.join("generated-demo").join(format!("{}.dng", photo.id.0))),
            _ => None,
        })
        .collect();
    job.adoption_key = Some(sha256(&serde_json::to_vec(&(job.connection.storage_snapshot(), &ids)).map_err(|e| error(e.to_string()))?).0);
    job.adoption = Some(photos);
    job.request_options = Some(RequestOptions {
        requested: p.get("requestId").map(|v| v.as_str().ok_or_else(|| error("requestId must be a string"))).transpose()?.map(str::to_owned),
        fresh: p.get("newRequest").map(|v| v.as_bool().ok_or_else(|| error("newRequest must be true or false"))).transpose()?.unwrap_or(false),
        policy: Some(requests::DuplicatePolicy::Allow),
    });
    launch_job(s, p, job)
}

impl Job {
    pub(super) fn prepare_adoption(&self, cancel: &AtomicBool, progress: &crate::activity::TaskHandle) -> Result<Vec<Manifest>> {
        let request = self.request.as_ref().ok_or_else(|| error("adoption request is missing"))?;
        let photos = self.adoption.as_ref().ok_or_else(|| error("adoption snapshots are missing"))?;
        if photos.len() != request.candidates.len() {
            return Err(error("adoption request is inconsistent; data retained"));
        }
        let mut prepared = Vec::new();
        for (index, (candidate, photo)) in request.candidates.iter().zip(photos).enumerate() {
            progress.detail(&photo.file_name);
            match self.prepare_existing(candidate, photo, cancel) {
                Ok(manifest) => prepared.push(manifest),
                Err(e) => self.failure(&candidate.id, &photo.file_name, e.to_string(), cancel)?,
            }
            progress.progress(index as u64 + 1, photos.len() as u64);
        }
        Ok(prepared)
    }
    fn prepare_existing(&self, candidate: &requests::Candidate, photo: &Photo, cancel: &AtomicBool) -> Result<Manifest> {
        let mut target = if let Source::File { path } = &photo.source {
            Some(Target {
                photo_id: photo.id,
                path: path.clone(),
                content_hash: photo
                    .content_hash
                    .clone()
                    .ok_or_else(|| error("reload the original to establish its content identity before moving it"))?,
                size: photo.file_size,
                demo: None,
            })
        } else {
            None
        };
        if target.as_ref().is_some_and(|target| !target.valid()) {
            return Err(error("reload this photo's original before adopting it; source identity is unavailable"));
        }
        let record = self.root.join("imports").join(format!("{}.json", candidate.id));
        if record.try_exists().map_err(io)? {
            let meta = std::fs::symlink_metadata(&record).map_err(io)?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(error("adoption manifest is redirected; preserved"));
            }
            let mut manifest = read_manifest(&self.root, &candidate.id)?;
            let saved = manifest.adoption.as_ref().ok_or_else(|| error("this staged record is not an adoption; retained for recovery"))?;
            if saved.photo_id != photo.id || !saved.matches(photo) {
                return Err(error("adoption request and staged target differ; retained for recovery"));
            }
            if self.known.contains(&candidate.id) {
                return Ok(manifest);
            }
            if !self.root.join("blobs").join(&manifest.asset.original.id.0).try_exists().map_err(io)? {
                let bytes = original_bytes(photo, &candidate.path, cancel)?;
                if original_mismatch(&manifest, &bytes) {
                    return Err(error("regenerated original differs from staged identity; retained for recovery"));
                }
                self.stage(&manifest.asset.original, &bytes, cancel)?;
            }
            self.finish(&mut manifest, cancel)?;
            return Ok(manifest);
        }
        let bytes = original_bytes(photo, &candidate.path, cancel)?;
        let info = crate::files::probe_bytes(&photo.file_name, &bytes).map_err(error)?;
        if let Source::Demo { scene } = photo.source {
            target = Some(Target {
                photo_id: photo.id,
                path: candidate.path.clone(),
                content_hash: info.content_hash.clone().ok_or_else(|| error("generated original has no content identity"))?,
                size: info.file_size,
                demo: Some([scene, photo.width, photo.height]),
            });
        }
        let target = target.ok_or_else(|| error("adoption requires a file or generated original"))?;
        if info.preview_only.is_some()
            || info.kind == lightcraft_catalog::MediaKind::Video
            || info.content_hash.as_deref() != Some(target.content_hash.as_str())
            || info.file_size != target.size
            || (info.width, info.height) != (photo.width, photo.height)
        {
            return Err(error("original changed or cannot be fully decoded; reload the photo before moving it to cloud storage"));
        }
        let old = if let Some(sidecar) = crate::sidecar::find_sidecar(&candidate.path, self.naming) {
            Some(read_bounded(&sidecar, MAX_DOCUMENT_BYTES)?)
        } else {
            info.xmp.map(String::into_bytes)
        };
        let fresh = crate::sidecar::sidecar_packet(photo, &self.labels);
        let packet = match old {
            Some(old) => crate::sidecar::merge_managed_packet(&old, &fresh).map_err(error)?,
            None => fresh,
        };
        let original = make_blob(
            &bytes,
            &self.connection.id,
            keys::in_library(&self.connection.prefix, &keys::original(&sha256(&bytes)).map_err(|e| error(e.to_string()))?)
                .map_err(|e| error(e.to_string()))?,
        );
        let mut render = photo.clone();
        render.source = Source::Managed { asset_id: AssetId(candidate.id.clone()) };
        if target.demo.is_some() {
            render.file_name = Path::new(&photo.file_name).with_extension("dng").to_string_lossy().into_owned();
            render.format = "DNG".into();
            render.kind = lightcraft_catalog::MediaKind::Raw;
            render.content_hash = info.content_hash;
            render.file_size = info.file_size;
            render.as_shot_wb = info.as_shot_wb;
        }
        let mut manifest = Manifest {
            version: if target.demo.is_some() { 5 } else { 4 },
            id: candidate.id.clone(),
            asset_id: AssetId(candidate.id.clone()),
            asset: Asset {
                original,
                associated: [("xmp".into(), self.document("xmp", packet.as_bytes())?)].into(),
                smart_preview: None,
                thumbnail: None,
            },
            file_name: render.file_name.clone(),
            photo: Some(Box::new(render)),
            prepared: false,
            target: None,
            adoption: Some(target),
            working: None,
            stack_with: None,
            origin_asset: None,
            duplicate_policy: Some(requests::DuplicatePolicy::Allow),
        };
        // Bytes precede the manifest, so recovery never depends on a mutable sidecar.
        {
            let _guard = self.resolver.durable_guard(cancel).map_err(|e| error(e.to_string()))?;
            stage(&self.root, &manifest.asset.original, &bytes)?;
            let document = manifest.asset.associated.get("xmp").ok_or_else(|| error("adoption XMP is missing"))?;
            stage(&self.root, document, packet.as_bytes())?;
            manifest.write(&self.root)?;
        }
        drop(bytes);
        self.finish(&mut manifest, cancel)?;
        Ok(manifest)
    }
}

/// Procedural sources have no original bytes. Preserve their full linear float
/// samples, including highlights above one, rather than baking edits into a JPEG.
fn original_bytes(photo: &Photo, path: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let Source::Demo { scene } = photo.source else { return read_bounded(Path::new(path), MAX_BYTES) };
    if cancel.load(Ordering::Relaxed) {
        return Err(error("cloud import cancelled"));
    }
    let scene = lightcraft_scenes::demo_library().into_iter().find(|s| s.id == scene).ok_or_else(|| error("unknown generated photo scene"))?;
    if photo.width == 0 || photo.height == 0 || photo.width > scene.width || photo.height > scene.height {
        return Err(error("invalid generated original dimensions"));
    }
    let pixels = crate::demo::scene_pixels(&scene, photo.width.max(photo.height) as usize).map_err(error)?;
    if (pixels.width, pixels.height) != (photo.width as usize, photo.height as usize) {
        return Err(error("generated original dimensions differ from catalog"));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(error("cloud import cancelled"));
    }
    let color = lightcraft_merge::FrameColor::Linear {
        to_xyz_d50: lightcraft_color::bradford(lightcraft_color::D65, lightcraft_color::D50).mul(&lightcraft_color::REC2020.to_xyz()),
    };
    lightcraft_merge::write_linear_dng(
        &pixels,
        &color,
        lightcraft_geom::Orientation::Normal,
        &Default::default(),
        0.0,
        lightcraft_merge::DngSamples::Float,
    )
    .map_err(|e| error(e.to_string()))
}

pub(super) fn plan(catalog: &mut lightcraft_catalog::Catalog, manifest: Manifest) -> Result<Plan> {
    let target = manifest.adoption.as_ref().ok_or_else(|| error("missing adoption target"))?;
    if let Some(intent) = catalog.upload_intents().get(&manifest.id) {
        let photo = catalog.photo(intent.photo_id).ok_or_else(|| error("adopted photo no longer exists"))?;
        if !matches!(photo.source, Source::Managed { .. }) {
            return Err(error("this adoption was undone; use Redo or start a new request"));
        }
        let ids =
            catalog.photos().filter(|p| (p.id == photo.id || p.copy_of == Some(photo.id)) && p.source == photo.source).map(|p| p.id.0).collect();
        return Ok(Plan { ids, ..Default::default() });
    }
    let current = catalog.photo(target.photo_id).ok_or_else(|| error("adoption photo no longer exists; staged bytes retained"))?;
    if !target.matches(current) {
        return Err(error("photo source changed during preparation; staged bytes retained; reload or retry with a new request"));
    }
    let family: Vec<_> = catalog
        .photos()
        .filter(|p| (p.id == current.id || p.copy_of == Some(current.id)) && target.matches(p))
        .map(|p| (p.id, p.file_name.clone(), p.local))
        .collect();
    let mut plan = Plan { ids: family.iter().map(|(id, _, _)| id.0).collect(), ..Default::default() };
    plan.ops.push(Op::SetAsset { id: manifest.asset_id.clone(), asset: Some(Box::new(manifest.asset)) });
    for (id, file_name, local) in family {
        let generated = target.demo.is_some();
        plan.ops.push(Op::Relink {
            id,
            file_name: if generated { Path::new(&file_name).with_extension("dng").to_string_lossy().into_owned() } else { file_name },
            source: Source::Managed { asset_id: manifest.asset_id.clone() },
            format: generated.then(|| "DNG".into()),
        });
        if generated {
            let photo = manifest.photo.as_ref().ok_or_else(|| error("generated original metadata missing"))?;
            plan.ops.push(Op::SetMediaKind { id, kind: lightcraft_catalog::MediaKind::Raw });
            plan.ops.push(Op::SetContent {
                id,
                width: photo.width,
                height: photo.height,
                file_size: photo.file_size,
                content_hash: photo.content_hash.clone(),
                preview_only: None,
            });
            plan.ops.push(Op::SetAsShotWb { id, wb: photo.as_shot_wb });
        }
        if local {
            plan.ops.push(Op::SetLocal { id, local: false });
        }
    }
    plan.ops.push(Op::SetUploadIntent {
        id: manifest.id,
        intent: Some(Box::new(UploadIntent {
            asset_id: manifest.asset_id,
            photo_id: target.photo_id,
            state: UploadState::SavedLocally,
            verified: Default::default(),
            error: None,
            finalization: None,
            blocked: None,
        })),
    });
    catalog.apply(Op::Batch { ops: plan.ops.clone() })?;
    Ok(plan)
}

/// Retain durable publication resources while undo changes only user-visible sources.
pub(super) fn retain_resources(op: &mut Op) {
    let mut pending = vec![op];
    while let Some(op) = pending.pop() {
        if let Op::Batch { ops } = op {
            ops.retain(|op| !matches!(op, Op::SetAsset { .. } | Op::SetUploadIntent { .. } | Op::SetCloudIdentity { .. }));
            pending.extend(ops.iter_mut());
        }
    }
}
