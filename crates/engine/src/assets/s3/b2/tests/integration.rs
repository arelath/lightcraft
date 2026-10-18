//! Explicit live acceptance for the adapter, separate from ordinary sync routing.
use super::*;
use crate::cloud_control::{Boundary, Control, Plan};
use crate::cloud_records::{Batch, Checkpoint, CheckpointRef, Head, LegacyIntake, object_key};
use lightcraft_catalog::replication::{Change, SyncState};
use std::collections::BTreeSet;

struct Probe {
    c: S3Connection,
    cancel: AtomicBool,
    keys: BTreeSet<String>,
    heads: BTreeSet<String>,
}
impl Probe {
    fn catalog(&mut self, epoch: u64, kind: &str, value: &impl Serialize) -> Result<RecordRef> {
        let bytes = serde_json::to_vec(value).map_err(|_| invalid("test encoding"))?;
        let hash = sha256(&bytes);
        let key = object_key(&self.c.prefix, epoch, kind, &hash.0);
        self.keys.insert(key.clone());
        self.c.b2_put_catalog_immutable(&key, &bytes, &self.cancel)?;
        Ok(RecordRef { key, size: bytes.len() as u64, hash })
    }
    fn append(&mut self, key: String, bytes: &[u8]) -> Result<()> {
        self.keys.insert(key.clone());
        self.c.put_immutable_verified(&key, bytes, &self.cancel)
    }
    fn track_head(&mut self, head: &Head) -> Result<()> {
        let hash = sha256(&serde_json::to_vec(head).map_err(|_| invalid("test head"))?);
        let key = format!("{}/catalog/authority/{}.json", self.c.prefix, hash.0);
        self.keys.insert(key.clone());
        self.heads.insert(key);
        Ok(())
    }
    fn cleanup(&self, before: &Value) -> Result<()> {
        let current = self.c.b2_read_authority(&self.cancel)?;
        if let Some(head) = current.head() {
            if !self.heads.contains(&head.key) {
                return Err(invalid("unfamiliar test authority; cleanup refused"));
            }
            let mut info = current.info.clone();
            info.remove(&current.entry);
            update(&current, &info, &current.auth, &self.cancel)?;
        }
        let confirmed = self.c.b2_read_authority(&self.cancel)?;
        if confirmed.head().is_some() {
            return Err(invalid("test authority cleanup not confirmed"));
        }
        let versions = self.c.library_versions(&self.cancel)?;
        if versions.iter().any(|v| !self.keys.contains(&v.key)) {
            return Err(invalid("unfamiliar test object; cleanup refused"));
        }
        for version in versions {
            self.c.erase_version(&version, &self.cancel)?;
        }
        if !self.c.library_versions(&self.cancel)?.is_empty() {
            return Err(invalid("test objects remain"));
        }
        let after = bucket(&self.c, &confirmed.auth, &self.cancel)?;
        for property in [
            "bucketInfo",
            "bucketType",
            "corsRules",
            "lifecycleRules",
            "defaultServerSideEncryption",
            "fileLockConfiguration",
            "replicationConfiguration",
        ] {
            if before.get(property) != after.get(property) {
                return Err(invalid("unrelated live bucket data changed"));
            }
        }
        Ok(())
    }
}
fn marker(parent: Option<RecordRef>) -> Batch {
    Batch { version: 3, parent, imports: vec![], mutations: vec![], checkpoint_marker: true }
}
fn require(condition: bool, why: &str) -> Result<()> {
    if condition { Ok(()) } else { Err(invalid(why)) }
}

