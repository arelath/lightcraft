use super::*;
use crate::assets::s3::AccessKeys;
use crate::assets::s3::fixture as s3_fixture;
use crate::cloud_records::{Batch, Head, LegacyIntake};
use lightcraft_catalog::replication::{Change, SyncState};

struct Combined {
    native: Server,
    objects: s3_fixture::Fixture,
    c: S3Connection,
    fingerprint: String,
}
impl Combined {
    fn new() -> Self {
        let native = Server::new();
        native.state.lock().unwrap().bucket_name = Some("photos".into());
        let objects = s3_fixture::Fixture::new();
        let access = format!("TEST_{}", objects.endpoint.rsplit(':').next().unwrap());
        {
            let mut storage = objects.state.lock().unwrap();
            storage.allow_typed_keys = true;
            storage.append_mode = true;
            storage.additional_keys.insert(access.clone(), "TEST_SECRET".into());
        }
        let c: S3Connection = serde_json::from_value(
            json!({"id":"personal","bucket":"photos","region":"us-east-005","prefix":"library","endpoint":"https://s3.us-east-005.backblazeb2.com"}),
        )
        .unwrap();
        let c = c
            .with_keys(AccessKeys::new(&access, "TEST_SECRET").unwrap())
            .with_metadata_cache(Some(Arc::new(crate::cloud_metadata::Cache::new(objects.dir.join("metadata")))))
            .test_endpoint(&objects.endpoint);
        let fingerprint = sha256(&serde_json::to_vec(&(&access, "TEST_SECRET", &c.endpoint)).unwrap()).0;
        let mut auth = native.observed().auth;
        Arc::get_mut(&mut auth).unwrap().admission = c.admission_scope(&c.credentials().unwrap());
        AUTH_CACHE
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(fingerprint.clone(), Cached { expires: Instant::now() + Duration::from_secs(300), auth });
        let combined = Self { native, objects, c, fingerprint };
        combined.seed();
        combined
    }
    fn record(&self, epoch: u64, kind: &str, value: &impl Serialize) -> RecordRef {
        let bytes = serde_json::to_vec(value).unwrap();
        let hash = sha256(&bytes);
        let key = crate::cloud_records::object_key(&self.c.prefix, epoch, kind, &hash.0);
        self.objects.state.lock().unwrap().objects.insert(format!("/photos/{key}"), bytes.clone());
        RecordRef { key, size: bytes.len() as u64, hash }
    }
    fn seed(&self) {
        let mut writer = SyncState::new("library".into(), "writer".into(), "personal".into()).unwrap();
        writer.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(1) }]).unwrap();
        let tip = self.record(
            2,
            "changes",
            &Batch { version: 3, parent: None, imports: vec![], mutations: writer.pending.values().cloned().collect(), checkpoint_marker: false },
        );
        let intake = self.record(1, "legacy", &LegacyIntake::new("library", Default::default()).unwrap());
        let mut control = crate::cloud_control::Control::initial("library");
        control.epoch = 2;
        control.active_metadata_root = crate::cloud_control::epoch_root(2);
        let mut head = Head::next(None, tip, "library", None, control).unwrap();
        head.version = 5;
        head.legacy_append = Some(intake);
        let bytes = serde_json::to_vec(&head).unwrap();
        let hash = sha256(&bytes);
        let reference = RecordRef { key: format!("library/catalog/authority/{}.json", hash.0), size: bytes.len() as u64, hash };
        let scope = Scope::new(&self.c).unwrap();
        let descriptor = serde_json::to_vec(&json!({"version":1,"protocol":"b2-bucket-head-v1","binding":scope.binding})).unwrap();
        let mut storage = self.objects.state.lock().unwrap();
        storage.objects.insert(format!("/photos/{}", reference.key), bytes);
        storage.objects.insert("/photos/library/catalog/b2-authority.json".into(), descriptor);
        storage.objects.insert(
            format!("/photos/{}", crate::cloud_append::manifest_key("library", "library").unwrap()),
            crate::cloud_append::manifest("library").unwrap(),
        );
        self.native.state.lock().unwrap().info.insert(scope.entry.clone(), scope.pointer(&reference).unwrap());
    }
    fn sync(&self, local: &SyncState) -> crate::cloud_outcome::Outcome {
        crate::cloud_replication::synchronize(&self.c, local, false, &AtomicBool::new(false)).unwrap()
    }
    fn accepted(&self, outcome: &crate::cloud_outcome::Outcome) -> SyncState {
        let mut state = SyncState::restore_durable(outcome.state(), "reader".into(), self.c.id.clone()).unwrap();
        let position = outcome.head_position().unwrap();
        state.cursor = Some(position.cursor().to_owned());
        state.generation = position.generation();
        state
    }
    fn clear(&self) {
        self.native.state.lock().unwrap().requests = 0;
        let mut storage = self.objects.state.lock().unwrap();
        storage.requests.clear();
        storage.traffic.clear();
    }
    fn install_head(&self, head: &Head) {
        let bytes = serde_json::to_vec(head).unwrap();
        let hash = sha256(&bytes);
        let reference = RecordRef { key: format!("library/catalog/authority/{}.json", hash.0), size: bytes.len() as u64, hash };
        self.objects.state.lock().unwrap().objects.insert(format!("/photos/{}", reference.key), bytes);
        let scope = Scope::new(&self.c).unwrap();
        let mut bucket = self.native.state.lock().unwrap();
        bucket.info.insert(scope.entry.clone(), scope.pointer(&reference).unwrap());
        bucket.revision += 1;
    }
}
impl Drop for Combined {
    fn drop(&mut self) {
        AUTH_CACHE.get_or_init(Default::default).lock().unwrap().remove(&self.fingerprint);
    }
}
#[test]
fn unchanged_b2_pass_has_one_fresh_bucket_observation_and_no_immutable_body_gets() {
    let f = Combined::new();
    let local = SyncState::new("library".into(), "reader".into(), f.c.id.clone()).unwrap();
    let state = f.accepted(&f.sync(&local));
    for _ in 0..3 {
        f.clear();
        let result = f.sync(&state);
        assert!(matches!(result, crate::cloud_outcome::Outcome::Unchanged { .. }));
        assert_eq!(f.native.state.lock().unwrap().requests, 1);
        assert!(f.objects.state.lock().unwrap().requests.is_empty());
    }
    f.clear();
    f.c.verification_connection().b2_read_head(&AtomicBool::new(false)).unwrap();
    assert_eq!(f.native.state.lock().unwrap().requests, 1);
    assert_eq!(f.objects.state.lock().unwrap().requests.iter().filter(|(method, _)| method == "GET").count(), 4);
    assert_eq!(f.objects.state.lock().unwrap().signature_errors, 0);
    f.clear();
    crate::cloud_replication::receive_only(&f.c, &state, &AtomicBool::new(false)).unwrap();
    assert_eq!(f.native.state.lock().unwrap().requests, 1);
    assert_eq!(f.objects.state.lock().unwrap().requests.len(), 1, "warm receive-only reads just the history batch");
    f.clear();
    crate::cloud_replication::receive_only(&f.c.verification_connection(), &state, &AtomicBool::new(false)).unwrap();
    assert_eq!(f.native.state.lock().unwrap().requests, 1);
    assert_eq!(f.objects.state.lock().unwrap().requests.len(), 5, "fresh verification reads each immutable body once");
}
#[test]
fn b2_publication_reuses_the_observation_but_success_loss_and_conflict_all_require_a_new_one() {
    for mode in ["success", "lost", "conflict"] {
        let f = Combined::new();
        let cancel = AtomicBool::new(false);
        let c = f.c.authority_operation();
        let (head, token, _) = c.b2_read_head(&cancel).unwrap();
        c.clone().b2_read_head(&cancel).unwrap();
        assert_eq!(f.native.state.lock().unwrap().requests, 1);
        let tip = f.record(
            2,
            "changes",
            &Batch { version: 3, parent: Some(head.tip.clone()), imports: vec![], mutations: vec![], checkpoint_marker: true },
        );
        let next = Head::next(Some(&head), tip, "library", head.checkpoint.clone(), head.control().unwrap()).unwrap();
        {
            let mut state = f.native.state.lock().unwrap();
            state.lose_reply = mode == "lost";
            if mode == "conflict" {
                state.revision += 1;
                state.info.insert("external".into(), json!("preserve"));
            }
        }
        let result = c.b2_write_head(&token, &next, &cancel);
        match mode {
            "success" => assert!(result.is_ok()),
            "lost" => assert!(result.unwrap_err().uncertain_write()),
            _ => assert!(matches!(result, Err(AssetError::Conflict))),
        }
        assert_eq!(f.native.state.lock().unwrap().requests, 2, "one observed revision, one CAS");
        let (confirmed, _, _) = c.b2_read_head(&cancel).unwrap();
        assert_eq!(f.native.state.lock().unwrap().requests, 3, "the previous observation is not confirmation");
        assert_eq!(confirmed.generation, if mode == "conflict" { head.generation } else { next.generation });
        c.authority_operation().b2_read_head(&cancel).unwrap();
        assert_eq!(f.native.state.lock().unwrap().requests, 4, "independent operations always refresh");
        assert!(f.native.state.lock().unwrap().info.contains_key("keep"));
        if mode == "conflict" {
            assert_eq!(f.native.state.lock().unwrap().info["external"], json!("preserve"));
        }
        assert_eq!(f.objects.state.lock().unwrap().signature_errors, 0);
    }
}
#[test]
fn scheduled_legacy_discovery_receives_late_input_and_still_detects_a_new_old_sequence_fork() {
    let f = Combined::new();
    let cancel = AtomicBool::new(false);
    let local = SyncState::new("library".into(), "reader".into(), f.c.id.clone()).unwrap();
    let local = f.accepted(&f.sync(&local));
    let mut writer = SyncState::new("library".into(), "late-writer".into(), f.c.id.clone()).unwrap();
    writer.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
    writer.enqueue(vec![Change { entity: "photo:one".into(), field: "meta.caption".into(), base: None, value: json!("late input") }]).unwrap();
    let mut fork = writer.pending.values().next().unwrap().clone();
    crate::cloud_append::native::synchronize(&f.c, &writer, false, &cancel).unwrap();
    f.clear();
    assert!(matches!(f.sync(&local), crate::cloud_outcome::Outcome::Unchanged { .. }));
    assert!(f.objects.state.lock().unwrap().requests.is_empty(), "an unscheduled poll performs no legacy listings");
    let due = f.c.clone().with_history_discovery(true);
    let received = crate::cloud_replication::synchronize(&due, &local, false, &cancel).unwrap();
    let mut local = f.accepted(&received);
    assert_eq!(local.winning("photo:one", "meta.caption").unwrap().1, &json!("late input"));
    assert!(f.objects.state.lock().unwrap().requests.iter().any(|(_, key)| key.contains("list-type")));
    fork.changes[0].value = json!("conflicting late input");
    let key = crate::cloud_append::mutation_key(&f.c.prefix, &fork).unwrap();
    f.objects.state.lock().unwrap().objects.insert(format!("/photos/{key}"), crate::cloud_append::encode(&fork).unwrap());
    let base = local.winning("photo:one", "rating").map(|(stamp, _)| stamp);
    local.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base, value: json!(4) }]).unwrap();
    let pending = local.pending.clone();
    let writes = f.native.state.lock().unwrap().writes;
    let result = crate::cloud_replication::synchronize(&due, &local, false, &cancel);
    assert!(result.is_err(), "new conflicting keys behind a known frontier must be rejected");
    assert_eq!(f.native.state.lock().unwrap().writes, writes);
    assert_eq!(local.pending, pending);
    assert_eq!(f.objects.state.lock().unwrap().signature_errors, 0);
}

