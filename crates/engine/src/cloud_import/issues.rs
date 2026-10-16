//! Durable preparation failures, separate from the ownership manifests whose
//! corruption must continue to block staging maintenance.
use super::{Result, error, io, read_bounded};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

const LIMIT: u64 = 16 << 20;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Issue {
    pub id: String,
    pub name: String,
    pub reason: String,
    pub command: String,
    pub retry: Value,
}
pub fn load(root: &Path) -> Result<BTreeMap<String, Issue>> {
    let path = root.join("import-issues.json");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(error("import recovery status is redirected; retained for repair")),
        Err(e) => return Err(io(e)),
    }
    let issues: BTreeMap<String, Issue> =
        serde_json::from_slice(&read_bounded(&path, LIMIT)?).map_err(|_| error("import recovery status is unreadable; retained for repair"))?;
    if issues.len() > 100_000 || issues.iter().any(|(id, issue)| id != &issue.id) {
        return Err(error("invalid import recovery status; retained for repair"));
    }
    Ok(issues)
}
/// The caller holds the resolver's durable guard for this read/modify/write.
pub fn update(root: &Path, failures: &[Issue], completed: &[String]) -> Result<()> {
    if failures.is_empty() && completed.is_empty() {
        return Ok(());
    }
    let mut issues = load(root)?;
    for id in completed {
        issues.remove(id);
    }
    for issue in failures {
        issues.insert(issue.id.clone(), issue.clone());
    }
    if issues.len() > 100_000 {
        return Err(error("too many unresolved import failures; repair retained requests first"));
    }
    let bytes = serde_json::to_vec(&issues).map_err(|e| error(e.to_string()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(error("import recovery status exceeds 16 MiB; retained data is safe"));
    }
    std::fs::create_dir_all(root).map_err(io)?;
    lightcraft_catalog::safe_file::write_atomic(&root.join("import-issues.json"), &bytes).map_err(io)
}
