use super::*;
use serde_json::json;

/// Models the conditional head: a transition is accepted only against the exact
/// revision it was derived from (the ETag compare-and-swap).
struct Authority(Control);
impl Authority {
    fn cas(&mut self, expected: &Control, next: Control) -> bool {
        if &self.0 != expected {
            return false;
        }
        self.0 = next;
        true
    }
}
fn tok(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}
fn source() -> Boundary {
    Boundary { tip: "a".repeat(64), generation: 7 }
}

#[test]
fn initial_control_is_active_epoch_one_in_the_legacy_root() {
    let c = Control::initial("library");
    c.validate().unwrap();
    c.may_publish().unwrap();
    assert_eq!(c.active_metadata_root, "catalog");
    assert_eq!(epoch_root(8), "catalog/epochs/8");
}

#[test]
fn migration_registry_uses_actual_schemas_and_converter_compatibility() {
    for plan in [Plan::Rebase, Plan::AppendLog] {
        let spec = MigrationSpec::resolve(plan, SCHEMA, SCHEMA).unwrap();
        assert_eq!(spec.converter(), Converter::Identity);
        assert_eq!((spec.source_schema(), spec.target_schema()), (SCHEMA, SCHEMA));
        assert_eq!((spec.minimum_reader(), spec.minimum_writer()), (1, 1));
        for (source, target) in [(SCHEMA, SCHEMA + 1), (SCHEMA + 1, SCHEMA + 1), (0, SCHEMA)] {
            assert!(matches!(MigrationSpec::resolve(plan, source, target), Err(Blocked::UpdateToUpgrade(_))));
        }
    }
    // Identical numbers alone do not imply this build implements their wire schema.
    assert!(translate_mutation(SCHEMA + 1, SCHEMA + 1).is_err());
    let base = Control::initial("library");
    let upgrade = base.begin("m", Plan::Rebase, source(), "owner", &tok(1), 0).unwrap();
    let spec = upgrade.migration_spec().unwrap();
    let active = upgrade.activate(&tok(1)).unwrap();
    assert_eq!((active.schema, active.minimum_reader, active.minimum_writer), (spec.target_schema(), spec.minimum_reader(), spec.minimum_writer()));
}

#[test]
fn readable_future_migration_is_refused_before_ownership_or_activation() {
    for plan in [Plan::Rebase, Plan::AppendLog] {
        let mut c = Control::initial("library").begin("future", plan, source(), "old-owner", &tok(1), 0).unwrap();
        c.migration.as_mut().unwrap().target_schema = SCHEMA + 1;
        c.may_read().unwrap(); // A valid newer upgrader's readable maintenance record.
        let original = c.clone();
        assert!(matches!(c.takeover("new-owner", &tok(2), LEASE_MS, false), Err(Blocked::UpdateToUpgrade(_))));
        assert!(
            matches!(c.takeover("old-owner", &tok(2), 0, true), Err(Blocked::UpdateToUpgrade(_))),
            "same-installation resume also checks capability"
        );
        assert!(matches!(c.activate(&tok(1)), Err(Blocked::UpdateToUpgrade(_))));
        assert!(matches!(
            c.retire(&tok(1), Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: "b".repeat(64) }),
            Err(Blocked::UpdateToUpgrade(_))
        ));
        assert!(matches!(c.cancel(&tok(1)), Err(Blocked::UpdateToUpgrade(_))));
        assert_eq!(c, original);
    }
    let mut compatible = Control::initial("library");
    compatible.schema = SCHEMA + 1;
    compatible.may_publish().unwrap(); // Read/write compatibility is not conversion capability.
    assert!(matches!(compatible.begin("m", Plan::Rebase, source(), "owner", &tok(1), 0), Err(Blocked::UpdateToUpgrade(_))));
}

#[test]
fn compatibility_separates_reading_from_writing_and_refuses_newer_envelopes() {
    let mut value = serde_json::to_value(Control::initial("library")).unwrap();
    value["controlVersion"] = json!(CONTROL_VERSION + 1);
    value["somethingNew"] = json!(true);
    assert!(matches!(Control::decode(&value), Err(Blocked::UpdateToRead(_))), "newer envelope refused before field decoding");
    let mut readable = Control::initial("library");
    readable.schema = WRITER + 1;
    readable.minimum_writer = WRITER + 1;
    readable.may_read().unwrap();
    assert!(matches!(readable.may_publish(), Err(Blocked::UpdateToWrite(_))), "deserializable is not writable");
    let mut unreadable = serde_json::to_value(&readable).unwrap();
    unreadable["minimumReader"] = json!(READER + 1);
    assert!(matches!(Control::decode(&unreadable), Err(Blocked::UpdateToRead(_))));
    let mut compatible = Control::initial("library");
    compatible.schema = WRITER + 1;
    compatible.may_publish().unwrap();
}

