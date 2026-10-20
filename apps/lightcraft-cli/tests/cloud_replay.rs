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
    // The last uncovered batch is visited by later recovery walks. No claim of
    // cross-walk body reuse or an incremental pending overlay is made here.
    assert_eq!(storage.traffic.iter().filter(|request| request.method == "GET" && request.target == newest).count(), 3);
}

#[test]
fn pending_edits_keep_the_full_replay_guard_but_each_pass_reuses_its_own_bytes() {
    for count in [10, 100, 1000] {
        let f = Fixture::new();
        let path = initialize(&f);
        let sizes = history::extend_markers(&f, count);
        run(&f, &path, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
        assert_eq!(catalog(&path).sync_state().unwrap().pending.len(), 1);
        clear(&f);
        run(&f, &path, &[("cloud.sync", json!({}))]);
        // Pre-publication and confirmation are separate certified reads. CR.80
        // owns making this independent of the already acknowledged tail length.
        history::assert_cost(&f, &sizes, 2);
        let final_catalog = catalog(&path);
        assert!(final_catalog.sync_state().unwrap().pending.is_empty());
        assert_eq!(final_catalog.photo(PhotoId(1)).unwrap().rating, 5);
    }
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
