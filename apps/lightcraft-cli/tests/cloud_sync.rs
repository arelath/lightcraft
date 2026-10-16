#[path = "support/cloud_scale.rs"]
mod scale;
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog as lightcraft_catalog;
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
    for name in ["catalog.snap", "catalog.log", "cloud-connections.json"] {
        std::fs::copy(source.join(name), destination.join(name)).unwrap();
    }
}
fn saved_catalog(path: &Path) -> lightcraft_catalog::Catalog {
    lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::FsStore::open(path).unwrap())).unwrap().1
}
// Manufacture valid immutable marker batches without thousands of CLI launches.
// The real reader still validates every hash, link, signature and checkpoint.
fn extend_history(f: &Fixture, count: usize) -> Value {
    let mut storage = f.state.lock().unwrap();
    let key = "/photos/library/catalog/head.json";
    let mut head: Value = serde_json::from_slice(&storage.objects[key]).unwrap();
    for _ in 0..count {
        let bytes = serde_json::to_vec(&json!({"version":3,"parent":head["tip"],"imports":[],"checkpointMarker":true})).unwrap();
        let hash = support::hash(&bytes);
        let key = format!("library/catalog/changes/{hash}.json");
        head["tip"] = json!({"key":key,"hash":hash,"size":bytes.len()});
        head["generation"] = json!(head["generation"].as_u64().unwrap() + 1);
        storage.objects.insert(format!("/photos/{key}"), bytes);
    }
    storage.objects.insert(key.into(), serde_json::to_vec(&head).unwrap());
    head
}
#[test]
fn overlong_head_history_checkpoints_verified_prefixes_and_resumes_without_losing_pending_work() {
    let f = Fixture::new();
    let (a, b, library) = initialize(&f);
    let old = extend_history(&f, 4097);
    success(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
    let pending = saved_catalog(&a).sync_state().unwrap().pending.clone();
    f.state.lock().unwrap().stop_after_recovery_prefix = true;
    let (out, rows) = f.run(&a, &[("cloud.sync", json!({"checkpoint":true}))]);
    assert!(!out.status.success(), "{rows:?}");
    let prefix: Value = serde_json::from_slice(&f.state.lock().unwrap().objects["/photos/library/catalog/head.json"]).unwrap();
    assert!(prefix["checkpoint"]["generation"].as_u64().unwrap() < old["generation"].as_u64().unwrap());
    assert_ne!(prefix["checkpoint"]["covers"], old["tip"]);
    assert_eq!(saved_catalog(&a).sync_state().unwrap().pending, pending);
    {
        let mut storage = f.state.lock().unwrap();
        storage.unauthorized = false;
        storage.requests.clear();
    }
    // Ordinary publication finishes maintenance after a process restart too.
    success(&f, &a, &[("cloud.sync", json!({"checkpoint":true}))]);
    assert!(saved_catalog(&a).sync_state().unwrap().pending.is_empty());
    let storage = f.state.lock().unwrap();
    let final_head: Value = serde_json::from_slice(&storage.objects["/photos/library/catalog/head.json"]).unwrap();
    assert!(final_head["checkpoint"]["generation"].as_u64().unwrap() >= old["generation"].as_u64().unwrap());
    assert!(
        storage.requests.iter().filter(|(method, key)| method == "GET" && key.contains("/catalog/changes/")).count() < 20,
        "completed prefix must not be traversed on retry"
    );
    drop(storage);
    let fresh = f.dir.join("fresh-after-overlong");
    success(
        &f,
        &fresh,
        &[("cloud.configure", configure(&f)), ("cloud.syncEnable", json!({"connection":"personal","library":library})), ("cloud.sync", json!({}))],
    );
    assert_eq!(saved_catalog(&fresh).photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 5);
    success(&f, &b, &[("photo.flag", json!({"ids":[1],"flag":"pick"})), ("cloud.sync", json!({}))]);
    success(&f, &a, &[("cloud.sync", json!({}))]);
    assert_eq!(saved_catalog(&a).photo(lightcraft_catalog::PhotoId(1)).unwrap().flag, lightcraft_catalog::Flag::Pick);
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
}
#[test]
fn an_unchanged_head_automatically_checkpoints_before_the_normal_tail_boundary() {
    use lightcraft_catalog::{FsStore, Journal, Op};
    let f = Fixture::new();
    let (a, _, _) = initialize(&f);
    let head = extend_history(&f, 1024);
    // Marker-only history changes no document. Model an already caught-up idle
    // device to exercise automatic maintenance through the unchanged fast path.
    let (mut journal, mut catalog, _) = Journal::open(Box::new(FsStore::open(&a).unwrap())).unwrap();
    let mut state = catalog.sync_state().unwrap().clone();
    state.acknowledge(&Default::default(), head["tip"]["hash"].as_str().unwrap().into(), head["generation"].as_u64().unwrap()).unwrap();
    let op = Op::SetSyncState { state: Some(Box::new(state)) };
    catalog.apply(op.clone()).unwrap();
    journal.append(&[op]).unwrap();
    drop(journal);
    success(&f, &a, &[("cloud.sync", json!({}))]);
    let storage = f.state.lock().unwrap();
    let current: Value = serde_json::from_slice(&storage.objects["/photos/library/catalog/head.json"]).unwrap();
    assert_eq!(current["checkpoint"]["covers"], head["tip"]);
    assert_eq!(current["checkpoint"]["generation"], head["generation"]);
    assert_eq!(storage.signature_errors, 0);
}

#[test]
fn shared_upload_ack_finalizes_receipts_after_restart_without_media_requests() {
    use lightcraft_catalog::uploads::UploadState;
    for append in [false, true] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = append;
        let protocol = if append { "append-log-v1" } else { "conditional-head-v1" };
        let a = f.dir.join("shared-receipts");
        success(&f, &a, &[("cloud.configure", configure(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol}))]);
        let paths: Vec<_> = (0..2)
            .map(|n| {
                let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, n * 100, 255]);
                let path = f.dir.join(format!("receipt-{n}.png"));
                std::fs::write(&path, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap())
                    .unwrap();
                path
            })
            .collect();
        success(&f, &a, &[("cloud.import", json!({"connection":"personal","paths":paths}))]);
        let before = saved_catalog(&a);
        let imports: Vec<_> = before.upload_intents().keys().cloned().collect();
        assert_eq!(imports.len(), 2);
        let first = &imports[0];
        let photo = before.upload_intents()[first].photo_id;
        let rows = success(&f, &a, &[("cloud.upload", json!({"imports":[first]}))]);
        assert_eq!(rows[0]["result"]["results"][0]["publicationPending"], true);
        assert_eq!(saved_catalog(&a).upload_intents()[first].state, UploadState::ObjectsVerified);
        let path = a.join("Managed/uploads").join(format!("{first}.json"));
        let held = f.dir.join("held-receipt.json");
        let unfinished: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(unfinished["generation"].is_null());
        assert!(unfinished.get("appendProof").is_none());
        std::fs::rename(&path, &held).unwrap();
        std::fs::create_dir(&path).unwrap(); // Force a local completion error, not an upload retry.
        success(&f, &a, &[("cloud.upload", json!({"imports":[imports[1]]}))]);
        let acknowledged = saved_catalog(&a);
        let intent = &acknowledged.upload_intents()[first];
        assert_eq!(intent.state, UploadState::Synchronized);
        let proof = intent.finalization.clone().unwrap();
        assert!(!proof.proofs.is_empty());
        assert!(acknowledged.sync_state().unwrap().pending.is_empty());
        let (requests, objects) = {
            let state = f.state.lock().unwrap();
            (state.requests.len(), state.objects.clone())
        };
        let blocked = success(&f, &a, &[("cloud.removeLocalCopy", json!({"ids":[photo]}))]);
        assert!(!blocked[0]["result"]["failed"].as_array().unwrap().is_empty());
        assert_eq!(saved_catalog(&a).upload_intents()[first].finalization.as_ref(), Some(&proof));
        let asset = acknowledged.asset(&intent.asset_id).unwrap();
        assert!(a.join("Managed/blobs").join(&asset.original.id.0).is_file(), "failed finalization keeps staging protected");
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&held, &path).unwrap();
        let removed = success(&f, &a, &[("cloud.removeLocalCopy", json!({"ids":[photo]}))]);
        assert!(removed[0]["result"]["failed"].as_array().unwrap().is_empty(), "{removed:?}");
        assert!(!a.join("Managed/blobs").join(&asset.original.id.0).exists());
        let done: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(done["state"], "synchronized");
        for (id, digest) in &proof.proofs {
            assert_eq!(done["appendProof"][id], digest.as_str());
        }
        if let Some(generation) = proof.generation {
            assert!(done["generation"].as_u64().unwrap() >= generation);
        }
        let complete = saved_catalog(&a);
        assert!(complete.upload_intents()[first].finalization.is_none());
        assert!(complete.upload_intents()[first].error.is_none());
        assert!(complete.sync_state().unwrap().pending.is_empty());
        let state = f.state.lock().unwrap();
        assert_eq!(state.requests.len(), requests, "receipt retry and removal must make zero remote requests and transfer zero network bytes");
        assert_eq!(state.objects, objects);
        assert_eq!(state.signature_errors, 0);
    }
}

