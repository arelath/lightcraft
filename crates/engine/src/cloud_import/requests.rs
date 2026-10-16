//! Device-local request identities. The active pointer is written first; candidate
//! IDs are derived from it, so even a crash before the request file is recoverable.
use super::{Result, error, io, new_identity, read_bounded};
use crate::assets::{s3::S3Connection, sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

const LIMIT: u64 = 16 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DuplicatePolicy {
    #[default]
    Skip,
    Allow,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub path: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    version: u32,
    pub id: String,
    pub storage: S3Connection,
    pub duplicate_policy: DuplicatePolicy,
    pub candidates: Vec<Candidate>,
    pub recipe: Value,
}
fn group(id: &str) -> Result<&str> {
    let (prefix, nonce) = id.split_once('-').ok_or_else(|| error("invalid import request identity"))?;
    if prefix.len() != 64 || nonce.len() != 32 || !prefix.bytes().chain(nonce.bytes()).all(|b| b.is_ascii_hexdigit()) {
        return Err(error("invalid import request identity"));
    }
    Ok(prefix)
}
pub(super) fn ensure_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(error("import request directory is redirected; data preserved")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path).map_err(io),
        Err(e) => Err(io(e)),
    }
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => read_bounded(path, LIMIT).map(Some),
        Ok(_) => Err(error("import request record is redirected or invalid; data preserved")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(e)),
    }
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| error(e.to_string()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(error("import request exceeds 16 MiB; choose smaller groups"));
    }
    lightcraft_catalog::safe_file::write_atomic(path, &bytes).map_err(io)
}
fn normalize(path: &Path) -> Result<String> {
    let full = crate::originals::prospective_path(path).or_else(|_| std::path::absolute(path).map_err(|e| e.to_string())).map_err(error)?;
    let text = full.to_string_lossy().replace('\\', "/");
    Ok(text)
}
impl Request {
    fn validate(&self) -> Result<()> {
        group(&self.id)?;
        self.storage.validate().map_err(|e| error(e.to_string()))?;
        if self.version != 1 || self.candidates.is_empty() || self.candidates.len() > 1000 {
            return Err(error("invalid or future import request; data preserved"));
        }
        for (index, candidate) in self.candidates.iter().enumerate() {
            let expected = sha256(format!("{}\0{index}\0{}", self.id, candidate.path).as_bytes()).0;
            if candidate.id != expected || candidate.path.is_empty() || candidate.path.len() > 32_768 || candidate.path.contains('\0') {
                return Err(error("import request candidate identity is inconsistent; data preserved"));
            }
        }
        Ok(())
    }
}

/// Caller holds the durable guard across registration, including the pointer and
/// request writes. This is local metadata; paths/recipes never enter cloud objects.
#[allow(clippy::too_many_arguments)]
pub(super) fn register_keyed(
    root: &Path,
    storage: &S3Connection,
    paths: &[PathBuf],
    requested: Option<&str>,
    fresh: bool,
    policy: Option<DuplicatePolicy>,
    recipe: Value,
    fingerprint: Option<String>,
) -> Result<Request> {
    if requested.is_some() && fresh {
        return Err(error("requestId resumes an existing attempt; omit newRequest"));
    }
    let keyed_selection = fingerprint.is_some();
    let paths = paths.iter().map(|path| normalize(path)).collect::<Result<Vec<_>>>()?;
    let fingerprint = if let Some(id) = requested {
        group(id)?.to_owned()
    } else if let Some(fingerprint) = fingerprint {
        fingerprint
    } else {
        let mut keys = paths.clone();
        keys.sort();
        sha256(&serde_json::to_vec(&(storage.storage_snapshot(), keys, policy.unwrap_or_default())).map_err(|e| error(e.to_string()))?).0
    };
    ensure_directory(root)?;
    ensure_directory(&root.join("requests"))?;
    let dir = root.join("requests").join(&fingerprint);
    ensure_directory(&dir)?;
    let id = if let Some(id) = requested {
        id.to_owned()
    } else if !fresh && let Some(bytes) = read(&dir.join("active"))? {
        serde_json::from_slice::<String>(&bytes).map_err(|_| error("invalid active import request; retained for repair"))?
    } else {
        let nonce: String = new_identity()?.chars().take(32).collect();
        let id = format!("{fingerprint}-{nonce}");
        write(&dir.join("active"), &id)?;
        id
    };
    if group(&id)? != fingerprint {
        return Err(error("active import request differs from its source group; data preserved"));
    }
    let file = dir.join(format!("{id}.json"));
    if let Some(bytes) = read(&file)? {
        let saved: Request = serde_json::from_slice(&bytes).map_err(|_| error("invalid import request; data preserved"))?;
        saved.validate()?;
        if saved.id != id || !saved.storage.same_destination(storage) || policy.is_some_and(|policy| policy != saved.duplicate_policy) {
            return Err(error("import request destination or duplicate policy changed; resume the original request or start a new one"));
        }
        if !keyed_selection && !paths.is_empty() {
            let mut expected: Vec<_> = saved.candidates.iter().map(|candidate| candidate.path.clone()).collect();
            let mut actual = paths;
            expected.sort();
            actual.sort();
            if actual != expected {
                return Err(error("import request paths changed; use its original paths or start a new request"));
            }
        }
        return Ok(saved);
    }
    if requested.is_some() || paths.is_empty() {
        return Err(error("import request is unavailable; provide the original source paths to resume"));
    }
    let candidates =
        paths.into_iter().enumerate().map(|(index, path)| Candidate { id: sha256(format!("{id}\0{index}\0{path}").as_bytes()).0, path }).collect();
    let request = Request { version: 1, id, storage: storage.storage_snapshot(), duplicate_policy: policy.unwrap_or_default(), candidates, recipe };
    request.validate()?;
    write(&file, &request)?;
    Ok(request)
}
