//! Byte evidence belongs to the live publication coordinator. Recovery may reuse
//! a completed byte proof only after checking its exact immutable provider version.
use crate::assets::s3::S3Connection;
use crate::assets::{AssetError, Result};
use lightcraft_catalog::assets::{Blob, BlobId};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const MAX_EVIDENCE: usize = 4096;
type Key = (BlobId, String, Option<String>, u64);
/// Short-held readiness lock: never guards network, staging I/O, or owner replies.
#[derive(Default)]
pub(crate) struct SharedMedia(Mutex<Shared>);
#[derive(Default)]
struct Shared {
    revision: Option<u64>,
    destination: Option<S3Connection>,
    ready: BTreeSet<Key>,
    active: BTreeMap<Key, usize>,
    transferring: BTreeSet<Key>,
    transferred: BTreeSet<Key>,
}
impl SharedMedia {
    pub(crate) fn prepare(&self, revision: u64, connection: &S3Connection, pending: bool) {
        let mut shared = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if shared.revision.is_some_and(|saved| saved > revision) {
            return;
        }
        if shared.revision != Some(revision) || shared.destination.as_ref().is_none_or(|saved| !saved.same_destination(connection)) {
            *shared = Shared { revision: Some(revision), destination: Some(connection.storage_snapshot()), ..Shared::default() };
        } else if !pending && shared.active.is_empty() {
            shared.ready.clear();
            shared.transferred.clear();
        }
    }
    pub(crate) fn producer(self: &Arc<Self>, revision: u64, connection: &S3Connection, blobs: impl Iterator<Item = Blob>, fresh: bool) -> Producer {
        let keys: BTreeSet<_> = blobs.map(|blob| VerifiedMedia::key(&blob)).take(MAX_EVIDENCE).collect();
        let mut shared = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if shared.revision == Some(revision) && shared.destination.as_ref().is_some_and(|saved| saved.same_destination(connection)) {
            for key in &keys {
                if fresh {
                    shared.ready.remove(key);
                    shared.transferred.remove(key);
                }
                if shared.active.len() < MAX_EVIDENCE || shared.active.contains_key(key) {
                    let count = shared.active.entry(key.clone()).or_default();
                    *count = count.saturating_add(1);
                }
            }
        }
        Producer { shared: self.clone(), revision, destination: connection.storage_snapshot(), keys }
    }
    fn status(&self, revision: u64, connection: &S3Connection, key: &Key) -> (bool, bool) {
        let shared = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if shared.revision != Some(revision) || shared.destination.as_ref().is_none_or(|saved| !saved.same_destination(connection)) {
            return (false, false);
        }
        (shared.ready.contains(key), shared.active.contains_key(key))
    }
}
pub(crate) struct Producer {
    shared: Arc<SharedMedia>,
    revision: u64,
    destination: S3Connection,
    keys: BTreeSet<Key>,
}
impl Drop for Producer {
    fn drop(&mut self) {
        let mut shared = self.shared.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if shared.revision != Some(self.revision) || shared.destination.as_ref().is_none_or(|saved| !saved.same_destination(&self.destination)) {
            return;
        }
        for key in &self.keys {
            if let Some(count) = shared.active.get_mut(key) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    shared.active.remove(key);
                }
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct VerifiedMedia {
    destination: Option<S3Connection>,
    keys: BTreeSet<Key>,
    bytes: Option<Arc<AtomicU64>>,
    shared: Option<(Arc<SharedMedia>, u64)>,
    publication: bool,
    fresh: bool,
}
impl VerifiedMedia {
    pub(crate) fn coordinated(shared: Arc<SharedMedia>, revision: u64, bytes: Option<Arc<AtomicU64>>, fresh: bool) -> Self {
        Self { shared: Some((shared, revision)), bytes, fresh, ..Self::default() }
    }
    pub(crate) fn publication(&mut self) {
        self.publication = true;
        self.fresh = false;
    }
    /// Called only after all blob phase acknowledgements are durable on the owner.
    pub(crate) fn ready(&self, connection: &S3Connection, blobs: impl Iterator<Item = Blob>) {
        let Some((shared, revision)) = &self.shared else { return };
        let mut shared = shared.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if shared.revision != Some(*revision) || shared.destination.as_ref().is_none_or(|saved| !saved.same_destination(connection)) {
            return;
        }
        for blob in blobs {
            let key = Self::key(&blob);
            if self.contains(connection, &blob) && shared.ready.len() < MAX_EVIDENCE {
                shared.ready.insert(key);
            }
        }
    }
    /// Transfer-only reuse follows a saved per-blob owner phase. Publication still
    /// requires every import blob and its ObjectsVerified phase to be durable.
    pub(crate) fn transferred(&self, connection: &S3Connection, blob: &Blob) {
        let Some((shared, revision)) = &self.shared else { return };
        if !self.contains(connection, blob) {
            return;
        }
        let mut state = shared.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revision == Some(*revision)
            && state.destination.as_ref().is_some_and(|saved| saved.same_destination(connection))
            && state.transferred.len() < MAX_EVIDENCE
        {
            state.transferred.insert(Self::key(blob));
        }
    }
    fn key(blob: &Blob) -> Key {
        (blob.id.clone(), blob.remote.key.clone(), blob.remote.version.clone(), blob.size)
    }
    /// An opaque conclusive provider proof still needs exact-version admission.
    /// Explicit verification always reads remote bytes instead.
    pub(crate) fn admit_upload(
        &mut self,
        connection: &S3Connection,
        blob: &Blob,
        evidence: Option<crate::assets::s3::UploadEvidence>,
        cancel: &AtomicBool,
    ) -> Result<()> {
        if self.fresh || connection.fresh_history() {
            return Ok(());
        }
        if let Some(evidence) = evidence
            && evidence.certify(connection, blob, cancel)?
        {
            self.record(connection, blob);
        }
        Ok(())
    }
    pub fn contains(&self, connection: &S3Connection, blob: &Blob) -> bool {
        self.destination.as_ref().is_some_and(|saved| saved.same_destination(connection)) && self.keys.contains(&Self::key(blob))
    }
    fn record(&mut self, connection: &S3Connection, blob: &Blob) {
        if self.destination.as_ref().is_none_or(|saved| !saved.same_destination(connection)) {
            self.keys.clear();
            self.destination = Some(connection.storage_snapshot());
        }
        if self.keys.len() < MAX_EVIDENCE {
            self.keys.insert(Self::key(blob));
        }
    }
    pub fn verify(&mut self, connection: &S3Connection, blob: &Blob, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<bool> {
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        connection.validate_blob(blob)?;
        let _traffic = crate::cloud_traffic::Scope::purpose(crate::cloud_traffic::Purpose::UploadVerification);
        if self.contains(connection, blob) {
            crate::cloud_traffic::reuse(false);
            return Ok(true);
        }
        if let Some((shared, revision)) = &self.shared {
            let (ready, active) = shared.status(*revision, connection, &Self::key(blob));
            let transferred = if self.publication {
                false
            } else {
                let state = shared.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.revision == Some(*revision)
                    && state.destination.as_ref().is_some_and(|saved| saved.same_destination(connection))
                    && state.transferred.contains(&Self::key(blob))
            };
            if (ready || transferred) && !self.fresh {
                crate::cloud_traffic::reuse(true);
                self.record(connection, blob);
                return Ok(true);
            }
            // Transfer workers own these reads. Metadata waits for their durable readiness.
            if self.publication && active {
                return Ok(false);
            }
        }
        if !self.fresh
            && let Some(version) = connection.media_receipt(blob, cancel)?
        {
            match connection.stat_blob(blob, cancel) {
                Ok(Some(stat)) if stat.version.as_deref() == Some(version.as_str()) && stat.size == blob.size => {
                    crate::cloud_traffic::reuse(false);
                    self.record(connection, blob);
                    return Ok(true);
                }
                Ok(None) => return Ok(false),
                Ok(Some(_)) | Err(AssetError::Unsupported(_) | AssetError::Invalid(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let mut check = Check { hash: Sha256::new(), count: 0, expected: blob.size, exceeded: false };
        let mut previous = 0;
        let result = connection.download_observed(blob, &mut check, cancel, &mut |done| {
            if let Some(bytes) = &self.bytes {
                let delta = done.saturating_sub(previous);
                let _ = bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| Some(value.saturating_add(delta)));
            }
            previous = done;
            progress(done);
        });
        match result {
            Err(_) if check.exceeded => Err(AssetError::Integrity),
            Err(AssetError::Missing) => Ok(false),
            Err(reason) => Err(reason),
            Ok(version) if check.count == blob.size && format!("{:x}", check.hash.finalize()) == blob.id.0 => {
                if blob.remote.version.as_ref().zip(version.as_ref()).is_some_and(|(requested, actual)| requested != actual) {
                    return Err(AssetError::Integrity);
                }
                connection.remember_media_receipt(blob, version.as_deref(), crate::cloud_metadata::MediaVerification::StreamedSha256, cancel)?;
                self.record(connection, blob);
                Ok(true)
            }
            Ok(_) => Err(AssetError::Integrity),
        }
    }
}
struct Check {
    hash: Sha256,
    count: u64,
    expected: u64,
    exceeded: bool,
}
impl Write for Check {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(count) = self.count.checked_add(bytes.len() as u64) else {
            self.exceeded = true;
            return Err(std::io::Error::other("remote object length overflow"));
        };
        if count > self.expected {
            self.exceeded = true;
            return Err(std::io::Error::other("remote object exceeds its declared size"));
        }
        self.count = count;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serializes one complete check/upload/verification turn for identical blobs.
/// The short-held shared-state mutex never covers I/O or owner acknowledgement.
pub(crate) struct BlobPermit {
    shared: Arc<SharedMedia>,
    revision: u64,
    destination: S3Connection,
    key: Key,
}
impl Drop for BlobPermit {
    fn drop(&mut self) {
        let mut state = self.shared.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revision == Some(self.revision) && state.destination.as_ref().is_some_and(|saved| saved.same_destination(&self.destination)) {
            state.transferring.remove(&self.key);
        }
    }
}
impl VerifiedMedia {
    pub(crate) fn acquire_blob(&self, connection: &S3Connection, blob: &Blob, cancel: &AtomicBool) -> Result<Option<BlobPermit>> {
        let Some((shared, revision)) = &self.shared else { return Ok(None) };
        let key = Self::key(blob);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AssetError::Cancelled);
            }
            {
                let mut state = shared.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.revision != Some(*revision) || state.destination.as_ref().is_none_or(|saved| !saved.same_destination(connection)) {
                    return Err(AssetError::Invalid("media destination changed; staged data retained".into()));
                }
                if !state.transferring.contains(&key) {
                    if state.transferring.len() >= MAX_EVIDENCE {
                        return Err(AssetError::Capacity("too many concurrent cloud media transfers".into()));
                    }
                    state.transferring.insert(key.clone());
                    return Ok(Some(BlobPermit { shared: shared.clone(), revision: *revision, destination: connection.storage_snapshot(), key }));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::assets::RemoteLocation;
    #[test]
    fn cr31_coordinator_waits_for_producers_and_reuses_only_fresh_durable_readiness() {
        let connection = S3Connection {
            id: "personal".into(),
            bucket: "photos".into(),
            region: "us-east-1".into(),
            prefix: "library".into(),
            endpoint: Some("http://127.0.0.1:9".into()),
            profile: None,
            keys: Default::default(),
            metadata: Default::default(),
        };
        let blob = Blob {
            id: BlobId("a".repeat(64)),
            size: 3,
            remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
        };
        let shared = Arc::new(SharedMedia::default());
        shared.prepare(1, &connection, true);
        let producer = shared.producer(1, &connection, std::iter::once(blob.clone()), false);
        let mut publisher = VerifiedMedia::coordinated(shared.clone(), 1, None, false);
        publisher.publication();
        // An active transfer prevents any dependency GET, even on an unreachable endpoint.
        assert!(!publisher.verify(&connection, &blob, &AtomicBool::new(false), &mut |_| {}).unwrap());
        let mut transfer = VerifiedMedia::coordinated(shared.clone(), 1, None, false);
        transfer.record(&connection, &blob);
        transfer.transferred(&connection, &blob);
        let mut sibling_transfer = VerifiedMedia::coordinated(shared.clone(), 1, None, false);
        assert!(sibling_transfer.verify(&connection, &blob, &AtomicBool::new(false), &mut |_| {}).unwrap());
        assert!(!publisher.verify(&connection, &blob, &AtomicBool::new(false), &mut |_| {}).unwrap());
        transfer.ready(&connection, std::iter::once(blob.clone()));
        assert!(publisher.verify(&connection, &blob, &AtomicBool::new(false), &mut |_| {}).unwrap());
        drop(producer);
        let mut sibling = VerifiedMedia::coordinated(shared.clone(), 1, None, false);
        assert!(sibling.verify(&connection, &blob, &AtomicBool::new(false), &mut |_| {}).unwrap());
        // Explicit verification invalidates shared evidence; later metadata waits for its result.
        let audit = shared.producer(1, &connection, std::iter::once(blob.clone()), true);
        assert_eq!(shared.status(1, &connection, &VerifiedMedia::key(&blob)), (false, true));
        drop(audit);
        // A connection epoch change cannot be repopulated by an old completion.
        shared.prepare(2, &connection, true);
        transfer.ready(&connection, std::iter::once(blob.clone()));
        assert_eq!(shared.status(2, &connection, &VerifiedMedia::key(&blob)), (false, false));
        let mut fresh = VerifiedMedia::coordinated(shared.clone(), 2, None, false);
        fresh.record(&connection, &blob);
        fresh.ready(&connection, std::iter::once(blob.clone()));
        shared.prepare(2, &connection, false);
        assert_eq!(shared.status(2, &connection, &VerifiedMedia::key(&blob)), (false, false));
    }
    #[test]
    fn cr31_fresh_evidence_is_bound_to_destination_hash_size_and_version_and_remains_bounded() {
        let connection = S3Connection {
            id: "personal".into(),
            bucket: "photos".into(),
            region: "us-east-1".into(),
            prefix: "library".into(),
            endpoint: Some("http://127.0.0.1:9".into()),
            profile: None,
            keys: Default::default(),
            metadata: Default::default(),
        };
        let blob = Blob {
            id: BlobId("a".repeat(64)),
            size: 3,
            remote: RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
        };
        let mut evidence = VerifiedMedia::default();
        assert!(!evidence.contains(&connection, &blob));
        evidence.record(&connection, &blob);
        assert!(evidence.contains(&connection, &blob));
        let mut other = connection.clone();
        other.bucket = "other-photos".into();
        assert!(!evidence.contains(&other, &blob));
        for change in 0..3 {
            let mut changed = blob.clone();
            match change {
                0 => changed.size = 4,
                1 => changed.id = BlobId("b".repeat(64)),
                _ => changed.remote.version = Some("other-version".into()),
            };
            assert!(!evidence.contains(&connection, &changed));
        }
        for n in 0..MAX_EVIDENCE + 1 {
            let mut changed = blob.clone();
            changed.remote.key = format!("library/{n}");
            evidence.record(&connection, &changed);
        }
        assert_eq!(evidence.keys.len(), MAX_EVIDENCE);
        assert!(!evidence.contains(
            &connection,
            &Blob { remote: RemoteLocation { key: format!("library/{MAX_EVIDENCE}"), ..blob.remote.clone() }, ..blob.clone() }
        ));
        evidence.record(&other, &blob);
        assert!(evidence.contains(&other, &blob));
        assert!(!evidence.contains(&connection, &blob));
        assert!(matches!(evidence.verify(&other, &blob, &AtomicBool::new(true), &mut |_| {}), Err(AssetError::Cancelled)));
    }
}

#[cfg(test)]
mod blob_gate_tests {
    use super::*;
    #[test]
    fn identical_media_waits_with_cancellation_while_other_blobs_and_new_destinations_progress() {
        let c: S3Connection = serde_json::from_value(
            serde_json::json!({"id":"test","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:1"}),
        )
        .unwrap();
        let blob = Blob {
            id: crate::assets::sha256(b"blob"),
            size: 4,
            remote: lightcraft_catalog::assets::RemoteLocation { connection: "test".into(), key: "library/original".into(), version: None },
        };
        let shared = Arc::new(SharedMedia::default());
        shared.prepare(1, &c, true);
        let media = VerifiedMedia::coordinated(shared.clone(), 1, None, false);
        let first = media.acquire_blob(&c, &blob, &AtomicBool::new(false)).unwrap().unwrap();
        let mut another = blob.clone();
        another.remote.key.push_str("-different");
        let independent = media.acquire_blob(&c, &another, &AtomicBool::new(false)).unwrap().unwrap();
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let worker_media = VerifiedMedia::coordinated(shared.clone(), 1, None, false);
            let c = &c;
            let blob = &blob;
            let cancel = &cancel;
            let worker = scope.spawn(move || send.send(worker_media.acquire_blob(c, blob, cancel).err()).unwrap());
            assert!(matches!(receive.recv_timeout(std::time::Duration::from_millis(50)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)));
            cancel.store(true, Ordering::Relaxed);
            assert_eq!(receive.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), Some(AssetError::Cancelled));
            worker.join().unwrap();
        });
        shared.prepare(2, &c, true);
        let replacement =
            VerifiedMedia::coordinated(shared.clone(), 2, None, false).acquire_blob(&c, &blob, &AtomicBool::new(false)).unwrap().unwrap();
        drop(first);
        drop(independent);
        assert_eq!(shared.0.lock().unwrap().transferring.len(), 1, "old leases cannot release a replacement scope");
        drop(replacement);
        assert!(shared.0.lock().unwrap().transferring.is_empty());
    }
}
