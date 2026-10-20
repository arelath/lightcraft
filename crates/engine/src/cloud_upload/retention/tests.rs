use super::*;
use lightcraft_catalog::replication::{Change, SyncState};
#[test]
fn historical_references_survive_supersession_and_missing_operations_refuse_gc() {
    let mut state = SyncState::new("library".into(), "author".into(), "personal".into()).unwrap();
    let mut audit = audit::Audit::default();
    let mut keys = BTreeSet::new();
    let old = "a".repeat(64);
    for value in [json!(format!("library/originals/{old}")), json!("new value"), json!(true)] {
        let mutation = state.enqueue(vec![Change { entity: "photo:p".into(), field: "content".into(), base: None, value }]).unwrap().unwrap();
        let key = crate::cloud_append::mutation_key("library", &mutation).unwrap();
        let bytes = crate::cloud_append::encode(&mutation).unwrap();
        audit.record("library", &key, &bytes).unwrap();
        keys.insert(key);
    }
    assert!(audit.protects(&format!("library/originals/{old}"), &old));
    audit.complete(&keys).unwrap();
    let mut incomplete = audit::Audit::default();
    let mutation =
        state.enqueue(vec![Change { entity: "photo:p".into(), field: "deleted".into(), base: None, value: json!(true) }]).unwrap().unwrap();
    let key = crate::cloud_append::mutation_key("library", &mutation).unwrap();
    incomplete.record("library", &key, &crate::cloud_append::encode(&mutation).unwrap()).unwrap();
    assert!(incomplete.complete(&[key].into()).is_err());
}
#[test]
fn archived_receipts_bind_operations_and_causal_context_before_reclamation() {
    let mut state = SyncState::new("library".into(), "author".into(), "personal".into()).unwrap();
    let first = state.enqueue(vec![Change { entity: "photo:p".into(), field: "rating".into(), base: None, value: json!(1) }]).unwrap().unwrap();
    let second = state.enqueue(vec![Change { entity: "photo:p".into(), field: "rating".into(), base: None, value: json!(2) }]).unwrap().unwrap();
    state.pending.clear();
    let plan = state.archive_plan().unwrap();
    state.compact_receipts(&plan).unwrap();
    assert!(!state.received.contains_key(&first.revision.id()));
    for variant in 0..3 {
        let mut audit = audit::Audit::default();
        let mut keys = BTreeSet::new();
        audit.local_clocks("library", &json!({"state":state})).unwrap();
        let mut operation = first.clone();
        if variant == 1 {
            operation.changes.get_mut(0).unwrap().value = json!(5);
        } else if variant == 2 {
            operation.context.insert("other".into(), 1);
        }
        for mutation in [operation, second.clone()] {
            let key = crate::cloud_append::mutation_key("library", &mutation).unwrap();
            audit.record("library", &key, &crate::cloud_append::encode(&mutation).unwrap()).unwrap();
            keys.insert(key);
        }
        for (hash, node) in &plan.objects {
            let key = crate::cloud_history::key("library", "library", hash).unwrap();
            audit.record("library", &key, &serde_json::to_vec(node).unwrap()).unwrap();
            keys.insert(key);
        }
        if variant == 0 {
            audit.complete(&keys).unwrap();
        } else {
            assert!(audit.complete(&keys).unwrap_err().to_string().contains("archived mutation identity/context collision"));
        }
    }
    // A well-hashed branch pointing at a leaf with the wrong range is also invalid.
    let mut audit = audit::Audit::default();
    let mut node = plan.objects.values().find(|node| node.count == 2).unwrap().clone();
    if let lightcraft_catalog::replication::archive::Value::Branch { left, right } = &mut node.value {
        *left = right.clone();
    }
    let key = crate::cloud_history::key("library", "library", &node.digest().unwrap()).unwrap();
    assert!(audit.record("library", &key, &serde_json::to_vec(&node).unwrap()).unwrap_err().to_string().contains("scope collision"));
}
#[test]
fn upgraded_heads_epoch_records_and_append_control_are_audited_not_refused() {
    use crate::cloud_records::{Batch, Head, RecordRef, object_key};
    let mut state = SyncState::new("library".into(), "author".into(), "personal".into()).unwrap();
    let old = "b".repeat(64);
    let mutation = state
        .enqueue(vec![Change { entity: "photo:p".into(), field: "content".into(), base: None, value: json!(format!("library/originals/{old}")) }])
        .unwrap()
        .unwrap();
    let batch =
        serde_json::to_vec(&Batch { version: 3, parent: None, imports: Vec::new(), checkpoint_marker: false, mutations: vec![mutation] }).unwrap();
    let hash = sha256(&batch);
    let mut audit = audit::Audit::default();
    // A rebased (epoch 2) change batch is a recognised record and keeps its media protected.
    audit.record("library", &object_key("library", 2, "changes", &hash.0), &batch).unwrap();
    assert!(audit.protects(&format!("library/originals/{old}"), &old));
    assert!(audit.record("library", &format!("library/catalog/epochs/02/changes/{}.json", hash.0), &batch).is_err());
    // Version-4 heads carry the library control envelope.
    let tip = RecordRef { key: object_key("library", 1, "changes", &hash.0), size: batch.len() as u64, hash };
    let head = Head::next(None, tip, "library", None, crate::cloud_control::Control::initial("library")).unwrap();
    assert_eq!(head.version, 4);
    audit.record("library", "library/catalog/head.json", &serde_json::to_vec(&head).unwrap()).unwrap();
    // Append control records must be canonical for their key.
    let control = crate::cloud_control::Control::initial_append("library");
    let key = crate::cloud_append::control::key("library", &control).unwrap();
    let bytes = crate::cloud_append::control::encode(&control).unwrap();
    audit.record("library", &key, &bytes).unwrap();
    assert!(audit.record("library", &key.replace(".json", "0.json"), &bytes).is_err());
}
#[test]
fn b2_authority_requires_legacy_manifest_and_all_referenced_metadata() {
    use crate::cloud_records::{Batch, Head, LegacyIntake, RecordRef, object_key};
    let intake = LegacyIntake::new("library", Default::default()).unwrap();
    let legacy_bytes = serde_json::to_vec(&intake).unwrap();
    let legacy_hash = sha256(&legacy_bytes);
    let legacy = RecordRef { key: object_key("library", 1, "legacy", &legacy_hash.0), size: legacy_bytes.len() as u64, hash: legacy_hash };
    let batch = serde_json::to_vec(&Batch { version: 3, parent: None, imports: vec![], mutations: vec![], checkpoint_marker: true }).unwrap();
    let batch_hash = sha256(&batch);
    let tip = RecordRef { key: object_key("library", 2, "changes", &batch_hash.0), size: batch.len() as u64, hash: batch_hash };
    let mut control = crate::cloud_control::Control::initial("library");
    control.epoch = 2;
    control.active_metadata_root = crate::cloud_control::epoch_root(2);
    let mut head = Head::next(None, tip.clone(), "library", None, control).unwrap();
    head.version = 5;
    head.legacy_append = Some(legacy.clone());
    let head_bytes = serde_json::to_vec(&head).unwrap();
    let head_key = format!("library/catalog/authority/{}.json", sha256(&head_bytes).0);
    let manifest_key = crate::cloud_append::manifest_key("library", "library").unwrap();
    let descriptor = serde_json::to_vec(&json!({"version":1,"protocol":"b2-bucket-head-v1","binding":"a".repeat(64)})).unwrap();
    let records = [
        ("library/catalog/b2-authority.json".into(), descriptor),
        (manifest_key.clone(), crate::cloud_append::manifest("library").unwrap()),
        (legacy.key.clone(), legacy_bytes),
        (tip.key.clone(), batch),
        (head_key, head_bytes),
    ];
    let mut audit = audit::Audit::default();
    audit.b2 = true;
    let mut keys = BTreeSet::new();
    for (key, bytes) in &records {
        audit.record("library", key, bytes).unwrap();
        keys.insert(key.clone());
    }
    audit.complete(&keys).unwrap();
    for missing in [&legacy.key, &tip.key, &manifest_key] {
        let mut incomplete = keys.clone();
        incomplete.remove(missing);
        assert!(audit.complete(&incomplete).is_err(), "missing {missing} must refuse reclamation");
    }
    for (key, bytes) in records.iter().filter(|(key, _)| key.contains("authority") || key.contains("legacy")) {
        assert!(audit::Audit::default().record("library", key, bytes).is_err(), "B2 metadata requires a validated provider authority");
    }
    let mut future = serde_json::to_value(&head).unwrap();
    future["version"] = json!(6);
    let bytes = serde_json::to_vec(&future).unwrap();
    assert!(audit.record("library", &format!("library/catalog/authority/{}.json", sha256(&bytes).0), &bytes).is_err());
}
#[test]
fn retention_dates_and_candidate_paths_are_strict() {
    assert_eq!(modified("Mon, 01 Jan 2024 00:00:00 GMT").unwrap(), 1704067200);
    assert!(modified("Mon, 99 Jan 2024 00:00:00 GMT").is_err());
    assert!(modified("yesterday").is_err());
    let id = "a".repeat(64);
    assert_eq!(media_hash("library", &format!("library/originals/{id}")), Some(id.as_str()));
    for key in [
        format!("other/originals/{id}"),
        format!("library/originals/../{id}"),
        format!("library/associated/xmp/{id}.xmp"),
        format!("library/previews/{id}/v2/{id}.lcsp"),
    ] {
        assert!(media_hash("library", &key).is_none());
    }
}
#[test]
fn apply_guard_blocks_edits_undo_switch_and_preserves_query_and_cancellation() {
    let root = std::env::temp_dir().join(format!("lightcraft-retention-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut s = Session::new().with_fs();
    s.open_library(&root, false).unwrap();
    s.execute("album.create", &json!({"name":"before"})).unwrap();
    let revision = s.catalog.revision;
    let undo = s.undo.len();
    let (sender, receiver) = mpsc::sync_channel(2);
    let guard = s.activity.start("uploadCleanup", "Reclaim", crate::activity::Cancel::Yes);
    let id = guard.id();
    let cancel = guard.cancel_flag().unwrap();
    let resolver = s.media.asset_resolver.clone().unwrap();
    s.cloud_uploads.pending.push(Pending {
        receiver,
        library: s.library_token(),
        connection_revision: resolver.connection_revision(),
        resolver,
        ids: BTreeMap::new(),
        verification_ids: BTreeSet::new(),
        item_outcomes: BTreeMap::new(),
        connections: ["personal".into()].into(),
        maintenance: Some(Maintenance::Retention { connection: "personal".into(), apply: true }),
        media_verified: BTreeSet::new(),
        guard,
        cancel,
    });
    for command in ["album.create", "edit.undo", "cloud.attach"] {
        assert!(s.execute(command, &json!({"name":"blocked"})).is_err());
    }
    assert!(s.undo_step().unwrap_err().to_string().contains("frozen preview"));
    assert!(s.redo_step().unwrap_err().to_string().contains("frozen preview"));
    assert!(s.close_library().is_err());
    assert!(s.open_library(root.join("other"), false).is_err());
    assert!(s.apply_received(Op::SetRating { id: lightcraft_catalog::PhotoId(1), rating: 1 }).unwrap_err().to_string().contains("frozen preview"));
    assert_eq!(s.catalog.revision, revision);
    assert_eq!(s.undo.len(), undo);
    assert!(s.execute("albums.list", &json!({})).is_ok());
    assert!(s.execute("activity.cancel", &json!({"id":id})).is_ok());
    sender.send(Event::Finished(Ok(json!({"applied":true})))).unwrap();
    s.poll_cloud_uploads();
    assert!(s.ensure_reclamation_idle().is_ok());
    s.close_library().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