#[test]
fn b2_immutable_cache_survives_restart_repairs_damage_and_fresh_reads_detect_remote_damage() {
    let f = Combined::new();
    let cancel = AtomicBool::new(false);
    let (_, _, reference) = f.c.b2_read_head(&cancel).unwrap();
    let root = f.objects.dir.join("metadata");
    for file in std::fs::read_dir(&root).unwrap() {
        let path = file.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "body") {
            std::fs::write(path, b"damaged disposable cache").unwrap();
        }
    }
    f.clear();
    f.c.b2_read_head(&cancel).unwrap();
    assert_eq!(f.objects.state.lock().unwrap().requests.len(), 4, "damaged bodies require verified remote reads");
    let restarted = f.c.clone().with_metadata_cache(Some(Arc::new(crate::cloud_metadata::Cache::new(root)))).test_endpoint(&f.objects.endpoint);
    f.clear();
    restarted.b2_read_head(&cancel).unwrap();
    assert_eq!(f.native.state.lock().unwrap().requests, 1);
    assert!(f.objects.state.lock().unwrap().requests.is_empty(), "immutable bodies survive a new cache instance");
    f.objects.state.lock().unwrap().objects.insert(format!("/photos/{}", reference.key), b"damaged remote head".to_vec());
    f.clear();
    restarted.b2_read_head(&cancel).unwrap();
    assert!(f.objects.state.lock().unwrap().requests.is_empty(), "ordinary known-body reuse deliberately delays remote damage detection");
    assert!(restarted.verification_connection().b2_read_head(&cancel).is_err());
    assert!(f.objects.state.lock().unwrap().requests.iter().any(|(_, key)| key == &format!("/photos/{}", reference.key)));
    let mut other = restarted;
    other.id = "another-destination-binding".into();
    assert!(other.b2_read_head(&cancel).is_err(), "another destination binding cannot borrow the old cached proof");
}

