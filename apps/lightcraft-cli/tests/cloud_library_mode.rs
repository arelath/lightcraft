#[path = "support/cloud_upload.rs"]
mod support;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

struct Desktop {
    child: std::process::Child,
    socket: BufReader<TcpStream>,
}
impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Desktop {
    fn start(f: &support::Fixture, library: &Path, artifacts: &Path) -> Self {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let log = std::fs::File::create(artifacts.join("native.log")).unwrap();
        let child = std::process::Command::new(std::env::var("LIGHTCRAFT_NATIVE_LIBRARY_APP").unwrap())
            .args(["--library", library.to_str().unwrap(), "--no-demo", "--control", &port.to_string()])
            .env("LIGHTCRAFT_CONFIG_DIR", f.dir.join("config"))
            .env("AWS_SHARED_CREDENTIALS_FILE", f.dir.join("credentials"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        let socket = loop {
            if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
                break stream;
            }
            assert!(Instant::now() < until, "native control server");
            std::thread::sleep(Duration::from_millis(100));
        };
        socket.set_read_timeout(Some(Duration::from_secs(45))).unwrap();
        Self { child, socket: BufReader::new(socket) }
    }
    fn ask(&mut self, method: &str, params: Value) -> Value {
        let result = self.raw(method, params);
        assert_eq!(result["ok"], true, "{result}");
        result["result"].clone()
    }
    fn raw(&mut self, method: &str, params: Value) -> Value {
        writeln!(self.socket.get_mut(), "{}", json!({"method":method,"params":params})).unwrap();
        self.socket.get_mut().flush().unwrap();
        let mut line = String::new();
        self.socket.read_line(&mut line).unwrap();
        let result: Value = serde_json::from_str(&line).unwrap();
        result
    }
    fn execute(&mut self, command: &str, params: Value) -> Value {
        self.ask("engine.execute", json!({"command":command,"params":params}))
    }
    fn settled(&mut self, f: &support::Fixture, originals: &[String]) {
        let until = Instant::now() + Duration::from_secs(45);
        loop {
            let status = self.execute("cloud.overview", json!({}));
            if originals.iter().all(|key| f.state.lock().unwrap().objects.contains_key(key))
                && status["summary"]["pendingChanges"] == 0
                && status["summary"]["queuedFiles"] == 0
                && status["preparingFiles"] == 0
                && status["tasks"].as_array().is_some_and(Vec::is_empty)
            {
                return;
            }
            assert!(Instant::now() < until, "cloud mode did not finish: {status}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn prepared(&mut self, command: &str, photo: u64) -> Value {
        let started = self.execute(command, json!({"ids":[photo],"wait":false}));
        let task = started["task"].as_u64().or_else(|| started["managed"]["task"].as_u64()).expect("background media task");
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.execute("cloud.importStatus", json!({}));
            if let Some(report) = status["reports"].as_array().unwrap().iter().find(|report| report["task"] == task) {
                assert_eq!(report["ok"], true, "{command}: {report}");
                assert_eq!(report["result"]["failed"], json!([]), "{command}: {report}");
                return report["result"].clone();
            }
            assert!(Instant::now() < until, "{command}: {status}");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
fn photo(f: &support::Fixture, name: &str, shade: u8) -> (std::path::PathBuf, Vec<u8>, String) {
    let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, shade, 255]);
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
    let path = f.dir.join(name);
    std::fs::write(&path, &bytes).unwrap();
    let key = format!("/photos/library/originals/{}", support::hash(&bytes));
    (path, bytes, key)
}

#[test]
#[ignore = "explicit native cloud-toggle acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn cloud_toggle_native_confirmation_blocking_progress_and_durable_completion() {
    let f = support::Fixture::new();
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("toggle");
    std::fs::create_dir_all(&artifacts).unwrap();
    let publisher = f.dir.join("publisher");
    let local = f.dir.join("local");
    let (import, source) = f.import(&publisher, false);
    let (out, rows) = f.run(&publisher, &[("cloud.upload", json!({"imports":[import]})), ("cloud.sync", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let (_, saved, _) = lightcraft_engine::catalog::Journal::open(Box::new(lightcraft_engine::catalog::FsStore::open(&publisher).unwrap())).unwrap();
    let library = saved.sync_state().unwrap().library.clone();
    let original = saved.assets().values().next().unwrap().original.clone();
    let (out, rows) = f.run(
        &local,
        &[
            (
                "cloud.configure",
                json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
            ),
            ("cloud.syncEnable", json!({"connection":"personal","library":library})),
            ("cloud.sync", json!({})),
        ],
    );
    assert!(out.status.success(), "{rows:?}");
    let mut app = Desktop::start(&f, &local, &artifacts);
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let widgets = app.ask("ui.widgets", json!({"filter":"icon:cloud"}));
        if widgets.as_array().is_some_and(|widgets| widgets.iter().any(|widget| widget["id"] == "icon:cloud")) {
            break;
        }
        assert!(Instant::now() < until, "first native frame did not show the cloud icon: {widgets}");
        std::thread::sleep(Duration::from_millis(100));
    }
    app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("enabled.png")}));
    app.execute("dialog.cloudDisable", json!({}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("confirmation.png")}));
    let remote = f.state.lock().unwrap().objects.clone();
    {
        let mut storage = f.state.lock().unwrap();
        storage.requests.clear();
        storage.hold_get = Some(format!("/photos/{}", original.remote.key));
    }
    app.ask("ui.clickWidget", json!({"id":"button:cloudDisableConfirm"}));
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let status = app.execute("cloud.transitionStatus", json!({}));
        let presented = app.ask("ui.inspect", json!({}))["cloudTransition"].clone();
        if status["stage"] == "downloading" && presented["currentDownloadTotal"].as_u64().is_some_and(|n| n > 0) {
            break;
        }
        assert!(Instant::now() < until, "download did not reach held original: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.ask("ui.screenshot", json!({"path":artifacts.join("downloading.png")}));
    let blocked = app.raw("engine.execute", json!({"command":"library.preferences","params":{}}));
    assert_eq!(blocked["ok"], false, "{blocked}");
    app.ask("ui.key", json!({"key":"Escape"}));
    assert_eq!(app.execute("cloud.transitionStatus", json!({}))["blocking"], true);
    f.state.lock().unwrap().hold_get = None;
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let status = app.execute("cloud.transitionStatus", json!({}));
        let presented = app.ask("ui.inspect", json!({}))["cloudTransition"].clone();
        if status["stage"] == "complete" && presented["stage"] == "complete" {
            assert_eq!(status["localOnly"], true);
            break;
        }
        assert!(Instant::now() < until, "conversion did not complete: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.ask("ui.screenshot", json!({"path":artifacts.join("complete.png")}));
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.ask("ui.clickWidget", json!({"id":"button:cloudDisableDone"}));
    app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("off.png")}));
    app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
    app.execute("panel.info", json!({}));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let widgets = app.ask("ui.widgets", json!({"filter":"cloudSyncTurnOn"}));
        let window = app.ask("ui.inspect", json!({}))["window"].clone();
        let height = window[1].as_f64().unwrap();
        if widgets.as_array().is_some_and(|widgets| {
            widgets.iter().any(|widget| {
                let y = widget["rect"][1].as_f64().unwrap();
                let h = widget["rect"][3].as_f64().unwrap();
                y > 80. && y + h < height - 40.
            })
        }) {
            break;
        }
        assert!(Instant::now() < until, "Info did not present the explicit turn-on action: {widgets}");
        app.ask("ui.move", json!({"x":window[0].as_f64().unwrap()-180.,"y":window[1].as_f64().unwrap()/2.}));
        app.ask("ui.scroll", json!({"dy":-350.}));
        std::thread::sleep(Duration::from_millis(50));
    }
    app.ask("ui.screenshot", json!({"path":artifacts.join("off-info.png")}));
    app.execute("app.settings", json!({"tab":"cache"}));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let widgets = app.ask("ui.widgets", json!({"filter":"cacheLocalCloudEnable"}));
        if widgets.as_array().is_some_and(|widgets| !widgets.is_empty()) {
            break;
        }
        assert!(Instant::now() < until, "Cache settings did not show permanent local storage guidance");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.ask("ui.screenshot", json!({"path":artifacts.join("off-cache-settings.png")}));
    app.ask("ui.dialog.cancel", json!({}));
    let requests_before_local_work = f.state.lock().unwrap().requests.len();
    app.prepared("photo.saveMetadataToFile", 1);
    let edit = app.prepared("photo.editExternal", 1);
    let derived = edit["id"].as_u64().unwrap();
    std::fs::copy(&source, edit["path"].as_str().unwrap()).unwrap();
    app.prepared("photo.reload", derived);
    assert_eq!(f.state.lock().unwrap().requests.len(), requests_before_local_work, "local background revisions perform no network requests");
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    assert_eq!(std::fs::read(local.join("Local Originals").join("blobs").join(original.id.0)).unwrap(), std::fs::read(source).unwrap());
    let storage = f.state.lock().unwrap();
    assert_eq!(storage.objects, remote);
    assert!(storage.requests.iter().all(|(m, _)| matches!(m.as_str(), "GET" | "HEAD")));
}