#[test]
fn two_simultaneous_upgraders_elect_exactly_one_owner() {
    let base = Control::initial("library");
    let mut head = Authority(base.clone());
    let a = base.begin("m1", Plan::Rebase, source(), "device-a", &tok(1), 0).unwrap();
    let b = base.begin("m2", Plan::Rebase, source(), "device-b", &tok(2), 0).unwrap();
    assert!(head.cas(&base, a.clone()));
    assert!(!head.cas(&base, b), "loser's CAS fails against the changed head");
    let reread = head.0.clone();
    assert!(matches!(reread.begin("m2", Plan::Rebase, source(), "device-b", &tok(2), 0), Err(Blocked::Maintenance { .. })));
    let active = reread.activate(&tok(1)).unwrap();
    assert!(head.cas(&reread, active));
    assert_eq!(head.0.epoch, 2);
    assert_eq!(head.0.active_metadata_root, "catalog/epochs/2");
}

#[test]
fn ordinary_commit_racing_maintenance_cannot_slip_past_the_frozen_boundary() {
    let base = Control::initial("library");
    let mut head = Authority(base.clone());
    // Writer prepared against `base`; maintenance wins the CAS first.
    let upgrading = base.begin("m1", Plan::Rebase, source(), "device-a", &tok(1), 0).unwrap();
    assert!(head.cas(&base, upgrading));
    let writer_commit = base.clone();
    assert!(!head.cas(&base, writer_commit), "stale writer commit rejected");
    assert!(matches!(head.0.may_publish(), Err(Blocked::Maintenance { .. })), "reread stops publication");
    head.0.may_read().unwrap();
}

#[test]
fn crashed_upgrader_takeover_invalidates_the_stale_owner() {
    let base = Control::initial("library");
    let mut head = Authority(base.clone());
    let a = base.begin("m1", Plan::Rebase, source(), "device-a", &tok(1), 1_000).unwrap();
    assert!(head.cas(&base, a.clone()));
    assert!(matches!(a.takeover("device-b", &tok(2), 1_000 + LEASE_MS - 1, false), Err(Blocked::Maintenance { .. })), "lease still held");
    let b = a.takeover("device-b", &tok(2), 1_000 + LEASE_MS, false).unwrap();
    assert_eq!(b.migration.as_ref().unwrap().id, "m1", "same migration resumes");
    assert_eq!(b.migration.as_ref().unwrap().source, source(), "same frozen source");
    assert!(head.cas(&a, b.clone()));
    assert!(matches!(b.may_publish(), Err(Blocked::Maintenance { .. })), "takeover never lets ordinary writers resume");
    // The resumed stale owner can neither activate from its old view nor from the new one.
    assert!(!head.cas(&a, a.activate(&tok(1)).unwrap()));
    assert!(b.activate(&tok(1)).is_err());
    assert!(b.cancel(&tok(1)).is_err());
    assert!(head.cas(&b, b.activate(&tok(2)).unwrap()));
}

#[test]
fn lost_activation_response_is_resolved_by_rereading_the_token_outcome() {
    let base = Control::initial("library");
    let mut head = Authority(base.clone());
    let a = base.begin("m1", Plan::Rebase, source(), "device-a", &tok(1), 0).unwrap();
    head.cas(&base, a.clone());
    let active = a.activate(&tok(1)).unwrap();
    head.cas(&a, active.clone()); // response lost
    // Retrying against the stale view fails; the reread shows our epoch is authoritative.
    assert!(!head.cas(&a, active.clone()));
    assert_eq!(head.0, active);
    assert!(head.0.activate(&tok(1)).is_err(), "activation cannot be applied twice");
}

#[test]
fn cancellation_restores_the_same_epoch_and_retirement_points_at_the_successor() {
    let base = Control::initial("library");
    let a = base.begin("m1", Plan::Rebase, source(), "device-a", &tok(1), 0).unwrap();
    let back = a.cancel(&tok(1)).unwrap();
    assert_eq!((back.epoch, back.state), (1, State::Active));
    back.may_publish().unwrap();
    let m = base.begin("m2", Plan::AppendLog, source(), "device-a", &tok(3), 0).unwrap();
    assert!(m.activate(&tok(3)).is_err(), "protocol migrations retire instead");
    let retired = m.retire(&tok(3), Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: "b".repeat(64) }).unwrap();
    assert!(matches!(retired.may_publish(), Err(Blocked::Retired(_))));
    retired.may_read().unwrap();
    assert!(retired.begin("m3", Plan::Rebase, source(), "x", &tok(4), 0).is_err());
}

