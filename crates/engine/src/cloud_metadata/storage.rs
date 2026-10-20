use super::*;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{MutexGuard, TryLockError},
    time::SystemTime,
};

#[derive(Clone)]
struct Entry {
    bytes: u64,
    touched: u64,
}
#[derive(Default)]
pub(super) struct State {
    loaded: bool,
    stamp: u64,
    disabled: bool,
    bytes: u64,
    other_files: usize,
    tick: u64,
    entries: BTreeMap<String, Entry>,
}
fn disk(error: std::io::Error) -> AssetError {
    AssetError::Disk(error.to_string())
}
// Explicit formatting preserves leading zeroes, unlike Debug formatting.
fn filename(id: &[u8; 32]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect::<String>() + ".body"
}
fn owned(name: &str) -> bool {
    name.strip_suffix(".body").is_some_and(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f')))
}
fn regular_root(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(disk)?;
    if !fs::symlink_metadata(root).map_err(disk)?.file_type().is_dir() {
        return Err(AssetError::Disk("immutable history cache is not a directory".into()));
    }
    Ok(())
}
// Separate cache instances (including a cancelled worker from the old catalog)
// serialize through the OS lock. The durable generation changes BEFORE mutation,
// so a crash or another writer makes stale in-memory indexes rescan.
fn gate(root: &Path, cancel: &AtomicBool) -> Result<File> {
    regular_root(root)?;
    let path = root.join(".cache-lock");
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => return Err(AssetError::Disk("history cache lock is not a regular file".into())),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(disk(e)),
        _ => {}
    }
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(disk)?;
    loop {
        cancelled(cancel)?;
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(std::fs::TryLockError::Error(e)) => return Err(disk(e)),
        }
    }
}
fn stamp(gate: &mut File) -> Result<u64> {
    let len = gate.metadata().map_err(disk)?.len();
    if len == 0 {
        return Ok(0);
    }
    if len != 8 {
        return Err(AssetError::Disk("history cache generation is damaged".into()));
    }
    gate.seek(SeekFrom::Start(0)).map_err(disk)?;
    let mut bytes = [0; 8];
    gate.read_exact(&mut bytes).map_err(disk)?;
    Ok(u64::from_le_bytes(bytes))
}
impl State {
    fn changed(&mut self, gate: &mut File) -> Result<()> {
        let next = self.stamp.checked_add(1).ok_or_else(|| AssetError::Disk("history cache generation is exhausted".into()))?;
        gate.seek(SeekFrom::Start(0)).map_err(disk)?;
        gate.write_all(&next.to_le_bytes()).map_err(disk)?;
        gate.sync_all().map_err(disk)?;
        self.stamp = next;
        Ok(())
    }
    fn load(&mut self, cache: &Cache, gate: &mut File, cancel: &AtomicBool) -> Result<()> {
        let stamp = stamp(gate)?;
        if self.loaded && self.stamp == stamp {
            return Ok(());
        }
        // Build a bounded trial. Unknown files (including interrupted atomic-write
        // temps) are never reused/deleted and consume admission capacity.
        let mut entries = Vec::new();
        let mut bytes = 0u64;
        let mut other_files = 0usize;
        for (index, entry) in fs::read_dir(&cache.root).map_err(disk)?.enumerate() {
            cancelled(cancel)?;
            if index >= MAX_SCAN {
                self.disabled = true;
                self.loaded = true;
                self.stamp = stamp;
                return Ok(());
            }
            let entry = entry.map_err(disk)?;
            if entry.file_name() == ".cache-lock" {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path()).map_err(disk)?;
            if !meta.file_type().is_file() {
                other_files = other_files.saturating_add(1);
                continue;
            }
            bytes = bytes.saturating_add(meta.len());
            let name = entry.file_name().to_string_lossy().into_owned();
            if owned(&name) && entries.len() < MAX_SCAN {
                entries.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), name, meta.len()));
            } else {
                other_files = other_files.saturating_add(1);
            }
        }
        entries.sort();
        self.entries = entries.into_iter().enumerate().map(|(i, (_, name, bytes))| (name, Entry { bytes, touched: i as u64 })).collect();
        self.tick = self.entries.len() as u64;
        self.bytes = bytes;
        self.other_files = other_files;
        self.loaded = true;
        self.stamp = stamp;
        self.disabled = false;
        self.trim(cache, gate, None, 0, cancel)?;
        Ok(())
    }
    fn touch(&mut self, name: &str) {
        self.tick = self.tick.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(name) {
            entry.touched = self.tick
        }
    }
    fn remove(&mut self, cache: &Cache, name: &str) -> Result<()> {
        let path = cache.root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() => fs::remove_file(&path).map_err(disk)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err(AssetError::Disk("history cache entry changed type".into())),
            Err(e) => return Err(disk(e)),
        }
        if let Some(entry) = self.entries.remove(name) {
            self.bytes = self.bytes.saturating_sub(entry.bytes)
        }
        Ok(())
    }
    fn trim(&mut self, cache: &Cache, gate: &mut File, target: Option<&str>, reserve: u64, cancel: &AtomicBool) -> Result<()> {
        let extra = usize::from(target.is_some_and(|t| !self.entries.contains_key(t)));
        let mut invalidated = false;
        while self.bytes.saturating_add(reserve) > cache.max_bytes
            || self.entries.len().saturating_add(self.other_files).saturating_add(extra) > cache.max_entries
        {
            cancelled(cancel)?;
            let victim = self.entries.iter().filter(|(name, _)| Some(name.as_str()) != target).min_by_key(|(_, e)| e.touched).map(|(n, _)| n.clone());
            let Some(victim) = victim else { return Err(AssetError::Disk("history cache capacity is unavailable".into())) };
            if !invalidated {
                self.changed(gate)?;
                invalidated = true;
            }
            self.remove(cache, &victim)?;
        }
        Ok(())
    }
}
impl Cache {
    fn lock(&self, cancel: &AtomicBool) -> Result<MutexGuard<'_, State>> {
        loop {
            cancelled(cancel)?;
            match self.state.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(e)) => return Ok(e.into_inner()),
                Err(TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
    }
    pub(super) fn forget(&self, id: &[u8; 32], cancel: &AtomicBool) -> Result<()> {
        let mut state = self.lock(cancel)?;
        let mut gate = gate(&self.root, cancel)?;
        state.load(self, &mut gate, cancel)?;
        state.changed(&mut gate)?;
        state.remove(self, &filename(id))
    }
    pub(super) fn load(&self, id: &[u8; 32], max: usize, cancel: &AtomicBool) -> Result<Option<Vec<u8>>> {
        let mut state = self.lock(cancel)?;
        let mut gate = gate(&self.root, cancel)?;
        state.load(self, &mut gate, cancel)?;
        let name = filename(id);
        if state.disabled || !state.entries.contains_key(&name) {
            return Ok(None);
        }
        let read = (|| {
            let path = self.root.join(&name);
            if !fs::symlink_metadata(&path).map_err(disk)?.file_type().is_file() {
                return Ok(None);
            }
            let mut file = File::open(path).map_err(disk)?;
            let length = file.metadata().map_err(disk)?.len();
            if length < HEADER || length > (max as u64).saturating_add(HEADER) {
                return Ok(None);
            }
            let mut header = [0u8; HEADER as usize];
            file.read_exact(&mut header).map_err(disk)?;
            let size = header.get(72..80).and_then(|v| v.try_into().ok()).map(u64::from_le_bytes);
            if header.get(..8) != Some(MAGIC.as_slice()) || header.get(8..40) != Some(id.as_slice()) || size != Some(length - HEADER) {
                return Ok(None);
            }
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 64 << 10];
            loop {
                cancelled(cancel)?;
                let n = file.read(&mut buffer).map_err(disk)?;
                if n == 0 {
                    break;
                }
                if bytes.len().saturating_add(n) > max {
                    return Ok(None);
                }
                bytes.extend_from_slice(buffer.get(..n).ok_or(AssetError::Integrity)?);
            }
            if bytes.len() as u64 != length - HEADER || header.get(40..72) != Some(Sha256::digest(&bytes).as_slice()) {
                return Ok(None);
            }
            Ok(Some(bytes))
        })();
        if matches!(read, Ok(Some(_))) {
            state.touch(&name)
        } else if !matches!(read, Err(AssetError::Cancelled)) {
            let _ = state.changed(&mut gate).and_then(|_| state.remove(self, &name));
        }
        read
    }
    pub(super) fn save(&self, id: &[u8; 32], bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
        if bytes.len() > crate::cloud_append::MAX_OBJECT {
            return Ok(());
        }
        let mut state = self.lock(cancel)?;
        let mut gate = gate(&self.root, cancel)?;
        state.load(self, &mut gate, cancel)?;
        let name = filename(id);
        let length = (bytes.len() as u64).saturating_add(HEADER);
        if state.disabled || length > self.max_bytes {
            return Ok(());
        }
        // Reserve the whole temporary file as well as an existing target. At most
        // one writer holds this library cache's lock; no over-budget publication.
        state.trim(self, &mut gate, Some(&name), length, cancel)?;
        state.changed(&mut gate)?;
        let result = lightcraft_catalog::safe_file::write_atomic_with(&self.root.join(&name), &mut |writer| {
            writer.write_all(MAGIC)?;
            writer.write_all(id)?;
            writer.write_all(&Sha256::digest(bytes))?;
            writer.write_all(&(bytes.len() as u64).to_le_bytes())?;
            for chunk in bytes.chunks(64 << 10) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                writer.write_all(chunk)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            cancelled(cancel)?;
            return Err(disk(error));
        }
        // Cancellation can arrive during fsync/rename. Account for a committed
        // file before reporting it, so retries cannot escape the capacity bound.
        if let Some(old) = state.entries.insert(name.clone(), Entry { bytes: length, touched: 0 }) {
            state.bytes = state.bytes.saturating_sub(old.bytes)
        }
        state.bytes = state.bytes.saturating_add(length);
        state.touch(&name);
        cancelled(cancel)?;
        Ok(())
    }
}
