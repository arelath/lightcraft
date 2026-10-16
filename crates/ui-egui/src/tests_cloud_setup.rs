//! Cloud setup end to end in a headless window: provider, a connection check against a
//! loopback S3-shaped server (signed with the typed keys, listed in the activity stack), then
//! joining the library it found.

use crate::cloud_setup::{Check, Choice, Secret, Step};
use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const T: Duration = Duration::from_secs(20);
const LIBRARY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Answers `requests` S3 requests: a listing with one append library, 404 for anything else.
/// The first reply waits for `release`, so the test sees the check running. Records each
/// request line and Authorization header.
fn s3(requests: usize, release: mpsc::Receiver<()>) -> (String, Arc<Mutex<Vec<(String, String)>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for n in 0..requests {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut auth = String::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                if let Some(value) = header.strip_prefix("authorization:").or_else(|| header.strip_prefix("Authorization:")) {
                    auth = value.trim().to_owned();
                }
            }
            if n == 0 {
                let _ = release.recv_timeout(T);
            }
            log.lock().unwrap().push((line.trim().to_owned(), auth));
            let (status, body) = if line.contains("list-type=2") {
                (
                    200,
                    format!(
                        "<ListBucketResult><Prefix>lightcraft/sync-v2/</Prefix><IsTruncated>false</IsTruncated><CommonPrefixes><Prefix>lightcraft/sync-v2/{LIBRARY}/</Prefix></CommonPrefixes></ListBucketResult>"
                    ),
                )
            } else {
                (404, String::new())
            };
            let mut stream = reader.into_inner();
            write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (endpoint, seen)
}

fn setup(h: &Headless) -> crate::cloud_setup::Setup {
    match &h.app.ui.dialog {
        Some(Dialog::CloudSetup { setup }) => (**setup).clone(),
        other => panic!("no cloud setup: {other:?}"),
    }
}

fn click(h: &mut Headless, id: &str) {
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..3 {
        h.step();
    }
}