#[test]
fn malformed_controls_are_actionable_errors_not_panics() {
    for value in [json!(null), json!({}), json!({"controlVersion":1}), json!({"controlVersion":1,"minimumReader":"x"})] {
        assert!(Control::decode(&value).is_err());
    }
    let mut c = Control::initial("library");
    c.state = State::Upgrading;
    assert!(c.validate().is_err());
    let mut c = Control::initial("library");
    c.active_metadata_root = "catalog/epochs/9".into();
    assert!(c.validate().is_err());
    let mut c = Control::initial("library");
    c.revision = u64::MAX;
    assert!(c.begin("m", Plan::Rebase, source(), "o", &tok(1), u64::MAX).is_err());
    assert!(translate_mutation(1, 2).is_err());
    translate_mutation(1, 1).unwrap();
}

#[test]
fn head_v4_carries_control_and_older_heads_read_as_active_epoch_one() {
    use crate::cloud_records::{Head, object_epoch, object_key};
    let hash = "c".repeat(64);
    assert_eq!(object_key("p", 1, "changes", &hash), format!("p/catalog/changes/{hash}.json"));
    assert_eq!(object_epoch("p", "changes", &format!("p/catalog/epochs/3/changes/{hash}.json"), &hash), Some(3));
    for bad in [
        format!("p/catalog/epochs/1/changes/{hash}.json"),
        format!("p/catalog/epochs/x/changes/{hash}.json"),
        format!("q/catalog/changes/{hash}.json"),
    ] {
        assert_eq!(object_epoch("p", "changes", &bad, &hash), None, "{bad}");
    }
    let tip = json!({"key":object_key("p", 1, "changes", &hash),"hash":hash,"size":10});
    let v3 = serde_json::to_vec(&json!({"version":3,"generation":2,"tip":tip,"library":"library"})).unwrap();
    let head = Head::decode(&v3).unwrap();
    head.validate_scope("p", "library").unwrap();
    assert_eq!(head.control().unwrap(), Control::initial("library"));
    let next = Head::next(Some(&head), head.tip.clone(), "library", None, head.control().unwrap()).unwrap();
    assert_eq!((next.version, next.generation), (4, 3));
    next.validate_scope("p", "library").unwrap();
    let missing = serde_json::to_vec(&json!({"version":4,"generation":2,"tip":tip,"library":"library"})).unwrap();
    assert!(Head::decode(&missing).unwrap().validate_scope("p", "library").is_err(), "v4 requires control");
    let mut wrong_epoch = next.clone();
    let mut control = Control::initial("library");
    control.epoch = 2;
    control.active_metadata_root = epoch_root(2);
    wrong_epoch.control = Some(control);
    assert!(wrong_epoch.validate_scope("p", "library").is_err(), "tip outside active epoch");
    let mut future = serde_json::to_value(&next).unwrap();
    future["control"]["controlVersion"] = json!(CONTROL_VERSION + 1);
    future["control"]["lease"] = json!("new field");
    let error = Head::decode(&serde_json::to_vec(&future).unwrap()).err().unwrap();
    assert!(error.contains("update LightCraft"), "{error}");
    future["version"] = json!(9);
    future["control"] = serde_json::to_value(Control::initial("library")).unwrap();
    assert!(Head::decode(&serde_json::to_vec(&future).unwrap()).err().unwrap().contains("newer LightCraft"));
}

#[test]
fn deleted_library_blocks_reads_publication_and_upgrade_and_cannot_reactivate() {
    for active in [Control::initial("library"), Control::initial_append("library")] {
        let deleted = active.delete().unwrap();
        assert_eq!(deleted.state, State::Deleted);
        assert!(matches!(deleted.may_read(), Err(Blocked::Deleted)));
        assert!(matches!(deleted.may_publish(), Err(Blocked::Deleted)));
        assert!(deleted.begin("upgrade", Plan::Rebase, Boundary { tip: "a".repeat(64), generation: 1 }, "device", &"b".repeat(64), 0).is_err());
        assert_eq!(deleted.delete().unwrap(), deleted);
        assert_eq!(Control::decode(&serde_json::to_value(&deleted).unwrap()).unwrap(), deleted);
        assert!(crate::cloud_append::control::publication(Ok(deleted)).is_err());
    }
}