#[test]
#[ignore = "requires explicit disposable B2 bucket/profile; modifies and cleans only a fresh random probe prefix"]
fn live_adapter_recovers_late_append_input_and_preserves_terminal_barrier() {
    let bucket_name = std::env::var("LIGHTCRAFT_B2_TEST_BUCKET").expect("explicit disposable bucket");
    let profile = std::env::var("LIGHTCRAFT_B2_TEST_PROFILE").expect("explicit saved profile");
    let endpoint = std::env::var("LIGHTCRAFT_B2_TEST_ENDPOINT").expect("explicit B2 endpoint");
    let region = crate::cloud_setup::b2_endpoint(&endpoint).unwrap().1.to_owned();
    let token = crate::cloud_control::token().unwrap();
    let c = serde_json::from_value(json!({"id":"b2-upgrade-test","bucket":bucket_name,"region":region,"profile":profile,"endpoint":endpoint,"prefix":format!("library/cr43-head-adapter-{token}")})).unwrap();
    let mut probe = Probe { c, cancel: AtomicBool::new(false), keys: BTreeSet::new(), heads: BTreeSet::new() };
    let observed = probe.c.b2_read_authority(&probe.cancel).unwrap();
    assert!(observed.head().is_none());
    observed.require_writable().unwrap();
    let before = bucket(&probe.c, &observed.auth, &probe.cancel).unwrap();
    let result = (|| -> Result<()> {
        let library = format!("cr43-{token}");
        let mut source = SyncState::new(library.clone(), "old-device".into(), probe.c.id.clone())?;
        let first = source
            .enqueue(vec![Change { entity: format!("library:{library}"), field: "name".into(), base: None, value: json!("Before") }])?
            .ok_or_else(|| invalid("test source mutation"))?;
        let first_bytes = crate::cloud_append::encode(&first)?;
        probe.append(crate::cloud_append::manifest_key(&probe.c.prefix, &library)?, &crate::cloud_append::manifest(&library)?)?;
        probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &first)?, &first_bytes)?;
        let frozen = source.portable();
        let source_marker = probe.catalog(1, "changes", &marker(None))?;
        let cp = probe.catalog(
            1,
            "checkpoints",
            &Checkpoint { version: 1, generation: 1, covers: source_marker.clone(), state: frozen.clone(), imports: Default::default() },
        )?;
        let intake = LegacyIntake::new(&library, Default::default()).map_err(AssetError::Blocked)?;
        let intake_ref = probe.catalog(1, "legacy", &intake)?;
        let control = Control::initial(&library)
            .begin("adapter-test", Plan::Rebase, Boundary { tip: source_marker.hash.0.clone(), generation: 1 }, "owner", &token, 0)
            .map_err(AssetError::Blocked)?
            .begin_output(&token, 1)
            .map_err(AssetError::Blocked)?;
        let maintenance = probe.catalog(1, "changes", &marker(Some(source_marker.clone())))?;
        let mut head =
            Head::next(None, maintenance, &library, Some(CheckpointRef { object: cp, covers: source_marker, generation: 1 }), control.clone())
                .map_err(AssetError::Invalid)?;
        head.generation = 2;
        head.version = 5;
        head.legacy_append = Some(intake_ref);
        probe.track_head(&head)?;
        probe.keys.insert(format!("{}/catalog/b2-authority.json", probe.c.prefix));
        // A readable newer source must refuse the upgrade before installing
        // either authority marker or pointer. Remove only this known test key
        // afterwards to restore the original implicit source control.
        let mut future = Control::initial_append(&library);
        future.control_version = 2;
        future.schema = 2;
        future.minimum_writer = 2;
        let future_key = crate::cloud_append::control::key(&probe.c.prefix, &future)?;
        probe.append(future_key.clone(), &crate::cloud_append::control::encode(&future)?)?;
        require(
            matches!(probe.c.b2_initialize_head(&head, &probe.cancel), Err(AssetError::Blocked(crate::cloud_control::Blocked::UpdateToWrite(_)))),
            "newer source writer was not refused",
        )?;
        require(
            !probe.c.b2_head_enabled(&probe.cancel)? && probe.c.b2_read_authority(&probe.cancel)?.head().is_none(),
            "refused source installed authority",
        )?;
        for version in probe.c.library_versions(&probe.cancel)?.into_iter().filter(|v| v.key == future_key) {
            probe.c.erase_version(&version, &probe.cancel)?;
        }
        probe.c.b2_initialize_head(&head, &probe.cancel)?;
        let (_, expected, _) = probe.c.b2_read_head(&probe.cancel)?;
        let fence = Control::initial_append(&library).retire_append_source(&intake.manifest).map_err(AssetError::Blocked)?;
        probe.append(crate::cloud_append::control::key(&probe.c.prefix, &fence)?, &crate::cloud_append::control::encode(&fence)?)?;
        let first2 = probe.catalog(2, "changes", &marker(None))?;
        let cp2 = probe.catalog(
            2,
            "checkpoints",
            &Checkpoint { version: 1, generation: 3, covers: first2.clone(), state: frozen.clone(), imports: Default::default() },
        )?;
        let active_tip = probe.catalog(2, "changes", &marker(Some(first2.clone())))?;
        let mut active = Head::next(
            Some(&head),
            active_tip,
            &library,
            Some(CheckpointRef { object: cp2, covers: first2, generation: 3 }),
            control.activate(&token).map_err(AssetError::Blocked)?,
        )
        .map_err(AssetError::Invalid)?;
        active.generation = 4;
        probe.track_head(&active)?;
        probe.c.b2_write_head(&expected, &active, &probe.cancel)?;
        let late = source
            .enqueue(vec![Change { entity: format!("library:{library}"), field: "name".into(), base: None, value: json!("After") }])?
            .ok_or_else(|| invalid("late test mutation"))?;
        probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &late)?, &crate::cloud_append::encode(&late)?)?;
        let mut offline = SyncState::new(library.clone(), "offline-author".into(), probe.c.id.clone())?;
        offline.merge(&frozen)?;
        let offline_op = offline
            .enqueue(vec![Change { entity: format!("library:{library}"), field: "name".into(), base: None, value: json!("Offline") }])?
            .ok_or_else(|| invalid("offline test mutation"))?;
        probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &offline_op)?, &crate::cloud_append::encode(&offline_op)?)?;
        let recovered = crate::cloud_append::native::receive_legacy(&probe.c, &active, &frozen, true, &probe.cancel)?;
        let digest = late.digest()?;
        require(
            recovered.state.received.get(&late.revision.id()).is_some_and(|r| r.digest == digest),
            "late source mutation was not recovered with its exact identity",
        )?;
        let offline_digest = offline_op.digest()?;
        require(
            recovered.state.received.get(&offline_op.revision.id()).is_some_and(|r| r.digest == offline_digest),
            "late new author was not discovered",
        )?;
        require(source.pending.get(&late.revision.id()) == Some(&late), "legacy receive modified the old outbox")?;
        let intake_ref = probe.catalog(1, "legacy", &recovered.intake)?;
        let cp3 = probe.catalog(
            2,
            "checkpoints",
            &Checkpoint {
                version: 1,
                generation: active.generation,
                covers: active.tip.clone(),
                state: recovered.state.clone(),
                imports: Default::default(),
            },
        )?;
        let tip = probe.catalog(2, "changes", &marker(Some(active.tip.clone())))?;
        let (_, expected, _) = probe.c.b2_read_head(&probe.cancel)?;
        let mut committed = Head::next(
            Some(&active),
            tip,
            &library,
            Some(CheckpointRef { object: cp3, covers: active.tip.clone(), generation: active.generation }),
            active.control().map_err(AssetError::Invalid)?,
        )
        .map_err(AssetError::Invalid)?;
        committed.legacy_append = Some(intake_ref);
        probe.track_head(&committed)?;
        probe.c.b2_write_head(&expected, &committed, &probe.cancel)?;
        let (restart, expected, _) = probe.c.b2_read_head(&probe.cancel)?;
        let cp = restart.checkpoint.as_ref().ok_or_else(|| invalid("recovered checkpoint missing"))?;
        require(
            crate::cloud_replication::read_checkpoint(&probe.c, cp, &probe.cancel)?.state == recovered.state,
            "restart lost recovered causal data",
        )?;
        let again = crate::cloud_append::native::receive_legacy(&probe.c, &restart, &recovered.state, false, &probe.cancel)?;
        require(again.state == recovered.state, "late legacy operation applied twice")?;
        // An old key can arrive behind a certified author's tail cursor. The
        // periodic audit must still detect its conflicting original identity.
        let mut fork = first.clone();
        fork.changes.first_mut().ok_or_else(|| invalid("test fork change"))?.value = json!("Fork");
        probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &fork)?, &crate::cloud_append::encode(&fork)?)?;
        require(
            matches!(
                crate::cloud_append::native::receive_legacy(&probe.c, &restart, &recovered.state, true, &probe.cancel),
                Err(AssetError::Invalid(_) | AssetError::Integrity)
            ),
            "historical audit ignored an old-author fork behind its cursor",
        )?;
        let mut deleted = committed.clone();
        deleted.version = 4;
        deleted.generation = 1;
        deleted.control = Some(committed.control().map_err(AssetError::Invalid)?.delete().map_err(AssetError::Blocked)?);
        deleted.checkpoint = None;
        deleted.legacy_append = None;
        deleted.tip = RecordRef {
            key: object_key(&probe.c.prefix, 2, "changes", &"0".repeat(64)),
            hash: lightcraft_catalog::assets::BlobId("0".repeat(64)),
            size: 1,
        };
        probe.track_head(&deleted)?;
        probe.c.b2_write_head(&expected, &deleted, &probe.cancel)?;
        let (_, terminal, _) = probe.c.b2_read_head(&probe.cancel)?;
        require(
            matches!(probe.c.b2_write_head(&terminal, &committed, &probe.cancel), Err(AssetError::Blocked(crate::cloud_control::Blocked::Deleted))),
            "deleted authority revived",
        )?;
        Ok(())
    })();
    let cleanup = probe.cleanup(&before);
    cleanup.unwrap();
    result.unwrap();
}
