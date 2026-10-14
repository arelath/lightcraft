//! Settings → Cache → "Storage location for originals": the folder where this device keeps the
//! cloud cache (verified downloads, offline copies and their retention settings).
//!
//! The choice is a small pointer file in the library's device cache folder, never catalog data.
//! Changing it copies every cached blob to the new folder and verifies its SHA-256 there (on a
//! cancellable activity task), copies the retention settings, saves the pointer, reopens the
//! library on the new folder and only then deletes the old copies. Nothing is lost when the copy
//! fails or is cancelled: the old folder stays in use. A chosen folder that is unavailable when
//! the library opens (a disconnected drive) is reported and the library folder is used meanwhile;
//! the choice is kept for when it is back.

use crate::cmd::bad;
use crate::{Result, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

/// The pointer file, in the library's device cache folder.
pub const POINTER: &str = "cloud-cache-location.json";
/// The default cache folder, inside the library's device cache folder.
pub const DEFAULT_DIR: &str = "assets-cache";
const RETENTION: &str = "retention.json";
const MAX_FILES: usize = 400_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    version: u32,
    path: PathBuf,
}

/// The cloud cache folder for a library whose device cache folder is `cache_dir`, and a warning
/// when the chosen folder can't be used now.
pub(crate) fn resolve(cache_dir: &Path) -> (PathBuf, Option<String>) {
    let default = cache_dir.join(DEFAULT_DIR);
    let bytes = match std::fs::read(cache_dir.join(POINTER)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (default, None),
        Err(e) => return (default, Some(format!("cloud cache location could not be read ({e}); using the library folder"))),
    };
    let pointer = match serde_json::from_slice::<Pointer>(&bytes) {
        Ok(p) if p.version == 1 && p.path.is_absolute() => p,
        _ => return (default, Some(format!("{} is damaged; the cloud cache uses the library folder", cache_dir.join(POINTER).display()))),
    };
    if !pointer.path.is_dir() {
        let warning = format!(
            "cloud cache folder {} is not available (drive disconnected?); the library folder is used until it is back",
            pointer.path.display()
        );
        return (default, Some(warning));
    }
    (pointer.path, None)
}

/// A move in progress (desktop): its worker's result arrives here.
pub struct PendingMove {
    receiver: mpsc::Receiver<std::result::Result<Moved, String>>,
    library: Arc<()>,
    task: u64,
}

/// What a finished copy leaves to do on the owner thread.
pub struct Moved {
    from: PathBuf,
    copied: Vec<String>,
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    crate::originals::paths_overlap(a, b).unwrap_or(false) && {
        let (a, b) = (std::path::absolute(a).unwrap_or_else(|_| a.to_path_buf()), std::path::absolute(b).unwrap_or_else(|_| b.to_path_buf()));
        #[cfg(windows)]
        {
            a.to_string_lossy().trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches(['\\', '/']))
        }
        #[cfg(not(windows))]
        {
            a.components().eq(b.components())
        }
    }
}