#[test]
fn overlapping_shared_stacks_converge_with_a_device_local_member_in_both_protocols() {
    use lightcraft_catalog::{FsStore, Journal, Op, PhotoId};
    for append in [false, true] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = append;
        let protocol = if append { "append-log-v1" } else { "conditional-head-v1" };
        let a = f.dir.join("stack-a");
        let b = f.dir.join("stack-b");
        let c = f.dir.join("stack-c");
        let rows = success(&f, &a, &[("cloud.configure", configure(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol}))]);
        let library = rows[1]["result"]["library"].as_str().unwrap();
        for _ in 0..4 {
            f.import_unsynchronized(&a, false);
        }
        success(&f, &a, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
        let origin = saved_catalog(&a);
        let globals: Vec<_> = (1..=4).map(|id| origin.cloud_identity(&format!("photo:{id}")).unwrap().to_owned()).collect();
        let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 200, 255]);
        let local = f.dir.join("stack-local.png");
        std::fs::write(&local, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap()).unwrap();
        success(&f, &b, &[("library.import", json!({"paths":[local]}))]);
        for path in [&b, &c] {
            success(
                &f,
                path,
                &[
                    ("cloud.configure", configure(&f)),
                    ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":protocol})),
                    ("cloud.sync", json!({})),
                ],
            );
        }
        for (path, global, members) in [(&a, "aaa", vec![0, 1]), (&b, "bbb", vec![1, 2]), (&c, "ccc", vec![2, 3])] {
            // Freeze lexical identity order in the fixture, while creating and
            // publishing the actual proposals through the real command path.
            let (mut journal, mut catalog, _) = Journal::open(Box::new(FsStore::open(path).unwrap())).unwrap();
            let mapping = Op::SetCloudIdentity { entity: "stack:1".into(), id: Some(global.into()) };
            catalog.apply(mapping.clone()).unwrap();
            journal.append(&[mapping]).unwrap();
            journal.snapshot(&catalog).unwrap();
            drop(journal);
            let mut ids: Vec<_> = members
                .into_iter()
                .map(|index| {
                    catalog
                        .cloud_identities()
                        .iter()
                        .find(|(key, value)| key.starts_with("photo:") && *value == &globals[index])
                        .unwrap()
                        .0
                        .strip_prefix("photo:")
                        .unwrap()
                        .parse::<u64>()
                        .unwrap()
                })
                .collect();
            if path == &b {
                ids.push(1);
            }
            success(
                &f,
                path,
                &[("library.select", json!({"ids":ids,"active":ids[0]})), ("stack.group", json!({"ids":ids,"top":ids[0],"collapsed":false}))],
            );
        }
        for path in [&a, &b, &c, &a, &b, &c] {
            success(&f, path, &[("cloud.sync", json!({}))]);
        }
        let expected = BTreeMap::from([
            ("aaa".to_owned(), vec![globals[0].clone(), globals[1].clone()]),
            ("ccc".to_owned(), vec![globals[2].clone(), globals[3].clone()]),
        ]);
        for path in [&a, &b, &c] {
            let catalog = saved_catalog(path);
            let shared: BTreeMap<_, _> = catalog
                .stacks()
                .filter_map(|stack| {
                    let photos: Vec<_> =
                        stack.photos.iter().filter_map(|id| catalog.cloud_identity(&format!("photo:{}", id.0))).map(str::to_owned).collect();
                    (photos.len() >= 2).then(|| (catalog.cloud_identity(&format!("stack:{}", stack.id.0)).unwrap().to_owned(), photos))
                })
                .collect();
            assert_eq!(shared, expected);
            assert!(catalog.sync_state().unwrap().pending.is_empty());
            if path == &b {
                assert!(catalog.photo(PhotoId(1)).is_some());
                assert!(catalog.stack_of(PhotoId(1)).is_none(), "local singleton must not keep a shared singleton alive");
            }
        }
    }
}

