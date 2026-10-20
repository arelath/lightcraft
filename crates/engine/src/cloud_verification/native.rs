use super::*;
use crate::assets::{s3::S3Connection, sha256};
use lightcraft_catalog::replication::SyncState;
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

fn disk(e: std::io::Error) -> AssetError {
    AssetError::Disk(e.to_string())
}
pub(crate) struct Run {
    path: PathBuf,
    _lock: File,
    schedule: Schedule,
    selected: bool,
    now: u64,
}
impl Run {
    pub(crate) fn open(dir: &Path, connection: &S3Connection, local: &SyncState, now: u64, explicit: bool, automatic: bool) -> Result<Self> {
        let destination = sha256(format!("{}\0{}", crate::cloud_traffic::destination(connection), local.library).as_bytes()).0;
        let root = dir.join("remote-verification-v1");
        fs::create_dir_all(&root).map_err(disk)?;
        if !fs::symlink_metadata(&root).map_err(disk)?.file_type().is_dir() {
            return Err(AssetError::Disk("verification schedule directory was redirected".into()));
        }
        let path = root.join(format!("{destination}.json"));
        let lock_path = root.join(".lock");
        if fs::symlink_metadata(&lock_path).is_ok_and(|m| !m.file_type().is_file()) {
            return Err(AssetError::Disk("verification lock was redirected".into()));
        }
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(disk)?;
        lock.try_lock().map_err(|_| AssetError::Disk("another remote verification is still stopping; retry shortly".into()))?;
        let mut schedule = match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() && m.len() <= MAX_RECORD_BYTES as u64 => {
                let mut bytes = Vec::new();
                File::open(&path).map_err(disk)?.take(MAX_RECORD_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(disk)?;
                Schedule::decode(&bytes, &destination)?
            }
            Ok(_) => return Err(AssetError::Disk("verification schedule is oversized or redirected".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Schedule::new(destination, now),
            Err(e) => return Err(disk(e)),
        };
        if schedule.failed && !explicit {
            return Err(AssetError::Invalid(
                "Remote verification found damaged history. Restore the remote objects, then choose Verify remote history.".into(),
            ));
        }
        let selected = if explicit || automatic { schedule.reserve(now, explicit)? } else { false };
        let run = Self { path, _lock: lock, schedule, selected, now };
        // Reservation and initial weekly deadline survive failures and restarts.
        run.save()?;
        Ok(run)
    }
    fn save(&self) -> Result<()> {
        lightcraft_catalog::safe_file::write_atomic(&self.path, &self.schedule.encode()?).map_err(disk)
    }
    pub(crate) fn selected(&self) -> bool {
        self.selected
    }
    pub(crate) fn discovery(&mut self, manual: bool) -> Result<bool> {
        let selected = self.schedule.discover(self.now, manual);
        self.save()?;
        Ok(selected)
    }
    pub(crate) fn failed(&self) -> bool {
        self.schedule.failed
    }
    pub(crate) fn verify(&mut self, connection: &S3Connection, local: &SyncState, cancel: &AtomicBool) -> Result<serde_json::Value> {
        if !self.selected {
            return Ok(serde_json::json!({"verification":true,"complete":false,"deferred":true}));
        }
        let connection = connection.verification_connection();
        let _traffic = crate::cloud_traffic::Scope::purpose(crate::cloud_traffic::Purpose::Audit);
        let result = self.verify_inner(&connection, local, cancel);
        if result.as_ref().is_err_and(|e| matches!(e, AssetError::Integrity | AssetError::Missing | AssetError::Invalid(_) | AssetError::Blocked(_)))
        {
            self.schedule.failed = true;
            self.save()?;
        }
        result
    }
    fn verify_inner(&mut self, connection: &S3Connection, local: &SyncState, cancel: &AtomicBool) -> Result<serde_json::Value> {
        if local.protocol.is_head() {
            // Existing head reader verifies checkpoint, ancestry, archive receipts
            // and causal replay without publishing or acknowledging anything.
            let result = crate::cloud_replication::receive_only(connection, local, cancel);
            match result {
                Ok(_) => {
                    let mut cursor = self.schedule.cursor.clone();
                    cursor.complete = true;
                    self.schedule.completed_slice(cursor, self.now);
                }
                Err(AssetError::HistoryBudget) => {}
                Err(e) => return Err(e),
            }
            self.save()?;
            return Ok(serde_json::json!({"verification":true,"complete":self.schedule.cursor.complete}));
        }
        let root = crate::cloud_append::root(&connection.prefix, &local.library)?;
        let mut planner = crate::cloud_append::Planner::new(local)?;
        let expected = expected(local)?;
        planner.scan.audit = self.schedule.cursor.clone();
        let mut audit = crate::cloud_append::discovery::Audit::new(&planner.scan, false)?;
        let attempt = (|| -> Result<()> {
            crate::cloud_append::native::ensure_readable(connection, &local.library, cancel)?;
            let key = crate::cloud_append::manifest_key(&connection.prefix, &local.library)?;
            let manifest = connection.get_small(&key, 16 << 10, cancel)?.ok_or(AssetError::Missing)?;
            if manifest.bytes != crate::cloud_append::manifest(&local.library)? {
                return Err(AssetError::Integrity);
            }
            for namespace in ["checkpoints", "ops", "receipts"] {
                let Some(mut request) = audit.request(&root, namespace) else { continue };
                let mut tokens = std::collections::BTreeSet::new();
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(AssetError::Cancelled);
                    }
                    if !audit.start_page()? {
                        break;
                    }
                    let page = connection.list_objects_v2(&request, cancel)?;
                    if !page.prefixes.is_empty() {
                        return Err(AssetError::Integrity);
                    }
                    for key in page.keys {
                        if !audit.available() {
                            return Ok(());
                        }
                        let relative = crate::cloud_append::discovery::relative(&root, &key)?;
                        listing_gap(&expected, namespace, audit.cursor().after.as_deref(), Some(relative))?;
                        let size = if namespace == "checkpoints" {
                            crate::cloud_append::native::read_checkpoint(connection, &mut planner, &root, &key, cancel)?
                        } else if namespace == "ops" {
                            crate::cloud_append::native::read_operation(connection, &mut planner, &key, cancel)?
                        } else {
                            verify_receipt(connection, &key, cancel)?
                        };
                        audit.verified(&root, &key, size)?;
                        // Persist exact successful progress even if the next request,
                        // save, cancellation or process exit interrupts this slice.
                        self.schedule.completed_slice(audit.cursor().clone(), self.now);
                        self.save()?;
                    }
                    let Some(next) = page.next else {
                        listing_gap(&expected, namespace, audit.cursor().after.as_deref(), None)?;
                        if namespace == "receipts" {
                            audit.complete();
                        }
                        break;
                    };
                    if !audit.available() {
                        return Ok(());
                    }
                    if !tokens.insert(next.clone()) {
                        return Err(AssetError::Integrity);
                    }
                    request.continuation = Some(next);
                    request.start_after = None;
                }
            }
            Ok(())
        })();
        audit.save(&mut planner.scan);
        self.schedule.completed_slice(planner.scan.audit, self.now);
        self.save()?;
        match attempt {
            Ok(()) | Err(AssetError::HistoryBudget) => Ok(serde_json::json!({"verification":true,"complete":self.schedule.cursor.complete})),
            Err(e) => Err(e),
        }
    }
}

fn verify_receipt(connection: &S3Connection, key: &str, cancel: &AtomicBool) -> Result<usize> {
    use lightcraft_catalog::replication::archive::{MAX_NODE_BYTES, Node, Value};
    let hash = key.rsplit('/').next().and_then(|s| s.strip_suffix(".json")).ok_or(AssetError::Integrity)?;
    let reply = connection.history_record(key, MAX_NODE_BYTES, cancel, |bytes| Node::decode(hash, bytes).map_err(AssetError::from))?;
    if let Value::Branch { left, right } = &reply.value.value {
        for hash in [left, right] {
            let key = crate::cloud_history::key(&connection.prefix, &reply.value.library, hash)?;
            connection.history_record(&key, MAX_NODE_BYTES, cancel, |bytes| Node::decode(hash, bytes).map_err(AssetError::from))?;
        }
    }
    Ok(reply.size)
}
