#[path = "support/cloud_upload.rs"]
mod support;
use serde_json::{Value, json};
use std::path::Path;
use support::Fixture;
fn run(f: &Fixture, lib: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (out, rows) = f.run(lib, commands);
    assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
    rows
}
fn setup(f: &Fixture, protocol: &str) -> (std::path::PathBuf, String) {
    f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
    let lib = f.dir.join("erase-library");
    f.import_unsynchronized(&lib, false);
    let rows = run(
        f,
        &lib,
        &[
            ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
            ("cloud.upload", json!({})),
            ("cloud.pause", json!({})),
            ("cloud.overview", json!({})),
        ],
    );
    let state = run(f, &lib, &[("cloud.syncStatus", json!({}))]);
    let id = state[0]["result"]["library"].as_str().or_else(|| rows[0]["result"]["library"].as_str()).unwrap().to_owned();
    (lib, id)
}
fn preview(f: &Fixture, lib: &Path, id: &str) -> Value {
    run(f, lib, &[("cloud.deleteLibrary", json!({"library":id}))])[0]["result"].clone()
}
fn apply(id: &str, p: &Value) -> Value {
    json!({"library":id,"apply":true,"preview":p["preview"],"confirmation":"DELETE CLOUD LIBRARY","allDevicesStopped":true})
}
#[test]
fn erase_requires_typed_consent_fresh_inventory_and_removes_versions_parts_and_history() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = Fixture::new();
        let (lib, id) = setup(&f, protocol);
        let hidden = format!("/photos/library/originals/{}", support::hash(b"historical-only"));
        {
            let mut s = f.state.lock().unwrap();
            s.track_versions = true;
            s.list_page_size = 2;
            s.historical_versions.insert((hidden.clone(), "old+version/=1".into()), Some(b"old private photo".to_vec()));
            s.historical_versions.insert((hidden.clone(), "delete-marker-1".into()), None);
            s.multipart.insert("unfinished+/id".into(), (hidden.clone(), std::collections::BTreeMap::from([(1, b"part".to_vec())])));
        }
        let p = preview(&f, &lib, &id);
        assert_eq!(p["multipartUploads"], 1);
        let deletes = || f.state.lock().unwrap().requests.iter().filter(|(m, _)| m == "DELETE").count();
        assert_eq!(deletes(), 0);
        for phrase in ["", "delete cloud library", "DELETE CLOUD LIBRARY "] {
            let mut args = apply(&id, &p);
            args["confirmation"] = json!(phrase);
            let (out, _) = f.run(&lib, &[("cloud.deleteLibrary", args)]);
            assert!(!out.status.success());
            assert_eq!(deletes(), 0);
        }
        let changed = format!("/photos/library/originals/{}", support::hash(b"new"));
        f.state.lock().unwrap().objects.insert(changed, b"new".to_vec());
        let (out, _) = f.run(&lib, &[("cloud.deleteLibrary", apply(&id, &p))]);
        assert!(!out.status.success());
        assert_eq!(deletes(), 0);
        let p = preview(&f, &lib, &id);
        let done = run(&f, &lib, &[("cloud.deleteLibrary", apply(&id, &p))]);
        assert_eq!(done[0]["result"]["applied"], true);
        let s = f.state.lock().unwrap();
        assert!(s.multipart.is_empty());
        assert!(s.historical_versions.is_empty());
        assert!(s.objects.keys().all(|k| k.ends_with("/catalog/head.json") || k.contains("/control/")));
        assert!(!s.objects.is_empty());
        assert_eq!(s.signature_errors, 0);
        drop(s);
        let status = run(&f, &lib, &[("cloud.overview", json!({}))]);
        assert_eq!(status[0]["result"]["erasure"]["complete"], true);
        assert_eq!(status[0]["result"]["summary"]["cloudOriginalBytes"], 0);
        let (out, _) = f.run(&lib, &[("cloud.resume", json!({})), ("cloud.sync", json!({}))]);
        assert!(!out.status.success(), "erased library must never initialize itself again");
    }
}
#[test]
fn restored_catalog_cannot_recreate_media_after_remote_erasure() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = Fixture::new();
        let (lib, id) = setup(&f, protocol);
        let p = preview(&f, &lib, &id);
        run(&f, &lib, &[("cloud.deleteLibrary", apply(&id, &p))]);
        // Simulate restoring a catalog backup without its device-local marker.
        let marker = lib.join("Managed").join(format!("erased-library-{}.json", support::hash(id.as_bytes())));
        std::fs::remove_file(marker).unwrap();
        f.import_unsynchronized(&lib, true);
        run(&f, &lib, &[("cloud.resume", json!({}))]);
        f.state.lock().unwrap().requests.clear();
        let (out, rows) = f.run(&lib, &[("cloud.upload", json!({}))]);
        assert!(!out.status.success(), "{protocol}: {rows:?}");
        let s = f.state.lock().unwrap();
        assert!(!s.requests.iter().any(|(m, _)| m == "PUT" || m == "POST"), "no media or multipart writes after terminal control: {:?}", s.requests);
        assert!(s.objects.keys().all(|k| k.ends_with("/catalog/head.json") || k.contains("/control/")));
    }
}
#[test]
fn erase_refuses_other_libraries_and_unrelated_files_before_writing() {
    for foreign in ["sync-v2/someone-else/manifest/a.json", "holiday.jpg", "originals/holiday.jpg"] {
        let f = Fixture::new();
        let (lib, id) = setup(&f, "append-log-v1");
        f.state.lock().unwrap().objects.insert(format!("/photos/library/{foreign}"), b"keep".to_vec());
        let (out, _) = f.run(&lib, &[("cloud.deleteLibrary", json!({"library":id}))]);
        assert!(!out.status.success());
        assert!(!f.state.lock().unwrap().requests.iter().any(|(m, _)| m == "DELETE"));
    }
}

