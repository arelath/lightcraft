use super::{Provider, metadata};
use crate::backend::Backend;
use lightcraft_engine::{
    assets::sha256,
    catalog::replication::SyncState,
    cloud_append as core,
    cloud_status::{CloudFailure, FailureKind},
    cloud_verification::{DEFERRED, MAX_RECORD_BYTES, Schedule},
};
use serde_json::{Value, json};
pub(super) struct Run {
    _lock: super::cache::storage::Lock,
    backend: Backend,
    path: String,
    schedule: Schedule,
    pub selected: bool,
    now: u64,
}
impl Run {
    pub async fn open(
        backend: Backend,
        provider: &Provider,
        local: &SyncState,
        now: u64,
        explicit: bool,
        automatic: bool,
    ) -> Result<Self, CloudFailure> {
        let lock = super::cache::storage::verification_lock().await.map_err(CloudFailure::local)?;
        let destination = sha256(format!("{}\0{}", metadata::destination(&provider.connection), local.library).as_bytes()).0;
        let path = format!("cloud/remote-verification-v1/{destination}.json");
        let mut schedule = match backend.read_limited(&path, MAX_RECORD_BYTES as u64).await? {
            Some(bytes) => Schedule::decode(&bytes, &destination).map_err(CloudFailure::from)?,
            None => Schedule::new(destination, now),
        };
        if schedule.failed && !explicit {
            return Err(CloudFailure::integrity(
                "Remote verification found damaged history. Restore the remote objects, then choose Verify remote history.",
            ));
        }
        let selected = if explicit || automatic { schedule.reserve(now, explicit).map_err(CloudFailure::from)? } else { false };
        let run = Self { _lock: lock, backend, path, schedule, selected, now };
        run.save().await?;
        Ok(run)
    }
    async fn save(&self) -> Result<(), CloudFailure> {
        self.backend.write(&self.path, &self.schedule.encode().map_err(CloudFailure::from)?).await.map_err(CloudFailure::local)
    }
    pub async fn discovery(&mut self, manual: bool) -> Result<bool, CloudFailure> {
        let selected = self.schedule.discover(self.now, manual);
        self.save().await?;
        Ok(selected)
    }
    pub fn failed(&self) -> bool {
        self.schedule.failed
    }
    pub async fn verify(&mut self, provider: &Provider, local: &SyncState) -> Result<Value, CloudFailure> {
        if !self.selected {
            return Ok(json!({"verification":true,"complete":false,"deferred":true}));
        }
        let provider = provider.verification_provider();
        let result = self.verify_inner(&provider, local).await;
        if result.as_ref().is_err_and(|e| matches!(e.kind, FailureKind::Integrity | FailureKind::InvalidHistory)) {
            self.schedule.failed = true;
            self.save().await?;
        }
        result
    }
    async fn verify_inner(&mut self, provider: &Provider, local: &SyncState) -> Result<Value, CloudFailure> {
        if local.protocol.is_head() {
            let result = super::protocol::read(provider, &local.library, false, &mut Default::default()).await;
            match result {
                Ok(_) => {
                    let mut cursor = self.schedule.cursor.clone();
                    cursor.complete = true;
                    self.schedule.completed_slice(cursor, self.now);
                }
                Err(e) if e.message == DEFERRED => {}
                Err(e) => return Err(e),
            }
            self.save().await?;
            return Ok(json!({"verification":true,"complete":self.schedule.cursor.complete}));
        }
        let root = core::root(&provider.connection.prefix, &local.library).map_err(CloudFailure::from)?;
        let mut planner = core::Planner::new(local).map_err(CloudFailure::from)?;
        let expected = lightcraft_engine::cloud_verification::expected(local).map_err(CloudFailure::from)?;
        planner.scan.audit = self.schedule.cursor.clone();
        let mut audit = core::discovery::Audit::new(&planner.scan, false).map_err(CloudFailure::from)?;
        let result = async {
            super::append::library_control(provider, &local.library).await?.may_read()?;
            let key = core::manifest_key(&provider.connection.prefix, &local.library).map_err(CloudFailure::from)?;
            let manifest = provider.request("GET", &key, None, None, None, 16 << 10).await?;
            if manifest.status != 200 || manifest.bytes != core::manifest(&local.library).map_err(CloudFailure::from)? {
                return Err(CloudFailure::integrity("append manifest unavailable or changed"));
            }
            for namespace in ["checkpoints", "ops", "receipts"] {
                let Some(mut request) = audit.request(&root, namespace) else { continue };
                let mut tokens = std::collections::BTreeSet::new();
                loop {
                    if !audit.start_page().map_err(CloudFailure::from)? {
                        break;
                    }
                    let page = provider.list(&request).await?;
                    if !page.prefixes.is_empty() {
                        return Err(CloudFailure::integrity("unexpected verification prefixes"));
                    }
                    for key in page.keys {
                        if !audit.available() {
                            return Ok(());
                        }
                        let relative = core::discovery::relative(&root, &key).map_err(CloudFailure::from)?;
                        lightcraft_engine::cloud_verification::listing_gap(&expected, namespace, audit.cursor().after.as_deref(), Some(relative))
                            .map_err(CloudFailure::from)?;
                        let size = if namespace == "checkpoints" {
                            super::append::read_checkpoint(provider, &mut planner, &root, &key).await?
                        } else if namespace == "ops" {
                            super::append::read_operation(provider, &mut planner, &key).await?
                        } else {
                            verify_receipt(provider, &key).await?
                        };
                        audit.verified(&root, &key, size).map_err(CloudFailure::from)?;
                        self.schedule.completed_slice(audit.cursor().clone(), self.now);
                        self.save().await?;
                    }
                    let Some(next) = page.next else {
                        lightcraft_engine::cloud_verification::listing_gap(&expected, namespace, audit.cursor().after.as_deref(), None)
                            .map_err(CloudFailure::from)?;
                        if namespace == "receipts" {
                            audit.complete();
                        }
                        break;
                    };
                    if !audit.available() {
                        return Ok(());
                    }
                    if !tokens.insert(next.clone()) {
                        return Err(CloudFailure::integrity("verification listing repeated a cursor"));
                    }
                    request.continuation = Some(next);
                    request.start_after = None;
                }
            }
            Ok(())
        }
        .await;
        audit.save(&mut planner.scan);
        self.schedule.completed_slice(planner.scan.audit, self.now);
        self.save().await?;
        match result {
            Ok(()) => {}
            Err(e) if e.message == DEFERRED => {}
            Err(e) => return Err(e),
        }
        Ok(json!({"verification":true,"complete":self.schedule.cursor.complete}))
    }
}

async fn verify_receipt(provider: &Provider, key: &str) -> Result<usize, CloudFailure> {
    use lightcraft_engine::catalog::replication::archive::{MAX_NODE_BYTES, Node, Value};
    let hash = key.rsplit('/').next().and_then(|s| s.strip_suffix(".json")).ok_or("invalid receipt object key")?;
    let (node, size) = provider.history_record(key, MAX_NODE_BYTES, |bytes| Node::decode(hash, bytes).map_err(CloudFailure::from)).await?;
    if let Value::Branch { left, right } = &node.value {
        for hash in [left, right] {
            let key = lightcraft_engine::cloud_history::key(&provider.connection.prefix, &node.library, hash).map_err(CloudFailure::from)?;
            provider.history_record(&key, MAX_NODE_BYTES, |bytes| Node::decode(hash, bytes).map_err(CloudFailure::from)).await?;
        }
    }
    Ok(size)
}