fn valid_blob_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `cloud.cacheLocation`: report, change (`path`) or reset (`reset: true`) the folder.
pub(crate) fn command(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.cacheLocation";
    let lib = s.library.as_ref().ok_or_else(|| bad(C, "open a library first"))?;
    let cache_dir = lib.cache_dir.clone().filter(|_| lib.on_disk).ok_or_else(|| bad(C, "open a native persistent library first"))?;
    let library_dir = lib.dir.clone();
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| bad(C, "open a native persistent library first"))?;
    let current = resolver.root().to_path_buf();
    let default = cache_dir.join(DEFAULT_DIR);
    let reset = match p.get("reset") {
        None => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(C, "reset must be true or false"))?,
    };
    let path = crate::cmd::str_param(p, "path");
    let status = |s: &Session| {
        json!({
            "path": current,
            "default": default,
            "custom": !paths_equal(&current, &default),
            "moving": s.cache_move.as_ref().map(|m| m.task),
            "lastMove": s.cache_move_report,
        })
    };
    let target = match (path, reset) {
        (None, false) => return Ok(status(s)),
        (Some(_), true) => return Err(bad(C, "give either path or reset")),
        (None, true) => default.clone(),
        (Some(path), false) => {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(bad(C, "the cloud cache folder must be an absolute path"));
            }
            path
        }
    };
    if paths_equal(&target, &current) {
        return Ok(status(s));
    }
    let wait = match p.get("wait") {
        None => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(C, "wait must be true or false"))?,
    };
    if wait && crate::assets::is_cached_only() {
        return Err(bad(C, "wait is available in headless CLI/MCP sessions; the desktop moves the cache as a background task"));
    }
    if s.cache_move.is_some() {
        return Err(bad(C, "the cloud cache is already being moved; wait for it to finish or cancel it in Activity"));
    }
    let overlaps = |a: &Path, b: &Path| crate::originals::paths_overlap(a, b).map_err(|e| bad(C, e));
    if overlaps(&target, &current)? {
        return Err(bad(C, "choose a folder outside the current cloud cache folder"));
    }
    for protected in [library_dir.join("Managed"), library_dir.join("Originals")] {
        if overlaps(&target, &protected)? {
            return Err(bad(C, format!("the cloud cache cannot share {}: it holds photos that are not disposable", protected.display())));
        }
    }
    if let Some(durable) = resolver.durable_root()
        && overlaps(&target, &durable)?
    {
        return Err(bad(C, "the cloud cache cannot share the folder of durable imported originals"));
    }
    if !paths_equal(&target, &default) && target.join(RETENTION).exists() {
        return Err(bad(C, "that folder already holds a cloud cache (another library's?); choose an empty folder"));
    }
    if !resolver.transfer_status().active.is_empty() {
        return Err(bad(C, "cloud downloads are running; wait for them to finish (or cancel them) before moving the cache"));
    }
    std::fs::create_dir_all(&target).map_err(|e| bad(C, format!("{}: {e}", target.display())))?;
    let probe = target.join(".lightcraft-write-test");
    lightcraft_catalog::safe_file::write_atomic(&probe, b"ok").map_err(|e| bad(C, format!("{} is not writable: {e}", target.display())))?;
    let _ = std::fs::remove_file(&probe);
    // the blobs to copy (a cache never holds more than the retention limits allow)
    let mut files = Vec::new();
    let mut total = 0u64;
    if let Ok(dir) = std::fs::read_dir(&current) {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !valid_blob_name(&name) || !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            if files.len() >= MAX_FILES {
                return Err(bad(C, "the cloud cache holds too many files to move; clear it first"));
            }
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
            files.push(name);
        }
    }
    let pointer = if paths_equal(&target, &default) { None } else { Some(Pointer { version: 1, path: target.clone() }) };
    let job = Job { from: current.clone(), to: target.clone(), cache_dir, files, total, pointer };
    if wait {
        let moved = job.run(&AtomicBool::new(false), None).map_err(|e| bad(C, e))?;
        let report = finish(s, moved);
        return Ok(json!({"background": false, "path": target, "result": report}));
    }
    let task = s.activity.start("cache", "Move Cloud Cache", crate::activity::Cancel::Yes);
    task.set_unit(crate::activity::Unit::Bytes);
    let cancel = task.cancel_flag().ok_or_else(|| bad(C, "could not create a cancellation flag"))?;
    let id = task.id();
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("cloud-cache-move".into())
        .spawn(move || {
            let result = job.run(&cancel, Some(&task.handle()));
            drop(task);
            let _ = sender.send(result);
        })
        .map_err(|e| bad(C, format!("could not start moving the cache: {e}")))?;
    s.cache_move = Some(PendingMove { receiver, library: s.library_token(), task: id });
    Ok(json!({"background": true, "task": id, "path": target}))
}

struct Job {
    from: PathBuf,
    to: PathBuf,
    cache_dir: PathBuf,
    files: Vec<String>,
    total: u64,
    pointer: Option<Pointer>,
}