#[test]
#[ignore = "explicit presented-window generated-original acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn a_new_append_library_uploads_generated_demo_photos_without_manual_import() {
    use lightcraft_engine::{
        Session,
        catalog::{Op, Photo, Source},
    };
    let f = support::Fixture::new();
    f.state.lock().unwrap().append_mode = true;
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("generated-demo");
    std::fs::create_dir_all(&artifacts).unwrap();
    let library = f.dir.join("demo-library");
    let (out, rows) = f.run(
        &library,
        &[(
            "cloud.configure",
            json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
        )],
    );
    assert!(out.status.success(), "{rows:?}");
    {
        let mut session = Session::new().with_fs();
        session.open_library(&library, false).unwrap();
        let id = session.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::Demo { scene: 1 }, "demo.JPG", "JPEG", 60, 40, "2026-10-16T09:00:00");
        session.commit("Add generated photo", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        session.close_library().unwrap();
    }
    let mut app = Desktop::start(&f, &library, &artifacts);
    app.execute("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"}));
    let until = Instant::now() + Duration::from_secs(45);
    let key = loop {
        let key = f.state.lock().unwrap().objects.keys().find(|key| key.starts_with("/photos/library/originals/")).cloned();
        if let Some(key) = key {
            break key;
        }
        assert!(Instant::now() < until, "no generated original uploaded: {}", app.execute("cloud.overview", json!({})));
        std::thread::sleep(Duration::from_millis(100));
    };
    app.settled(&f, std::slice::from_ref(&key));
    let bytes = f.state.lock().unwrap().objects.get(&key).unwrap().clone();
    let raw = lightcraft_raw::decode(&bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits), (60, 40, 32));
    app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("popup.png").to_string_lossy()}));
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.execute("cloud.pause", json!({}));
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    let state = f.state.lock().unwrap();
    assert_eq!(state.signature_errors, 0);
    assert_eq!(state.objects.keys().filter(|key| key.starts_with("/photos/library/originals/")).count(), 1);
}

