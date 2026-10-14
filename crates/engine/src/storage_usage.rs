//! What the cloud cache's drive holds, for Settings → Cache: the volume's size and free space,
//! and how much the library's own files take ("App & user data").
//!
//! Every query here can fail (a removed drive, a permission error, an unsupported filesystem):
//! the answer is then `None` and the page shows the figure as unknown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The drive or volume a path is on.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    /// `C:` on Windows, the mount point elsewhere (`/`, `/Volumes/Photos`).
    pub label: String,
    pub total_bytes: u64,
    /// Space this user can still write.
    pub free_bytes: u64,
}

/// The volume `path` is on, measured at its nearest existing ancestor (the cache folder may not
/// have been created yet).
pub fn volume(path: &Path) -> Option<Volume> {
    let existing = path.ancestors().find(|p| !p.as_os_str().is_empty() && p.exists())?;
    let stats = fs4::statvfs(existing).ok()?;
    Some(Volume { label: label(existing), total_bytes: stats.total_space(), free_bytes: stats.available_space() })
}

#[cfg(windows)]
fn label(path: &Path) -> String {
    use std::path::{Component, Prefix};
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match absolute.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => format!("{}:", char::from(letter).to_ascii_uppercase()),
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                format!(r"\\{}\{}", server.to_string_lossy(), share.to_string_lossy())
            }
            _ => prefix.as_os_str().to_string_lossy().into_owned(),
        },
        _ => absolute.display().to_string(),
    }
}

/// The mount point: the highest ancestor on the same device.
#[cfg(unix)]
fn label(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Ok(device) = std::fs::metadata(&absolute).map(|m| m.dev()) else { return absolute.display().to_string() };
    let mut mount = absolute.as_path();
    for parent in absolute.ancestors().skip(1) {
        match std::fs::metadata(parent) {
            Ok(meta) if meta.dev() == device => mount = parent,
            _ => break,
        }
    }
    mount.display().to_string()
}

#[cfg(not(any(windows, unix)))]
fn label(path: &Path) -> String {
    path.display().to_string()
}

/// Bounds for [`dir_size`]: deeper or larger trees are reported as measured so far.
const MAX_DEPTH: usize = 32;
const MAX_ENTRIES: usize = 2_000_000;

/// Bytes in the files under `dir`, skipping `exclude` (and everything under it) and never
/// following symbolic links. Unreadable entries count as empty.
pub fn dir_size(dir: &Path, exclude: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen = seen.saturating_add(1);
            if seen > MAX_ENTRIES {
                return total;
            }
            let path = entry.path();
            if exclude.contains(&path) {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
    total
}

/// How long a measured folder size is reused before it is measured again.
const FRESH: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Default)]
struct Measured {
    bytes: Option<u64>,
    at: Option<std::time::Instant>,
    running: bool,
}

static SIZES: Mutex<BTreeMap<PathBuf, Measured>> = Mutex::new(BTreeMap::new());

/// [`dir_size`] measured on a worker thread: the last result (`None` until the first one is
/// in), starting a new measurement when it is older than a few seconds. `wait` measures now.
pub fn dir_size_cached(dir: &Path, exclude: Vec<PathBuf>, wait: bool) -> Option<u64> {
    if wait {
        let bytes = dir_size(dir, &exclude);
        let mut sizes = SIZES.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = sizes.entry(dir.to_path_buf()).or_default();
        entry.bytes = Some(bytes);
        entry.at = Some(std::time::Instant::now());
        return Some(bytes);
    }
    let mut sizes = SIZES.lock().unwrap_or_else(PoisonError::into_inner);
    if sizes.len() > 64 {
        sizes.retain(|_, m| m.running);
    }
    let entry = sizes.entry(dir.to_path_buf()).or_default();
    let stale = entry.at.is_none_or(|at| at.elapsed() > FRESH);
    if stale && !entry.running {
        entry.running = true;
        let key = dir.to_path_buf();
        let spawned = std::thread::Builder::new().name("storage-usage".into()).spawn(move || {
            let bytes = dir_size(&key, &exclude);
            let mut sizes = SIZES.lock().unwrap_or_else(PoisonError::into_inner);
            let entry = sizes.entry(key).or_default();
            entry.bytes = Some(bytes);
            entry.at = Some(std::time::Instant::now());
            entry.running = false;
        });
        if spawned.is_err() {
            entry.running = false;
        }
    }
    entry.bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightcraft-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn volume_of_a_missing_folder_is_its_existing_ancestors() {
        let dir = scratch("volume");
        let v = volume(&dir.join("not/yet/created")).unwrap();
        assert!(v.total_bytes > 0 && v.free_bytes <= v.total_bytes);
        assert!(!v.label.is_empty());
        #[cfg(windows)]
        assert!(v.label.ends_with(':') || v.label.starts_with(r"\\"), "{}", v.label);
        let again = volume(&dir).unwrap();
        assert_eq!((again.label, again.total_bytes), (v.label, v.total_bytes));
    }

    #[test]
    fn an_unusable_path_has_no_volume_instead_of_failing() {
        assert_eq!(volume(Path::new("")), None);
    }

    #[test]
    fn dir_size_counts_nested_files_and_skips_the_excluded_folder() {
        let dir = scratch("size");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        std::fs::write(dir.join("top"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("a/b/deep"), [0u8; 5]).unwrap();
        std::fs::write(dir.join("cache/blob"), [0u8; 100]).unwrap();
        assert_eq!(dir_size(&dir, &[]), 115);
        assert_eq!(dir_size(&dir, &[dir.join("cache")]), 15);
        assert_eq!(dir_size(&dir.join("missing"), &[]), 0);
        assert_eq!(dir_size_cached(&dir, vec![dir.join("cache")], true), Some(15));
        assert_eq!(dir_size_cached(&dir, vec![dir.join("cache")], false), Some(15));
    }
}
