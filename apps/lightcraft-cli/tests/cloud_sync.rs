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
