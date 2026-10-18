use super::*;
use crate::cloud_control::{Boundary, Control, LEASE_MS};
use crate::cloud_records::{CheckpointRef, object_key};
use lightcraft_catalog::assets::BlobId;

fn connection() -> S3Connection {
    serde_json::from_value(
        serde_json::json!({"id":"test","bucket":"test","region":"us-east-005","prefix":"p","endpoint":"https://s3.us-east-005.backblazeb2.com"}),
    )
    .unwrap()
}
fn reference(epoch: u64, kind: &str, digit: char) -> RecordRef {
    let hash = digit.to_string().repeat(64);
    RecordRef { key: object_key("p", epoch, kind, &hash), hash: BlobId(hash), size: 10 }
}
fn active() -> Head {
    let mut control = Control::initial("library");
    control.epoch = 2;
    control.active_metadata_root = crate::cloud_control::epoch_root(2);
    let mut head = Head::next(None, reference(2, "changes", 'a'), "library", None, control).unwrap();
    head.version = 5;
    head.legacy_append = Some(reference(1, "legacy", 'c'));
    head.generation = 8;
    head
}
fn initial() -> Head {
    let source = reference(1, "changes", 'a');
    let token = "1".repeat(64);
    let control = Control::initial("library")
        .begin("m", Plan::Rebase, Boundary { tip: source.hash.0.clone(), generation: 1 }, "owner", &token, 0)
        .unwrap()
        .begin_output(&token, 1)
        .unwrap();
    let mut head = Head::next(
        None,
        reference(1, "changes", 'b'),
        "library",
        Some(CheckpointRef { object: reference(1, "checkpoints", 'd'), covers: source, generation: 1 }),
        control,
    )
    .unwrap();
    head.generation = 2;
    head.version = 5;
    head.legacy_append = Some(reference(1, "legacy", 'c'));
    head
}
#[test]
fn initialization_requires_exact_exposed_append_upgrade_shape() {
    let c = connection();
    let valid = initial();
    validate_initial(&c, &valid).unwrap();
    for kind in 0..7 {
        let mut bad = valid.clone();
        match kind {
            0 => bad.legacy_append = None,
            1 => bad.generation = 3,
            2 => bad.control.as_mut().unwrap().migration.as_mut().unwrap().exposure = Exposure::Preparing,
            3 => bad.control.as_mut().unwrap().migration.as_mut().unwrap().plan = Plan::AppendLog,
            4 => bad.checkpoint.as_mut().unwrap().generation = 2,
            5 => bad.control.as_mut().unwrap().migration.as_mut().unwrap().source.tip = "f".repeat(64),
            _ => bad.control = Some(Control::initial("library")),
        }
        assert!(validate_initial(&c, &bad).is_err(), "case {kind}");
    }
    assert!(validate_initial(&c, &active()).is_err(), "ordinary fresh heads cannot initialize B2 authority");
}