#[test]
fn erasing_a_migrated_prefix_retires_both_publication_authorities() {
    let f = Fixture::new();
    let (lib, id) = setup(&f, "conditional-head-v1");
    let manifest = serde_json::to_vec(&json!({"version":1,"library":id,"protocol":"append-log-v1"})).unwrap();
    let key = format!("/photos/library/sync-v2/{id}/manifest/{}.json", support::hash(&manifest));
    f.state.lock().unwrap().objects.insert(key, manifest);
    f.state.lock().unwrap().append_mode = true;
    let p = preview(&f, &lib, &id);
    run(&f, &lib, &[("cloud.deleteLibrary", apply(&id, &p))]);
    let state = f.state.lock().unwrap();
    assert!(state.objects.keys().any(|k| k.ends_with("/catalog/head.json")));
    assert!(state.objects.keys().any(|k| k.contains("/control/")));
    assert!(
        state
            .objects
            .values()
            .all(|b| serde_json::from_slice::<Value>(b).is_ok_and(|v| v["state"] == "deleted" || v["control"]["state"] == "deleted"))
    );
}

#[test]
fn erase_detects_late_writes_and_keeps_local_writers_disabled_until_retry_finishes() {
    let f = Fixture::new();
    let (lib, id) = setup(&f, "append-log-v1");
    let p = preview(&f, &lib, &id);
    f.state.lock().unwrap().object_after_erasure_barrier =
        Some((format!("/photos/library/originals/{}", support::hash(b"late writer")), b"late writer".to_vec()));
    let (out, rows) = f.run(&lib, &[("cloud.deleteLibrary", apply(&id, &p))]);
    assert!(!out.status.success(), "{rows:?}");
    assert!(!f.state.lock().unwrap().requests.iter().any(|(m, _)| m == "DELETE"));
    let (out, _) = f.run(&lib, &[("cloud.resume", json!({})), ("cloud.upload", json!({})), ("cloud.syncEnable", json!({"connection":"personal"}))]);
    assert!(!out.status.success());
    let p = preview(&f, &lib, &id);
    f.state.lock().unwrap().delete_conflict = true;
    let (out, _) = f.run(&lib, &[("cloud.deleteLibrary", apply(&id, &p))]);
    assert!(!out.status.success());
    f.state.lock().unwrap().delete_conflict = false;
    let p = preview(&f, &lib, &id);
    run(&f, &lib, &[("cloud.deleteLibrary", apply(&id, &p))]);
    assert!(f.state.lock().unwrap().objects.keys().all(|k| k.contains("/control/")));
}

#[test]
fn an_empty_writable_bucket_is_probed_read_only_then_initialized_on_first_sync() {
    let f = Fixture::new();
    let lib = f.dir.join("empty-bucket");
    let config = json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"});
    run(&f, &lib, &[("cloud.configure", config.clone())]);
    let probe = run(&f, &lib, &[("cloud.testConnection", config)]);
    assert_eq!(probe[0]["result"]["libraries"], json!([]));
    assert_eq!(probe[0]["result"]["headTaken"], false);
    assert!(f.state.lock().unwrap().objects.is_empty());
    run(&f, &lib, &[("cloud.syncEnable", json!({"connection":"personal","protocol":"conditional-head-v1"})), ("cloud.sync", json!({}))]);
    let s = f.state.lock().unwrap();
    assert!(s.objects.contains_key("/photos/library/catalog/head.json"));
    assert!(!s.requests.iter().any(|(method, key)| method == "PUT" && key == "/photos/"), "setup never creates the bucket");
}

