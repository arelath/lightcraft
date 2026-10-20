//! Explicit live acceptance for the adapter and native synchronization routing.
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
    /// The ordinary synchronizer stages its own records. Admit only verified,
    /// content-addressed metadata under this test's fresh random prefix to cleanup.
    fn track_sync_outputs(&mut self, library: &str) -> Result<()> {
        for version in self.c.library_versions(&self.cancel)? {
            if self.keys.contains(&version.key) {
                continue;
            }
            let reply = self.c.get_small(&version.key, 64 << 20, &self.cancel)?.ok_or(AssetError::Missing)?;
            let hash = sha256(&reply.bytes);
            let reference = RecordRef { key: version.key.clone(), size: reply.bytes.len() as u64, hash: hash.clone() };
            if version.key == format!("{}/catalog/authority/{}.json", self.c.prefix, hash.0) {
                let head = Head::decode(&reply.bytes).map_err(AssetError::Blocked)?;
                require(head.library.as_deref() == Some(library), "unfamiliar sync head library")?;
                validate_head(&self.c, &head)?;
                self.heads.insert(version.key.clone());
            } else if reference.validate_scope(&self.c.prefix, "changes", 2 << 20).is_ok() {
                let batch: Batch = serde_json::from_slice(&reply.bytes).map_err(|_| invalid("test batch encoding"))?;
                batch.validate_at(&self.c.prefix, library, &reference).map_err(AssetError::Invalid)?;
            } else if reference.validate_scope(&self.c.prefix, "checkpoints", 64 << 20).is_ok() {
                let checkpoint: Checkpoint = serde_json::from_slice(&reply.bytes).map_err(|_| invalid("test checkpoint encoding"))?;
                require(checkpoint.state.library == library, "unfamiliar sync checkpoint library")?;
                checkpoint.state.validate()?;
            } else if reference.validate_scope(&self.c.prefix, "legacy", 64 << 20).is_ok_and(|epoch| epoch == 1) {
                LegacyIntake::decode(&reply.bytes, library).map_err(AssetError::Blocked)?;
            } else if version.key.starts_with(&format!("{}/sync-v2/{library}/control/", self.c.prefix))
                || version.key.starts_with(&format!("{}/sync-v2/{library}/receipts/", self.c.prefix))
            {
                crate::cloud_append::immutable_metadata(&version.key, &reply.bytes)?;
            } else {
                return Err(invalid("unfamiliar sync output; cleanup refused"));
            }
            self.keys.insert(version.key);
        }
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
    live_recovery(false, false);
}

#[test]
#[ignore = "requires explicit disposable B2 bucket/profile; modifies and cleans only a fresh random probe prefix"]
fn live_native_sync_admits_legacy_before_ack_and_routes_rebase() {
    live_recovery(true, false);
}

#[test]
#[ignore = "requires explicit disposable B2 bucket/profile; modifies and cleans only a fresh random probe prefix"]
fn live_native_append_upgrade_preserves_pending_and_rejoins() {
    live_recovery(true, true);
}

