//! Disposable pure-remote state, anchored to the catalog's durable head position.
//! A cache write cannot acknowledge anything: a crash before the owner saves the
//! matching cursor makes the new entry unreachable, and a miss keeps full replay.
use super::*;
use crate::cloud_outcome::{HeadPosition, Outcome};
use crate::cloud_records::CheckpointRef;
use lightcraft_catalog::replication::{Portable, SyncState};
use serde::{Deserialize, Serialize};

const MAX_BASELINE: usize = 64 << 20;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Baseline {
    version: u32,
    destination: String,
    cursor: String,
    generation: u64,
    checkpoint: Option<CheckpointRef>,
    remote: Portable,
}
pub(crate) struct HeadBaseline {
    pub state: SyncState,
    pub checkpoint: Option<CheckpointRef>,
}
fn identity(connection: &S3Connection, library: &str, position: &HeadPosition) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"lightcraft-certified-head-baseline-v1\0");
    let destination = crate::cloud_traffic::destination(connection);
    for value in [destination.as_str(), library, position.cursor()] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    hash.update(position.generation().to_le_bytes());
    hash.finalize().into()
}

impl S3Connection {
    pub(crate) fn head_baseline(&self, local: &SyncState, cancel: &AtomicBool) -> Result<Option<HeadBaseline>> {
        cancelled(cancel)?;
        if self.fresh_history() || !local.protocol.is_head() {
            return Ok(None);
        }
        let (Some(cache), Some(cursor)) = (&self.metadata.cache, &local.cursor) else { return Ok(None) };
        let Ok(position) = HeadPosition::new(cursor.clone(), local.generation) else { return Ok(None) };
        let id = identity(self, &local.library, &position);
        let bytes = match cache.load(&id, MAX_BASELINE, cancel) {
            Ok(Some(bytes)) => bytes,
            Err(AssetError::Cancelled) => return Err(AssetError::Cancelled),
            Ok(None) | Err(_) => return Ok(None),
        };
        let checked = || -> Result<HeadBaseline> {
            let baseline: Baseline = serde_json::from_slice(&bytes).map_err(|_| AssetError::Integrity)?;
            if baseline.version != 1
                || baseline.destination != crate::cloud_traffic::destination(self)
                || baseline.cursor != *cursor
                || baseline.generation != local.generation
                || baseline.remote.library != local.library
                || !baseline.remote.proofs.is_empty()
            {
                return Err(AssetError::Integrity);
            }
            let mut state = SyncState::restore_durable(&baseline.remote, "remote".into(), self.id.clone()).map_err(AssetError::from)?;
            state.cursor = Some(cursor.clone());
            state.generation = local.generation;
            Ok(HeadBaseline { state, checkpoint: baseline.checkpoint })
        };
        match checked() {
            Ok(state) => {
                cancelled(cancel)?;
                crate::cloud_traffic::reuse(false);
                Ok(Some(state))
            }
            Err(_) => {
                if let Err(AssetError::Cancelled) = cache.forget(&id, cancel) {
                    return Err(AssetError::Cancelled);
                }
                Ok(None)
            }
        }
    }
    pub(crate) fn remember_head_baseline(&self, outcome: &Outcome, checkpoint: Option<CheckpointRef>, cancel: &AtomicBool) -> Result<()> {
        cancelled(cancel)?;
        // Polling an unchanged cursor does not rewrite a large cache body.
        if matches!(outcome, Outcome::Unchanged { .. }) || self.fresh_history() {
            return Ok(());
        }
        let (Some(cache), Some(position)) = (&self.metadata.cache, outcome.head_position()) else { return Ok(()) };
        let mut remote = outcome.state().clone();
        remote.proofs.clear();
        let baseline = Baseline {
            version: 1,
            destination: crate::cloud_traffic::destination(self),
            cursor: position.cursor().to_owned(),
            generation: position.generation(),
            checkpoint,
            remote,
        };
        let Ok(bytes) = serde_json::to_vec(&baseline) else { return Ok(()) };
        if bytes.len() <= MAX_BASELINE
            && let Err(AssetError::Cancelled) = cache.save(&identity(self, &baseline.remote.library, position), &bytes, cancel)
        {
            return Err(AssetError::Cancelled);
        }
        // Eviction, full disks and damaged cache storage only remove an optimization.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::replication::Change;
    use serde_json::json;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 32];
            getrandom::fill(&mut nonce).unwrap();
            Self(std::env::temp_dir().join(format!("lightcraft-baseline-{}", crate::assets::sha256(&nonce).0)))
        }
        fn connection(&self) -> S3Connection {
            serde_json::from_value::<S3Connection>(json!({"id":"cloud","bucket":"photos","region":"us-east-1","prefix":"library"}))
                .unwrap()
                .with_metadata_cache(Some(Arc::new(Cache::new(self.0.clone()))))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn state() -> SyncState {
        let mut state = SyncState::new("library".into(), "device".into(), "cloud".into()).unwrap();
        state.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(1) }]).unwrap();
        state.acknowledge(&state.pending.keys().cloned().collect(), "a".repeat(64), 1).unwrap();
        state
    }
    fn outcome(state: &SyncState) -> Outcome {
        Outcome::head(state.portable(), HeadPosition::new(state.cursor.clone().unwrap(), state.generation).unwrap(), Default::default())
    }
    #[test]
    fn restart_uses_only_the_destination_and_durable_cursor_bound_remote_state() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(false);
        let remote = state();
        fixture.connection().remember_head_baseline(&outcome(&remote), None, &cancel).unwrap();
        let mut local = remote.clone();
        let base = local.winning("photo:one", "rating").map(|(stamp, _)| stamp);
        local.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base, value: json!(5) }]).unwrap();
        let snapshot = local.clone();
        let baseline = fixture.connection().head_baseline(&local, &cancel).unwrap().unwrap();
        assert!(baseline.state.pending.is_empty());
        assert_eq!(baseline.state.winning("photo:one", "rating").unwrap().1, &json!(1));
        assert_eq!(local, snapshot, "loading remote proof cannot acknowledge the outbox");
        for field in ["id", "bucket", "region", "prefix", "endpoint"] {
            let connection = fixture.connection();
            let mut value = serde_json::to_value(&connection).unwrap();
            value[field] = json!(match field {
                "region" => "us-west-1",
                "endpoint" => "http://localhost:9001",
                _ => "other",
            });
            let connection: S3Connection = serde_json::from_value(value).unwrap();
            let connection = connection.with_metadata_cache(Some(Arc::new(Cache::new(fixture.0.clone()))));
            assert!(connection.head_baseline(&local, &cancel).unwrap().is_none(), "{field}");
        }
        local.cursor = Some("b".repeat(64));
        assert!(fixture.connection().head_baseline(&local, &cancel).unwrap().is_none());
        local.cursor = remote.cursor.clone();
        local.generation += 1;
        assert!(fixture.connection().head_baseline(&local, &cancel).unwrap().is_none());
        assert!(fixture.connection().fresh_metadata().head_baseline(&remote, &cancel).unwrap().is_none());
    }
    #[test]
    fn cache_ahead_of_failed_ack_is_unreachable_and_damage_or_loss_falls_back() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(false);
        let local = state();
        let snapshot = local.clone();
        let mut newer = local.clone();
        newer.cursor = Some("b".repeat(64));
        newer.generation = 2;
        fixture.connection().remember_head_baseline(&outcome(&newer), None, &cancel).unwrap();
        assert!(fixture.connection().head_baseline(&local, &cancel).unwrap().is_none());
        assert_eq!(local, snapshot);
        let connection = fixture.connection();
        connection.remember_head_baseline(&outcome(&local), None, &cancel).unwrap();
        let position = HeadPosition::new(local.cursor.clone().unwrap(), local.generation).unwrap();
        let cache = connection.metadata.cache.as_ref().unwrap();
        let id = identity(&connection, &local.library, &position);
        // Even a correctly hashed cache envelope cannot substitute another library.
        let mut value: serde_json::Value = serde_json::from_slice(&cache.load(&id, MAX_BASELINE, &cancel).unwrap().unwrap()).unwrap();
        value["remote"]["library"] = json!("other");
        cache.save(&id, &serde_json::to_vec(&value).unwrap(), &cancel).unwrap();
        assert!(connection.head_baseline(&local, &cancel).unwrap().is_none());
        connection.remember_head_baseline(&outcome(&local), None, &cancel).unwrap();
        let filename = id.iter().map(|byte| format!("{byte:02x}")).collect::<String>() + ".body";
        std::fs::write(fixture.0.join(&filename), b"truncated").unwrap();
        assert!(fixture.connection().head_baseline(&local, &cancel).unwrap().is_none());
        assert!(!fixture.0.join(filename).exists(), "a damaged entry is discarded");
        assert!(fixture.connection().head_baseline(&local, &cancel).unwrap().is_none());
        assert!(matches!(connection.head_baseline(&local, &AtomicBool::new(true)), Err(AssetError::Cancelled)));
    }
}
