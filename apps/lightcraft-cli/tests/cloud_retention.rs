#[path = "support/cloud_upload.rs"]
mod support;
use serde_json::{Value, json};
use std::path::Path;
use support::Fixture;
fn run(f: &Fixture, library: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (out, rows) = f.run(library, commands);
    assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
    rows
}
fn preview(f: &Fixture, library: &Path) -> Value {
    run(f, library, &[("cloud.reclaim", json!({"connection":"personal"}))])[0]["result"].clone()
}
fn apply(token: &Value) -> Value {
    json!({"connection":"personal","apply":true,"preview":token,"allWritersPausedAndPublished":true})
}

#[test]
fn reclamation_rechecks_preview_history_preconditions_and_preserves_deleted_published_media() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let library = f.dir.join("retention");
        let (_, source) = f.import_unsynchronized(&library, false);
        run(
            &f,
            &library,
            &[
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
                ("cloud.upload", json!({})),
                ("library.selectAll", json!({})),
                ("photo.delete", json!({})),
                ("library.emptyRecentlyDeleted", json!({})),
                ("cloud.sync", json!({})),
                ("cloud.sync", json!({"checkpoint":true})),
                ("cloud.pause", json!({})),
            ],
        );
        let key = format!("/photos/library/originals/{}", support::hash(b"orphan"));
        f.state.lock().unwrap().objects.insert(key.clone(), b"orphan".to_vec());
        let p = preview(&f, &library);
        assert_eq!(p["candidates"].as_array().unwrap().len(), 1, "{p}");
        assert_eq!(p["publishedHistoryRetained"], true);
        assert_eq!(f.state.lock().unwrap().requests.iter().filter(|(method, _)| method == "DELETE").count(), 0);
        let no_confirmation = json!({"connection":"personal","apply":true,"preview":p["preview"]});
        let (out, _) = f.run(&library, &[("cloud.reclaim", no_confirmation)]);
        assert!(!out.status.success());
        f.state.lock().unwrap().objects.insert(key.clone(), b"changed orphan".to_vec());
        let (out, rows) = f.run(&library, &[("cloud.reclaim", apply(&p["preview"]))]);
        assert!(!out.status.success(), "{rows:?}");
        assert_eq!(f.state.lock().unwrap().requests.iter().filter(|(method, _)| method == "DELETE").count(), 0);
        f.state.lock().unwrap().versioned_media = true;
        assert!(preview(&f, &library)["candidates"].as_array().unwrap().is_empty());
        f.state.lock().unwrap().versioned_media = false;
        f.state.lock().unwrap().retention_modified = Some("Fri, 01 Jan 2100 00:00:00 GMT".into());
        assert!(preview(&f, &library)["candidates"].as_array().unwrap().is_empty());
        f.state.lock().unwrap().retention_modified = None;
        let p = preview(&f, &library);
        f.state.lock().unwrap().unsupported_conditional = true;
        let (out, _) = f.run(&library, &[("cloud.reclaim", apply(&p["preview"]))]);
        assert!(!out.status.success());
        assert!(f.state.lock().unwrap().objects.contains_key(&key));
        f.state.lock().unwrap().unsupported_conditional = false;
        f.state.lock().unwrap().delete_conflict = true;
        let (out, _) = f.run(&library, &[("cloud.reclaim", apply(&p["preview"]))]);
        assert!(!out.status.success());
        assert!(f.state.lock().unwrap().objects.contains_key(&key));
        f.state.lock().unwrap().delete_conflict = false;
        let mut expected = f.state.lock().unwrap().objects.clone();
        expected.remove(&key);
        let result = run(&f, &library, &[("cloud.reclaim", apply(&p["preview"]))]);
        assert_eq!(result[0]["result"]["deleted"].as_array().unwrap().len(), 1);
        assert_eq!(f.state.lock().unwrap().objects, expected);
        assert_eq!(f.state.lock().unwrap().signature_errors, 0);
        assert!(source.is_file());
        let removed = {
            let mut state = f.state.lock().unwrap();
            let key = state.objects.keys().find(|key| key.contains("/ops/") || key.contains("/catalog/changes/")).unwrap().clone();
            let bytes = state.objects.remove(&key).unwrap();
            (key, bytes)
        };
        let deletes = f.state.lock().unwrap().requests.iter().filter(|(method, _)| method == "DELETE").count();
        let (out, _) = f.run(&library, &[("cloud.reclaim", json!({"connection":"personal"}))]);
        assert!(!out.status.success());
        assert_eq!(f.state.lock().unwrap().requests.iter().filter(|(method, _)| method == "DELETE").count(), deletes);
        f.state.lock().unwrap().objects.insert(removed.0, removed.1);
    }
}
#[test]
fn unreadable_local_or_remote_metadata_blocks_all_reclamation() {
    let f = Fixture::new();
    let library = f.dir.join("corrupt");
    f.import(&library, false);
    run(&f, &library, &[("cloud.upload", json!({})), ("cloud.pause", json!({}))]);
    let key = format!("/photos/library/originals/{}", support::hash(b"orphan"));
    f.state.lock().unwrap().objects.insert(key, b"orphan".to_vec());
    let path = library.join("Managed/imports/corrupt.json");
    std::fs::write(&path, b"not JSON").unwrap();
    let (out, _) = f.run(&library, &[("cloud.reclaim", json!({"connection":"personal"}))]);
    assert!(!out.status.success());
    std::fs::remove_file(path).unwrap();
    f.state.lock().unwrap().objects.insert("/photos/library/catalog/unknown.json".into(), b"{}".to_vec());
    let (out, _) = f.run(&library, &[("cloud.reclaim", json!({"connection":"personal"}))]);
    assert!(!out.status.success());
    assert!(!f.state.lock().unwrap().requests.iter().any(|(method, _)| method == "DELETE"));
}