#[test]
#[ignore = "explicit native presented-window acceptance; requires LIGHTCRAFT_NATIVE_ERASE_APP"]
fn native_cloud_library_deletion_requires_typed_phrase_and_erases_only_the_fixture() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;
    let f = Fixture::new();
    let (lib, id) = setup(&f, "append-log-v1");
    let executable = std::env::var("LIGHTCRAFT_NATIVE_ERASE_APP").unwrap();
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_ERASE_SNAPSHOTS").unwrap());
    std::fs::create_dir_all(&artifacts).unwrap();
    let log = std::fs::File::create(artifacts.join("native-run.log")).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    struct Desktop(std::process::Child);
    impl Drop for Desktop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut desktop = Desktop(
        std::process::Command::new(executable)
            .args(["--library", lib.to_str().unwrap(), "--no-demo", "--control", &port.to_string()])
            .env("LIGHTCRAFT_CONFIG_DIR", f.dir.join("native-config"))
            .env("AWS_SHARED_CREDENTIALS_FILE", f.dir.join("credentials"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let mut socket = None;
    for _ in 0..100 {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            socket = Some(stream);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let stream = socket.expect("native control server");
    stream.set_read_timeout(Some(Duration::from_secs(45))).unwrap();
    let mut reader = BufReader::new(stream);
    fn ask(reader: &mut BufReader<TcpStream>, method: &str, params: Value) -> Value {
        writeln!(reader.get_mut(), "{}", json!({"method":method,"params":params})).unwrap();
        reader.get_mut().flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ok"], true, "{v}");
        v["result"].clone()
    }
    fn click(reader: &mut BufReader<TcpStream>, id: &str) {
        ask(reader, "ui.clickWidget", json!({"id":id}));
        std::thread::sleep(Duration::from_millis(150));
    }
    fn screenshot(reader: &mut BufReader<TcpStream>, artifacts: &Path, name: &str) {
        ask(reader, "ui.screenshot", json!({"path":artifacts.join(format!("native-{name}.png")).to_string_lossy()}));
    }
    let status = ask(&mut reader, "engine.execute", json!({"command":"cloud.syncStatus"}));
    assert_eq!(status["library"], id, "never operate on any other library");
    ask(&mut reader, "engine.execute", json!({"command":"dialog.cloudDelete"}));
    click(&mut reader, "button:cloudDeletePreview");
    let mut ready = false;
    for _ in 0..200 {
        let status = ask(&mut reader, "engine.execute", json!({"command":"cloud.uploadStatus"}));
        if status["reports"].as_array().is_some_and(|v| v.iter().any(|r| r["result"]["preview"].is_string())) {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ready);
    std::thread::sleep(Duration::from_millis(200));
    screenshot(&mut reader, &artifacts, "review");
    click(&mut reader, "checkbox:cloudDeleteStopped");
    click(&mut reader, "field:cloudDeletePhrase");
    ask(&mut reader, "ui.text", json!({"text":"wrong"}));
    click(&mut reader, "button:cloudDeleteApply");
    assert!(!f.state.lock().unwrap().requests.iter().any(|(m, _)| m == "DELETE"));
    click(&mut reader, "field:cloudDeletePhrase");
    ask(&mut reader, "ui.key", json!({"key":"A","cmd":true}));
    ask(&mut reader, "ui.text", json!({"text":"DELETE CLOUD LIBRARY"}));
    std::thread::sleep(Duration::from_millis(150));
    screenshot(&mut reader, &artifacts, "confirmed");
    click(&mut reader, "button:cloudDeleteApply");
    let mut erased = false;
    for _ in 0..200 {
        let status = ask(&mut reader, "engine.execute", json!({"command":"cloud.uploadStatus"}));
        if status["reports"].as_array().is_some_and(|v| v.iter().any(|r| r["result"]["applied"] == true)) {
            erased = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(erased, "fixture erase completed");
    std::thread::sleep(Duration::from_millis(200));
    screenshot(&mut reader, &artifacts, "complete");
    let inspect = ask(&mut reader, "ui.inspect", json!({}));
    std::fs::write(artifacts.join("native-inspect.json"), serde_json::to_vec_pretty(&inspect).unwrap()).unwrap();
    ask(&mut reader, "ui.dialog.cancel", json!({}));
    click(&mut reader, "icon:cloud");
    screenshot(&mut reader, &artifacts, "deleted-popup");
    let overview = ask(&mut reader, "engine.execute", json!({"command":"cloud.overview"}));
    assert_eq!(overview["erasure"]["complete"], true);
    assert_eq!(overview["summary"]["cloudOriginalBytes"], 0);
    click(&mut reader, "button:cloudDeleteReview");
    screenshot(&mut reader, &artifacts, "deleted-details");
    ask(&mut reader, "app.quit", json!({}));
    assert!(desktop.0.wait().unwrap().success());
    let state = f.state.lock().unwrap();
    assert!(state.objects.keys().all(|k| k.contains("/control/")));
    assert_eq!(state.signature_errors, 0);
}
