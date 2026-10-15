//! Disk-backed reverse traversal: retain references, never a decoded history tail.
//! Replay refetches and revalidates each immutable batch. Recovery checkpoints make
//! completed prefixes durable remotely; scratch files carry no trusted catalog state.
use super::*;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

const MAX_REFERENCE: usize = crate::cloud_records::replay::MAX_REFERENCE_BYTES;
const SLOT_BYTES: u64 = MAX_REFERENCE as u64 + 4;
#[cfg(test)]
const MAX_INDEX: u64 = RECOVERY_SEGMENT_BATCHES * SLOT_BYTES;
fn disk(error: impl std::fmt::Display) -> AssetError {
    AssetError::Disk(format!(
        "history replay scratch: {error}; free temporary disk space and retry synchronization; catalog and remote objects retained"
    ))
}
pub(super) struct Index {
    file: Option<File>,
    path: PathBuf,
    popped: u64,
    pub(super) count: u64,
}
impl Index {
    pub(super) fn new() -> Result<Self> {
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(disk)?;
        let path = std::env::temp_dir().join(format!("lightcraft-history-{}.refs", sha256(&nonce).0));
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).map_err(disk)?;
        Ok(Self { file: Some(file), path, popped: 0, count: 0 })
    }
    fn file(&mut self) -> Result<&mut File> {
        self.file.as_mut().ok_or_else(|| disk("scratch file closed"))
    }
    pub(super) fn push(&mut self, reference: &RecordRef) -> Result<()> {
        let bytes = serde_json::to_vec(reference).map_err(disk)?;
        if bytes.len() > MAX_REFERENCE {
            return Err(invalid("history replay reference exceeds scratch record budget"));
        }
        let slot = (self.count % RECOVERY_SEGMENT_BATCHES) * SLOT_BYTES;
        self.file()?.seek(SeekFrom::Start(slot)).map_err(disk)?;
        self.file()?.write_all(&bytes).map_err(disk)?;
        self.file()?.seek(SeekFrom::Start(slot + MAX_REFERENCE as u64)).map_err(disk)?;
        self.file()?.write_all(&u32::try_from(bytes.len()).map_err(disk)?.to_le_bytes()).map_err(disk)?;
        self.count = self.count.checked_add(1).ok_or_else(|| invalid("history length exhausted"))?;
        Ok(())
    }
    pub(super) fn pop(&mut self) -> Result<Option<RecordRef>> {
        if self.popped >= self.count.min(RECOVERY_SEGMENT_BATCHES) {
            return Ok(None);
        }
        let slot = ((self.count - self.popped - 1) % RECOVERY_SEGMENT_BATCHES) * SLOT_BYTES;
        self.file()?.seek(SeekFrom::Start(slot + MAX_REFERENCE as u64)).map_err(disk)?;
        let mut length = [0u8; 4];
        self.file()?.read_exact(&mut length).map_err(disk)?;
        let length = usize::try_from(u32::from_le_bytes(length)).map_err(disk)?;
        if length == 0 || length > MAX_REFERENCE {
            return Err(disk("invalid reference frame length"));
        }
        self.file()?.seek(SeekFrom::Start(slot)).map_err(disk)?;
        let mut bytes = vec![0; length];
        self.file()?.read_exact(&mut bytes).map_err(disk)?;
        let reference = serde_json::from_slice(&bytes).map_err(disk)?;
        self.popped += 1;
        Ok(Some(reference))
    }
}
impl Drop for Index {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
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
            // Overwrite newer references in a fixed ring while seeking the oldest
            // prefix. No decoded batch or growing cycle set survives an iteration.
            index.push(&reference)?;
            seen.visit(&reference.hash).map_err(invalid)?;
            let batch = publication::read_batch(connection, &reference, cancel)?;
            next = batch.parent;
        }
    }
    pub(super) fn count(&self) -> u64 {
        self.index.count
    }
    pub(super) fn next(&mut self, connection: &S3Connection, cancel: &AtomicBool) -> Result<Option<(RecordRef, Batch)>> {
        canceled(cancel)?;
        let Some(reference) = self.index.pop()? else { return Ok(None) };
        let batch = publication::read_batch(connection, &reference, cancel)?;
        if batch.parent != self.boundary {
            return Err(AssetError::Integrity);
        }
        self.boundary = Some(reference.clone());
        Ok(Some((reference, batch)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_keeps_the_oldest_recovery_window_with_constant_disk_and_cleans_up() {
        let mut index = Index::new().unwrap();
        let path = index.path.clone();
        for n in 0..100_000 {
            index
                .push(&RecordRef {
                    key: format!("library/catalog/changes/{n:064x}.json"),
                    hash: lightcraft_catalog::assets::BlobId(format!("{n:064x}")),
                    size: 100,
                })
                .unwrap();
        }
        assert_eq!(index.count, 100_000);
        assert_eq!(index.file().unwrap().metadata().unwrap().len(), MAX_INDEX);
        for n in (100_000 - RECOVERY_SEGMENT_BATCHES..100_000).rev() {
            assert_eq!(index.pop().unwrap().unwrap().hash.0, format!("{n:064x}"));
        }
        assert!(index.pop().unwrap().is_none());
        drop(index);
        assert!(!path.exists());
    }
    #[test]
    fn cycle_detector_is_bounded_and_rejects_self_and_longer_cycles() {
        for period in [1, 2, 17, 4097] {
            let mut detector = Cycle::default();
            let mut rejected = false;
            for n in 0..period * 4 {
                if detector.visit(&lightcraft_catalog::assets::BlobId(format!("{:064x}", n % period))).is_err() {
                    rejected = true;
                    break;
                }
            }
            assert!(rejected, "cycle length {period}");
        }
        let mut detector = Cycle::default();
        for n in 0..100_000 {
            detector.visit(&lightcraft_catalog::assets::BlobId(format!("{n:064x}"))).unwrap();
        }
    }
    #[test]
    fn invalid_scratch_length_returns_an_actionable_error() {
        let mut index = Index::new().unwrap();
        index.file().unwrap().seek(SeekFrom::Start(MAX_REFERENCE as u64)).unwrap();
        index.file().unwrap().write_all(&u32::MAX.to_le_bytes()).unwrap();
        index.count = 1;
        let error = index.pop().unwrap_err().to_string();
        assert!(error.contains("scratch") && error.contains("retained"));
    }
}