#[test]
fn a_new_bucket_observation_preserves_newer_writer_and_deleted_fences_despite_cached_active_heads() {
    let f = Combined::new();
    let cancel = AtomicBool::new(false);
    let local = SyncState::new("library".into(), "reader".into(), f.c.id.clone()).unwrap();
    let mut local = f.accepted(&f.sync(&local));
    let base = local.winning("photo:one", "rating").map(|(stamp, _)| stamp);
    local.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base, value: json!(4) }]).unwrap();
    let pending = local.pending.clone();
    let (mut head, _, _) = f.c.b2_read_head(&cancel).unwrap();
    let control = head.control.as_mut().unwrap();
    control.schema = 2;
    control.minimum_writer = 2;
    f.install_head(&head);
    let outcome = f.sync(&local);
    assert!(matches!(outcome.publication(), crate::cloud_outcome::Publication::Blocked(crate::cloud_control::Blocked::UpdateToWrite(_))));
    assert!(outcome.acknowledged().is_empty());
    assert_eq!(f.native.state.lock().unwrap().writes, 0);
    // A separate original active authority models a supported writer's deletion.
    let erased = Combined::new();
    let state = SyncState::new("library".into(), "reader".into(), erased.c.id.clone()).unwrap();
    let mut state = erased.accepted(&erased.sync(&state));
    let base = state.winning("photo:one", "rating").map(|(stamp, _)| stamp);
    state.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base, value: json!(4) }]).unwrap();
    let erased_pending = state.pending.clone();
    let (mut head, _, _) = erased.c.b2_read_head(&cancel).unwrap();
    head.control = Some(head.control().unwrap().delete().unwrap());
    head.generation = 1;
    head.checkpoint = None;
    head.legacy_append = None;
    head.tip = RecordRef {
        key: crate::cloud_records::object_key("library", 2, "changes", &"0".repeat(64)),
        hash: lightcraft_catalog::assets::BlobId("0".repeat(64)),
        size: 1,
    };
    head.validate_deleted("library", "library").unwrap();
    erased.install_head(&head);
    let result = crate::cloud_replication::synchronize(&erased.c, &state, false, &cancel);
    assert!(
        matches!(&result, Err(AssetError::Blocked(crate::cloud_control::Blocked::Deleted))),
        "terminal authority must retain Deleted, got {:?}",
        result.as_ref().err()
    );
    assert_eq!(local.pending, pending);
    assert_eq!(state.pending, erased_pending);
    assert_eq!(f.native.state.lock().unwrap().writes, 0);
    assert_eq!(erased.native.state.lock().unwrap().writes, 0);
}
#[test]
fn incomplete_legacy_discovery_continues_without_starting_another_round_at_the_next_poll() {
    for after in [None, Some(format!("ops/old-author/00000000000000000001-{}.json", "a".repeat(64)))] {
        let f = Combined::new();
        let cancel = AtomicBool::new(false);
        let state = SyncState::new("library".into(), "reader".into(), f.c.id.clone()).unwrap();
        let local = f.accepted(&f.sync(&state));
        let (mut head, _, _) = f.c.b2_read_head(&cancel).unwrap();
        let mut intake = f.c.b2_legacy_intake(&head, &cancel).unwrap();
        intake.scan.audit.round = 1;
        intake.scan.audit.complete = false;
        intake.scan.audit.after = after.clone();
        intake.validate("library").unwrap();
        head.legacy_append = Some(f.record(1, "legacy", &intake));
        f.install_head(&head);
        f.clear();
        let outcome = f.sync(&local);
        assert!(!matches!(&outcome, crate::cloud_outcome::Outcome::Unchanged { .. }));
        assert_eq!(outcome.state(), &local.portable(), "continuation must not invent causal mutations");
        assert_eq!(outcome.head_position().unwrap().cursor(), local.cursor.as_deref().unwrap());
        let requests = f.objects.state.lock().unwrap().requests.clone();
        assert!(requests.iter().any(|(_, target)| target.contains("list-type")));
        if after.is_some() {
            assert!(requests.iter().any(|(_, target)| target.contains("start-after=")));
        }
        assert!(!requests.iter().any(|(method, target)| method == "PUT" && target.contains("/changes/")));
        let (head, _, _) = f.c.b2_read_head(&cancel).unwrap();
        let intake = f.c.b2_legacy_intake(&head, &cancel).unwrap();
        assert!(intake.scan.audit.complete);
        assert_eq!(intake.scan.audit.round, 1);
        f.clear();
        assert!(matches!(f.sync(&f.accepted(&outcome)), crate::cloud_outcome::Outcome::Unchanged { .. }));
        assert!(f.objects.state.lock().unwrap().requests.is_empty(), "completed scans wait for the scheduled deadline");
    }
}

