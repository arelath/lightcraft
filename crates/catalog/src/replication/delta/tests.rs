use super::*;
use crate::{Catalog, Journal, MemStore, Op};
use serde_json::json;

fn state() -> SyncState {
    let mut state = SyncState::new("library".into(), "author".into(), "personal".into()).unwrap();
    state
        .enqueue((0..40).map(|id| Change { entity: format!("photo:p{id}"), field: "rating".into(), base: None, value: json!(0) }).collect())
        .unwrap();
    state.acknowledge(&state.pending.keys().cloned().collect(), "a".repeat(64), 1).unwrap();
    state
}
fn edit(before: &SyncState) -> SyncState {
    let mut after = before.clone();
    after
        .enqueue(vec![Change {
            entity: "photo:p1".into(),
            field: "rating".into(),
            base: after.winning("photo:p1", "rating").map(|(stamp, _)| stamp),
            value: json!(4),
        }])
        .unwrap();
    after
}
#[test]
fn field_delta_replays_reverses_and_is_smaller_than_full_state() {
    let before = state();
    let after = edit(&before);
    let delta = SyncDelta::between(&before, &after);
    assert_eq!(delta.documents.len(), 1);
    assert_eq!(delta.documents["photo:p1"].fields.len(), 1);
    assert_eq!(delta.updated(&before).unwrap(), after);
    assert_eq!(delta.inverse().updated(&after).unwrap(), before);
    assert!(serde_json::to_vec(&delta).unwrap().len() < serde_json::to_vec(&after).unwrap().len());

    let store = MemStore::new();
    let (mut journal, mut catalog, _) = Journal::open(Box::new(store.clone())).unwrap();
    catalog.apply(Op::SetSyncState { state: Some(Box::new(before)) }).unwrap();
    journal.snapshot(&catalog).unwrap();
    let op = Op::PatchSyncState { delta: Box::new(delta) };
    let inverse = catalog.apply(op.clone()).unwrap();
    journal.append(&[op]).unwrap();
    drop(journal);
    let (_, replayed, report) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(report.replayed, 1);
    assert_eq!(replayed.to_snapshot(), catalog.to_snapshot());
    assert_eq!(replayed.sync_state(), Some(&after));
    catalog.apply(inverse).unwrap();
    assert_eq!(catalog.sync_state().unwrap().documents["photo:p1"]["rating"].0.values().next(), Some(&json!(0)));
}

