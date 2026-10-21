#[path = "support/cloud_history.rs"]
mod history;
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{Catalog, FsStore, Journal, PhotoId};
use serde_json::{Value, json};
use std::path::Path;
use support::Fixture;

fn catalog(path: &Path) -> Catalog {
    Journal::open(Box::new(FsStore::open(path).unwrap())).unwrap().1
}
fn run(f: &Fixture, path: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (out, rows) = f.run(path, commands);
    assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
    rows
}
fn initialize(f: &Fixture) -> std::path::PathBuf {
    let path = f.dir.join("reader");
    f.import(&path, false);
    run(f, &path, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
    path
}
fn clear(f: &Fixture) {
    let mut storage = f.state.lock().unwrap();
    storage.requests.clear();
    storage.traffic.clear();
}
fn scratch_command(f: &Fixture, path: &Path, scratch: &Path) -> std::process::Command {
    let mut command = f.command();
    command.env("TEMP", scratch).env("TMP", scratch).env("TMPDIR", scratch);
    command.args(["run", "--library"]).arg(path).args(["cloud.sync", "{\"checkpoint\":true}"]);
    command
}

#[test]
fn incremental_replay_fetches_each_new_batch_once_and_keeps_unchanged_polls_empty() {
    for count in [10, 100, 1000] {
        let f = Fixture::new();
        let path = initialize(&f);
        let old: Value = serde_json::from_slice(&f.state.lock().unwrap().objects["/photos/library/catalog/head.json"]).unwrap();
        let sizes = history::extend_markers(&f, count);
        clear(&f);
        run(&f, &path, &[("cloud.sync", json!({}))]);
        history::assert_cost(&f, &sizes, 1);
        let old_tip = format!("/photos/{}", old["tip"]["key"].as_str().unwrap());
        assert!(!f.state.lock().unwrap().requests.iter().any(|(method, key)| method == "GET" && key == &old_tip));
        let before = catalog(&path).to_snapshot();
        clear(&f);
        run(&f, &path, &[("cloud.sync", json!({}))]);
        history::assert_cost(&f, &sizes, 0);
        assert_eq!(catalog(&path).to_snapshot(), before);
    }
}

#[test]
fn an_incremental_window_overflow_checkpoints_its_certified_oldest_prefix_without_refetching_it() {
    let f = Fixture::new();
    let path = initialize(&f);
    let photo = catalog(&path).photo(PhotoId(1)).unwrap().clone();
    let mut sizes = history::extend_markers(&f, 4097);
    let head: Value = serde_json::from_slice(&f.state.lock().unwrap().objects["/photos/library/catalog/head.json"]).unwrap();
    let newest = format!("/photos/{}", head["tip"]["key"].as_str().unwrap());
    sizes.remove(&newest).unwrap();
    clear(&f);
    run(&f, &path, &[("cloud.sync", json!({}))]);
    history::assert_cost(&f, &sizes, 1);
    let after = catalog(&path);
    assert_eq!(after.photo(PhotoId(1)).unwrap(), &photo);
    assert!(after.sync_state().unwrap().pending.is_empty());
    let storage = f.state.lock().unwrap();
    let current: Value = serde_json::from_slice(&storage.objects["/photos/library/catalog/head.json"]).unwrap();
    assert!(current["checkpoint"]["generation"].as_u64().unwrap() >= head["generation"].as_u64().unwrap() - 1);
    // The last uncovered batch is visited by the initial scan and the read after
    // publishing the prefix checkpoint. Its new anchor skips a redundant cursor scan.
    assert_eq!(storage.traffic.iter().filter(|request| request.method == "GET" && request.target == newest).count(), 2);
}

#[test]
fn pending_edits_use_a_separate_remote_baseline_and_fetch_new_history_only_once() {
    for count in [10, 100, 1000] {
        let f = Fixture::new();
        let path = initialize(&f);
        let sizes = history::extend_markers(&f, count);
        run(&f, &path, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
        assert_eq!(catalog(&path).sync_state().unwrap().pending.len(), 1);
        clear(&f);
        run(&f, &path, &[("cloud.sync", json!({}))]);
        // New remote work is fetched once; confirming our publication starts from
        // that certified baseline, independently of the original pending overlay.
        history::assert_cost(&f, &sizes, 1);
        let final_catalog = catalog(&path);
        assert!(final_catalog.sync_state().unwrap().pending.is_empty());
        assert_eq!(final_catalog.photo(PhotoId(1)).unwrap().rating, 5);
    }
}

#[test]
fn warm_edits_after_restart_do_not_download_acknowledged_history_at_any_tail_length() {
    for count in [10, 100, 1000] {
        let f = Fixture::new();
        let path = initialize(&f);
        let sizes = history::extend_markers(&f, count);
        run(&f, &path, &[("cloud.sync", json!({}))]);
        for rating in [4, 5] {
            run(&f, &path, &[("photo.rate", json!({"ids":[1],"rating":rating}))]);
            clear(&f);
            run(&f, &path, &[("cloud.sync", json!({}))]);
            history::assert_cost(&f, &sizes, 0);
            let storage = f.state.lock().unwrap();
            let change_gets =
                storage.traffic.iter().filter(|request| request.method == "GET" && request.target.contains("/catalog/changes/")).count();
            assert_eq!(change_gets, 1, "history={count}: freshly confirm only the newly published batch");
            drop(storage);
            let after = catalog(&path);
            assert_eq!(after.photo(PhotoId(1)).unwrap().rating, rating);
            assert!(after.sync_state().unwrap().pending.is_empty());
        }
    }
}

#[test]
fn lost_or_corrupt_baselines_keep_pending_work_and_fall_back_to_certified_history() {
    for corrupt in [false, true] {
        let f = Fixture::new();
        let path = initialize(&f);
        let sizes = history::extend_markers(&f, 100);
        run(&f, &path, &[("cloud.sync", json!({}))]);
        let root = path.join("sync-record-cache-v1");
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "body") {
                if corrupt {
                    std::fs::write(path, b"broken cache body").unwrap();
                } else {
                    std::fs::remove_file(path).unwrap();
                }
            }
        }
        run(&f, &path, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
        let pending = catalog(&path).sync_state().unwrap().pending.clone();
        assert_eq!(pending.len(), 1);
        clear(&f);
        run(&f, &path, &[("cloud.sync", json!({}))]);
        history::assert_cost(&f, &sizes, 1);
        let after = catalog(&path);
        assert!(after.sync_state().unwrap().pending.is_empty());
        for (id, mutation) in pending {
            assert_eq!(after.sync_state().unwrap().received[&id].digest, mutation.digest().unwrap());
        }
        assert_eq!(after.photo(PhotoId(1)).unwrap().rating, 4);
    }
}

#[test]
fn a_warm_baseline_handles_competing_and_uncertain_publication_without_replaying_the_old_tail() {
    let f = Fixture::new();
    let a = initialize(&f);
    let b = f.dir.join("competitor");
    let library = catalog(&a).sync_state().unwrap().library.clone();
    run(
        &f,
        &b,
        &[
            (
                "cloud.configure",
                json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
            ),
            ("cloud.syncEnable", json!({"connection":"personal","library":library})),
            ("cloud.sync", json!({})),
        ],
    );
    let sizes = history::extend_markers(&f, 1000);
    run(&f, &a, &[("cloud.sync", json!({}))]);
    run(&f, &b, &[("cloud.sync", json!({}))]);
    let old_head = f.state.lock().unwrap().objects["/photos/library/catalog/head.json"].clone();
    run(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":3})), ("cloud.sync", json!({}))]);
    {
        let mut storage = f.state.lock().unwrap();
        let competing = storage.objects["/photos/library/catalog/head.json"].clone();
        storage.objects.insert("/photos/library/catalog/head.json".into(), old_head);
        storage.next_head = Some(competing);
        storage.lost_head = true;
        storage.lost_put = true;
    }
    run(&f, &a, &[("develop.set", json!({"id":1,"control":"light.exposure","value":0.75}))]);
    let pending = catalog(&a).sync_state().unwrap().pending.clone();
    clear(&f);
    run(&f, &a, &[("cloud.sync", json!({}))]);
    history::assert_cost(&f, &sizes, 0);
    let after = catalog(&a);
    assert!(after.sync_state().unwrap().pending.is_empty());
    for (id, mutation) in pending {
        assert_eq!(after.sync_state().unwrap().received[&id].digest, mutation.digest().unwrap());
    }
    assert_eq!(after.photo(PhotoId(1)).unwrap().rating, 3);
    assert_eq!(after.photo(PhotoId(1)).unwrap().develop.light.exposure, 0.75);
    run(&f, &b, &[("cloud.sync", json!({}))]);
    assert_eq!(catalog(&b).photo(PhotoId(1)).unwrap().develop, after.photo(PhotoId(1)).unwrap().develop);
}

#[test]
fn a_new_checkpoint_at_the_baseline_boundary_cannot_hide_checkpoint_only_recovery_edits() {
    use lightcraft_engine::catalog::assets::BlobId;
    use lightcraft_engine::catalog::replication::{Change, SyncState};
    use lightcraft_engine::cloud_records::{Checkpoint, CheckpointRef, Head, RecordRef};
    let f = Fixture::new();
    let path = initialize(&f);
    history::extend_markers(&f, 100);
    run(&f, &path, &[("cloud.sync", json!({}))]);
    let saved = catalog(&path);
    let local = saved.sync_state().unwrap();
    let mut recovered = SyncState::restore_durable(&local.portable(), "checkpoint-writer".into(), "personal".into()).unwrap();
    let entity = format!("photo:{}", saved.cloud_identity("photo:1").unwrap());
    let base = recovered.winning(&entity, "meta.caption").map(|(stamp, _)| stamp);
    recovered.enqueue(vec![Change { entity, field: "meta.caption".into(), base, value: json!("Recovered through the new checkpoint") }]).unwrap();
    run(&f, &path, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
    {
        let mut storage = f.state.lock().unwrap();
        let old: Head = serde_json::from_slice(&storage.objects["/photos/library/catalog/head.json"]).unwrap();
        let checkpoint =
            Checkpoint { version: 1, generation: old.generation, covers: old.tip.clone(), state: recovered.portable(), imports: Default::default() };
        let bytes = serde_json::to_vec(&checkpoint).unwrap();
        let hash = support::hash(&bytes);
        let object = RecordRef { key: format!("library/catalog/checkpoints/{hash}.json"), hash: BlobId(hash), size: bytes.len() as u64 };
        storage.objects.insert(format!("/photos/{}", object.key), bytes);
        let bytes = serde_json::to_vec(&json!({"version":3,"parent":old.tip,"imports":[],"checkpointMarker":true})).unwrap();
        let hash = support::hash(&bytes);
        let tip = RecordRef { key: format!("library/catalog/changes/{hash}.json"), hash: BlobId(hash), size: bytes.len() as u64 };
        storage.objects.insert(format!("/photos/{}", tip.key), bytes);
        let head = Head::next(
            Some(&old),
            tip,
            &local.library,
            Some(CheckpointRef { object, covers: old.tip.clone(), generation: old.generation }),
            old.control().unwrap(),
        )
        .unwrap();
        storage.objects.insert("/photos/library/catalog/head.json".into(), serde_json::to_vec(&head).unwrap());
    }
    run(&f, &path, &[("cloud.sync", json!({}))]);
    let after = catalog(&path);
    assert_eq!(after.photo(PhotoId(1)).unwrap().meta.caption, "Recovered through the new checkpoint");
    assert_eq!(after.photo(PhotoId(1)).unwrap().rating, 4);
    assert!(after.sync_state().unwrap().pending.is_empty());
}

#[test]
fn missing_or_corrupt_history_retains_pending_and_remote_objects_and_retry_cleans_scratch() {
    for missing in [false, true] {
        let f = Fixture::new();
        let path = initialize(&f);
        let sizes = history::extend_markers(&f, 20);
        run(&f, &path, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
        let before = catalog(&path).to_snapshot();
        let key = sizes.keys().nth(7).unwrap().clone();
        let bytes = f.state.lock().unwrap().objects.remove(&key).unwrap();
        if !missing {
            f.state.lock().unwrap().objects.insert(key.clone(), b"damaged remote record".to_vec());
        }
        let remote = f.state.lock().unwrap().objects.clone();
        let scratch = f.dir.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        clear(&f);
        let output = scratch_command(&f, &path, &scratch).output().unwrap();
        assert!(!output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert_eq!(catalog(&path).to_snapshot(), before);
        assert_eq!(f.state.lock().unwrap().objects, remote);
        assert!(!f.state.lock().unwrap().requests.iter().any(|(method, _)| method == "PUT"));
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        f.state.lock().unwrap().objects.insert(key, bytes);
        clear(&f);
        let output = scratch_command(&f, &path, &scratch).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        history::assert_cost(&f, &sizes, 1);
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        let after = catalog(&path);
        assert!(after.sync_state().unwrap().pending.is_empty());
        assert_eq!(after.photo(PhotoId(1)).unwrap().rating, 4);
    }
}

#[test]
fn one_read_uses_its_verified_snapshot_and_the_next_fresh_read_detects_remote_replacement() {
    let f = Fixture::new();
    let path = initialize(&f);
    let sizes = history::extend_markers(&f, 20);
    let head: Value = serde_json::from_slice(&f.state.lock().unwrap().objects["/photos/library/catalog/head.json"]).unwrap();
    let key = format!("/photos/{}", head["tip"]["key"].as_str().unwrap());
    let bytes = f.state.lock().unwrap().objects[&key].clone();
    let mut changed = f.state.lock().unwrap().objects.clone();
    changed.insert(key.clone(), b"changed after the first GET".to_vec());
    f.state.lock().unwrap().replace_after_get = Some((key.clone(), changed));
    clear(&f);
    run(&f, &path, &[("cloud.sync", json!({}))]);
    history::assert_cost(&f, &sizes, 1);
    let before = catalog(&path).to_snapshot();
    let (out, rows) = f.run(&path, &[("cloud.verifyHistory", json!({}))]);
    assert!(!out.status.success(), "{rows:?}");
    assert_eq!(catalog(&path).to_snapshot(), before);
    f.state.lock().unwrap().objects.insert(key, bytes);
    let rows = run(&f, &path, &[("cloud.verifyHistory", json!({}))]);
    assert_eq!(rows[0]["result"]["complete"], true);
    assert_eq!(catalog(&path).to_snapshot(), before);
}
