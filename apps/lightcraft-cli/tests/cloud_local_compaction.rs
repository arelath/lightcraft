//! Detaching an older device must carry proofs back to its unchanged owner.
#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{Catalog, FsStore, Journal};
use serde_json::{Value, json};
use std::path::Path;

fn catalog(path: &Path) -> Catalog {
    Journal::open(Box::new(FsStore::open(path).unwrap())).unwrap().1
}
fn run(f: &support::Fixture, path: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (output, rows) = f.run(path, commands);
    assert!(output.status.success(), "{rows:?}: {}", String::from_utf8_lossy(&output.stderr));
    rows
}
#[test]
fn older_device_can_finish_local_conversion_after_receipt_compaction_on_both_protocols() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = support::Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let publisher = f.dir.join("publisher");
        let older = f.dir.join("older");
        let (import, _) = f.import_unsynchronized(&publisher, false);
        run(
            &f,
            &publisher,
            &[
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
                ("cloud.upload", json!({"imports":[import]})),
                ("cloud.sync", json!({})),
            ],
        );
        let library = catalog(&publisher).sync_state().unwrap().library.clone();
        run(
            &f,
            &older,
            &[
                (
                    "cloud.configure",
                    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
                ),
                ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":protocol})),
                ("cloud.sync", json!({})),
            ],
        );
        run(&f, &older, &[("photo.flag", json!({"ids":[1],"flag":"pick"}))]);
        let retained = ["catalog.snap", "catalog.log"].map(|name| (name, std::fs::read(older.join(name)).unwrap()));
        assert!(!catalog(&older).sync_state().unwrap().pending.is_empty());
        run(&f, &older, &[("cloud.sync", json!({}))]);
        run(&f, &publisher, &[("cloud.sync", json!({}))]);
        let mut changes = (0..24).map(|n| ("photo.rate", json!({"ids":[1],"rating":2+n%4}))).collect::<Vec<_>>();
        changes.push(("cloud.sync", json!({"checkpoint":true})));
        run(&f, &publisher, &changes);
        assert!(!catalog(&publisher).sync_state().unwrap().archives.is_empty());
        // Simulate a device retaining the exact mutation IDs after the remote
        // write succeeded but its local acknowledgement was not made durable.
        for (name, bytes) in &retained {
            std::fs::write(older.join(name), bytes).unwrap();
        }
        let objects = f.state.lock().unwrap().objects.clone();
        f.state.lock().unwrap().requests.clear();
        let rows = run(&f, &older, &[("cloud.syncDisable", json!({"confirm":true,"wait":true}))]);
        assert_eq!(rows[0]["result"]["stage"], "complete", "{protocol}: {rows:?}");
        assert_eq!(rows[0]["result"]["localOnly"], true);
        assert_eq!(catalog(&older).photo(lightcraft_engine::catalog::PhotoId(1)).unwrap().rating, 5);
        assert!(catalog(&older).sync_state().unwrap().pending.is_empty(), "certified ACKs from earlier conversion passes are retained");
        let storage = f.state.lock().unwrap();
        assert_eq!(storage.objects, objects, "conversion is receive-only");
        assert!(storage.requests.iter().all(|(method, _)| matches!(method.as_str(), "GET" | "HEAD")));
        assert_eq!(storage.signature_errors, 0);
    }
}
