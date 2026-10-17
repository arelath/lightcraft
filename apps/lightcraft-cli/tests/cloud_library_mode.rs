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
        writeln!(self.socket.get_mut(), "{}", json!({"method":method,"params":params})).unwrap();
        self.socket.get_mut().flush().unwrap();
        let mut line = String::new();
        self.socket.read_line(&mut line).unwrap();
        let result: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(result["ok"], true, "{result}");
        result["result"].clone()
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
        f.state.lock().unwrap().objects.insert(first_key.clone(), bytes);
        let (out, rows) = f.run(
            &library,
            &[
                ("library.import", json!({"paths":[first,second]})),
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
        app.settled(&f, &[first_key.clone(), second_key.clone()]);
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
        app.settled(&f, &[first_key, second_key, later_key]);
        app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
        std::thread::sleep(Duration::from_millis(150));
        let widgets = app.ask("ui.widgets", json!({"filter":"cloud"}));
        assert!(!widgets.to_string().contains("button:cloudStorage"), "duplicate action removed: {widgets}");
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
