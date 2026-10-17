//! Device mode transitions against an isolated loopback provider, never live storage.
#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{FsStore, Journal};
use serde_json::{Value, json};
use std::path::Path;

fn run(f: &support::Fixture, library: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (output, rows) = f.run(library, commands);
    assert!(output.status.success(), "{rows:?} {}", String::from_utf8_lossy(&output.stderr));
    rows
}
fn catalog(path: &Path) -> lightcraft_engine::catalog::Catalog {
    Journal::open(Box::new(FsStore::open(path).unwrap())).unwrap().1
}
fn config(f: &support::Fixture) -> Value {
    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"})
}
#[test]
fn an_unpublished_empty_library_can_turn_off_without_creating_cloud_objects() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = support::Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let local = f.dir.join("unpublished");
        let rows = run(
            &f,
            &local,
            &[
                ("cloud.configure", config(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
                ("cloud.syncDisable", json!({"confirm":true,"wait":true})),
            ],
        );
        assert_eq!(rows[2]["result"]["stage"], "complete", "{rows:?}");
        let storage = f.state.lock().unwrap();
        assert!(storage.objects.is_empty());
        assert!(storage.requests.iter().all(|(m, _)| matches!(m.as_str(), "GET" | "HEAD")));
    }
}
#[test]
fn turning_off_materializes_remote_library_then_explicit_enable_merges_local_edits() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = support::Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let a = f.dir.join("a");
        let b = f.dir.join("b");
        let (import, source) = f.import_unsynchronized(&a, false);
        run(
            &f,
            &a,
            &[
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
                ("cloud.upload", json!({"imports":[import]})),
                ("cloud.sync", json!({})),
            ],
        );
        let library = catalog(&a).sync_state().unwrap().library.clone();
        let initial_remote = f.state.lock().unwrap().objects.clone();
        // B joins with a distinct local original, metadata and an album.
        let bytes = lightcraft_codecs::encode_png(
            &lightcraft_codecs::EncodeImage::rgba8(&lightcraft_raster::Rgba8::from_fn(12, 10, |_, _| [80, 90, 150, 255])),
            &Default::default(),
        )
        .unwrap();
        let extra = f.dir.join("extra.png");
        std::fs::write(&extra, &bytes).unwrap();
        run(
            &f,
            &b,
            &[
                ("cloud.configure", config(&f)),
                ("library.import", json!({"paths":[extra]})),
                ("photo.rate", json!({"id":1,"rating":5})),
                ("album.create", json!({"name":"Local before cloud"})),
                ("album.addPhotos", json!({"id":1,"ids":[1]})),
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol,"library":library})),
                ("cloud.adopt", json!({"connection":"personal","ids":[1]})),
                ("cloud.upload", json!({})),
                ("cloud.sync", json!({})),
            ],
        );
        assert_eq!(catalog(&a).photos().count(), 1);
        let before = f.state.lock().unwrap().objects.clone();
        // A second device publishes after the first head response, or during the
        // first original download in append mode. The final scan must discover it.
        let trigger = if protocol == "conditional-head-v1" {
            "/photos/library/catalog/head.json".to_owned()
        } else {
            let original = catalog(&a).assets().values().next().unwrap().original.remote.key.clone();
            for folder in ["Managed", "assets-cache"] {
                let disposable = a.join(folder);
                if disposable.exists() {
                    std::fs::remove_dir_all(disposable).unwrap();
                }
            }
            format!("/photos/{original}")
        };
        {
            let mut storage = f.state.lock().unwrap();
            storage.objects = initial_remote;
            storage.replace_after_get = Some((trigger, before.clone()));
        }
        f.state.lock().unwrap().requests.clear();
        let rows = run(
            &f,
            &a,
            &[("cloud.syncDisablePreview", json!({})), ("cloud.syncDisable", json!({"confirm":true,"wait":true})), ("cloud.syncStatus", json!({}))],
        );
        assert_eq!(rows[1]["result"]["stage"], "complete", "{rows:?}");
        assert_eq!(rows[2]["result"]["enabled"], false);
        let saved = catalog(&a);
        assert_eq!(saved.photos().filter(|p| !p.deleted).count(), 2);
        assert!(saved.photos().any(|p| p.file_name == "extra.png" && p.rating == 5));
        let extra_id = saved.photos().find(|p| p.file_name == "extra.png").unwrap().id;
        assert!(saved.albums().any(|album| album.name == "Local before cloud" && album.photos.contains(&extra_id)));
        let identity = saved.cloud_identities().clone();
        for asset in saved.assets().values() {
            for blob in asset.blobs() {
                let path = a.join("Local Originals").join("blobs").join(&blob.id.0);
                let bytes = std::fs::read(path).unwrap();
                assert_eq!(bytes.len() as u64, blob.size);
                assert_eq!(support::hash(&bytes), blob.id.0);
            }
        }
        {
            let storage = f.state.lock().unwrap();
            assert_eq!(storage.objects, before);
            assert!(storage.replace_after_get.is_none(), "concurrent publication was not injected");
            assert!(storage.requests.iter().all(|(m, _)| matches!(m.as_str(), "GET" | "HEAD")));
            if protocol == "conditional-head-v1" {
                assert!(storage.requests.iter().filter(|(m, k)| m == "GET" && k == "/photos/library/catalog/head.json").count() >= 3);
            }
        }
        // Remove only fixture-owned disposable bytes: local mode renders independently.
        if a.join("Managed").exists() {
            std::fs::remove_dir_all(a.join("Managed")).unwrap();
        }
        if a.join("assets-cache").exists() {
            std::fs::remove_dir_all(a.join("assets-cache")).unwrap();
        }
        let offline_requests = f.state.lock().unwrap().requests.len();
        let exported = f.dir.join(format!("local-{protocol}.png"));
        let rows = run(
            &f,
            &a,
            &[
                ("photo.rate", json!({"id":1,"rating":3})),
                ("cloud.overview", json!({})),
                ("app.export", json!({"ids":[1],"path":exported,"format":"original"})),
            ],
        );
        assert_eq!(rows[1]["result"]["syncEnabled"], false);
        assert_eq!(std::fs::read(exported).unwrap(), std::fs::read(&source).unwrap());
        assert_eq!(f.state.lock().unwrap().requests.len(), offline_requests);
        assert_eq!(catalog(&a).cloud_identities(), &identity);
        let requests = f.state.lock().unwrap().requests.len();
        let (out, rows) = f.run(&a, &[("cloud.resume", json!({})), ("cloud.reconnect", json!({}))]);
        assert!(!out.status.success());
        assert!(rows.iter().all(|r| r["ok"] == false));
        assert_eq!(f.state.lock().unwrap().requests.len(), requests);
        run(&f, &a, &[("cloud.syncEnable", json!({})), ("cloud.sync", json!({}))]);
        run(&f, &b, &[("cloud.sync", json!({}))]);
        assert_eq!(catalog(&a).photos().find(|p| p.file_name == source.file_name().unwrap().to_string_lossy()).unwrap().rating, 3);
        assert_eq!(catalog(&b).photos().find(|p| p.file_name == source.file_name().unwrap().to_string_lossy()).unwrap().rating, 3);
        assert_eq!(f.state.lock().unwrap().signature_errors, 0);
    }
}

#[test]
fn failed_download_stays_enabled_and_blocked_across_restart_until_retry_or_cancel() {
    let f = support::Fixture::new();
    let library = f.dir.join("retry");
    let (import, _) = f.import(&library, false);
    run(&f, &library, &[("cloud.upload", json!({"imports":[import]})), ("cloud.sync", json!({}))]);
    f.state.lock().unwrap().unauthorized = true;
    let rows = run(&f, &library, &[("cloud.syncDisable", json!({"confirm":true,"wait":true}))]);
    assert_eq!(rows[0]["result"]["stage"], "failed");
    assert_eq!(rows[0]["result"]["blocking"], true);
    let (out, rows) = f.run(&library, &[("photo.rate", json!({"id":1,"rating":1}))]);
    assert!(!out.status.success());
    assert_eq!(rows[0]["ok"], false);
    f.state.lock().unwrap().unauthorized = false;
    let rows = run(&f, &library, &[("cloud.syncDisable", json!({"confirm":true,"wait":true}))]);
    assert_eq!(rows[0]["result"]["stage"], "complete");
}