#[test]
fn album_membership_delta_does_not_copy_unrelated_members() {
    let mut before = state();
    before
        .enqueue((0..1000).map(|n| Change { entity: "album:large".into(), field: format!("member:p{n}"), base: None, value: json!(true) }).collect())
        .unwrap();
    let mut after = before.clone();
    after
        .enqueue(vec![Change {
            entity: "album:large".into(),
            field: "member:p1".into(),
            base: after.winning("album:large", "member:p1").map(|(stamp, _)| stamp),
            value: json!(false),
        }])
        .unwrap();
    let delta = SyncDelta::between(&before, &after);
    assert_eq!(delta.documents["album:large"].fields.len(), 1);
    assert_eq!(delta.updated(&before).unwrap(), after);
    assert_eq!(delta.inverse().updated(&after).unwrap(), before);
    // New pending mutation and receipt are small too; the old pending bulk
    // mutation already belongs to the base and must not be logged again.
    assert!(serde_json::to_vec(&delta).unwrap().len() < 10_000);
}
#[test]
fn stale_base_invalid_result_and_failed_batch_leave_everything_unchanged() {
    let before = state();
    let after = edit(&before);
    let mut catalog = Catalog::new();
    catalog.apply(Op::SetSyncState { state: Some(Box::new(before.clone())) }).unwrap();
    let snapshot = catalog.to_snapshot();
    let mut bad = SyncDelta::between(&before, &after);
    bad.documents.get_mut("photo:p1").unwrap().fields.get_mut("rating").unwrap().before = None;
    assert!(catalog.apply(Op::PatchSyncState { delta: Box::new(bad) }).is_err());
    assert_eq!(catalog.to_snapshot(), snapshot);
    let mut bad = SyncDelta::between(&before, &after);
    bad.header.after.clock.clear();
    assert!(catalog.apply(Op::PatchSyncState { delta: Box::new(bad) }).is_err());
    assert_eq!(catalog.to_snapshot(), snapshot);
    assert!(
        catalog
            .apply(Op::Batch {
                ops: vec![Op::PatchSyncState { delta: Box::new(SyncDelta::between(&before, &after)) }, Op::RemovePhoto { id: crate::PhotoId(999) }]
            })
            .is_err()
    );
    assert_eq!(catalog.to_snapshot(), snapshot);
}
#[test]
fn receipt_archiving_and_ack_removals_survive_delta_replay() {
    let mut before = state();
    for _ in 0..8 {
        before = edit(&before);
    }
    let mut after = before.clone();
    let ids = after.pending.keys().cloned().collect();
    after.acknowledge(&ids, "b".repeat(64), 2).unwrap();
    let plan = after.archive_plan().unwrap();
    after.compact_receipts(&plan).unwrap();
    assert!(!after.archives.is_empty());
    let delta = SyncDelta::between(&before, &after);
    assert_eq!(delta.updated(&before).unwrap(), after);
    assert_eq!(delta.inverse().updated(&after).unwrap(), before);
}
#[test]
fn append_publication_remote_uses_field_deltas_and_preserves_proofs() {
    let mut before = state();
    before.protocol = SyncProtocol::AppendLog;
    before.cursor = None;
    before.generation = 0;
    before.append = Some(Box::new(AppendProgress {
        version: 1,
        manifest: "a".repeat(64),
        frontiers: before.clock.clone(),
        remote: before.portable(),
        scan: Default::default(),
        gaps: BTreeSet::new(),
    }));
    let mut after = edit(&before);
    let progress = AppendProgress {
        version: 1,
        manifest: "a".repeat(64),
        frontiers: after.clock.clone(),
        remote: after.portable(),
        scan: Default::default(),
        gaps: BTreeSet::new(),
    };
    after.acknowledge_append(&after.pending.keys().cloned().collect(), progress).unwrap();
    let proofs = after.archive_plan().unwrap().objects;
    assert!(!proofs.is_empty());
    after.append.as_mut().unwrap().remote.proofs = proofs;
    after.validate().unwrap();
    let delta = SyncDelta::between(&before, &after);
    let AppendDelta::Patch { remote, .. } = &delta.append else { panic!("expected publication delta") };
    assert_eq!(remote.documents.len(), 1);
    assert_eq!(delta.updated(&before).unwrap(), after);
    assert_eq!(delta.inverse().updated(&after).unwrap(), before);
    let mut initialized = before.clone();
    initialized.append = None;
    let transition = SyncDelta::between(&initialized, &before);
    assert_eq!(transition.updated(&initialized).unwrap(), before);
    assert_eq!(transition.inverse().updated(&before).unwrap(), initialized);
}
#[test]
fn consecutive_states_compact_against_each_predecessor_including_disable_and_enable() {
    let before = state();
    let middle = edit(&before);
    let after = edit(&middle);
    let mut catalog = Catalog::new();
    catalog.apply(Op::SetSyncState { state: Some(Box::new(before)) }).unwrap();
    let op = Op::Batch {
        ops: vec![
            Op::SetSyncState { state: Some(Box::new(middle)) },
            Op::Batch { ops: vec![Op::SetSyncState { state: Some(Box::new(after.clone())) }] },
        ],
    };
    let compact = catalog.compact_sync_op(op).unwrap();
    assert!(!serde_json::to_string(&compact).unwrap().contains("setSyncState"));
    let undo = catalog.apply(compact).unwrap();
    assert_eq!(catalog.sync_state(), Some(&after));
    catalog.apply(undo).unwrap();
    let op = catalog
        .compact_sync_op(Op::Batch { ops: vec![Op::SetSyncState { state: None }, Op::SetSyncState { state: Some(Box::new(after.clone())) }] })
        .unwrap();
    catalog.apply(op).unwrap();
    assert_eq!(catalog.sync_state(), Some(&after));
}
#[test]
fn reverse_identity_index_tracks_clones_removal_restart_and_failed_batches() {
    let mut catalog = Catalog::new();
    catalog.apply(Op::SetCloudIdentity { entity: "photo:1".into(), id: Some("global".into()) }).unwrap();
    assert_eq!(catalog.cloud_local_identity("global"), Some("photo:1"));
    assert!(catalog.apply(Op::SetCloudIdentity { entity: "photo:2".into(), id: Some("global".into()) }).is_err());
    let mut copy = Catalog::from_snapshot(&catalog.to_snapshot()).unwrap().clone();
    assert_eq!(copy.cloud_local_identity("global"), Some("photo:1"));
    assert!(
        copy.apply(Op::Batch { ops: vec![Op::SetCloudIdentity { entity: "photo:1".into(), id: None }, Op::RemovePhoto { id: crate::PhotoId(99) }] })
            .is_err()
    );
    assert_eq!(copy.cloud_local_identity("global"), Some("photo:1"));
    copy.apply(Op::SetCloudIdentity { entity: "photo:1".into(), id: None }).unwrap();
    assert_eq!(copy.cloud_local_identity("global"), None);
    assert_eq!(catalog.cloud_local_identity("global"), Some("photo:1"));
}

