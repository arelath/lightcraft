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
}
impl Target {
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
    }
    fn matches(&self, photo: &Photo) -> bool {
        !photo.deleted
            && photo.source == (Source::File { path: self.path.clone() })
            && photo.content_hash.as_deref() == Some(self.content_hash.as_str())
            && photo.file_size == self.size
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
        if roots.insert(photo.id) && matches!(photo.source, Source::File { .. }) {
            photos.push((**photo).clone());
        }
    }
    let mut job = Job::new(s, p, true)?;
    job.recover = false;
    job.paths = photos
        .iter()
        .filter_map(|photo| match &photo.source {
            Source::File { path } => Some(PathBuf::from(path)),
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
        let Source::File { path } = &photo.source else {
            return Err(error("adoption request must describe a file-backed original"));
        };
        let target = Target {
            photo_id: photo.id,
            path: path.clone(),
            content_hash: photo
                .content_hash
                .clone()
                .ok_or_else(|| error("reload the original to establish its content identity before moving it"))?,
            size: photo.file_size,
        };
        if !target.valid() {
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
            if saved.photo_id != target.photo_id
                || saved.path != target.path
                || saved.content_hash != target.content_hash
                || saved.size != target.size
            {
                return Err(error("adoption request and staged target differ; retained for recovery"));
            }
            if self.known.contains(&candidate.id) {
                return Ok(manifest);
            }
            if !self.root.join("blobs").join(&manifest.asset.original.id.0).try_exists().map_err(io)? {
                self.stage(&manifest.asset.original, &read_bounded(Path::new(&candidate.path), MAX_BYTES)?, cancel)?;
            }
            self.finish(&mut manifest, cancel)?;
            return Ok(manifest);
        }
        let bytes = read_bounded(Path::new(&candidate.path), MAX_BYTES)?;
        let info = crate::files::probe_bytes(&photo.file_name, &bytes).map_err(error)?;
        if info.preview_only.is_some()
            || info.kind == lightcraft_catalog::MediaKind::Video
            || info.content_hash != photo.content_hash
            || info.file_size != photo.file_size
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
        let mut manifest = Manifest {
            version: 4,
            id: candidate.id.clone(),
            asset_id: AssetId(candidate.id.clone()),
            asset: Asset {
                original,
                associated: [("xmp".into(), self.document("xmp", packet.as_bytes())?)].into(),
                smart_preview: None,
                thumbnail: None,
            },
            file_name: photo.file_name.clone(),
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
        plan.ops.push(Op::Relink { id, file_name, source: Source::Managed { asset_id: manifest.asset_id.clone() }, format: None });
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
