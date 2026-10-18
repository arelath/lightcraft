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
        if let Err(error) = self.socket.read_line(&mut line) {
            panic!("control response for {method} {params}: {error}; app status: {:?}", self.child.try_wait());
        }
        let result: Value = serde_json::from_str(&line).unwrap();
        result
    }
    fn execute(&mut self, command: &str, params: Value) -> Value {
        self.ask("engine.execute", json!({"command":command,"params":params}))
    }
    fn wait_widget(&mut self, id: &str) {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let widgets = self.ask("ui.widgets", json!({"filter":id}));
            if widgets.to_string().contains(id) {
                break;
            }
            assert!(Instant::now() < until, "widget did not appear: {id}: {widgets}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn click_when_ready(&mut self, id: &str) {
        self.wait_widget(id);
        self.ask("ui.clickWidget", json!({"id":id}));
    }
    fn retention_consent(&mut self, confirmed: bool) {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let state = self.ask("ui.inspect", json!({}))["cloudRetention"].clone();
            if state["confirmed"] == confirmed {
                return;
            }
            assert!(Instant::now() < until, "retention consent did not become {confirmed}: {state}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn reveal_on_right(&mut self, id: &str) {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let widgets = self.ask("ui.widgets", json!({"filter":id}));
            let window = self.ask("ui.inspect", json!({}))["window"].clone();
            let height = window[1].as_f64().unwrap();
            if widgets.as_array().is_some_and(|items| {
                items.iter().any(|w| {
                    w["id"] == id
                        && w["rect"][1].as_f64().unwrap() > 80.
                        && w["rect"][1].as_f64().unwrap() + w["rect"][3].as_f64().unwrap() < height - 40.
                })
            }) {
                break;
            }
            assert!(Instant::now() < until, "Info control did not become visible: {id}: {widgets}");
            self.ask("ui.move", json!({"x":window[0].as_f64().unwrap()-180.,"y":height/2.}));
            self.ask("ui.scroll", json!({"dy":-300.}));
            std::thread::sleep(Duration::from_millis(50));
        }
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
#[ignore = "explicit native retention consent acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn retention_consent_is_fresh_after_reopening_and_resuming() {
    let f = support::Fixture::new();
    let local = f.dir.join("retention-consent");
    f.import(&local, false);
    let (out, rows) = f.run(&local, &[("cloud.pause", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("retention-consent");
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut app = Desktop::start(&f, &local, &artifacts);
    app.execute("dialog.cloudMaintenance", json!({}));
    app.click_when_ready("button:cloudRetentionTools");
    // The collapsing body registers controls before its opening clip is tall
    // enough to receive a pointer at the checkbox. Then wait for actual state,
    // since control-channel input is applied on subsequent frames.
    std::thread::sleep(Duration::from_millis(250));
    app.click_when_ready("checkbox:cloudRetentionConfirmed");
    app.retention_consent(true);
    app.ask("ui.screenshot", json!({"path":artifacts.join("checked-before-close.png")}));
    app.ask("ui.dialog.cancel", json!({}));
    app.execute("dialog.cloudMaintenance", json!({}));
    app.wait_widget("checkbox:cloudRetentionConfirmed");
    app.retention_consent(false);
    app.ask("ui.screenshot", json!({"path":artifacts.join("unchecked-after-reopen.png")}));
    app.click_when_ready("checkbox:cloudRetentionConfirmed");
    app.retention_consent(true);
    app.execute("cloud.resume", json!({}));
    app.retention_consent(false);
    app.ask("ui.screenshot", json!({"path":artifacts.join("unchecked-after-resume.png")}));
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.settled(&f, &[]);
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    let storage = f.state.lock().unwrap();
    assert_eq!(storage.signature_errors, 0);
    assert!(!storage.requests.iter().any(|(method, _)| method == "DELETE"));
}

#[test]
#[ignore = "explicit native Info Resume acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn info_resumes_paused_cloud_uploads_and_publication() {
    let f = support::Fixture::new();
    let local = f.dir.join("info-resume");
    let (_, source) = f.import(&local, false);
    let key = format!("/photos/library/originals/{}", support::hash(&std::fs::read(&source).unwrap()));
    let (out, rows) = f.run(&local, &[("cloud.pause", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("info-resume");
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut app = Desktop::start(&f, &local, &artifacts);
    let paused = app.execute("cloud.overview", json!({}));
    assert_eq!(paused["userPaused"], true);
    assert!(paused["summary"]["queuedFiles"].as_u64().unwrap_or(0) > 0, "acceptance fixture must have a waiting upload: {paused}");
    app.execute("library.select", json!({"ids":[1]}));
    app.execute("panel.info", json!({}));
    app.reveal_on_right("button:cloudSyncNow");
    app.ask("ui.screenshot", json!({"path":artifacts.join("paused-info.png")}));
    app.click_when_ready("button:cloudSyncNow");
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let status = app.execute("cloud.overview", json!({}));
        if status["userPaused"] == false {
            break;
        }
        assert!(Instant::now() < until, "Info dispatched Sync instead of Resume: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.settled(&f, std::slice::from_ref(&key));
    app.ask("ui.screenshot", json!({"path":artifacts.join("resumed-info.png")}));
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    let storage = f.state.lock().unwrap();
    assert_eq!(storage.signature_errors, 0);
    assert_eq!(storage.objects.get(&key), Some(&std::fs::read(source).unwrap()));
}

#[test]
#[ignore = "explicit native recovery paging acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn recovery_pages_reach_omitted_records_in_the_native_app() {
    use lightcraft_engine::{
        Session,
        catalog::{Op, PhotoId, replication::Change},
    };
    let f = support::Fixture::new();
    let local = f.dir.join("recovery-pages");
    f.import(&local, false);
    let (out, rows) = f.run(&local, &[("cloud.pause", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let before = {
        let mut s = Session::new().with_fs();
        s.open_library(&local, false).unwrap();
        let template = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
        for id in 2..=218 {
            let mut photo = template.clone();
            photo.id = PhotoId(id);
            photo.file_name = format!("recovery-photo-{id:03}.png");
            s.commit("fixture photo", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        let base = s.catalog.sync_state().unwrap().clone();
        let entities: Vec<_> = (1..=17).map(|id| format!("photo:{}", s.catalog.cloud_identity(&format!("photo:{id}")).unwrap())).collect();
        let mut state = base.clone();
        for n in 1..=9 {
            let mut branch = base.clone();
            branch.pending.clear();
            branch.device = format!("alternative-{n}");
            let changes = entities
                .iter()
                .map(|entity| Change {
                    entity: entity.clone(),
                    field: "caption".into(),
                    base: base.winning(entity, "caption").map(|(stamp, _)| stamp),
                    value: json!(format!("Caption from device {n}")),
                })
                .collect();
            state.receive(&branch.enqueue(changes).unwrap().unwrap()).unwrap();
        }
        s.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
        for id in 18..=218 {
            s.commit("fixture deletion", Op::RemovePhoto { id: PhotoId(id) }).unwrap();
        }
        let before = s.catalog.sync_state().unwrap().clone();
        s.close_library().unwrap();
        before
    };
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("recovery-pages");
    std::fs::create_dir_all(&artifacts).unwrap();
    let remote = f.state.lock().unwrap().objects.clone();
    let mut app = Desktop::start(&f, &local, &artifacts);
    let first = app.execute("cloud.syncStatus", json!({"section":"conflicts"}))["page"].clone();
    let seventeenth = app.execute("cloud.syncStatus", json!({"section":"conflicts","cursor":first["nextCursor"]}))["page"]["items"][0].clone();
    let entity = seventeenth["entity"].as_str().unwrap();
    let choices = app.execute("cloud.syncStatus", json!({"section":"choices","entity":entity,"field":"caption"}))["page"].clone();
    let ninth = app.execute("cloud.syncStatus",json!({"section":"choices","entity":entity,"field":"caption","cursor":choices["nextCursor"]}))["page"]["items"][0]["revision"].as_str().unwrap().to_owned();
    app.execute("dialog.cloudConflicts", json!({}));
    app.click_when_ready("button:cloudPageNext:conflicts");
    app.click_when_ready(&format!("button:cloudPageNext:choices:{entity}:caption"));
    app.wait_widget(&format!("button:cloudKeepRevision:{entity}:caption:{ninth}"));
    app.ask("ui.screenshot", json!({"path":artifacts.join("conflict-17-value-9.png")}));
    app.ask("ui.dialog.cancel", json!({}));
    app.execute("dialog.cloudMaintenance", json!({}));
    let omitted = {
        let mut cursor = Value::Null;
        let mut last = Value::Null;
        loop {
            let page = app.execute("cloud.syncStatus", json!({"section":"deleted","cursor":cursor}))["page"].clone();
            if let Some(item) = page["items"].as_array().unwrap().last() {
                last = item.clone();
            }
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                break last;
            }
        }
    };
    // Search reaches the last retained photo without recovering any earlier one.
    app.click_when_ready("field:cloudPageSearch:deleted");
    app.ask("ui.text", json!({"text":omitted["name"]}));
    app.wait_widget(&format!("button:cloudRecover:{}", omitted["entity"].as_str().unwrap()));
    app.ask("ui.screenshot", json!({"path":artifacts.join("deleted-201-search.png")}));
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    assert_eq!(
        app.execute("cloud.syncStatus", json!({"section":"conflicts"}))["page"]["revision"],
        first["revision"],
        "browsing leaves the opened catalog revision unchanged"
    );
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    // Read persisted data without creating another writing epoch. The app's
    // initial owner claim may rotate its local author; browsing must preserve
    // every register, proof, exact pending mutation and publication position.
    let (_, catalog, _) = lightcraft_engine::catalog::Journal::open(Box::new(lightcraft_engine::catalog::FsStore::open(&local).unwrap())).unwrap();
    let after = catalog.sync_state().unwrap();
    assert!(before.portable() == after.portable(), "retained cloud registers or causal proofs changed");
    assert!(before.pending == after.pending, "exact pending mutations changed");
    assert!(
        json!([
            before.connection,
            before.protocol,
            before.append,
            before.cursor,
            before.generation,
            before.migrated_from_head,
            before.require_existing
        ]) == json!([
            after.connection,
            after.protocol,
            after.append,
            after.cursor,
            after.generation,
            after.migrated_from_head,
            after.require_existing
        ]),
        "cloud connection or publication progress changed"
    );
    assert_eq!(f.state.lock().unwrap().objects, remote, "browsing does not publish or restore");
}

#[test]
#[ignore = "explicit native profile replacement acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn replacing_saved_keys_with_named_profile_probes_and_saves_the_selected_profile() {
    let f = support::Fixture::new();
    let local = f.dir.join("profile-replacement");
    let mut credentials = std::fs::OpenOptions::new().append(true).open(f.dir.join("credentials")).unwrap();
    writeln!(credentials, "\n[lightcraft-personal]\naws_access_key_id=EXPIRED_ACCESS\naws_secret_access_key=EXPIRED_SECRET").unwrap();
    drop(credentials);
    let (out, rows) = f.run(
        &local,
        &[(
            "cloud.configure",
            json!({"id":"personal","bucket":"photos","region":"us-east-1",
        "prefix":"library","endpoint":f.endpoint,"profile":"lightcraft-personal"}),
        )],
    );
    assert!(out.status.success(), "{rows:?}");
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("profile-replacement");
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut app = Desktop::start(&f, &local, &artifacts);
    app.execute("dialog.cloudStorage", json!({"id":"personal"}));
    app.click_when_ready("dropdown:cloudSetupAuthentication");
    app.click_when_ready("cloudSetupAuthentication:profile");
    app.click_when_ready("cloudSetupProfile");
    app.ask("ui.key", json!({"key":"A","cmd":true}));
    app.ask("ui.text", json!({"text":"acceptance"}));
    std::thread::sleep(Duration::from_millis(50));
    app.click_when_ready("button:cloudSetupTest");
    app.wait_widget("status:cloudSetupConnected");
    app.ask("ui.screenshot", json!({"path":artifacts.join("checked.png")}));
    app.click_when_ready("button:dialogOk");
    let until = Instant::now() + Duration::from_secs(10);
    let connections = loop {
        let value = app.execute("cloud.connections", json!({}));
        if value["connections"].as_array().unwrap().iter().any(|c| c["id"] == "personal" && c["profile"] == "acceptance") {
            break value;
        }
        assert!(Instant::now() < until, "selected profile was not saved: {value}");
        std::thread::sleep(Duration::from_millis(50));
    };
    std::fs::write(artifacts.join("connections.json"), serde_json::to_vec_pretty(&connections).unwrap()).unwrap();
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    let state = f.state.lock().unwrap();
    assert!(!state.requests.is_empty(), "the selected credentials must actually be probed");
    assert_eq!(state.signature_errors, 0, "using the expired managed profile must not pass");
    assert!(state.requests.iter().all(|(method, _)| method == "GET"), "connection editing is read-only against remote storage");
}

#[test]
#[ignore = "explicit native empty-library diagnostics acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn empty_cloud_library_error_details_preserve_diagnostic_and_repair_connection() {
    let f = support::Fixture::new();
    let local = f.dir.join("empty-cloud");
    let (out, rows) = f.run(
        &local,
        &[
            (
                "cloud.configure",
                json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
            ),
            ("cloud.syncEnable", json!({"connection":"personal"})),
            ("cloud.sync", json!({})),
        ],
    );
    assert!(out.status.success(), "{rows:?}");
    f.state.lock().unwrap().unauthorized = true;
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("empty-error");
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut app = Desktop::start(&f, &local, &artifacts);
    app.execute("cloud.sync", json!({"wait":false}));
    let until = Instant::now() + Duration::from_secs(30);
    let error = loop {
        let data = app.execute("cloud.overview", json!({}));
        assert_eq!(data["summary"]["cloudPhotos"], 0);
        if data["health"]["sync"]["lastError"]["kind"] == "authentication" {
            break data["health"]["sync"]["lastError"].clone();
        }
        assert!(Instant::now() < until, "authentication failure not visible: {data}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(error["httpStatus"], 403, "native transport preserves the actual rejection code");
    app.click_when_ready("icon:cloud");
    app.click_when_ready("button:cloudErrorDetails");
    let until = Instant::now() + Duration::from_secs(10);
    let inspect = loop {
        let inspect = app.ask("ui.inspect", json!({}));
        if inspect["cloudDiagnostic"].is_object() {
            break inspect;
        }
        assert!(Instant::now() < until, "Details did not open: {inspect}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(inspect["cloudDiagnostic"]["failure"]["message"], error["message"], "{inspect}");
    assert_eq!(inspect["cloudDiagnostic"]["failure"]["httpStatus"], error["httpStatus"]);
    assert_eq!(inspect["cloudDiagnostic"]["failure"]["recovery"], "editConnection");
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&inspect).unwrap()).unwrap();
    app.ask("ui.screenshot", json!({"path":artifacts.join("details.png")}));
    app.click_when_ready("button:cloudErrorConnection");
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let widgets = app.ask("ui.widgets", json!({"filter":"cloudSetup"}));
        if widgets.to_string().contains("button:cloudSetupMore") {
            break;
        }
        assert!(Instant::now() < until, "connection editor did not open: {widgets}");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.ask("ui.dialog.cancel", json!({}));
    f.state.lock().unwrap().unauthorized = false;
    app.execute("cloud.sync", json!({"wait":false}));
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let status = app.execute("cloud.syncStatus", json!({}));
        if status["health"]["state"] == "healthy" {
            break;
        }
        assert!(Instant::now() < until, "empty library did not recover: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
}

#[test]
#[ignore = "explicit native upgrade exposure acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn upgrade_successor_exposure_refuses_remote_cancellation_and_finishes_in_native_app() {
    let f = support::Fixture::new();
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("upgrade-exposure");
    std::fs::create_dir_all(&artifacts).unwrap();
    let local = f.dir.join("local");
    f.import(&local, false);
    let (out, rows) = f.run(
        &local,
        &[("cloud.upload", json!({})), ("cloud.syncEnable", json!({"connection":"personal"})), ("cloud.sync", json!({})), ("cloud.pause", json!({}))],
    );
    assert!(out.status.success(), "{rows:?}");
    {
        let mut state = f.state.lock().unwrap();
        state.append_mode = true;
        state.refuse_put = Some(("/catalog/head.json".into(), "\"state\":\"retired\"".into()));
    }
    let mut app = Desktop::start(&f, &local, &artifacts);
    let wait = |app: &mut Desktop, task: u64| -> Value {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let status = app.execute("cloud.syncStatus", json!({}));
            if let Some(report) = status["reports"].as_array().and_then(|reports| reports.iter().find(|report| report["task"] == task)) {
                return report.clone();
            }
            assert!(Instant::now() < until, "upgrade completion: {status}");
            std::thread::sleep(Duration::from_millis(40));
        }
    };
    let task = app.execute("cloud.upgradeLibrary", json!({"plan":"appendLog","wait":false}))["task"].as_u64().unwrap();
    assert_eq!(wait(&mut app, task)["ok"], false);
    let before = f.state.lock().unwrap().objects.clone();
    let task = app.execute("cloud.upgradeLibrary", json!({"cancel":true,"wait":false}))["task"].as_u64().unwrap();
    let refusal = wait(&mut app, task);
    assert_eq!(refusal["ok"], false, "{refusal}");
    assert!(refusal["error"].as_str().unwrap().contains("cancellation is unsafe"), "{refusal}");
    assert_eq!(f.state.lock().unwrap().objects, before);
    app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("refused.png")}));
    std::fs::write(artifacts.join("refusal.json"), serde_json::to_vec_pretty(&refusal).unwrap()).unwrap();
    f.state.lock().unwrap().refuse_put = None;
    let task = app.execute("cloud.upgradeLibrary", json!({"plan":"appendLog","wait":false}))["task"].as_u64().unwrap();
    let finished = wait(&mut app, task);
    assert_eq!(finished["ok"], true, "{finished}");
    assert_eq!(app.execute("cloud.syncStatus", json!({}))["protocol"], "append-log-v1");
    app.ask("ui.screenshot", json!({"path":artifacts.join("finished.png")}));
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
}

#[test]
#[ignore = "explicit native cache relocation acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn cache_relocation_adopts_verified_offline_originals_in_native_app() {
    let f = support::Fixture::new();
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("cache-relocation");
    std::fs::create_dir_all(&artifacts).unwrap();
    let publisher = f.dir.join("publisher");
    let local = f.dir.join("local");
    let (_, source) = f.import(&publisher, false);
    let (out, rows) = f.run(&publisher, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let (_, saved, _) = lightcraft_engine::catalog::Journal::open(Box::new(lightcraft_engine::catalog::FsStore::open(&publisher).unwrap())).unwrap();
    let library = saved.sync_state().unwrap().library.clone();
    let original = saved.assets().values().next().unwrap().original.id.clone();
    let (out, rows) = f.run(
        &local,
        &[
            (
                "cloud.configure",
                json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
            ),
            ("cloud.syncEnable", json!({"connection":"personal","library":library})),
            ("cloud.sync", json!({})),
            ("cloud.pin", json!({"ids":[1],"wait":true})),
            ("cloud.pause", json!({})),
        ],
    );
    assert!(out.status.success(), "{rows:?}");
    let mut app = Desktop::start(&f, &local, &artifacts);
    let before = app.execute("cloud.cacheLocation", json!({}));
    let old = std::path::PathBuf::from(before["path"].as_str().unwrap());
    let target = f.dir.join("relocated-cache");
    app.execute("cloud.cacheLocation", json!({"path":target,"wait":false}));
    let until = Instant::now() + Duration::from_secs(30);
    let report = loop {
        let status = app.execute("cloud.cacheLocation", json!({}));
        if !status["lastMove"].is_null() {
            assert_eq!(status["lastMove"]["ok"], true, "{status}");
            assert_eq!(std::path::Path::new(status["path"].as_str().unwrap()), target);
            break status;
        }
        assert!(Instant::now() < until, "native cache move completion: {status}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(std::fs::read(target.join(&original.0)).unwrap(), std::fs::read(source).unwrap());
    let cache = app.execute("cloud.cache", json!({}));
    assert!(cache["pins"].as_object().is_some_and(|pins| pins.contains_key("photo:1")), "offline owner retained: {cache}");
    app.execute("app.settings", json!({"tab":"cache"}));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let widgets = app.ask("ui.widgets", json!({"filter":"button:settingsCacheBrowse"}));
        if widgets.as_array().is_some_and(|widgets| !widgets.is_empty()) {
            break;
        }
        assert!(Instant::now() < until, "Cache settings did not appear: {widgets}");
        std::thread::sleep(Duration::from_millis(40));
    }
    app.ask("ui.screenshot", json!({"path":artifacts.join("adopted.png")}));
    std::fs::write(artifacts.join("move.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    // Native render leases may defer cleanup; the verified replacement and policy are
    // required independently of whether this optional space reclamation has finished.
    if old.join(&original.0).exists() {
        assert!(target.join(&original.0).exists());
    }
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
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
#[ignore = "explicit native local-only rejoin acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn local_only_rejoin_uploads_final_revisions_and_new_local_photos() {
    use lightcraft_engine::catalog::{FsStore, Journal, Source, uploads::UploadState};
    let f = support::Fixture::new();
    let local = f.dir.join("local-rejoin");
    let (import, source) = f.import(&local, false);
    let (out, rows) = f.run(
        &local,
        &[("cloud.upload", json!({"imports":[import]})), ("cloud.sync", json!({})), ("cloud.syncDisable", json!({"confirm":true,"wait":true}))],
    );
    assert!(out.status.success(), "{rows:?}");
    assert_eq!(rows[2]["result"]["stage"], "complete");
    let before = Journal::open(Box::new(FsStore::open(&local).unwrap())).unwrap().1;
    let library = before.sync_state().unwrap().library.clone();
    let original = before.assets().values().next().unwrap().original.clone();
    let permanent = local.join("Local Originals").join("blobs").join(&original.id.0);
    let expected = std::fs::read(&source).unwrap();
    assert_eq!(std::fs::read(&permanent).unwrap(), expected);
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("local-rejoin");
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut app = Desktop::start(&f, &local, &artifacts);
    let requests = f.state.lock().unwrap().requests.len();
    for rating in [3, 4] {
        app.execute("photo.rate", json!({"ids":[1],"rating":rating}));
        app.prepared("photo.saveMetadataToFile", 1);
    }
    let (source, _, _) = photo(&f, "added-while-local.png", 77);
    app.execute("library.import", json!({"paths":[source]}));
    app.execute("photo.rate", json!({"ids":[2],"rating":2}));
    assert_eq!(f.state.lock().unwrap().requests.len(), requests, "local revisions and imports never access cloud storage");
    app.execute("dialog.cloudSync", json!({}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("local-only-connect.png"),"headless":true}));
    app.ask("ui.clickWidget", json!({"id":"button:dialogOk"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("reenabling.png"),"headless":true}));
    let until = Instant::now() + Duration::from_secs(45);
    loop {
        let status = app.execute("cloud.overview", json!({}));
        if status["summary"]["cloudPhotos"] == 2 {
            break;
        }
        assert!(Instant::now() < until, "new local photo was not enrolled: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.settled(&f, &[]);
    app.ask("ui.clickWidget", json!({"id":"icon:cloud"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("rejoined.png")}));
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.ask("app.quit", json!({}));
    assert!(app.child.wait().unwrap().success());
    let saved = Journal::open(Box::new(FsStore::open(&local).unwrap())).unwrap().1;
    assert!(saved.local_sync_baseline().is_none());
    assert_eq!(saved.sync_state().unwrap().library, library);
    assert_eq!(saved.sync_state().unwrap().connection, "personal");
    assert_eq!(std::fs::read(&permanent).unwrap(), expected, "reenabling preserves permanent local originals");
    assert_eq!(saved.photos().count(), 2);
    assert!(saved.photos().all(|photo| matches!(photo.source, Source::Managed { .. })));
    assert!(saved.upload_intents().values().any(|intent| intent.state == UploadState::Superseded));
    assert!(saved.upload_intents().values().all(|intent| matches!(intent.state, UploadState::Synchronized | UploadState::Superseded)));
    assert_eq!(f.state.lock().unwrap().signature_errors, 0);
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

#[test]
#[ignore = "explicit native dialog lifecycle acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn control_confirmation_advances_cloud_setup_and_preserves_a_failed_form() {
    let f = support::Fixture::new();
    f.state.lock().unwrap().append_mode = true;
    let local = f.dir.join("dialog-lifecycle");
    let (out, rows) = f.run(&local, &[("catalog.query", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("dialog-lifecycle");
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut app = Desktop::start(&f, &local, &artifacts);
    app.execute("dialog.cloudSync", json!({}));
    app.click_when_ready("radio:cloudSetupProvider:other");
    app.ask("ui.screenshot", json!({"path":artifacts.join("provider.png"),"headless":true}));
    app.ask("ui.dialog.confirm", json!({}));
    for (field, value) in [("cloudSetupBucket", "photos"), ("cloudSetupEndpoint", f.endpoint.as_str()), ("cloudSetupRegion", "us-east-1")] {
        app.click_when_ready(field);
        app.ask("ui.key", json!({"key":"A","cmd":true}));
        app.ask("ui.text", json!({"text":value}));
        app.ask("ui.screenshot", json!({"path":artifacts.join("input-barrier.png"),"headless":true}));
    }
    app.click_when_ready("dropdown:cloudSetupAuthentication");
    app.click_when_ready("cloudSetupAuthentication:profile");
    app.wait_widget("cloudSetupProfile");
    app.ask("ui.screenshot", json!({"path":artifacts.join("input-barrier.png"),"headless":true}));
    if !app.ask("ui.widgets", json!({"filter":"cloudSetupPrefix"})).to_string().contains("cloudSetupPrefix") {
        app.click_when_ready("button:cloudSetupMore");
        std::thread::sleep(Duration::from_millis(250));
    }
    app.click_when_ready("cloudSetupPrefix");
    app.ask("ui.key", json!({"key":"A","cmd":true}));
    app.ask("ui.text", json!({"text":"library"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("input-barrier.png"),"headless":true}));
    assert_eq!(app.raw("ui.dialog.confirm", json!({}))["ok"], false);
    app.wait_widget("cloudSetupEndpoint");
    app.ask("ui.screenshot", json!({"path":artifacts.join("failed-form.png"),"headless":true}));
    app.click_when_ready("cloudSetupProfile");
    app.ask("ui.text", json!({"text":"acceptance"}));
    app.ask("ui.screenshot", json!({"path":artifacts.join("input-barrier.png"),"headless":true}));
    app.ask("ui.dialog.confirm", json!({}));
    app.wait_widget("radio:cloudSetupNew");
    app.ask("ui.screenshot", json!({"path":artifacts.join("library-review.png"),"headless":true}));
    app.ask("ui.dialog.confirm", json!({}));
    let status = app.execute("cloud.syncStatus", json!({}));
    assert_eq!(status["enabled"], true);
    assert_eq!(status["connection"], "storage");
    assert!(
        app.execute("cloud.connections", json!({}))["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "storage" && c["profile"] == "acceptance")
    );
    app.settled(&f, &[]);
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.ask("app.quit", json!({}));
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = app.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < until, "native app did not exit after its settled cloud tasks");
        std::thread::sleep(Duration::from_millis(50));
    }
    let state = f.state.lock().unwrap();
    assert_eq!(state.signature_errors, 0);
    assert!(state.requests.iter().all(|(method, _)| method != "DELETE"));
}

#[test]
#[ignore = "explicit native precise cloud status acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn popup_sync_now_checks_the_library_and_pause_resume_stays_explicit() {
    let f = support::Fixture::new();
    let local = f.dir.join("precise-status");
    let (out, rows) = f.run(
        &local,
        &[
            (
                "cloud.configure",
                json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
            ),
            ("cloud.syncEnable", json!({"connection":"personal"})),
            ("cloud.sync", json!({})),
        ],
    );
    assert!(out.status.success(), "{rows:?}");
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("precise-status");
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut app = Desktop::start(&f, &local, &artifacts);
    app.execute("cloud.sync", json!({"wait":false}));
    app.settled(&f, &[]);
    app.click_when_ready("icon:cloud");
    app.wait_widget("indicator:cloudFooterCheckCircle");
    app.ask("ui.screenshot", json!({"path":artifacts.join("up-to-date.png"),"headless":true}));
    let head_reads = |f: &support::Fixture| {
        f.state.lock().unwrap().requests.iter().filter(|(method, key)| method == "GET" && key.ends_with("catalog/head.json")).count()
    };
    let before = head_reads(&f);
    app.click_when_ready("button:cloudPopupSyncNow");
    let until = Instant::now() + Duration::from_secs(30);
    while head_reads(&f) <= before {
        assert!(Instant::now() < until, "Sync now did not perform a remote check");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.settled(&f, &[]);
    app.click_when_ready("icon:cloud");
    app.wait_widget("indicator:cloudFooterCheckCircle");
    app.click_when_ready("button:cloudPauseResume");
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if app.execute("cloud.overview", json!({}))["userPaused"] == true {
            break;
        }
        assert!(Instant::now() < until, "pause was not applied");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The input reply reports the action before the popup's next layout is visible.
    app.ask("ui.screenshot", json!({"path":artifacts.join("paused.png"),"headless":true}));
    if !app.ask("ui.widgets", json!({"filter":"popup:cloud"})).to_string().contains("popup:cloud") {
        app.click_when_ready("icon:cloud");
    }
    app.wait_widget("indicator:cloudFooterPause");
    app.ask("ui.screenshot", json!({"path":artifacts.join("paused.png"),"headless":true}));
    app.click_when_ready("button:cloudPauseResume");
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let status = app.execute("cloud.overview", json!({}));
        if status["userPaused"] == false && status["health"]["sync"]["state"] == "healthy" {
            break;
        }
        assert!(Instant::now() < until, "resume did not finish: {status}");
        std::thread::sleep(Duration::from_millis(50));
    }
    app.settled(&f, &[]);
    app.ask("app.quit", json!({}));
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = app.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < until, "native app did not exit after sync settled");
        std::thread::sleep(Duration::from_millis(50));
    }
    let state = f.state.lock().unwrap();
    assert_eq!(state.signature_errors, 0);
    assert!(state.requests.iter().all(|(method, _)| method != "DELETE"));
}

#[test]
#[ignore = "explicit native retained-value acceptance; requires LIGHTCRAFT_NATIVE_LIBRARY_APP"]
fn retained_values_show_full_metadata_and_independent_previews_without_resolving() {
    let f = support::Fixture::new();
    let local = f.dir.join("choice-local");
    let peer = f.dir.join("choice-peer");
    f.import(&local, false);
    let (out, rows) = f.run(&local, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let (_, saved, _) = lightcraft_engine::catalog::Journal::open(Box::new(lightcraft_engine::catalog::FsStore::open(&local).unwrap())).unwrap();
    let library = saved.sync_state().unwrap().library.clone();
    let entity = format!("photo:{}", saved.cloud_identity("photo:1").unwrap());
    let (out, rows) = f.run(
        &peer,
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
    let (out, rows) = f.run(
        &local,
        &[
            ("library.select", json!({"ids":[1],"active":1})),
            ("develop.set", json!({"values":{"light.exposure":0.25}})),
            ("photo.setMeta", json!({"ids":[1],"caption":"Shared prefix ".repeat(25)+"local caption tail"})),
            ("cloud.sync", json!({})),
        ],
    );
    assert!(out.status.success(), "{rows:?}");
    let (out, rows) = f.run(
        &peer,
        &[
            ("library.select", json!({"ids":[1],"active":1})),
            ("develop.set", json!({"values":{"light.exposure":1.5}})),
            ("crop.set", json!({"rect":[0.2,0.1,0.6,0.9]})),
            ("photo.setMeta", json!({"ids":[1],"caption":"Shared prefix ".repeat(25)+"peer caption tail"})),
            ("cloud.sync", json!({})),
        ],
    );
    assert!(out.status.success(), "{rows:?}");
    let (out, rows) = f.run(&local, &[("cloud.sync", json!({})), ("cloud.pause", json!({}))]);
    assert!(out.status.success(), "{rows:?}");
    let artifacts = std::path::PathBuf::from(std::env::var("LIGHTCRAFT_LIBRARY_SNAPSHOTS").unwrap()).join("retained-values");
    std::fs::create_dir_all(&artifacts).unwrap();
    let remote = f.state.lock().unwrap().objects.clone();
    let mut app = Desktop::start(&f, &local, &artifacts);
    let baseline = app.execute("cloud.syncStatus", json!({"section":"conflicts"}))["page"].clone();
    app.execute("dialog.cloudConflicts", json!({}));
    app.ask("ui.screenshot", json!({"headless":true,"path":artifacts.join("conflicts.png")}));
    for (field, mode) in [("meta.caption", "details"), ("develop", "preview")] {
        app.click_when_ready("field:cloudPageSearch:conflicts");
        app.ask("ui.key", json!({"key":"A","cmd":true}));
        app.ask("ui.text", json!({"text":field}));
        app.ask("ui.screenshot", json!({"headless":true,"path":artifacts.join(format!("{mode}-filtered.png"))}));
        let choices =
            app.execute("cloud.syncStatus", json!({"section":"choices","entity":entity,"field":field}))["page"]["items"].as_array().unwrap().clone();
        assert!(choices.len() >= 2, "{field}: {choices:?}");
        let choice = choices.iter().find(|choice| choice["current"] != true).unwrap();
        let revision = choice["revision"].as_str().unwrap();
        let value = app.execute("cloud.syncStatus", json!({"section":"choice","entity":entity,"field":field,"revision":revision}));
        if field == "meta.caption" {
            assert!(value["value"].as_str().unwrap().ends_with("caption tail"));
            assert!(value["value"].as_str().unwrap().len() > 300);
        }
        app.click_when_ready(&format!("button:cloudChoice:{mode}:{entity}:{field}:{revision}"));
        if mode == "preview" {
            app.wait_widget("image:cloudChoiceRetained");
        } else {
            app.wait_widget("button:cloudChoiceCopy");
        }
        app.ask("ui.screenshot", json!({"headless":true,"path":artifacts.join(format!("{mode}.png"))}));
        if mode == "preview" {
            app.click_when_ready("button:cloudChoiceCompare");
            app.ask("ui.screenshot", json!({"headless":true,"path":artifacts.join("compare-layout.png")}));
            app.wait_widget("image:cloudChoiceCurrent");
            app.wait_widget("image:cloudChoiceRetained");
            app.ask("ui.screenshot", json!({"headless":true,"path":artifacts.join("compare.png")}));
        }
        app.ask("ui.dialog.confirm", json!({}));
        app.ask("ui.screenshot", json!({"headless":true,"path":artifacts.join(format!("{mode}-done.png"))}));
    }
    let after = app.execute("cloud.syncStatus", json!({"section":"conflicts"}))["page"].clone();
    assert_eq!(after["revision"], baseline["revision"]);
    assert_eq!(after["items"], baseline["items"]);
    std::fs::write(artifacts.join("inspect.json"), serde_json::to_vec_pretty(&app.ask("ui.inspect", json!({}))).unwrap()).unwrap();
    app.ask("ui.dialog.cancel", json!({}));
    app.ask("app.quit", json!({}));
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = app.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < until, "native app quit");
        std::thread::sleep(Duration::from_millis(50));
    }
    let storage = f.state.lock().unwrap();
    assert_eq!(storage.objects, remote, "viewing never publishes or resolves a choice");
    assert_eq!(storage.signature_errors, 0);
}