#[test]
fn self_sync_preserves_nonportable_smart_rules_and_shares_explicit_empty_selection() {
    for append in [false, true] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = append;
        let protocol = if append { "append-log-v1" } else { "conditional-head-v1" };
        let a = f.dir.join("smart-origin");
        let b = f.dir.join("smart-peer");
        let rows = success(&f, &a, &[("cloud.configure", configure(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol}))]);
        let library = rows[1]["result"]["library"].as_str().unwrap();
        f.import(&a, false);
        success(&f, &a, &[("cloud.upload", json!({}))]);
        let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 200, 255]);
        let local = f.dir.join("private.png");
        std::fs::write(&local, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap()).unwrap();
        success(
            &f,
            &a,
            &[
                ("library.import", json!({"paths":[local]})),
                ("photo.rate", json!({"ids":[1,2],"rating":4})),
                ("album.createSmart", json!({"name":"Folder","rules":{"folder":f.dir,"rating":4}})),
                ("album.createSmart", json!({"name":"Local selection","rules":{"only":[2]}})),
                ("album.createSmart", json!({"name":"Mixed selection","rules":{"only":[1,2]}})),
                ("album.createSmart", json!({"name":"Dependent","rules":{"ruleSet":{"rules":[{"field":"album","op":"is","value":1}]}}})),
                ("album.createSmart", json!({"name":"Empty selection","rules":{"only":[],"onlyRestricted":true}})),
            ],
        );
        let before = saved_catalog(&a);
        let originals: Vec<_> = before.albums().map(|album| (album.id, album.smart.clone(), before.album_photos(album.id))).collect();
        assert_eq!(before.album_photos(lightcraft_catalog::AlbumId(1)), vec![lightcraft_catalog::PhotoId(2)]);
        success(&f, &a, &[("cloud.sync", json!({})), ("cloud.sync", json!({"checkpoint":true}))]);
        let after = saved_catalog(&a);
        for (id, rules, matches) in originals {
            assert_eq!(after.album(id).unwrap().smart, rules);
            assert_eq!(after.album_photos(id), matches);
        }
        assert_eq!(after.sync_state().unwrap().version, 3);
        success(
            &f,
            &b,
            &[
                ("cloud.configure", configure(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":protocol})),
                ("cloud.sync", json!({})),
                ("library.import", json!({"paths":[local]})),
            ],
        );
        let peer = saved_catalog(&b);
        assert_eq!(peer.photos().count(), 2);
        assert_eq!(peer.albums().count(), 1);
        let empty = peer.albums().next().unwrap();
        assert_eq!(empty.name, "Empty selection");
        assert_eq!(peer.album_count(empty.id), 0);
        assert!(empty.smart.as_ref().unwrap().only_restricted);
        assert!(peer.sync_state().unwrap().pending.is_empty());
    }
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
    // A read-only join does not publish another identity receipt. The creator's
    // first subsequent poll is already unchanged, as are the measured polls.
    let received = success(&f, &a, &[("cloud.sync", json!({}))]);
    assert_eq!(received[0]["result"]["unchanged"], true);
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
#[ignore = "explicit CR9 signed-provider 10k/50k scale acceptance"]
fn scale_unchanged_head_requests_journal_bytes_and_owner_latency() {
    for count in [10_000, 50_000] {
        let f = Fixture::new();
        let (a, _, _) = initialize(&f);
        let base = scale::seed(&f, &a, count);
        let state_bytes = serde_json::to_vec(base.sync_state().unwrap()).unwrap().len();
        let saved = ["catalog.snap", "catalog.log"].map(|name| (name, std::fs::read(a.join(name)).unwrap()));
        let request_start = f.state.lock().unwrap().requests.len();
        let rows = success(&f, &a, &[("cloud.sync", json!({})), ("cloud.sync", json!({})), ("cloud.sync", json!({}))]);
        assert!(rows.iter().all(|row| row["result"]["unchanged"] == true), "{rows:?}");
        let owner: Vec<_> = rows.iter().map(|row| row["result"]["ownerApplyMs"].as_f64().unwrap()).collect();
        for (name, bytes) in &saved {
            assert_eq!(std::fs::read(a.join(name)).unwrap(), *bytes, "{name}");
        }
        let (request_count, response_bytes) = {
            let storage = f.state.lock().unwrap();
            let requests = &storage.requests[request_start..];
            let marker =
                format!("/photos/{}", lightcraft_engine::cloud_append::manifest_key("library", &base.sync_state().unwrap().library).unwrap());
            assert_eq!(requests.len(), 6, "{requests:?}");
            assert!(
                requests.iter().all(|(method, path)| method == "GET" && (path.ends_with("/catalog/head.json") || path == &marker)),
                "{requests:?}"
            );
            assert_eq!(storage.signature_errors, 0);
            let response_bytes: u64 = storage.traffic[request_start..].iter().map(|request| request.response_bytes).sum();
            assert!(response_bytes < 16 << 10, "unchanged polls fetched {response_bytes} bytes");
            (requests.len(), response_bytes)
        };
        // Nonwaiting dispatch measures owner startup (snapshot/ownership/local
        // persistence and spawn) separately from network and result application.
        // Closing this read-only CLI session cancels its background poll safely.
        let startup = success(&f, &a, &[("cloud.sync", json!({"wait":false}))]);
        assert_eq!(startup[0]["result"]["background"], true);
        let startup_ms = startup[0]["ms"].as_f64().unwrap();
        let before_bytes = std::fs::metadata(a.join("catalog.log")).unwrap().len();
        let changed = success(&f, &a, &[("photo.rate", json!({"ids":[2],"rating":4})), ("cloud.sync", json!({})), ("library.info", json!({}))]);
        // The CLI compacts on close. Observe bytes in library.info before that
        // final snapshot, rather than mistaking a reset log for a zero-byte edit.
        let growth = changed[2]["result"]["logBytes"].as_u64().unwrap() - before_bytes;
        assert!(growth > 0 && growth < 40_000, "{count} photos wrote {growth} bytes");
        let restored = saved_catalog(&a);
        assert_eq!(restored.photo(lightcraft_catalog::PhotoId(2)).unwrap().rating, 4);
        assert!(restored.sync_state().unwrap().pending.is_empty());
        assert_eq!(restored.photos().count(), count);
        println!(
            "CR9 native photos={count} state_bytes={state_bytes} catalog_snapshot_bytes={} polls=3 GETs={request_count} response_bytes={response_bytes} unchanged_journal_growth=0 startup_dispatch_wall_ms={startup_ms} unchanged_owner_apply_ms={owner:?} edit_plus_ack_journal_bytes={growth} changed_owner_ms={} persistence={}",
            saved[0].1.len(),
            changed[1]["result"]["ownerApplyMs"],
            changed[2]["result"]["persistence"]
        );
    }
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
#[test]
fn compacted_history_bootstrap_fetches_each_archive_node_once_at_small_and_large_sizes() {
    use lightcraft_catalog::replication::{Change, Mutation, Register, Stamp, SyncState};
    use std::collections::BTreeSet;
    let mut counts = Vec::new();
    for count in [64u64, 4096] {
        let f = Fixture::new();
        let (a, _, library) = initialize(&f);
        success(&f, &a, &[("cloud.sync", json!({"checkpoint":true}))]);
        {
            let mut storage = f.state.lock().unwrap();
            let mut head: Value = serde_json::from_slice(&storage.objects["/photos/library/catalog/head.json"]).unwrap();
            let path = format!("/photos/{}", head["checkpoint"]["object"]["key"].as_str().unwrap());
            let mut checkpoint: Value = serde_json::from_slice(&storage.objects[&path]).unwrap();
            let portable = serde_json::from_value(checkpoint["state"].clone()).unwrap();
            let mut state = SyncState::restore_durable(&portable, "remote".into(), "personal".into()).unwrap();
            let context = state.clock.clone();
            let entity = state.documents.keys().find(|entity| entity.starts_with("photo:")).unwrap().clone();
            let mut mutations = Vec::new();
            for sequence in 1..=count {
                let mut context = context.clone();
                if sequence > 1 {
                    context.insert("scale".into(), sequence - 1);
                }
                let mutation = Mutation {
                    version: 2,
                    library: library.clone(),
                    revision: Stamp { device: "scale".into(), sequence },
                    context,
                    changes: vec![Change { entity: entity.clone(), field: "rating".into(), base: None, value: json!(4) }],
                };
                mutation.validate().unwrap();
                let receipt = lightcraft_engine::cloud_history::receipt(&mutation).unwrap();
                let id = mutation.revision.id();
                state.received.insert(id.clone(), receipt);
                state.documents.get_mut(&entity).unwrap().insert("rating".into(), Register(BTreeMap::from([(id, json!(4))])));
                mutations.push(mutation);
            }
            state.clock.insert("scale".into(), count);
            state.validate().unwrap();
            let plan = state.archive_plan().unwrap();
            state.compact_receipts(&plan).unwrap();
            for (hash, node) in plan.objects {
                let key = lightcraft_engine::cloud_history::key("library", &library, &hash).unwrap();
                storage.objects.insert(format!("/photos/{key}"), serde_json::to_vec(&node).unwrap());
            }
            let mut covers = head["tip"].clone();
            let mut generation = head["generation"].as_u64().unwrap();
            for chunk in mutations.chunks(128) {
                let bytes = serde_json::to_vec(&json!({"version":3,"parent":covers,"imports":[],"mutations":chunk})).unwrap();
                assert!(bytes.len() <= 2 << 20);
                let hash = support::hash(&bytes);
                covers = json!({"key":format!("library/catalog/changes/{hash}.json"),"hash":hash,"size":bytes.len()});
                storage.objects.insert(format!("/photos/{}", covers["key"].as_str().unwrap()), bytes);
                generation += 1;
            }
            checkpoint["state"] = serde_json::to_value(state.portable()).unwrap();
            checkpoint["generation"] = json!(generation);
            checkpoint["covers"] = covers.clone();
            let bytes = serde_json::to_vec(&checkpoint).unwrap();
            let hash = support::hash(&bytes);
            let object = json!({"key":format!("library/catalog/checkpoints/{hash}.json"),"hash":hash,"size":bytes.len()});
            storage.objects.insert(format!("/photos/{}", object["key"].as_str().unwrap()), bytes);
            head["checkpoint"] = json!({"generation":generation,"covers":covers,"object":object});
            let marker = serde_json::to_vec(&json!({"version":3,"parent":covers,"imports":[],"checkpointMarker":true})).unwrap();
            let hash = support::hash(&marker);
            let tip = json!({"key":format!("library/catalog/changes/{hash}.json"),"hash":hash,"size":marker.len()});
            storage.objects.insert(format!("/photos/{}", tip["key"].as_str().unwrap()), marker);
            head["tip"] = tip;
            head["generation"] = json!(generation + 1);
            storage.objects.insert("/photos/library/catalog/head.json".into(), serde_json::to_vec(&head).unwrap());
            storage.requests.clear();
        }
        let fresh = f.dir.join("fresh-scale");
        success(
            &f,
            &fresh,
            &[
                ("cloud.configure", configure(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","library":library})),
                ("cloud.sync", json!({})),
            ],
        );
        assert_eq!(saved_catalog(&fresh).photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 4);
        let storage = f.state.lock().unwrap();
        let gets: Vec<_> =
            storage.requests.iter().filter(|(method, path)| method == "GET" && path.contains("/receipts/")).map(|(_, path)| path).collect();
        assert!(!gets.is_empty());
        assert_eq!(gets.len(), gets.iter().copied().collect::<BTreeSet<_>>().len(), "{gets:?}");
        assert!(gets.len() <= count.ilog2() as usize + 16, "{gets:?}");
        println!("compacted_receipts={count} unique_archive_GETs={}", gets.len());
        counts.push(gets.len());
    }
    assert_eq!(counts[1] - counts[0], 6, "proof traffic grows with tree depth, not history length");
}