#[test]
fn setup_checks_the_typed_keys_then_joins_the_library_it_found() {
    let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let recorded = calls.clone();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, params| match id {
            "cloud.connections" => Some(Ok(json!({"connections": []}))),
            "cloud.configure" | "cloud.syncEnable" | "cloud.sync" => {
                recorded.lock().unwrap().push((id.to_owned(), params.clone()));
                Some(Ok(json!({})))
            }
            _ => None,
        })),
        ..Default::default()
    };
    // The wizard now checks the library's retained destination before saving
    // credentials. Keep that real boundary even though command dispatch is mocked.
    let mut session = lightcraft_engine::Session::new();
    session
        .open_library_in(
            lightcraft_engine::library::LibraryStores {
                dir: "cloud-setup-fixture".into(),
                catalog: Box::new(lightcraft_engine::catalog::MemStore::new()),
                files: Box::new(lightcraft_engine::catalog::MemStore::new()),
                on_disk: false,
            },
            true,
        )
        .unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, services), [1400., 1000.], 1.);
    h.step();
    h.app.run("dialog.cloudSync", json!({})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(h.view.painted_text().iter().any(|t| t == "Where do you want to keep your cloud library?"));
    click(&mut h, "radio:cloudSetupProvider:other");
    click(&mut h, "button:dialogOk");
    assert_eq!((setup(&h).step, setup(&h).name.as_str()), (Step::Connect, "storage"));
    let (endpoint, seen) = {
        let (release, wait) = mpsc::channel();
        let server = s3(2, wait);
        if let Some(Dialog::CloudSetup { setup }) = &mut h.app.ui.dialog {
            setup.endpoint = server.0.clone();
            setup.bucket = "photos".into();
            setup.key_id = "AKIDTEST".into();
            setup.secret = Secret("SECRETKEY".into());
        }
        click(&mut h, "button:dialogOk");
        // the check is running: its row in the activity stack and a line in the dialog
        assert_eq!(setup(&h).check, Check::Running);
        assert!(h.app.session.activity.list().iter().any(|task| task.kind == "cloudCheck" && task.label == "Checking cloud storage"));
        assert!(h.view.painted_text().iter().any(|t| t == "Checking the connection…"));
        release.send(()).unwrap();
        server
    };
    assert!(h.step_until(T, |h| setup(h).step == Step::Library), "{:?}", setup(&h).check);
    assert!(h.app.session.activity.list().iter().all(|task| task.kind != "cloudCheck"), "the row goes with the check");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "a listing and the head, nothing written: {requests:?}");
    assert!(requests.iter().all(|(line, auth)| line.starts_with("GET ") && auth.contains("Credential=AKIDTEST/")), "{requests:?}");
    assert_eq!(setup(&h).choice, Choice::Found(LIBRARY.into()), "the one library found is chosen");
    for _ in 0..3 {
        h.step();
    }
    let text = h.view.painted_text();
    assert!(text.iter().any(|t| t.contains("Join library 0123456789ab… (append-only)")), "{text:?}");
    assert!(text.iter().any(|t| t == "Enable & Synchronize"));
    click(&mut h, "button:cloudSetupBack");
    assert_eq!(setup(&h).step, Step::Connect);
    assert!(setup(&h).passed().is_some(), "going back keeps the passed check");
    click(&mut h, "button:dialogOk");
    assert_eq!(setup(&h).step, Step::Library, "a passed check isn't run again");
    // finish with the keys an earlier setup saved (typed keys would go to the real AWS file)
    if let Some(Dialog::CloudSetup { setup }) = &mut h.app.ui.dialog {
        setup.key_id.clear();
        setup.secret = Secret::default();
        setup.saved_keys = true;
    }
    for _ in 0..3 {
        h.step();
    }
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none(), "finishing closes the dialog: {:?}", h.app.ui.dialog);
    let calls = calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            (
                "cloud.configure".to_owned(),
                json!({"id":"storage","bucket":"photos","region":"us-east-1","prefix":"lightcraft","endpoint":endpoint,"profile":"lightcraft-storage"})
            ),
            ("cloud.syncEnable".to_owned(), json!({"connection":"storage","protocol":"append-log-v1","library":LIBRARY})),
            ("cloud.sync".to_owned(), json!({"wait":false})),
        ]
    );
}

#[test]
fn a_failed_check_says_what_to_fix_and_cloud_storage_saves_after_a_check() {
    let services = Services {
        cloud_action: Some(Box::new(|_, id, _| match id {
            "cloud.connections" => Some(Ok(json!({"connections": [
                {"id":"b2","bucket":"photos","region":"us-west-004","prefix":"lightcraft","endpoint":"https://s3.us-west-004.backblazeb2.com","profile":"lightcraft-b2"}
            ]}))),
            _ => None,
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
    h.step();
    h.app.run("dialog.cloudStorage", json!({})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    // a saved connection opens on its settings, with its saved keys kept
    let opened = setup(&h);
    assert_eq!((opened.step, opened.storage_only, opened.saved_keys), (Step::Connect, true, true));
    assert_eq!(opened.provider, lightcraft_engine::cloud_setup::Provider::Backblaze);
    assert_eq!(opened.endpoint, "s3.us-west-004.backblazeb2.com");
    assert!(h.view.painted_text().iter().any(|t| t == "Save Connection"));
    if let Some(Dialog::CloudSetup { setup }) = &mut h.app.ui.dialog {
        setup.endpoint = "not an endpoint".into();
    }
    click(&mut h, "button:dialogOk");
    match setup(&h).check {
        Check::Failed { error } => assert!(error.contains("endpoint as Backblaze shows it"), "{error}"),
        other => panic!("{other:?}"),
    }
    assert!(h.view.painted_text().iter().any(|t| t.contains("endpoint as Backblaze shows it")));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::CloudSetup { .. })), "the dialog stays open to fix it");
}