#[test]
#[ignore = "explicit presented-window cloud-mode acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn enabling_cloud_mode_uploads_existing_and_future_library_photos_automatically() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = support::Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join(protocol);
        std::fs::create_dir_all(&artifacts).unwrap();
        let library = f.dir.join("whole-library");
        let (first, bytes, first_key) = photo(&f, "already-remote.png", 70);
        let (second, _, second_key) = photo(&f, "local.png", 120);
        let mut paths = vec![first.clone(), second.clone()];
        let mut keys = vec![first_key.clone(), second_key.clone()];
        for n in 0..18 {
            let (path, _, key) = photo(&f, &format!("batch-{n}.png"), 121 + n);
            paths.push(path);
            keys.push(key);
        }
        f.state.lock().unwrap().objects.insert(first_key.clone(), bytes);
        let (out, rows) = f.run(
            &library,
            &[
                ("library.import", json!({"paths":paths})),
                ("photo.rate", json!({"id":1,"rating":4})),
                (
                    "cloud.configure",
                    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
                ),
            ],
        );
        assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
        let mut app = Desktop::start(&f, &library, &artifacts);
        app.execute("cloud.syncEnable", json!({"connection":"personal","protocol":protocol}));
        app.settled(&f, &keys);
        {
            let state = f.state.lock().unwrap();
            assert!(!state.requests.iter().any(|(method, key)| method == "PUT" && key == &first_key), "verified existing originals are reused");
            assert!(state.requests.iter().any(|(method, key)| method == "PUT" && key == &second_key));
        }
        app.execute("cloud.pause", json!({}));
        let (later, _, later_key) = photo(&f, "later.png", 190);
        app.execute("library.import", json!({"paths":[later]}));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!f.state.lock().unwrap().objects.contains_key(&later_key), "pause also stops new-library staging");
        app.execute("cloud.resume", json!({}));
        keys.push(later_key);
        app.settled(&f, &keys);
        app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
        std::thread::sleep(Duration::from_millis(150));
        let widgets = app.ask("ui.widgets", json!({"filter":"cloud"}));
        assert!(!widgets.to_string().contains("button:cloudSetup"), "redundant synchronization dialog removed: {widgets}");
        assert!(widgets.to_string().contains("button:cloudStorage"));
        app.ask("ui.screenshot", json!({"path":artifacts.join("popup.png").to_string_lossy()}));
        std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
        app.execute("cloud.pause", json!({}));
        app.ask("app.quit", json!({}));
        assert!(app.child.wait().unwrap().success());
        let state = f.state.lock().unwrap();
        assert_eq!(state.signature_errors, 0);
        assert!(state.requests.iter().all(|(m, _)| m != "DELETE"));
        assert!(first.is_file() && second.is_file());
    }
}
