//! Bounded operation-local wire-byte spool. Backward traversal admits verified
//! batches; forward replay checks the stored bytes again without a second GET.
//! Scratch files never grant authority or carry trusted catalog state.
use super::*;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(any(test, not(any(unix, windows))))]
use std::path::PathBuf;

const MAX_REFERENCE: usize = crate::cloud_records::replay::MAX_REFERENCE_BYTES;
const SLOT_BYTES: u64 = MAX_REFERENCE as u64 + 8 + 4;
#[cfg(test)]
const MAX_INDEX: u64 = RECOVERY_SEGMENT_BATCHES * SLOT_BYTES;
const MAX_BODIES: u64 = 512 << 20;
const IO_CHUNK: usize = 64 << 10;
fn disk(error: impl std::fmt::Display) -> AssetError {
    AssetError::Disk(format!(
        "history replay scratch: {error}; free temporary disk space and retry synchronization; catalog and remote objects retained"
    ))
}
struct Scratch {
    file: Option<File>,
    #[cfg(any(test, not(any(unix, windows))))]
    path: PathBuf,
}
impl Scratch {
    fn new(extension: &str) -> Result<Self> {
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(disk)?;
        let path = std::env::temp_dir().join(format!("lightcraft-history-{}.{extension}", sha256(&nonce).0));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_DELETE_ON_CLOSE: OS cleanup also covers process exit.
            // https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-createfilew
            options.custom_flags(0x0400_0000).share_mode(0);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(disk)?;
        // An anonymous Unix file remains usable through its descriptor and is
        // reclaimed on its last close, including a crash without Rust destructors.
        #[cfg(unix)]
        std::fs::remove_file(&path).map_err(disk)?;
        Ok(Self {
            file: Some(file),
            #[cfg(any(test, not(any(unix, windows))))]
            path,
        })
    }
    fn file(&mut self) -> Result<&mut File> {
        self.file.as_mut().ok_or_else(|| disk("scratch file closed"))
    }
    fn write(&mut self, offset: u64, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
        canceled(cancel)?;
        self.file()?.seek(SeekFrom::Start(offset)).map_err(disk)?;
        for chunk in bytes.chunks(IO_CHUNK) {
            canceled(cancel)?;
            self.file()?.write_all(chunk).map_err(disk)?;
        }
        canceled(cancel)
    }
    fn read(&mut self, offset: u64, bytes: &mut [u8], cancel: &AtomicBool) -> Result<()> {
        canceled(cancel)?;
        self.file()?.seek(SeekFrom::Start(offset)).map_err(disk)?;
        for chunk in bytes.chunks_mut(IO_CHUNK) {
            canceled(cancel)?;
            self.file()?.read_exact(chunk).map_err(disk)?;
        }
        canceled(cancel)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        self.file.take();
        #[cfg(not(any(unix, windows)))]
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) struct Index {
    refs: Scratch,
    bodies: Scratch,
    capacity: u64,
    written: u64,
    // At most 4096 offsets, not decoded payloads or a history-sized seen set.
    starts: VecDeque<u64>,
    popped: u64,
    pub(super) count: u64,
}
impl Index {
    pub(super) fn new() -> Result<Self> {
        Self::with_capacity(MAX_BODIES)
    }
    fn with_capacity(capacity: u64) -> Result<Self> {
        if capacity == 0 || capacity > MAX_BODIES {
            return Err(disk("invalid scratch byte budget"));
        }
        Ok(Self {
            refs: Scratch::new("refs")?,
            bodies: Scratch::new("bodies")?,
            capacity,
            written: 0,
            starts: VecDeque::with_capacity(RECOVERY_SEGMENT_BATCHES as usize),
            popped: 0,
            count: 0,
        })
    }
    pub(super) fn retained(&self) -> u64 {
        self.starts.len() as u64
    }
    pub(super) fn push(&mut self, reference: &RecordRef, body: &[u8], cancel: &AtomicBool) -> Result<()> {
        canceled(cancel)?;
        let bytes = serde_json::to_vec(reference).map_err(disk)?;
        if bytes.len() > MAX_REFERENCE
            || body.is_empty()
            || body.len() as u64 != reference.size
            || reference.size > self.capacity
            || reference.size > publication::MAX_BYTES as u64
        {
            return Err(invalid("history replay record exceeds scratch budget"));
        }
        let count = self.count.checked_add(1).ok_or_else(|| invalid("history length exhausted"))?;
        let end = self.written.checked_add(reference.size).ok_or_else(|| invalid("history byte length exhausted"))?;
        let offset = self.written % self.capacity;
        let first = usize::try_from(reference.size.min(self.capacity - offset)).map_err(disk)?;
        self.bodies.write(offset, body.get(..first).ok_or_else(|| disk("invalid body range"))?, cancel)?;
        if first < body.len() {
            self.bodies.write(0, body.get(first..).ok_or_else(|| disk("invalid body range"))?, cancel)?;
        }
        let slot = (self.count % RECOVERY_SEGMENT_BATCHES) * SLOT_BYTES;
        self.refs.write(slot, &self.written.to_le_bytes(), cancel)?;
        self.refs.write(slot + 8, &bytes, cancel)?;
        self.refs.write(slot + 8 + MAX_REFERENCE as u64, &u32::try_from(bytes.len()).map_err(disk)?.to_le_bytes(), cancel)?;
        if self.starts.len() as u64 == RECOVERY_SEGMENT_BATCHES {
            self.starts.pop_front();
        }
        self.starts.push_back(self.written);
        self.written = end;
        self.count = count;
        let floor = end.saturating_sub(self.capacity);
        while self.starts.front().is_some_and(|start| *start < floor) {
            self.starts.pop_front();
        }
        Ok(())
    }
    pub(super) fn pop(&mut self, connection: &S3Connection, cancel: &AtomicBool) -> Result<Option<(RecordRef, Batch)>> {
        canceled(cancel)?;
        if self.popped >= self.retained() {
            return Ok(None);
        }
        let slot = ((self.count - self.popped - 1) % RECOVERY_SEGMENT_BATCHES) * SLOT_BYTES;
        let mut length = [0u8; 4];
        self.refs.read(slot + 8 + MAX_REFERENCE as u64, &mut length, cancel)?;
        let length = usize::try_from(u32::from_le_bytes(length)).map_err(disk)?;
        if length == 0 || length > MAX_REFERENCE {
            return Err(disk("invalid reference frame length"));
        }
        let mut bytes = vec![0; length];
        self.refs.read(slot + 8, &mut bytes, cancel)?;
        let reference: RecordRef = serde_json::from_slice(&bytes).map_err(disk)?;
        reference.validate(connection).map_err(disk)?;
        let mut start = [0u8; 8];
        self.refs.read(slot, &mut start, cancel)?;
        let start = u64::from_le_bytes(start);
        if start < self.written.saturating_sub(self.capacity) || start.checked_add(reference.size).is_none_or(|end| end > self.written) {
            return Err(disk("invalid body frame range"));
        }
        let size = usize::try_from(reference.size).map_err(disk)?;
        let mut body = vec![0; size]; // reference.validate caps this at 2 MiB.
        let offset = start % self.capacity;
        let first = usize::try_from(reference.size.min(self.capacity - offset)).map_err(disk)?;
        self.bodies.read(offset, body.get_mut(..first).ok_or_else(|| disk("invalid body range"))?, cancel)?;
        if first < size {
            self.bodies.read(0, body.get_mut(first..).ok_or_else(|| disk("invalid body range"))?, cancel)?;
        }
        let batch = publication::decode_batch(connection, &reference, &body).map_err(disk)?;
        canceled(cancel)?;
        crate::cloud_traffic::reuse(false);
        self.popped += 1;
        Ok(Some((reference, batch)))
    }
}

use crate::cloud_records::replay::Cycle;

pub(super) struct Tail {
    index: Index,
    pub(super) boundary: Option<RecordRef>,
}
impl Tail {
    pub(super) fn scan(connection: &S3Connection, tip: RecordRef, boundary: Option<RecordRef>, cancel: &AtomicBool) -> Result<Self> {
        let mut index = Index::new()?;
        let mut seen = Cycle::default();
        let mut next = Some(tip);
        loop {
            canceled(cancel)?;
            let Some(reference) = next.take() else {
                if boundary.is_some() {
                    return Err(invalid("checkpoint boundary is absent from its history"));
                }
                return Ok(Self { index, boundary: None });
            };
            if boundary.as_ref().is_some_and(|boundary| boundary.hash == reference.hash) {
                if boundary.as_ref() != Some(&reference) {
                    return Err(AssetError::Integrity);
                }
                return Ok(Self { index, boundary });
            }
            seen.visit(&reference.hash).map_err(invalid)?;
            let (bytes, batch) = publication::read_batch_bytes(connection, &reference, cancel)?;
            index.push(&reference, &bytes, cancel)?;
            next = batch.parent;
        }
    }
    pub(super) fn count(&self) -> u64 {
        self.index.count
    }
    pub(super) fn retained(&self) -> u64 {
        self.index.retained()
    }
    pub(super) fn next(&mut self, connection: &S3Connection, cancel: &AtomicBool) -> Result<Option<(RecordRef, Batch)>> {
        let Some((reference, batch)) = self.index.pop(connection, cancel)? else { return Ok(None) };
        if batch.parent != self.boundary {
            return Err(AssetError::Integrity);
        }
        self.boundary = Some(reference.clone());
        Ok(Some((reference, batch)))
    }
}

#[cfg(test)]
mod tests;
