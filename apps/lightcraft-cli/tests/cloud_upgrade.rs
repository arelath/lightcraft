//! Real-binary library upgrades through the conditional head's maintenance state:
//! commits racing maintenance, takeover after a crashed upgrader, a lost activation
//! response, cancellation, and offline devices rejoining with unchanged pending work.
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{self as lightcraft_catalog, PhotoId};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use support::Fixture;

const HEAD: &str = "/photos/library/catalog/head.json";
fn ok(f: &Fixture, path: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (out, rows) = f.run(path, commands);
    assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
    rows
}
fn catalog(path: &Path) -> lightcraft_catalog::Catalog {
    lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(path).unwrap())).unwrap().1
}
fn head(f: &Fixture) -> Value {
    serde_json::from_slice(&f.state.lock().unwrap().objects[HEAD]).unwrap()
}
fn pending(path: &Path) -> Value {
    serde_json::to_value(&catalog(path).sync_state().unwrap().pending).unwrap()
}
fn initialize(f: &Fixture) -> (PathBuf, PathBuf) {
    let a = f.dir.join("device-a");
    let b = f.dir.join("device-b");
    f.import(&a, false);
    let rows = ok(f, &a, &[("cloud.upload", json!({})), ("cloud.syncEnable", json!({"connection":"personal"})), ("cloud.sync", json!({}))]);
    let library = rows[1]["result"]["library"].as_str().unwrap().to_owned();
    let config = json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"});
    ok(f, &b, &[("cloud.configure", config), ("cloud.syncEnable", json!({"connection":"personal","library":library})), ("cloud.sync", json!({}))]);
    (a, b)
}
/// A maintenance head written by another (crashed) upgrader: a marker batch after the
/// frozen source and an Upgrading control with that device's fencing token.
fn upgrading_head(f: &Fixture, lease_until_ms: u64) -> Vec<u8> {
    let current = head(f);
    let marker = serde_json::to_vec(&json!({"version":3,"parent":current["tip"],"imports":[],"checkpointMarker":true})).unwrap();
    let hash = support::hash(&marker);
    let key = format!("library/catalog/changes/{hash}.json");
    f.state.lock().unwrap().objects.insert(format!("/photos/{key}"), marker.clone());
    let mut control = current["control"].clone();
    assert_eq!(control["state"], "active", "the library starts publishable");
    control["state"] = json!("upgrading");
    control["controlVersion"] = json!(2);
    control["revision"] = json!(control["revision"].as_u64().unwrap() + 1);
    control["migration"] = json!({
        "id": "rebase-crashed", "plan": "rebase", "targetEpoch": 2, "targetSchema": 1,
        "source": {"tip": current["tip"]["hash"], "generation": current["generation"]},
        "owner": "crashed-device", "token": "ab".repeat(32), "leaseUntilMs": lease_until_ms,
        "exposure": "preparing"
    });
    let mut next = current.clone();
    next["generation"] = json!(current["generation"].as_u64().unwrap() + 1);
    next["tip"] = json!({"key":key,"hash":hash,"size":marker.len()});
    next["control"] = control;
    serde_json::to_vec(&next).unwrap()
}