fn live_recovery(ordinary_sync: bool, upgrade: bool) {
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
            .enqueue(vec![Change { entity: format!("library:{library}"), field: "label.Blue".into(), base: None, value: json!("Before") }])?
            .ok_or_else(|| invalid("test source mutation"))?;
        let first_bytes = crate::cloud_append::encode(&first)?;
        probe.append(crate::cloud_append::manifest_key(&probe.c.prefix, &library)?, &crate::cloud_append::manifest(&library)?)?;
        probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &first)?, &first_bytes)?;
        if upgrade {
            probe.keys.insert(format!("{}/catalog/b2-authority.json", probe.c.prefix));
            return native_upgrade(&mut probe, &library, &mut source, &first);
        }
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
            .enqueue(vec![Change { entity: format!("library:{library}"), field: "label.Blue".into(), base: None, value: json!("After") }])?
            .ok_or_else(|| invalid("late test mutation"))?;
        probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &late)?, &crate::cloud_append::encode(&late)?)?;
        let mut offline = SyncState::new(library.clone(), "offline-author".into(), probe.c.id.clone())?;
        offline.merge(&frozen)?;
        let offline_op = offline
            .enqueue(vec![Change { entity: format!("library:{library}"), field: "label.Blue".into(), base: None, value: json!("Offline") }])?
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
        if ordinary_sync {
            native_sync(&mut probe, &library, &first, &late, &offline_op)?;
            probe.track_sync_outputs(&library)?;
            committed = probe.c.b2_read_head(&probe.cancel)?.0;
        }
        let (restart, expected, _) = probe.c.b2_read_head(&probe.cancel)?;
        let cp = restart.checkpoint.as_ref().ok_or_else(|| invalid("recovered checkpoint missing"))?;
        let restarted_state = crate::cloud_replication::read_checkpoint(&probe.c, cp, &probe.cancel)?.state;
        if !ordinary_sync {
            require(restarted_state == recovered.state, "restart lost recovered causal data")?;
        }
        let again = crate::cloud_append::native::receive_legacy(&probe.c, &restart, &restarted_state, false, &probe.cancel)?;
        require(again.state == restarted_state, "late legacy operation applied twice")?;
        // An old key can arrive behind a certified author's tail cursor. The
        // periodic audit must still detect its conflicting original identity.
        let mut fork = first.clone();
        fork.changes.first_mut().ok_or_else(|| invalid("test fork change"))?.value = json!("Fork");
        probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &fork)?, &crate::cloud_append::encode(&fork)?)?;
        require(
            matches!(
                crate::cloud_replication::synchronize_with_audit(&probe.c, &reader(&probe.c, &library)?, false, true, &probe.cancel),
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
            key: object_key(&probe.c.prefix, committed.control().map_err(AssetError::Invalid)?.epoch, "changes", &"0".repeat(64)),
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
    let tracked = if ordinary_sync { probe.track_sync_outputs(&format!("cr43-{token}")) } else { Ok(()) };
    let cleanup = probe.cleanup(&before);
    cleanup.unwrap();
    tracked.unwrap();
    result.unwrap();
}

fn reader(c: &S3Connection, library: &str) -> Result<SyncState> {
    let mut reader = SyncState::new(library.into(), "upgraded-device".into(), c.id.clone())?;
    reader.require_existing = true;
    Ok(reader)
}

fn native_sync(
    probe: &mut Probe,
    library: &str,
    first: &lightcraft_catalog::replication::Mutation,
    late: &lightcraft_catalog::replication::Mutation,
    offline: &lightcraft_catalog::replication::Mutation,
) -> Result<()> {
    // A physically later mutation has not been admitted by the manual adapter
    // setup above. The real sync loop must commit it before returning any ACK.
    let mut old = SyncState::new(library.into(), late.revision.device.clone(), probe.c.id.clone())?;
    old.receive(first)?;
    old.receive(late)?;
    let delayed = old
        .enqueue(vec![Change { entity: format!("library:{library}"), field: "label.Blue".into(), base: None, value: json!("Physically delayed") }])?
        .ok_or_else(|| invalid("delayed native sync mutation"))?;
    probe.append(crate::cloud_append::mutation_key(&probe.c.prefix, &delayed)?, &crate::cloud_append::encode(&delayed)?)?;
    let c = &probe.c;
    let cancel = &probe.cancel;
    let mut local = reader(c, library)?;
    let outcome = crate::cloud_replication::synchronize_with_audit(c, &local, false, true, cancel)
        .map_err(|error| invalid(&format!("legacy admission: {error}")))?;
    for mutation in [first, late, offline, &delayed] {
        require(
            crate::cloud_history::native::published(
                c,
                outcome.state(),
                &mutation.revision.id(),
                &mutation.digest()?,
                &mut Default::default(),
                cancel,
            )?,
            "native sync lost an original legacy identity",
        )?;
    }
    // Certification may attach fetched receipt proofs, so compare identities
    // rather than encodings: everything returned must already be committed.
    let committed = crate::cloud_replication::read(c, library, cancel)?.state.portable();
    let mut witness = Default::default();
    for (id, receipt) in &outcome.state().received {
        if !crate::cloud_history::native::published(c, &committed, id, &receipt.digest, &mut witness, cancel)? {
            let (returned, stored) =
                (serde_json::to_value(outcome.state()).unwrap_or_default(), serde_json::to_value(&committed).unwrap_or_default());
            let fields: Vec<_> =
                returned.as_object().into_iter().flatten().filter(|(k, v)| stored.get(k.as_str()) != Some(v)).map(|(k, _)| k).collect();
            return Err(invalid(&format!("sync returned uncommitted legacy data: {id}; differing fields {fields:?}")));
        }
    }
    for id in outcome.acknowledged() {
        require(
            committed.received.contains_key(id) || lightcraft_catalog::replication::archive::covers(&committed.archives, id),
            "ACK precedes commit",
        )?;
    }
    local.merge(outcome.state())?;
    let position = outcome.head_position().ok_or_else(|| invalid("native sync has no head position"))?;
    local.acknowledge(outcome.acknowledged(), position.cursor().into(), position.generation())?;
    let own = local
        .enqueue(vec![Change { entity: format!("library:{library}"), field: "label.Blue".into(), base: None, value: json!("Native publication") }])?
        .ok_or_else(|| invalid("native publication mutation"))?;
    let published =
        crate::cloud_replication::synchronize(c, &local, false, cancel).map_err(|error| invalid(&format!("ordinary publication: {error}")))?;
    require(published.acknowledged().contains(&own.revision.id()), "native B2 commit was not acknowledged")?;
    require(local.pending.get(&own.revision.id()) == Some(&own), "transport modified local outbox")?;
    let rebase = crate::cloud_upgrade::run(
        c,
        &local,
        "sync-probe",
        crate::cloud_upgrade::Request::Upgrade { plan: Plan::Rebase, writers_stopped: false },
        cancel,
    )
    .map_err(|failure| invalid(&format!("rebase: {}", failure.message)))?;
    require(rebase.state().received.contains_key(&own.revision.id()), "B2 rebase lost ordinary publication")?;
    let (head, _, _) = c.b2_read_head(cancel)?;
    require(head.control().map_err(AssetError::Invalid)?.epoch == 3, "native B2 rebase did not activate next epoch")?;
    require(c.get_small(&format!("{}/catalog/head.json", c.prefix), 16 << 10, cancel)?.is_none(), "native sync recreated raw S3 authority")?;
    Ok(())
}

fn native_upgrade(probe: &mut Probe, library: &str, source: &mut SyncState, first: &lightcraft_catalog::replication::Mutation) -> Result<()> {
    source.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
    source.require_existing = true;
    let pending = source
        .enqueue(vec![Change {
            entity: format!("library:{library}"),
            field: "label.Blue".into(),
            base: None,
            value: json!("Unpublished local edit"),
        }])?
        .ok_or_else(|| invalid("pending upgrade mutation"))?;
    let before_pending = source.pending.clone();
    let c = &probe.c;
    let cancel = &probe.cancel;
    // Two devices start the same upgrade at once. Exactly one initialization and
    // lease may win; the other must refuse with a retryable state, then rejoin.
    let owners = ["append-upgrade-probe", "competing-upgrader"];
    let shared: &SyncState = source;
    let raced = std::thread::scope(|s| {
        let handles: Vec<_> = owners
            .iter()
            .map(|owner| {
                s.spawn(move || crate::cloud_upgrade::run(c, shared, owner, crate::cloud_upgrade::Request::Auto { writers_stopped: false }, cancel))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().map_err(|_| invalid("upgrader thread panicked"))).collect::<Result<Vec<_>>>()
    })?;
    let mut outcomes = Vec::new();
    for (owner, result) in owners.iter().zip(raced) {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(failure) => {
                require(
                    // Lease and initialization races map to Contention only.
                    failure.kind == crate::cloud_status::FailureKind::Contention,
                    &format!("competing upgrader refused with {:?}, not contention: {}", failure.kind, failure.message),
                )?;
                crate::cloud_upgrade::run(c, shared, owner, crate::cloud_upgrade::Request::Auto { writers_stopped: false }, cancel)
                    .map_err(|failure| invalid(&format!("competing upgrader retry: {}", failure.message)))?
            }
        };
        outcomes.push(outcome);
    }
    let upgraded = outcomes.pop().ok_or_else(|| invalid("no upgrade outcome"))?;
    require(outcomes.iter().all(|other| other.state() == upgraded.state()), "competing upgraders committed different states")?;
    require(
        c.b2_read_head(cancel)?.0.control().map_err(AssetError::Invalid)?.epoch == 2,
        "competing upgraders activated more than one successor epoch",
    )?;
    require(upgraded.protocol().is_head(), "upgrade did not return a head outcome")?;
    require(upgraded.acknowledged().contains(&first.revision.id()), "committed source identity was not certified")?;
    require(!upgraded.state().received.contains_key(&pending.revision.id()), "upgrade seeded unpublished local work")?;
    require(source.pending == before_pending, "upgrade rewrote the old outbox")?;
    let control = crate::cloud_append::native::library_control(c, library, cancel)?;
    require(control.control_version == 3 && control.state == crate::cloud_control::State::Retired, "upgrade did not fence old append writers")?;
    let (head, expected, _) = c.b2_read_head(cancel)?;
    require(head.control().map_err(AssetError::Invalid)?.epoch == 2, "append upgrade did not activate epoch two")?;
    let versions = c.library_versions(cancel)?;
    let rejoined = crate::cloud_upgrade::run(c, source, "other-device", crate::cloud_upgrade::Request::Auto { writers_stopped: false }, cancel)
        .map_err(|failure| invalid(&format!("append rejoin: {}", failure.message)))?;
    require(rejoined.state() == upgraded.state(), "rejoin changed committed state")?;
    require(c.b2_read_head(cancel)?.1 == expected && c.library_versions(cancel)? == versions, "rejoin published or restarted the upgrade")?;
    let mut local = source.clone();
    local.protocol = upgraded.protocol();
    local.append = None;
    local.migrated_from_head = false;
    local.merge(upgraded.state())?;
    let position = upgraded.head_position().ok_or_else(|| invalid("upgrade position"))?;
    local.acknowledge(upgraded.acknowledged(), position.cursor().into(), position.generation())?;
    require(local.pending.get(&pending.revision.id()) == Some(&pending), "protocol conversion lost pending work")?;
    let published = crate::cloud_replication::synchronize(c, &local, false, cancel)?;
    require(published.acknowledged().contains(&pending.revision.id()), "new head did not publish preserved pending identity")?;
    require(
        c.get_small(&crate::cloud_append::mutation_key(&c.prefix, &pending)?, 2 << 20, cancel)?.is_none(),
        "upgraded writer continued appending to the retired root",
    )?;
    require(c.get_small(&format!("{}/catalog/head.json", c.prefix), 16 << 10, cancel)?.is_none(), "upgrade created a raw S3 head")?;
    let found = crate::cloud_setup::probe(c, cancel).map_err(|why| invalid(&format!("upgraded discovery: {why}")))?;
    require(
        found.head_taken && found.libraries.len() == 1 && found.libraries.first().is_some_and(|item| item.id == library && item.protocol.is_head()),
        "discovery offered the retired append source",
    )?;
    // A prefix-wide authority belongs only to its selected source. Existing
    // unrelated append libraries must remain readable and publishable.
    let other_library = format!("other-{library}");
    let mut other = SyncState::new(other_library.clone(), "other-append-device".into(), c.id.clone())?;
    other.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
    let operation = other
        .enqueue(vec![Change { entity: format!("library:{other_library}"), field: "label.Blue".into(), base: None, value: json!("Other library") }])?
        .ok_or_else(|| invalid("other append operation"))?;
    probe.append(crate::cloud_append::manifest_key(&probe.c.prefix, &other_library)?, &crate::cloud_append::manifest(&other_library)?)?;
    probe.keys.insert(crate::cloud_append::mutation_key(&probe.c.prefix, &operation)?);
    let outcome = crate::cloud_append::native::synchronize(&probe.c, &other, false, &probe.cancel)?;
    require(outcome.acknowledged().contains(&operation.revision.id()), "unrelated append library was incorrectly fenced")?;
    let found = crate::cloud_setup::probe(&probe.c, &probe.cancel).map_err(|why| invalid(&format!("multiple-library discovery: {why}")))?;
    require(
        found.libraries.len() == 2 && found.libraries.iter().any(|item| item.id == other_library && !item.protocol.is_head()),
        "discovery hid the unrelated append library",
    )?;
    // Remove only the exact second test library before dedicated-prefix erase.
    let other_root = format!("{}/sync-v2/{other_library}/", probe.c.prefix);
    for version in probe.c.library_versions(&probe.cancel)?.into_iter().filter(|v| v.key.starts_with(&other_root)) {
        require(probe.keys.contains(&version.key), "unfamiliar other-library cleanup key")?;
        probe.c.erase_version(&version, &probe.cancel)?;
    }
    native_management(&probe.c, library, &probe.cancel)
}

fn native_management(c: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<()> {
    let root = std::env::temp_dir().join(format!("lightcraft-cr43-management-{}", crate::cloud_control::token().map_err(AssetError::Invalid)?));
    let result = (|| -> Result<()> {
        let mut session = crate::Session::new().with_fs();
        session.open_library(&root, false).map_err(|e| invalid(&e.to_string()))?;
        let mut execute = |id, params: &Value| session.execute(id, params).map_err(|e| invalid(&format!("{id}: {e}")));
        execute("cloud.configure", &serde_json::to_value(c).map_err(|_| invalid("test connection encoding"))?)?;
        execute("cloud.syncEnable", &json!({"connection":c.id,"library":library,"protocol":"conditional-head-v1"}))?;
        execute("cloud.sync", &json!({"wait":true}))?;
        execute("cloud.pause", &json!({}))?;
        let retention = execute("cloud.reclaim", &json!({"connection":c.id,"wait":true}))?;
        require(
            retention["applySupported"] == false && retention["candidates"].as_array().is_some_and(Vec::is_empty),
            "B2 retention preview lost conservative provider limits",
        )?;
        let review = execute("cloud.deleteLibrary", &json!({"library":library,"wait":true}))?;
        let erased = execute(
            "cloud.deleteLibrary",
            &json!({"library":library,"wait":true,"apply":true,"preview":review["preview"],"confirmation":"DELETE CLOUD LIBRARY","allDevicesStopped":true}),
        )?;
        require(
            erased["applied"] == true && erased["deletionMarkers"] == 4 && erased["deletedVersions"].as_u64().is_some_and(|n| n > 0),
            "B2 whole-library erasure did not retain exactly the nonpersonal barriers",
        )?;
        let status = execute("cloud.overview", &json!({}))?;
        require(status["erasure"]["complete"] == true, "native deletion completion was not durable")?;
        require(c.library_versions(cancel)?.len() == 4, "B2 personal history remains after erasure")?;
        require(
            matches!(crate::cloud_publication::read_head(c, cancel), Err(AssetError::Blocked(crate::cloud_control::Blocked::Deleted))),
            "ordinary reader accepted a deleted B2 authority",
        )?;
        Ok(())
    })();
    if root.exists() {
        std::fs::remove_dir_all(root).map_err(|e| AssetError::Disk(e.to_string()))?;
    }
    result
}
