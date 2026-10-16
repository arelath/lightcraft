//! A skipped candidate is a settled local decision, not an upload that owns media.
//! Retain its complete description for idempotent retry, outside active staging.
use super::{MAX_MANIFEST_BYTES, Manifest, Result, error, io, read_bounded, read_manifest, requests};
use lightcraft_catalog::PhotoId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Skipped {
    version: u32,
    pub manifest: Manifest,
    pub photo_id: PhotoId,
}
fn path(root: &Path, id: &str) -> Result<PathBuf> {
    if !lightcraft_catalog::assets::identifier(id) {
        return Err(error("invalid settled import identity"));
    }
    Ok(root.join("skipped-imports").join(format!("{id}.json")))
}
impl Skipped {
    pub fn target_matches(&self, catalog: &lightcraft_catalog::Catalog) -> bool {
        catalog.photo(self.photo_id).is_some_and(|photo| {
            !photo.deleted
                && match &photo.source {
                    lightcraft_catalog::Source::Managed { asset_id } => catalog.asset(asset_id).is_some_and(|asset| {
                        asset.original.id == self.manifest.asset.original.id
                            && asset.original.remote.connection == self.manifest.asset.original.remote.connection
                    }),
                    _ => false,
                }
        })
    }
    pub fn new(manifest: Manifest, photo_id: PhotoId) -> Self {
        Self { version: 1, manifest, photo_id }
    }
    fn validate(&self) -> Result<()> {
        self.manifest.validate()?;
        if self.version != 1
            || !self.manifest.prepared
            || self.manifest.target.is_some()
            || self.manifest.stack_with.is_some()
            || self.photo_id.0 == 0
            || self.photo_id.0 == u64::MAX
        {
            return Err(error("invalid settled duplicate decision; staging retained"));
        }
        Ok(())
    }
}
pub(super) fn read(root: &Path, id: &str) -> Result<Option<Skipped>> {
    let file = path(root, id)?;
    match std::fs::symlink_metadata(root.join("skipped-imports")) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(_) => return Err(error("settled import directory is redirected; preserved")),
        Err(e) => return Err(io(e)),
    }
    match std::fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(error("settled import record is redirected; preserved")),
        Err(e) => return Err(io(e)),
    }
    let record: Skipped =
        serde_json::from_slice(&read_bounded(&file, MAX_MANIFEST_BYTES + 1024)?).map_err(|_| error("invalid settled import record; preserved"))?;
    record.validate()?;
    if record.manifest.id != id {
        return Err(error("settled import identity differs from its filename; preserved"));
    }
    Ok(Some(record))
}
/// Caller holds the durable guard. The decision precedes retirement of the active
/// manifest; neither this operation nor a retry deletes any original/rendition bytes.
pub(super) fn save(root: &Path, record: &Skipped) -> Result<()> {
    record.validate()?;
    let active = root.join("imports").join(format!("{}.json", record.manifest.id));
    let exists = match std::fs::symlink_metadata(&active) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            let previous = read_manifest(root, &record.manifest.id)?;
            if serde_json::to_value(previous).map_err(|e| error(e.to_string()))?
                != serde_json::to_value(&record.manifest).map_err(|e| error(e.to_string()))?
            {
                return Err(error("skipped staging record changed; preserved for recovery"));
            }
            true
        }
        Ok(_) => return Err(error("skipped staging record is redirected; preserved")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(io(e)),
    };
    if let Some(previous) = read(root, &record.manifest.id)? {
        if serde_json::to_value(previous).map_err(|e| error(e.to_string()))? != serde_json::to_value(record).map_err(|e| error(e.to_string()))? {
            return Err(error("settled duplicate decision changed; both records retained"));
        }
    } else {
        requests::ensure_directory(&root.join("skipped-imports"))?;
        let bytes = serde_json::to_vec(record).map_err(|e| error(e.to_string()))?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES + 1024 {
            return Err(error("settled import record exceeds its safety limit"));
        }
        lightcraft_catalog::safe_file::write_atomic(&path(root, &record.manifest.id)?, &bytes).map_err(io)?;
    }
    if exists {
        std::fs::remove_file(active).map_err(io)?;
    }
    Ok(())
}