#[test]
fn unsupported_expired_migrations_leave_remote_objects_and_pending_work_unchanged() {
    use lightcraft_engine::cloud_control::{Control, SCHEMA};
    let f = Fixture::new();
    let (a, _) = initialize(&f);
    ok(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":3}))]);
    let offline = pending(&a);
    let maintenance = upgrading_head(&f, 0);
    for plan in ["rebase", "appendLog"] {
        let mut newer: Value = serde_json::from_slice(&maintenance).unwrap();
        newer["control"]["migration"]["plan"] = json!(plan);
        newer["control"]["migration"]["targetSchema"] = json!(SCHEMA + 1);
        Control::decode(&newer["control"]).unwrap().may_read().unwrap();
        f.state.lock().unwrap().objects.insert(HEAD.into(), serde_json::to_vec(&newer).unwrap());
        for request in [json!({"plan":plan}), json!({"plan":plan,"writersStopped":true}), json!({"cancel":true})] {
            let (objects, request_count) = {
                let state = f.state.lock().unwrap();
                (state.objects.clone(), state.requests.len())
            };
            let snapshot = catalog(&a).to_snapshot();
            let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", request), ("cloud.syncStatus", json!({}))]);
            assert!(!out.status.success(), "{rows:?}");
            assert!(rows[0]["error"].as_str().unwrap().contains("update LightCraft to complete"), "{rows:?}");
            let health = &rows[1]["result"]["health"];
            assert_eq!(health["lastError"]["kind"], "unsupported", "{rows:?}");
            assert_eq!(health["retry"]["scheduled"], false, "upgrade needs a capable application");
            let state = f.state.lock().unwrap();
            assert_eq!(state.objects, objects, "no ownership transition, checkpoint, seed or activation");
            assert!(state.requests.iter().skip(request_count).all(|(method, _)| method == "GET"), "refusal happens before any remote write");
            assert_eq!(state.signature_errors, 0);
            drop(state);
            assert_eq!(pending(&a), offline);
            assert_eq!(catalog(&a).to_snapshot(), snapshot, "failed upgrades preserve the catalog");
        }
    }
}

#[test]
fn commit_racing_maintenance_stays_pending_then_takeover_rebases_and_offline_work_rejoins() {
    let f = Fixture::new();
    let (a, b) = initialize(&f);
    ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":3}))]);
    let offline = pending(&b);
    assert_eq!(offline.as_object().unwrap().len(), 1);
    // Maintenance wins the CAS immediately before B's prepared commit.
    let maintenance = upgrading_head(&f, 0);
    f.state.lock().unwrap().next_head = Some(maintenance.clone());
    let rows = ok(&f, &b, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
    assert!(rows[0]["result"]["publicationBlocked"].as_str().unwrap().contains("maintenance"), "{rows:?}");
    assert!(rows[1]["result"]["publicationBlocked"].as_str().unwrap().contains("maintenance"), "status shows the pause: {rows:?}");
    assert_eq!(f.state.lock().unwrap().objects[HEAD], maintenance, "the stale commit did not replace the maintenance head");
    assert_eq!(pending(&b), offline, "blocked publication keeps the exact pending mutation");

    // The crashed upgrader's lease has expired: A takes over the same migration and
    // loses the activation response, which it resolves by rereading the head.
    f.state.lock().unwrap().lost_head_matching = Some("\"activeMetadataRoot\":\"catalog/epochs/2\"".into());
    let rows = ok(&f, &a, &[("cloud.upgradeLibrary", json!({"plan":"rebase"}))]);
    assert_eq!(rows[0]["result"]["upgraded"], true, "{rows:?}");
    assert!(f.state.lock().unwrap().lost_head_matching.is_none(), "activation response was lost");
    let active = head(&f);
    assert_eq!(active["control"]["state"], "active");
    assert_eq!(active["control"]["epoch"], 2);
    assert_eq!(active["control"]["activeMetadataRoot"], "catalog/epochs/2");
    assert!(active["tip"]["key"].as_str().unwrap().starts_with("library/catalog/epochs/2/changes/"));
    assert!(f.state.lock().unwrap().objects.keys().any(|k| k.contains("/catalog/changes/")), "old epoch retained");

    // The offline device detects the new epoch, rebootstraps and publishes its
    // preserved mutation with the original identity and payload.
    let rows = ok(&f, &b, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
    assert_eq!(rows[1]["result"]["pending"], 0, "{rows:?}");
    let tip = head(&f);
    let batch: Value = serde_json::from_slice(&f.state.lock().unwrap().objects[&format!("/photos/{}", tip["tip"]["key"].as_str().unwrap())]).unwrap();
    let published = &batch["mutations"][0];
    let (id, original) = offline.as_object().unwrap().iter().next().unwrap();
    assert_eq!(published, original, "mutation {id} kept its identity and digest across the epoch change");
    ok(&f, &a, &[("cloud.sync", json!({}))]);
    assert_eq!(catalog(&a).photo(PhotoId(1)).unwrap().rating, 3);
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
}

#[test]
fn concurrent_edit_after_activation_never_advances_cursor_without_its_state() {
    for lost_response in [false, true] {
        let f = Fixture::new();
        let (a, b) = initialize(&f);
        ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":3}))]);
        let offline = pending(&b);
        {
            let mut state = f.state.lock().unwrap();
            state.after_activation_mutation = Some(offline.as_object().unwrap().values().next().unwrap().clone());
            if lost_response {
                state.lost_head_matching = Some("\"activeMetadataRoot\":\"catalog/epochs/2\"".into());
            }
        }
        ok(&f, &a, &[("cloud.upgradeLibrary", json!({"plan":"rebase"}))]);
        assert!(f.state.lock().unwrap().after_activation_mutation.is_none(), "the concurrent edit was accepted");
        let current = catalog(&a);
        let remote = head(&f);
        if current.sync_state().unwrap().cursor.as_deref() == remote["tip"]["hash"].as_str() {
            assert_eq!(current.photo(PhotoId(1)).unwrap().rating, 3, "an advanced cursor must include the concurrent edit; lost={lost_response}");
        }
        ok(&f, &a, &[("cloud.sync", json!({})), ("cloud.sync", json!({}))]);
        assert_eq!(catalog(&a).photo(PhotoId(1)).unwrap().rating, 3, "a clean poll must not permanently skip the edit");
        assert_eq!(pending(&b), offline, "the other device's exact pending mutation is preserved");
        assert!(f.state.lock().unwrap().lost_head_matching.is_none());
        assert_eq!(f.state.lock().unwrap().signature_errors, 0);
    }
}

#[test]
fn held_lease_refuses_other_upgraders_and_cancellation_restores_the_same_epoch() {
    let f = Fixture::new();
    let (a, b) = initialize(&f);
    let far = 4_000_000_000_000u64;
    let maintenance = upgrading_head(&f, far);
    f.state.lock().unwrap().objects.insert(HEAD.into(), maintenance.clone());
    for request in [json!({"plan":"rebase"}), json!({"cancel":true}), json!({})] {
        let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", request), ("cloud.syncStatus", json!({}))]);
        assert!(!out.status.success(), "{rows:?}");
        assert!(rows[0]["error"].as_str().unwrap().contains("maintenance"), "{rows:?}");
        // Another device holding maintenance is contention, not an invalid library.
        let last = &rows[1]["result"]["health"]["lastError"];
        assert_eq!(last["kind"], "contention", "{rows:?}");
        // An automatic retry would only poll, so a refused upgrade waits for the user.
        assert_eq!((last["retryable"].clone(), rows[1]["result"]["health"]["retry"]["scheduled"].clone()), (json!(false), json!(false)), "{rows:?}");
        assert_eq!(f.state.lock().unwrap().objects[HEAD], maintenance, "refused requests leave the head unchanged");
    }
    // After the lease expires another device may cancel; the epoch is unchanged.
    let expired = upgrading_head_with_lease(&f, &maintenance, 0);
    f.state.lock().unwrap().objects.insert(HEAD.into(), expired);
    ok(&f, &a, &[("cloud.upgradeLibrary", json!({"cancel":true}))]);
    let restored = head(&f);
    assert_eq!((restored["control"]["state"].clone(), restored["control"]["epoch"].clone()), (json!("active"), json!(1)));
    assert!(restored["control"].get("migration").is_none());
    ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":2})), ("cloud.sync", json!({}))]);
    assert!(pending(&b).as_object().unwrap().is_empty());
}
fn upgrading_head_with_lease(_: &Fixture, bytes: &[u8], lease: u64) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    value["control"]["migration"]["leaseUntilMs"] = json!(lease);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn cancellation_after_possible_successor_exposure_refuses_without_erasing_and_retry_finishes() {
    // The checkpoint is the complete committed-history seed in this protocol.
    for stage in ["before-manifest", "after-manifest", "after-seed"] {
        let f = Fixture::new();
        let (a, b) = initialize(&f);
        ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
        let offline = pending(&b);
        let library = catalog(&a).sync_state().unwrap().library.clone();
        let root = format!("/photos/library/sync-v2/{library}/");
        let manifest = format!("/photos/{}", lightcraft_engine::cloud_append::manifest_key("library", &library).unwrap());
        let fault = match stage {
            "before-manifest" => (manifest.clone(), String::new()),
            "after-manifest" => (format!("{root}checkpoints/"), String::new()),
            _ => (HEAD.into(), "\"state\":\"retired\"".into()),
        };
        {
            let mut state = f.state.lock().unwrap();
            state.append_mode = true;
            state.refuse_put = Some(fault);
            if stage == "before-manifest" {
                state.lost_head_matching = Some("\"exposure\":\"publishing\"".into());
            }
        }
        let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", json!({"plan":"appendLog"}))]);
        assert!(!out.status.success(), "{stage}: {rows:?}");
        let maintenance = head(&f);
        assert_eq!(maintenance["control"]["state"], "upgrading", "{stage}");
        assert_eq!(maintenance["control"]["migration"]["exposure"], "publishing", "{stage}");
        let (objects, count) = {
            let state = f.state.lock().unwrap();
            let seeded = state.objects.keys().any(|k| k.starts_with(&format!("{root}checkpoints/")));
            assert!(state.lost_head_matching.is_none(), "uncertain exposure CAS was resolved");
            assert_eq!(state.objects.contains_key(&manifest), stage != "before-manifest", "{stage}");
            assert_eq!(seeded, stage == "after-seed", "{stage}");
            (state.objects.clone(), state.requests.len())
        };
        let snapshot = catalog(&a).to_snapshot();
        let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", json!({"cancel":true})), ("cloud.syncStatus", json!({}))]);
        assert!(!out.status.success(), "{stage}: {rows:?}");
        assert!(rows[0]["error"].as_str().unwrap().contains("cancellation is unsafe"), "{stage}: {rows:?}");
        let failure = &rows[1]["result"]["health"]["lastError"];
        assert_eq!(failure["kind"], "unsupported", "{rows:?}");
        assert!(failure["action"].as_str().unwrap().contains("finish the same upgrade"), "{rows:?}");
        assert_eq!(rows[1]["result"]["health"]["retry"]["scheduled"], false);
        {
            let state = f.state.lock().unwrap();
            assert_eq!(state.objects, objects, "cancellation preserves every object at {stage}");
            assert!(state.requests.iter().skip(count).all(|(method, _)| method == "GET"));
        }
        assert_eq!(catalog(&a).to_snapshot(), snapshot, "local catalog retained");
        f.state.lock().unwrap().refuse_put = None;
        ok(&f, &a, &[("cloud.upgradeLibrary", json!({"plan":"appendLog"}))]);
        assert_eq!(head(&f)["control"]["state"], "retired", "retry finishes {stage}");
        {
            let state = f.state.lock().unwrap();
            for (key, bytes) in &objects {
                if key != HEAD {
                    assert_eq!(state.objects.get(key), Some(bytes), "output retained: {key}");
                }
            }
            assert!(!state.requests.iter().any(|(method, _)| method == "DELETE"));
            assert_eq!(state.signature_errors, 0);
        }
        assert_eq!(pending(&b), offline);
        ok(&f, &b, &[("cloud.upgradeLibrary", json!({})), ("cloud.sync", json!({}))]);
        ok(&f, &a, &[("cloud.sync", json!({}))]);
        assert!(pending(&b).as_object().unwrap().is_empty());
        assert_eq!(catalog(&a).photo(PhotoId(1)).unwrap().rating, 4);
    }
}

#[test]
fn cancellation_before_output_latch_restores_old_authority_but_legacy_unknown_exposure_cannot() {
    let f = Fixture::new();
    let (a, _) = initialize(&f);
    f.state.lock().unwrap().refuse_put = Some((HEAD.into(), "\"exposure\":\"publishing\"".into()));
    let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", json!({"plan":"appendLog"}))]);
    assert!(!out.status.success(), "{rows:?}");
    assert_eq!(head(&f)["control"]["migration"]["exposure"], "preparing");
    f.state.lock().unwrap().refuse_put = None;
    ok(&f, &a, &[("cloud.upgradeLibrary", json!({"cancel":true}))]);
    assert_eq!(head(&f)["control"]["state"], "active");
    assert!(!f.state.lock().unwrap().objects.keys().any(|k| k.contains("/sync-v2/")));
    // A v1 upgrader carried no exposure marker. Absence of visible successor data
    // cannot prove its paused worker will not publish after a lease takeover.
    let mut legacy: Value = serde_json::from_slice(&upgrading_head(&f, 0)).unwrap();
    legacy["control"]["controlVersion"] = json!(1);
    legacy["control"]["migration"].as_object_mut().unwrap().remove("exposure");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    f.state.lock().unwrap().objects.insert(HEAD.into(), bytes.clone());
    let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", json!({"cancel":true}))]);
    assert!(!out.status.success(), "{rows:?}");
    assert!(rows[0]["error"].as_str().unwrap().contains("cancellation is unsafe"), "{rows:?}");
    assert_eq!(f.state.lock().unwrap().objects[HEAD], bytes);
    ok(&f, &a, &[("cloud.upgradeLibrary", json!({"plan":"rebase"}))]);
    assert_eq!(head(&f)["control"]["epoch"], 2);
}

#[test]
fn future_control_head_preserves_typed_update_recovery() {
    let f = Fixture::new();
    let (a, _) = initialize(&f);
    let mut future = head(&f);
    future["control"]["controlVersion"] = json!(lightcraft_engine::cloud_control::CONTROL_VERSION + 1);
    future["control"]["newField"] = json!(true);
    f.state.lock().unwrap().objects.insert(HEAD.into(), serde_json::to_vec(&future).unwrap());
    let snapshot = catalog(&a).to_snapshot();
    let (out, rows) = f.run(&a, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
    assert!(!out.status.success(), "{rows:?}");
    let failure = &rows[1]["result"]["health"]["lastError"];
    assert_eq!(failure["kind"], "unsupported", "{rows:?}");
    assert!(failure["action"].as_str().unwrap().starts_with("Update LightCraft"), "{rows:?}");
    assert_eq!(rows[1]["result"]["health"]["retry"]["scheduled"], false);
    assert_eq!(catalog(&a).to_snapshot(), snapshot);
}

#[test]
fn interrupted_rebase_cannot_cancel_after_output_authorization_and_resumes_same_epoch() {
    for namespace in ["changes", "checkpoints"] {
        let f = Fixture::new();
        let (a, _) = initialize(&f);
        f.state.lock().unwrap().refuse_put = Some((format!("/catalog/epochs/2/{namespace}/"), String::new()));
        let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", json!({"plan":"rebase"}))]);
        assert!(!out.status.success(), "{namespace}: {rows:?}");
        let frozen = head(&f);
        assert_eq!(frozen["control"]["migration"]["exposure"], "publishing");
        let objects = f.state.lock().unwrap().objects.clone();
        let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", json!({"cancel":true}))]);
        assert!(!out.status.success(), "{rows:?}");
        assert!(rows[0]["error"].as_str().unwrap().contains("cancellation is unsafe"), "{rows:?}");
        assert_eq!(f.state.lock().unwrap().objects, objects);
        f.state.lock().unwrap().refuse_put = None;
        ok(&f, &a, &[("cloud.upgradeLibrary", json!({"plan":"rebase"}))]);
        let active = head(&f);
        assert_eq!(active["control"]["epoch"], 2);
        assert_eq!(active["control"]["state"], "active");
        for (key, bytes) in &objects {
            if key != HEAD {
                assert_eq!(f.state.lock().unwrap().objects.get(key), Some(bytes));
            }
        }
        assert_eq!(f.state.lock().unwrap().signature_errors, 0);
    }
}

#[test]
fn append_migration_retires_the_head_and_offline_devices_rejoin_with_unchanged_pending_ids() {
    let f = Fixture::new();
    let (a, b) = initialize(&f);
    f.state.lock().unwrap().append_mode = true;
    ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
    let offline = pending(&b);
    let rows = ok(&f, &a, &[("cloud.upgradeLibrary", json!({})), ("cloud.syncStatus", json!({}))]);
    assert_eq!(rows[0]["result"]["migrated"], true, "{rows:?}");
    let retired = head(&f);
    assert_eq!(retired["control"]["state"], "retired");
    assert_eq!(retired["control"]["successor"]["protocol"], "append-log-v1");
    assert!(catalog(&a).sync_state().unwrap().protocol == lightcraft_catalog::replication::SyncProtocol::AppendLog);
    // A head-mode commit is refused before anything is published; work stays pending.
    let (out, rows) = f.run(&b, &[("cloud.sync", json!({}))]);
    assert!(!out.status.success());
    assert!(rows[0]["error"].as_str().unwrap().contains("cloud.upgradeLibrary"), "{rows:?}");
    assert_eq!(pending(&b), offline);
    let head_puts = f.state.lock().unwrap().requests.iter().filter(|(m, k)| m == "PUT" && k.ends_with("catalog/head.json")).count();
    // Rejoining writes nothing remotely and keeps the exact pending mutations.
    ok(&f, &b, &[("cloud.upgradeLibrary", json!({}))]);
    assert_eq!(pending(&b), offline);
    assert!(catalog(&b).sync_state().unwrap().protocol == lightcraft_catalog::replication::SyncProtocol::AppendLog);
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    assert!(pending(&b).as_object().unwrap().is_empty());
    ok(&f, &a, &[("cloud.sync", json!({}))]);
    assert_eq!(catalog(&a).photo(PhotoId(1)).unwrap().rating, 4);
    let state = f.state.lock().unwrap();
    assert_eq!(
        state.requests.iter().filter(|(m, k)| m == "PUT" && k.ends_with("catalog/head.json")).count(),
        head_puts,
        "no head writes after retirement"
    );
    assert_eq!(serde_json::from_slice::<Value>(&state.objects[HEAD]).unwrap(), retired);
}

#[test]
fn append_control_records_pause_or_refuse_publication_while_receiving_and_keep_pending_work() {
    use lightcraft_engine::{cloud_append::control, cloud_control::Control};
    let f = Fixture::new();
    {
        let mut s = f.state.lock().unwrap();
        s.append_mode = true;
        s.unsupported_conditional = true;
    }
    let a = f.dir.join("append-control");
    let config = json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"});
    ok(&f, &a, &[("cloud.configure", config), ("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"}))]);
    f.import_unsynchronized(&a, false);
    ok(&f, &a, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
    let library = catalog(&a).sync_state().unwrap().library.clone();
    ok(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":2}))]);
    let offline = pending(&a);
    assert_eq!(offline.as_object().unwrap().len(), 1);
    let publish = |c: &Control| {
        let key = format!("/photos/{}", control::key("library", c).unwrap());
        f.state.lock().unwrap().objects.insert(key.clone(), control::encode(c).unwrap());
        key
    };
    // A newer writer schema: this build still receives, but publishes nothing.
    let mut newer_writer = Control::initial_append(&library);
    newer_writer.revision = 1;
    newer_writer.schema = lightcraft_engine::cloud_control::WRITER + 1;
    newer_writer.minimum_writer = lightcraft_engine::cloud_control::WRITER + 1;
    let first = publish(&newer_writer);
    let ops = |f: &Fixture| f.state.lock().unwrap().requests.iter().filter(|(m, k)| m == "PUT" && k.contains("/ops/")).count();
    let before = ops(&f);
    let rows = ok(&f, &a, &[("cloud.sync", json!({}))]);
    assert!(rows[0]["result"]["publicationBlocked"].as_str().unwrap().contains("update LightCraft to publish"), "{rows:?}");
    assert_eq!(ops(&f), before, "no operation published while blocked");
    assert_eq!(pending(&a), offline);
    // A newer reader requirement refuses the scan without touching the catalog.
    let mut value = serde_json::to_value(Control { revision: 2, ..newer_writer.clone() }).unwrap();
    value["minimumReader"] = json!(lightcraft_engine::cloud_control::READER + 1);
    let bytes = serde_json::to_vec(&value).unwrap();
    let second = format!("/photos/library/sync-v2/{library}/control/{:020}-{}.json", 2, support::hash(&bytes));
    f.state.lock().unwrap().objects.insert(second.clone(), bytes);
    let snapshot = catalog(&a).to_snapshot();
    let (out, rows) = f.run(&a, &[("cloud.sync", json!({}))]);
    assert!(!out.status.success());
    assert!(rows[0]["error"].as_str().unwrap().contains("update LightCraft to read"), "{rows:?}");
    assert_eq!(catalog(&a).to_snapshot(), snapshot);
    // A valid readable envelope does not authorize this epoch-one append runner
    // to use its old namespace or discovery cursors after an epoch/root change.
    for (revision, epoch, root) in [
        (3, 2, lightcraft_engine::cloud_control::append_root(&library, 2)),
        (4, 1, lightcraft_engine::cloud_control::epoch_root(1)),
        (5, 2, lightcraft_engine::cloud_control::epoch_root(2)),
    ] {
        let control = Control { revision, epoch, active_metadata_root: root, ..Control::initial_append(&library) };
        control.may_read().unwrap();
        control.may_publish().unwrap();
        let key = publish(&control);
        let puts = f.state.lock().unwrap().requests.iter().filter(|(m, _)| m == "PUT").count();
        let objects = f.state.lock().unwrap().objects.clone();
        let (out, rows) = f.run(&a, &[("cloud.sync", json!({"checkpoint":true}))]);
        assert!(!out.status.success(), "{rows:?}");
        assert!(rows[0]["error"].as_str().unwrap().contains("namespace"), "{rows:?}");
        assert_eq!(f.state.lock().unwrap().requests.iter().filter(|(m, _)| m == "PUT").count(), puts, "no manifest, operation or checkpoint PUT");
        assert_eq!(f.state.lock().unwrap().objects, objects);
        assert_eq!(catalog(&a).to_snapshot(), snapshot);
        assert_eq!(pending(&a), offline);
        f.state.lock().unwrap().objects.remove(&key);
    }
    // A later Active record (as a newer upgrader would write) resumes publication.
    f.state.lock().unwrap().objects.remove(&second);
    f.state.lock().unwrap().objects.remove(&first);
    let rows = ok(&f, &a, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
    assert!(rows[0]["result"].get("publicationBlocked").is_none(), "{rows:?}");
    assert_eq!(rows[1]["result"]["pending"], 0);
}

#[test]
fn simultaneous_upgraders_elect_one_owner_and_the_loser_changes_nothing() {
    let f = Fixture::new();
    let (a, _) = initialize(&f);
    // Another installation's begin wins the CAS immediately before A's begin.
    let winner = upgrading_head(&f, 4_000_000_000_000);
    f.state.lock().unwrap().next_head = Some(winner.clone());
    let (out, rows) = f.run(&a, &[("cloud.upgradeLibrary", json!({"plan":"rebase"}))]);
    assert!(!out.status.success(), "{rows:?}");
    assert!(rows[0]["error"].as_str().unwrap().contains("maintenance"), "{rows:?}");
    assert_eq!(f.state.lock().unwrap().objects[HEAD], winner, "only the winner's migration is authoritative");
    assert!(!f.state.lock().unwrap().objects.keys().any(|k| k.contains("/catalog/epochs/")), "the loser built no epoch");
}

#[test]
fn signed_transport_failures_preserve_http_and_update_remedies_in_health() {
    let f = Fixture::new();
    let (a, _) = initialize(&f);
    ok(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
    let offline = pending(&a);
    for code in [401_u16, 403] {
        f.state.lock().unwrap().reject_status = Some(code);
        let (out, rows) = f.run(&a, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
        assert!(!out.status.success());
        let health = &rows[1]["result"]["health"];
        assert_eq!(health["lastError"]["kind"], "authentication", "{rows:?}");
        assert_eq!(health["lastError"]["httpStatus"], code);
        assert_eq!(health["lastError"]["recovery"], "editConnection");
        assert_eq!(health["retry"]["scheduled"], false);
        assert_eq!(pending(&a), offline);
    }
    f.state.lock().unwrap().reject_status = None;
    let mut future = head(&f);
    future["control"]["minimumReader"] = json!(lightcraft_engine::cloud_control::READER + 1);
    f.state.lock().unwrap().objects.insert(HEAD.into(), serde_json::to_vec(&future).unwrap());
    let before = catalog(&a).to_snapshot();
    let (out, rows) = f.run(&a, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
    assert!(!out.status.success());
    let health = &rows[1]["result"]["health"];
    assert_eq!(health["lastError"]["kind"], "unsupported", "{rows:?}");
    assert_eq!(health["lastError"]["recovery"], "update");
    assert_eq!(health["retry"]["scheduled"], false);
    assert_eq!(catalog(&a).to_snapshot(), before);
    assert_eq!(pending(&a), offline);
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
}