#[test]
fn descriptor_future_format_keeps_update_recovery_and_other_bytes_fail_closed() {
    let scope = Scope::new(&connection()).unwrap();
    scope.verify_descriptor(&scope.descriptor().unwrap()).unwrap();
    let error = scope.verify_descriptor(br#"{"version":2,"newFields":true}"#).unwrap_err();
    let failure = crate::cloud_status::CloudFailure::from(error);
    assert_eq!(failure.remedy, Some(crate::cloud_status::Recovery::Update));
    for bytes in [br#"{"version":0}"#.as_slice(), b"{}", b"null"] {
        assert!(scope.verify_descriptor(bytes).is_err());
    }
    let mut other = connection();
    other.prefix = "elsewhere".into();
    assert!(Scope::new(&other).unwrap().verify_descriptor(&scope.descriptor().unwrap()).is_err());
}
#[test]
fn scan_only_cas_cannot_change_position_checkpoint_or_control() {
    let c = connection();
    let old = active();
    let mut next = old.clone();
    next.legacy_append = Some(reference(1, "legacy", 'd'));
    validate_transition(&c, &old, &next).unwrap();
    next.tip = reference(2, "changes", 'f');
    assert!(validate_transition(&c, &old, &next).is_err());
    next = old.clone();
    next.generation -= 1;
    assert!(validate_transition(&c, &old, &next).is_err());
    next = old.clone();
    next.legacy_append = None;
    assert!(validate_transition(&c, &old, &next).is_err());
    next = old.clone();
    next.checkpoint = Some(CheckpointRef { object: reference(2, "checkpoints", 'b'), covers: old.tip.clone(), generation: 7 });
    assert!(validate_transition(&c, &old, &next).is_err());
}
#[test]
fn ordinary_publication_preserves_legacy_and_fenced_control() {
    let c = connection();
    let old = active();
    let next = Head::next(Some(&old), reference(2, "changes", 'b'), "library", None, old.control().unwrap()).unwrap();
    assert_eq!(next.version, 5);
    assert_eq!(next.legacy_append, old.legacy_append);
    validate_transition(&c, &old, &next).unwrap();
    let mut bypass = next.clone();
    bypass.control.as_mut().unwrap().epoch = 3;
    bypass.control.as_mut().unwrap().active_metadata_root = crate::cloud_control::epoch_root(3);
    bypass.tip = reference(3, "changes", 'b');
    assert!(validate_transition(&c, &old, &bypass).is_err(), "cannot activate without acquiring maintenance");
}

#[test]
fn readable_future_writer_and_converter_cannot_modify_authority() {
    let c = connection();
    let mut old = active();
    let control = old.control.as_mut().unwrap();
    control.schema = 2;
    control.minimum_writer = 2;
    old.validate_scope_typed("p", "library").unwrap();
    let next = Head::next(Some(&old), reference(2, "changes", 'b'), "library", None, old.control().unwrap()).unwrap();
    assert!(matches!(validate_transition(&c, &old, &next), Err(AssetError::Blocked(crate::cloud_control::Blocked::UpdateToWrite(_)))));
    let mut old = initial();
    old.control.as_mut().unwrap().migration.as_mut().unwrap().target_schema = 2;
    old.validate_scope_typed("p", "library").unwrap();
    let renewal = old.control().unwrap().renew(&"1".repeat(64), 2).unwrap();
    let next = Head::next(Some(&old), reference(1, "changes", 'e'), "library", old.checkpoint.clone(), renewal).unwrap();
    assert!(matches!(validate_transition(&c, &old, &next), Err(AssetError::Blocked(crate::cloud_control::Blocked::UpdateToUpgrade(_)))));
}
#[test]
fn takeover_retains_source_exposure_and_rejects_stale_activation() {
    let c = connection();
    let old = initial();
    let control = old.control().unwrap();
    let takeover = control.takeover("next", &"2".repeat(64), LEASE_MS + 1, false).unwrap();
    let next = Head::next(Some(&old), reference(1, "changes", 'e'), "library", old.checkpoint.clone(), takeover).unwrap();
    validate_transition(&c, &old, &next).unwrap();
    let mut stale = Head::next(Some(&next), reference(2, "changes", 'f'), "library", None, control.activate(&"1".repeat(64)).unwrap()).unwrap();
    assert!(validate_transition(&c, &next, &stale).is_err());
    stale.control = Some(next.control().unwrap().activate(&"2".repeat(64)).unwrap());
    validate_transition(&c, &next, &stale).unwrap();
    let mut cancelled = next.clone();
    cancelled.generation += 1;
    cancelled.control = Some(Control::initial("library"));
    assert!(validate_transition(&c, &next, &cancelled).is_err());
}
#[test]
fn deletion_retains_no_personal_references_and_never_revives() {
    let c = connection();
    let old = active();
    let mut deleted = old.clone();
    deleted.control = Some(old.control().unwrap().delete().unwrap());
    deleted.generation = 1;
    deleted.checkpoint = None;
    deleted.legacy_append = None;
    deleted.tip = RecordRef { key: object_key("p", 2, "changes", &"0".repeat(64)), hash: BlobId("0".repeat(64)), size: 1 };
    validate_transition(&c, &old, &deleted).unwrap();
    assert!(validate_transition(&c, &deleted, &old).is_err());
    let mut personal = deleted.clone();
    personal.legacy_append = old.legacy_append.clone();
    assert!(validate_transition(&c, &old, &personal).is_err());
    assert!(matches!(deleted.validate_scope_typed("p", "library"), Err(crate::cloud_control::Blocked::Deleted)));
}

#[test]
fn unconditional_catalog_writes_require_exact_hash_namespace_and_valid_records() {
    let c = connection();
    let batch = crate::cloud_records::Batch { version: 3, parent: None, imports: vec![], mutations: vec![], checkpoint_marker: true };
    let bytes = serde_json::to_vec(&batch).unwrap();
    let key = object_key("p", 2, "changes", &sha256(&bytes).0);
    validate_catalog_record(&c, &key, &bytes).unwrap();
    for bad in ["p/catalog/head.json", "p/catalog/b2-authority.json", "other/catalog/changes/a.json"] {
        assert!(validate_catalog_record(&c, bad, &bytes).is_err());
    }
    assert!(validate_catalog_record(&c, &key, b"{}").is_err());
    let intake = LegacyIntake::new("library", Default::default()).unwrap();
    let bytes = serde_json::to_vec(&intake).unwrap();
    validate_catalog_record(&c, &object_key("p", 1, "legacy", &sha256(&bytes).0), &bytes).unwrap();
    assert!(validate_catalog_record(&c, &object_key("p", 2, "legacy", &sha256(&bytes).0), &bytes).is_err());
    assert!(
        crate::cloud_append::immutable_metadata(&key, &serde_json::to_vec(&batch).unwrap()).is_err(),
        "the existing append-only API stays narrow"
    );
}