#[test]
fn account_caps_and_cooldowns_cross_native_b2_and_s3_without_additional_requests() {
    for native in [false, true] {
        let f = Combined::new();
        let cancel = AtomicBool::new(false);
        if native {
            let mut state = f.native.state.lock().unwrap();
            state.fault = Some(403);
            state.cap_code = Some("transaction_cap_exceeded".into());
        } else {
            f.objects.state.lock().unwrap().download_cap = true;
        }
        let failure =
            if native { f.c.b2_read_authority(&cancel).err().unwrap() } else { f.c.get_small("library/refused", 1024, &cancel).unwrap_err() };
        assert_eq!(failure, AssetError::ProviderLimit { status: 403 });
        assert!(matches!(f.c.b2_read_authority(&cancel), Err(AssetError::ProviderLimit { .. })));
        assert_eq!(f.c.get_small("library/untouched", 1024, &cancel).unwrap_err(), AssetError::ProviderLimit { status: 403 });
        assert_eq!(f.native.state.lock().unwrap().requests, if native { 1 } else { 0 });
        assert_eq!(f.objects.state.lock().unwrap().requests.len(), if native { 0 } else { 1 });
    }
    let f = Combined::new();
    let cancel = AtomicBool::new(false);
    {
        let mut state = f.native.state.lock().unwrap();
        state.fault = Some(429);
        state.retry_after = Some("60".into());
    }
    assert!(matches!(f.c.b2_read_authority(&cancel), Err(AssetError::Throttled { status: 429, retry_after_ms: 60000 })));
    assert!(matches!(f.c.get_small("library/untouched", 1024, &cancel), Err(AssetError::Throttled { .. })));
    assert_eq!(f.native.state.lock().unwrap().requests, 1);
    assert!(f.objects.state.lock().unwrap().requests.is_empty());
}