impl Job {
    /// Copy and verify every blob, then the retention settings, then save the pointer. The old
    /// folder is untouched.
    fn run(self, cancel: &AtomicBool, progress: Option<&crate::activity::TaskHandle>) -> std::result::Result<Moved, String> {
        let mut done = 0u64;
        let mut copied = Vec::with_capacity(self.files.len());
        for name in &self.files {
            if cancel.load(Ordering::Relaxed) {
                return Err("moving the cloud cache was cancelled; the old folder is still in use".into());
            }
            let source = self.from.join(name);
            let target = self.to.join(name);
            if !target.exists() || !file_hash(&target).is_ok_and(|h| h == *name) {
                // gone meanwhile (evicted): nothing to copy
                let Ok(mut input) = std::fs::File::open(&source) else { continue };
                let mut hash = sha2::Sha256::new();
                let written = lightcraft_catalog::safe_file::write_atomic_with(&target, &mut |out| {
                    let mut buf = vec![0u8; 1 << 20];
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(std::io::Error::other("cancelled"));
                        }
                        let n = input.read(&mut buf)?;
                        if n == 0 {
                            return Ok(());
                        }
                        let chunk = buf.get(..n).ok_or_else(|| std::io::Error::other("read past buffer"))?;
                        hash.update(chunk);
                        out.write_all(chunk)?;
                    }
                });
                if cancel.load(Ordering::Relaxed) {
                    let _ = std::fs::remove_file(&target);
                    return Err("moving the cloud cache was cancelled; the old folder is still in use".into());
                }
                let size = written.map_err(|e| format!("{}: {e}", target.display()))?;
                if format!("{:x}", hash.finalize()) != *name {
                    // a damaged cache file: leave it behind, it would be refetched anyway
                    let _ = std::fs::remove_file(&target);
                    continue;
                }
                done = done.saturating_add(size);
            } else {
                done = done.saturating_add(std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0));
            }
            // the copy now reads back with the right content
            if !file_hash(&target).is_ok_and(|h| h == *name) {
                let _ = std::fs::remove_file(&target);
                return Err(format!("{} did not verify after copying; the old folder is still in use", target.display()));
            }
            copied.push(name.clone());
            if let Some(task) = progress {
                task.progress(done, self.total.max(done));
            }
        }
        match std::fs::read(self.from.join(RETENTION)) {
            Ok(bytes) => lightcraft_catalog::safe_file::write_atomic(&self.to.join(RETENTION), &bytes).map_err(|e| format!("cache settings: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cache settings: {e}")),
        }
        let pointer_path = self.cache_dir.join(POINTER);
        match &self.pointer {
            Some(pointer) => {
                let bytes = serde_json::to_vec_pretty(pointer).map_err(|e| e.to_string())?;
                lightcraft_catalog::safe_file::write_atomic(&pointer_path, &bytes).map_err(|e| format!("{}: {e}", pointer_path.display()))?;
            }
            None => match std::fs::remove_file(&pointer_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", pointer_path.display())),
            },
        }
        Ok(Moved { from: self.from, copied })
    }
}

fn file_hash(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(format!("{:x}", hash.finalize()));
        }
        hash.update(buf.get(..n).unwrap_or_default());
    }
}

/// Reopen the library on the new folder, then delete the old copies (in the background; a file
/// still open elsewhere stays and is harmless).
fn finish(s: &mut Session, moved: Moved) -> Value {
    let reopened = match s.library.as_ref().map(|l| l.dir.clone()) {
        Some(dir) => s.close_library().and_then(|_| s.open_library(&dir, false).map(|_| ())).map_err(|e| e.to_string()),
        None => Err("the library was closed".into()),
    };
    let report = match &reopened {
        Ok(()) => {
            let Moved { from, copied } = moved;
            let count = copied.len();
            let _ = std::thread::Builder::new().name("cloud-cache-cleanup".into()).spawn(move || {
                for name in copied {
                    let _ = std::fs::remove_file(from.join(name));
                }
                let _ = std::fs::remove_file(from.join(RETENTION));
                // only an emptied folder goes
                let _ = std::fs::remove_dir(&from);
            });
            json!({"ok": true, "moved": count})
        }
        Err(e) => json!({"ok": false, "error": format!("the cache was copied but the library could not be reopened: {e}")}),
    };
    s.cache_move_report = Some(report.clone());
    report
}

impl Session {
    /// Finish a cache move whose copy completed: reopen on the new folder. True when the
    /// library was reopened (hosts drop what they cached about it).
    pub fn poll_cache_move(&mut self) -> bool {
        let Some(pending) = &self.cache_move else { return false };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err("the cache move stopped; the old folder is still in use".into()),
        };
        let same_library = Arc::ptr_eq(&pending.library, &self.library_token());
        self.cache_move = None;
        match result {
            Ok(moved) if same_library => {
                finish(self, moved);
                true
            }
            Ok(_) => {
                self.cache_move_report = Some(json!({"ok": false, "error": "the library changed while the cache was copied"}));
                false
            }
            Err(error) => {
                self.cache_move_report = Some(json!({"ok": false, "error": error}));
                false
            }
        }
    }
}
