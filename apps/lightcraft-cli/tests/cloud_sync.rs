#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog as lightcraft_catalog;
use serde_json::{Value, json};
use std::path::Path;
use support::Fixture;

fn success(fixture: &Fixture, library: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (output, rows) = fixture.run(library, commands);
    assert!(output.status.success(), "{rows:?} {}", String::from_utf8_lossy(&output.stderr));
    rows
}
fn configure(fixture: &Fixture) -> Value {
    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":fixture.endpoint,"profile":"acceptance"})
}
fn copy_catalog_files(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for name in ["catalog.snap", "catalog.log"] {
        std::fs::copy(source.join(name), destination.join(name)).unwrap();
    }
}
fn saved_catalog(path: &Path) -> lightcraft_catalog::Catalog {
    lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(path).unwrap())).unwrap().1
}
#[test]
fn fresh_device_bootstraps_deleted_smart_album_targets_in_both_protocols() {
    for append in [false, true] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = append;
        let protocol = if append { "append-log-v1" } else { "conditional-head-v1" };
        let a = f.dir.join("deleted-target-source");
        let rows = success(&f, &a, &[("cloud.configure", configure(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol}))]);
        let library = rows[1]["result"]["library"].as_str().unwrap();
        f.import(&a, false);
        success(
            &f,
            &a,
            &[
                ("cloud.upload", json!({})),
                ("album.create", json!({"name":"Target"})),
                ("album.addPhotos", json!({"id":1,"ids":[1]})),
                ("album.createSmart", json!({"name":"In target","rules":{"album":1}})),
                ("album.createSmart", json!({"name":"Not in target","rules":{"ruleSet":{"rules":[{"field":"album","op":"isNot","value":"1"}]}}})),
                ("album.delete", json!({"id":1})),
                ("cloud.sync", json!({"checkpoint":true})),
            ],
        );
        let source = saved_catalog(&a);
        let target = source.cloud_identity("album:1").unwrap();
        let b = f.dir.join("fresh-deleted-target");
        success(
            &f,
            &b,
            &[
                ("cloud.configure", configure(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":protocol})),
                ("cloud.sync", json!({})),
            ],
        );
        let peer = saved_catalog(&b);
        assert_eq!(peer.photos().count(), 1);
        assert_eq!(peer.albums().count(), 2);
        assert!(peer.sync_state().unwrap().pending.is_empty());
        let retained = peer.cloud_identities().iter().find(|(_, global)| global.as_str() == target).unwrap().0;
        let retained = lightcraft_catalog::AlbumId(retained.strip_prefix("album:").unwrap().parse().unwrap());
        assert!(peer.album(retained).is_none());
        for album in peer.albums() {
            assert_eq!(peer.album_count(album.id), 0);
            assert_eq!(peer.smart_album_problems(album.id)[0].issue, lightcraft_catalog::rules::Issue::NoSuchAlbum);
        }
        success(&f, &b, &[("album.create", json!({"name":"New local album"}))]);
        assert!(saved_catalog(&b).album(retained).is_none(), "restart must not reuse a retained identity");
    }
}
#[test]
fn copied_conditional_catalog_uses_distinct_writers_for_independent_offline_edits() {
    let f = Fixture::new();
    let (a, _, _) = initialize(&f);
    let b = f.dir.join("copied-head");
    copy_catalog_files(&a, &b);
    let original = saved_catalog(&a).sync_state().unwrap().device.clone();
    success(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
    success(&f, &b, &[("cloud.configure", configure(&f)), ("photo.flag", json!({"ids":[1],"flag":"pick"}))]);
    assert_ne!(saved_catalog(&b).sync_state().unwrap().device, original);
    success(&f, &a, &[("cloud.sync", json!({}))]);
    success(&f, &b, &[("cloud.sync", json!({}))]);
    success(&f, &a, &[("cloud.sync", json!({}))]);
    for path in [&a, &b] {
        let catalog = saved_catalog(path);
        assert_eq!(catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 5);
        assert_eq!(catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().flag, lightcraft_catalog::Flag::Pick);
        assert!(catalog.sync_state().unwrap().pending.is_empty());
    }
}
#[test]
fn copied_head_preserves_historical_pending_ids_and_publishes_causal_order() {
    let f = Fixture::new();
    let (a, _, _) = initialize(&f);
    success(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":2}))]);
    let before = saved_catalog(&a).sync_state().unwrap().clone();
    let b = f.dir.join("copied-pending-head");
    copy_catalog_files(&a, &b);
    success(&f, &b, &[("cloud.configure", configure(&f)), ("photo.flag", json!({"ids":[1],"flag":"pick"}))]);
    let copied = saved_catalog(&b).sync_state().unwrap().clone();
    assert_ne!(copied.device, before.device);
    for (id, mutation) in &before.pending {
        assert_eq!(copied.pending.get(id), Some(mutation));
    }
    let new = copied.pending.values().find(|m| m.revision.device == copied.device).unwrap();
    assert_eq!(new.revision.sequence, 1);
    assert!(new.context.get(&before.device).copied().unwrap() > 1);
    success(&f, &b, &[("cloud.sync", json!({}))]);
    success(&f, &a, &[("cloud.sync", json!({}))]);
    let catalog = saved_catalog(&a);
    assert_eq!(catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 2);
    assert_eq!(catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().flag, lightcraft_catalog::Flag::Pick);
    let state = f.state.lock().unwrap();
    let batch = state
        .objects
        .iter()
        .filter(|(key, _)| key.contains("/catalog/changes/"))
        .map(|(_, bytes)| serde_json::from_slice::<Value>(bytes).unwrap())
        .find(|batch| batch["mutations"].as_array().is_some_and(|list| list.iter().any(|m| m["revision"]["device"] == copied.device)))
        .unwrap();
    let devices: Vec<_> = batch["mutations"].as_array().unwrap().iter().map(|m| m["revision"]["device"].as_str().unwrap()).collect();
    assert_eq!(devices, vec![before.device.as_str(), copied.device.as_str()]);
}
#[test]
fn older_same_path_backup_starts_a_new_writer_epoch_in_both_protocols() {
    for append in [false, true] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = append;
        let a = f.dir.join("same-path-backup");
        success(
            &f,
            &a,
            &[
                ("cloud.configure", configure(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","protocol":if append {"append-log-v1"} else {"conditional-head-v1"}})),
            ],
        );
        f.import(&a, false);
        success(&f, &a, &[("cloud.upload", json!({})), ("photo.rate", json!({"ids":[1],"rating":2}))]);
        let old = saved_catalog(&a).sync_state().unwrap().clone();
        let saved = ["catalog.snap", "catalog.log"].map(|name| (name, std::fs::read(a.join(name)).unwrap()));
        success(&f, &a, &[("photo.flag", json!({"ids":[1],"flag":"pick"})), ("cloud.sync", json!({}))]);
        for (name, bytes) in saved {
            std::fs::write(a.join(name), bytes).unwrap();
        }
        success(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
        let restored = saved_catalog(&a).sync_state().unwrap().clone();
        assert_ne!(restored.device, old.device);
        for (id, mutation) in &old.pending {
            assert_eq!(restored.pending.get(id), Some(mutation));
        }
        assert_eq!(restored.clock.get(&restored.device), Some(&1));
        success(&f, &a, &[("cloud.sync", json!({}))]);
        let catalog = saved_catalog(&a);
        assert_eq!(catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 4);
        assert_eq!(catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().flag, lightcraft_catalog::Flag::Pick);
        assert!(catalog.sync_state().unwrap().pending.is_empty());
    }
}
#[test]
fn actual_remote_rollback_with_pending_work_or_checkpoint_request_retains_local_state() {
    let f = Fixture::new();
    let (a, _, _) = initialize(&f);
    let old_head = f.state.lock().unwrap().objects["/photos/library/catalog/head.json"].clone();
    success(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":2})), ("cloud.sync", json!({})), ("photo.rate", json!({"ids":[1],"rating":4}))]);
    let before = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1;
    assert!(!before.sync_state().unwrap().pending.is_empty());
    f.state.lock().unwrap().objects.insert("/photos/library/catalog/head.json".into(), old_head);
    for params in [json!({}), json!({"checkpoint":true})] {
        let (output, rows) = f.run(&a, &[("cloud.sync", params)]);
        assert!(!output.status.success());
        assert!(rows[0]["error"].as_str().unwrap().contains("rolled back"), "{rows:?}");
        let after = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1;
        assert_eq!(after.to_snapshot(), before.to_snapshot());
    }
}
#[test]
fn unchanged_head_cli_polls_preserve_exact_catalog_files_and_only_read_the_head() {
    let f = Fixture::new();
    let (a, _, _) = initialize(&f);
    // Peer enablement publishes its own causal identity receipt, even when the
    // library identity value agrees. Receive that real progress before measuring.
    let received = success(&f, &a, &[("cloud.sync", json!({}))]);
    assert_eq!(received[0]["result"]["unchanged"], false);
    let saved = ["catalog.snap", "catalog.log"].map(|name| (name, std::fs::read(a.join(name)).unwrap()));
    let before = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1;
    let requests = f.state.lock().unwrap().requests.len();
    let rows = success(&f, &a, &[("cloud.sync", json!({})), ("cloud.sync", json!({})), ("cloud.sync", json!({}))]);
    assert!(rows.iter().all(|r| r["result"]["unchanged"] == true), "{rows:?}");
    for (name, bytes) in saved {
        assert_eq!(std::fs::read(a.join(name)).unwrap(), bytes, "{name}");
    }
    let after = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1;
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.to_snapshot(), before.to_snapshot());
    let state = f.state.lock().unwrap();
    let new = &state.requests[requests..];
    let marker = format!("/photos/{}", lightcraft_engine::cloud_append::manifest_key("library", &before.sync_state().unwrap().library).unwrap());
    // One migration-marker check and one mutable-head GET per poll. No mutation,
    // checkpoint, receipt archive, media or write requests on an unchanged head.
    assert_eq!(new.len(), 6, "{new:?}");
    assert!(new.iter().all(|(method, path)| method == "GET" && (path == "/photos/library/catalog/head.json" || path == &marker)), "{new:?}");
}
#[test]
fn archived_ack_and_missing_or_corrupt_archive_preserve_exact_pending_on_restart() {
    let f = Fixture::new();
    let (a, b, _) = initialize(&f);
    success(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":2}))]);
    let saved = ["catalog.snap", "catalog.log"].map(|name| (name, std::fs::read(a.join(name)).unwrap()));
    let pending =
        lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1.sync_state().unwrap().pending.clone();
    success(&f, &a, &[("cloud.sync", json!({}))]);
    success(&f, &b, &[("cloud.sync", json!({})), ("photo.rate", json!({"ids":[1],"rating":5})), ("cloud.sync", json!({"checkpoint":true}))]);
    let archives = {
        let mut s = f.state.lock().unwrap();
        let objects: Vec<_> = s.objects.iter().filter(|(key, _)| key.contains("/receipts/")).map(|(k, v)| (k.clone(), v.clone())).collect();
        assert!(!objects.is_empty());
        for (key, _) in &objects {
            s.objects.remove(key);
        }
        objects
    };
    for (name, bytes) in &saved {
        std::fs::write(a.join(name), bytes).unwrap();
    }
    let (out, rows) = f.run(&a, &[("cloud.sync", json!({}))]);
    assert!(!out.status.success());
    assert!(rows[0]["error"].to_string().contains("archive"), "{rows:?}");
    let after = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1;
    assert_eq!(after.sync_state().unwrap().pending, pending);
    {
        let mut s = f.state.lock().unwrap();
        for (key, _) in &archives {
            s.objects.insert(key.clone(), b"corrupt".to_vec());
        }
    }
    let (out, _) = f.run(&a, &[("cloud.sync", json!({}))]);
    assert!(!out.status.success());
    {
        let mut s = f.state.lock().unwrap();
        for (key, bytes) in archives {
            s.objects.insert(key, bytes);
        }
    }
    success(&f, &a, &[("cloud.sync", json!({}))]);
    let after = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1;
    assert!(after.sync_state().unwrap().pending.is_empty());
    assert_eq!(after.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 5);
}
fn initialize(fixture: &Fixture) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let a = fixture.dir.join("device-a");
    let b = fixture.dir.join("device-b");
    fixture.import(&a, false);
    let rows =
        success(fixture, &a, &[("cloud.upload", json!({})), ("cloud.syncEnable", json!({"connection":"personal"})), ("cloud.sync", json!({}))]);
    let library = rows[1]["result"]["library"].as_str().unwrap().to_owned();
    success(
        fixture,
        &b,
        &[
            ("cloud.configure", configure(fixture)),
            ("cloud.syncEnable", json!({"connection":"personal","library":library})),
            ("cloud.sync", json!({})),
        ],
    );
    (a, b, library)
}
#[test]
fn obsolete_pre_release_head_is_rejected_without_changing_catalog_or_pending_work() {
    let fixture = Fixture::new();
    let (a, _, _) = initialize(&fixture);
    success(&fixture, &a, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
    let (_, before, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    {
        let mut storage = fixture.state.lock().unwrap();
        let head = storage.objects.get_mut("/photos/library/catalog/head.json").unwrap();
        let mut obsolete: Value = serde_json::from_slice(head).unwrap();
        obsolete["version"] = json!(1);
        obsolete.as_object_mut().unwrap().remove("library");
        obsolete.as_object_mut().unwrap().remove("checkpoint");
        *head = serde_json::to_vec(&obsolete).unwrap();
    }
    let (output, rows) = fixture.run(&a, &[("cloud.sync", json!({}))]);
    assert!(!output.status.success());
    assert!(rows[0]["error"].as_str().unwrap().contains("obsolete pre-release"), "{rows:?}");
    let (_, after, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    assert_eq!(before.to_snapshot(), after.to_snapshot());
}

#[test]
fn enabled_conditional_imports_use_causal_author_instead_of_one_replica_per_photo() {
    let fixture = Fixture::new();
    let a = fixture.dir.join("causal-imports");
    success(&fixture, &a, &[("cloud.configure", configure(&fixture)), ("cloud.syncEnable", json!({"connection":"personal"}))]);
    for _ in 0..3 {
        fixture.import(&a, false);
        success(&fixture, &a, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
    }
    let (_, catalog, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    let state = catalog.sync_state().unwrap();
    assert!(state.pending.is_empty());
    assert_eq!(state.clock.len(), 1);
    assert!(state.clock.contains_key(&state.device));
    for entry in std::fs::read_dir(a.join("Managed/uploads")).unwrap() {
        let receipt: Value = serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
        assert_eq!(receipt["version"], 4);
        assert!(receipt["generation"].as_u64().unwrap() > 0);
        assert!(!receipt["publicationIntent"].as_object().unwrap().is_empty());
    }
    let objects = &fixture.state.lock().unwrap().objects;
    for (key, bytes) in objects.iter().filter(|(key, _)| key.contains("/catalog/changes/")) {
        let batch: Value = serde_json::from_slice(bytes).unwrap();
        assert!(batch["imports"].as_array().is_none_or(|imports| imports.is_empty()), "{key}: standalone legacy import unexpectedly published");
    }
}

#[test]
fn unuploaded_import_does_not_prevent_receiving_remote_changes_or_acknowledge_missing_media() {
    let fixture = Fixture::new();
    let (a, b, _) = initialize(&fixture);
    let (_, source) = fixture.import(&fixture.dir.join("seed"), true);
    let rows = success(&fixture, &a, &[("cloud.import", json!({"connection":"personal","paths":[source]}))]);
    let id = lightcraft_catalog::PhotoId(rows[0]["result"]["ids"][0].as_u64().unwrap());
    let before = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap().1;
    let import = before.upload_intents().iter().find(|(_, intent)| intent.photo_id == id).unwrap().0.clone();
    let waiting = before.sync_state().unwrap().pending.clone();
    success(&fixture, &b, &[("photo.rate", json!({"ids":[1],"rating":5})), ("cloud.sync", json!({}))]);
    success(&fixture, &a, &[("cloud.sync", json!({}))]);
    let (_, after, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    assert_eq!(after.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 5);
    assert_eq!(after.sync_state().unwrap().pending, waiting);
    success(
        &fixture,
        &a,
        &[
            ("photo.deletePermanently", json!({"ids":[after.upload_intents()[&import].photo_id.0]})),
            ("cloud.upload", json!({})),
            ("cloud.sync", json!({})),
        ],
    );
    let (_, after, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    assert!(after.sync_state().unwrap().pending.is_empty());
    assert_eq!(after.len(), 1);
}

#[test]
fn real_cli_reconciles_a_competing_head_and_lost_responses_without_replaying_edits() {
    let fixture = Fixture::new();
    let (a, b, _) = initialize(&fixture);
    let old_head = fixture.state.lock().unwrap().objects["/photos/library/catalog/head.json"].clone();
    success(&fixture, &b, &[("photo.rate", json!({"ids":[1],"rating":3})), ("cloud.sync", json!({}))]);
    {
        let mut state = fixture.state.lock().unwrap();
        let competing = state.objects["/photos/library/catalog/head.json"].clone();
        state.objects.insert("/photos/library/catalog/head.json".into(), old_head);
        state.next_head = Some(competing);
        state.lost_head = true;
        state.lost_put = true;
    }
    let rows = success(
        &fixture,
        &a,
        &[
            ("develop.set", json!({"id":1,"control":"light.exposure","value":0.75})),
            ("cloud.sync", json!({})),
            ("cloud.syncStatus", json!({})),
            ("photo.inspect", json!({"id":1})),
        ],
    );
    assert_eq!(rows[2]["result"]["pending"], 0);
    let rows = success(&fixture, &b, &[("cloud.sync", json!({})), ("photo.inspect", json!({"id":1}))]);
    let serialized = rows[1]["result"].to_string();
    assert!(serialized.contains("0.75"), "{rows:?}");
    let (_, a_catalog, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    let (_, b_catalog, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&b).unwrap())).unwrap();
    assert_eq!(a_catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 3);
    assert_eq!(a_catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().develop, b_catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().develop);
    let before = fixture.state.lock().unwrap().requests.iter().filter(|(method, key)| method == "PUT" && key.ends_with("catalog/head.json")).count();
    success(&fixture, &a, &[("cloud.sync", json!({}))]);
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.requests.iter().filter(|(method, key)| method == "PUT" && key.ends_with("catalog/head.json")).count(), before);
    assert_eq!(state.signature_errors, 0);
}
#[test]
fn checkpoint_rebootstrap_keeps_offline_mutations_after_covered_history_expires() {
    let fixture = Fixture::new();
    let (a, b, _) = initialize(&fixture);
    success(&fixture, &b, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
    success(&fixture, &a, &[("develop.set", json!({"id":1,"control":"light.exposure","value":0.5})), ("cloud.sync", json!({"checkpoint":true}))]);
    {
        let mut state = fixture.state.lock().unwrap();
        let head: Value = serde_json::from_slice(&state.objects["/photos/library/catalog/head.json"]).unwrap();
        let tip = format!("/photos/{}", head["tip"]["key"].as_str().unwrap());
        assert!(head.get("checkpoint").is_some());
        state.objects.retain(|key, _| !key.contains("/catalog/changes/") || key == &tip);
    }
    let rows = success(&fixture, &b, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
    assert_eq!(rows[1]["result"]["pending"], 0);
    success(&fixture, &a, &[("cloud.sync", json!({}))]);
    let (_, catalog, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    let photo = catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap();
    assert_eq!(photo.rating, 4);
    assert_eq!(photo.develop.light.exposure, 0.5);
    assert_eq!(fixture.state.lock().unwrap().signature_errors, 0);
}
#[test]
fn unsupported_conditional_publication_keeps_original_mutation_identities() {
    let fixture = Fixture::new();
    let (a, _, _) = initialize(&fixture);
    success(&fixture, &a, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
    let (_, before, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    fixture.state.lock().unwrap().unsupported_conditional = true;
    let (output, rows) = fixture.run(&a, &[("cloud.sync", json!({})), ("cloud.syncStatus", json!({}))]);
    assert!(!output.status.success());
    assert!(rows[0]["error"].to_string().contains("unsupported"), "{rows:?}");
    let (_, after, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&a).unwrap())).unwrap();
    assert_eq!(before.sync_state().unwrap().pending, after.sync_state().unwrap().pending);
    fixture.state.lock().unwrap().unsupported_conditional = false;
    success(&fixture, &a, &[("cloud.sync", json!({}))]);
}

#[test]
fn acknowledged_device_reads_only_new_batches_and_rebootstraps_when_cursor_expires() {
    let fixture = Fixture::new();
    let (a, b, _) = initialize(&fixture);
    let old_head: Value = serde_json::from_slice(&fixture.state.lock().unwrap().objects["/photos/library/catalog/head.json"]).unwrap();
    let old_tip = format!("/photos/{}", old_head["tip"]["key"].as_str().unwrap());
    success(&fixture, &a, &[("photo.rate", json!({"ids":[1],"rating":2})), ("cloud.sync", json!({}))]);
    fixture.state.lock().unwrap().requests.clear();
    success(&fixture, &b, &[("cloud.sync", json!({}))]);
    assert!(!fixture.state.lock().unwrap().requests.iter().any(|(method, key)| method == "GET" && key == &old_tip));
    success(&fixture, &a, &[("photo.rate", json!({"ids":[1],"rating":4})), ("cloud.sync", json!({"checkpoint":true}))]);
    {
        let mut state = fixture.state.lock().unwrap();
        let head: Value = serde_json::from_slice(&state.objects["/photos/library/catalog/head.json"]).unwrap();
        let tip = format!("/photos/{}", head["tip"]["key"].as_str().unwrap());
        state.objects.retain(|key, _| !key.contains("/catalog/changes/") || key == &tip);
    }
    success(&fixture, &b, &[("cloud.sync", json!({}))]);
    let (_, catalog, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(&b).unwrap())).unwrap();
    assert_eq!(catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 4);
}
