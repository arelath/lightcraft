//! Create/join semantics and append capacity safety through the real binary.
#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{
    FsStore, Journal,
    replication::{Change, SyncProtocol, SyncState},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};
use support::Fixture;
fn config(f: &Fixture) -> Value {
    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"})
}
fn run(f: &Fixture, path: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (output, rows) = f.run(path, commands);
    assert!(output.status.success(), "{rows:?} {}", String::from_utf8_lossy(&output.stderr));
    rows
}
fn catalog(path: &Path) -> lightcraft_engine::catalog::Catalog {
    Journal::open(Box::new(FsStore::open(path).unwrap())).unwrap().1
}
#[test]
fn repeated_fresh_joins_do_not_publish_initialization_or_consume_register_and_author_capacity() {
    for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = !protocol.is_head();
        let creator = f.dir.join("creator");
        let rows = run(
            &f,
            &creator,
            &[("cloud.configure", config(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})), ("cloud.sync", json!({}))],
        );
        let library = rows[1]["result"]["library"].as_str().unwrap();
        let baseline = catalog(&creator).sync_state().unwrap().portable();
        f.state.lock().unwrap().requests.clear();
        let mut authors = BTreeSet::new();
        for n in 0..129 {
            let path = f.dir.join(format!("join-{n}"));
            let rows = run(
                &f,
                &path,
                &[
                    ("cloud.configure", config(&f)),
                    ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":protocol})),
                    ("cloud.sync", json!({})),
                ],
            );
            assert_eq!(rows[1]["result"]["pending"], 0);
            let joined = catalog(&path);
            let state = joined.sync_state().unwrap();
            authors.insert(state.device.clone());
            assert!(state.require_existing && state.pending.is_empty());
            assert_eq!(state.portable(), baseline);
            assert_eq!(state.documents[&format!("library:{library}")]["identity"].0.len(), 1);
            assert_eq!(state.clock.len(), 1);
        }
        assert_eq!(authors.len(), 129);
        let remote = f.state.lock().unwrap();
        assert!(!remote.requests.iter().any(|(method, _)| method == "PUT"), "read-only joins must not publish");
        assert_eq!(remote.signature_errors, 0);
    }
}
#[test]
fn a_join_cannot_create_an_absent_destination_even_with_new_local_work() {
    for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = !protocol.is_head();
        let path = f.dir.join("missing");
        run(
            &f,
            &path,
            &[
                ("cloud.configure", config(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","library":"missing-library","protocol":protocol})),
                ("album.create", json!({"name":"Offline work"})),
            ],
        );
        let pending = catalog(&path).sync_state().unwrap().pending.clone();
        assert!(!pending.is_empty());
        f.state.lock().unwrap().requests.clear();
        let (out, rows) = f.run(&path, &[("cloud.sync", json!({}))]);
        assert!(!out.status.success(), "{rows:?}");
        assert!(rows[0]["error"].to_string().contains("cannot join"), "{rows:?}");
        assert_eq!(catalog(&path).sync_state().unwrap().pending, pending);
        assert!(!f.state.lock().unwrap().requests.iter().any(|(method, _)| method == "PUT"));
    }
}
#[test]
fn predictable_append_register_capacity_failure_does_not_publish_the_frozen_operation() {
    let f = Fixture::new();
    f.state.lock().unwrap().append_mode = true;
    let path = f.dir.join("capacity");
    run(&f, &path, &[("cloud.configure", config(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"}))]);
    let saved = catalog(&path);
    let local = saved.sync_state().unwrap();
    let pending = local.pending.clone();
    {
        let mut storage = f.state.lock().unwrap();
        storage.objects.insert(
            format!("/photos/{}", lightcraft_engine::cloud_append::manifest_key("library", &local.library).unwrap()),
            lightcraft_engine::cloud_append::manifest(&local.library).unwrap(),
        );
        for n in 0..128 {
            let mut author = SyncState::new(local.library.clone(), format!("historical-{n:03}"), "personal".into()).unwrap();
            let mutation = author
                .enqueue(vec![Change {
                    entity: format!("library:{}", local.library),
                    field: "identity".into(),
                    base: None,
                    value: json!(local.library),
                }])
                .unwrap()
                .unwrap();
            storage.objects.insert(
                format!("/photos/{}", lightcraft_engine::cloud_append::mutation_key("library", &mutation).unwrap()),
                serde_json::to_vec(&mutation).unwrap(),
            );
        }
        storage.requests.clear();
    }
    let (out, rows) = f.run(&path, &[("cloud.sync", json!({}))]);
    assert!(!out.status.success(), "{rows:?}");
    assert!(rows[0]["error"].to_string().contains("128"), "{rows:?}");
    assert_eq!(catalog(&path).sync_state().unwrap().pending, pending);
    let storage = f.state.lock().unwrap();
    assert!(!storage.requests.iter().any(|(method, key)| method == "PUT" && key.contains("/ops/")), "locally rejected operation escaped to storage");
    assert_eq!(storage.signature_errors, 0);
}

#[test]
fn an_explicit_empty_library_id_fails_before_enrollment() {
    let f = Fixture::new();
    let path = f.dir.join("invalid-join");
    run(&f, &path, &[("cloud.configure", config(&f))]);
    let (out, rows) = f.run(&path, &[("cloud.syncEnable", json!({"connection":"personal","library":""}))]);
    assert!(!out.status.success(), "{rows:?}");
    let saved = catalog(&path);
    assert!(saved.sync_state().is_none());
    assert!(saved.cloud_identity("library").is_none());
    assert!(f.state.lock().unwrap().requests.is_empty());
}