#[test]
fn version_17_snapshot_upgrades_before_new_delta_records_are_written() {
    let before = state();
    let after = edit(&before);
    let mut catalog = Catalog::new();
    catalog.apply(Op::SetSyncState { state: Some(Box::new(before.clone())) }).unwrap();
    let store = MemStore::new();
    store.set(crate::journal::SNAPSHOT, serde_json::to_vec(&json!({"format":"lightcraft-catalog","version":17,"seq":0,"catalog":catalog})).unwrap());
    let (mut journal, mut loaded, report) = Journal::open(Box::new(store.clone())).unwrap();
    assert_eq!(report.upgraded_from, Some(17));
    let snapshot: serde_json::Value = serde_json::from_slice(&store.get(crate::journal::SNAPSHOT).unwrap()).unwrap();
    assert_eq!(snapshot["version"], 19);
    let op = Op::PatchSyncState { delta: Box::new(SyncDelta::between(&before, &after)) };
    loaded.apply(op.clone()).unwrap();
    journal.append(&[op]).unwrap();
    drop(journal);
    let (_, reopened, report) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(report.replayed, 1);
    assert_eq!(reopened.sync_state(), Some(&after));
}

#[test]
fn append_scan_delta_only_records_new_checkpoint_keys_and_audit_cursor() {
    let mut before = state();
    before.protocol = SyncProtocol::AppendLog;
    before.cursor = None;
    before.generation = 0;
    before.append = Some(Box::new(AppendProgress {
        version: 1,
        manifest: "a".repeat(64),
        frontiers: before.clock.clone(),
        remote: before.portable(),
        gaps: BTreeSet::new(),
        scan: Default::default(),
    }));
    for n in 0..1000 {
        before.append.as_mut().unwrap().scan.checkpoints.insert(format!("checkpoints/a/{n:064x}.json"));
    }
    let mut after = before.clone();
    let scan = &mut after.append.as_mut().unwrap().scan;
    scan.checkpoints.insert(format!("checkpoints/b/{}.json", "b".repeat(64)));
    scan.audit = AppendAudit { round: 1, complete: false, after: Some(format!("ops/a/{:020}-{}.json", 1, "c".repeat(64))) };
    let delta = SyncDelta::between(&before, &after);
    assert!(serde_json::to_vec(&delta).unwrap().len() < 4000);
    assert_eq!(delta.updated(&before).unwrap(), after);
    assert_eq!(delta.inverse().updated(&after).unwrap(), before);
}
