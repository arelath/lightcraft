//! Cloud setup end to end in a headless window: provider, a connection check against a
//! loopback S3-shaped server (signed with the typed keys, listed in the activity stack), then
//! joining the library it found.

use crate::cloud_setup::{Authentication, Check, Choice, Secret, Step};
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
    s3_response(requests, release, false)
}

fn s3_response(requests: usize, release: mpsc::Receiver<()>, forbidden: bool) -> (String, Arc<Mutex<Vec<(String, String)>>>) {
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
            let (status, body) = if forbidden {
                (403, String::new())
            } else if line.contains("list-type=2") {
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
    run_wizard(false);
}

#[test]
fn control_confirmation_checks_keys_and_advances_the_same_wizard() {
    run_wizard(true);
}

fn confirm(h: &mut Headless, control: bool) {
    if control {
        assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], true);
    } else {
        click(h, "button:dialogOk");
    }
}

fn run_wizard(control: bool) {
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
    confirm(&mut h, control);
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
        confirm(&mut h, control);
        // the check is running: its row in the activity stack and a line in the dialog
        assert_eq!(setup(&h).check, Check::Running);
        assert!(h.app.session.activity.list().iter().any(|task| task.kind == "cloudCheck" && task.label == "Checking cloud storage"));
        assert!(h.view.painted_text().iter().any(|t| t == "Checking the connection…"));
        assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], false, "a running check cannot be confirmed twice");
        assert_eq!(setup(&h).check, Check::Running);
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
    confirm(&mut h, control);
    assert_eq!(setup(&h).step, Step::Library, "a passed check isn't run again");
    // finish with the keys an earlier setup saved (typed keys would go to the real AWS file)
    if let Some(Dialog::CloudSetup { setup }) = &mut h.app.ui.dialog {
        setup.key_id.clear();
        setup.secret = Secret::default();
        setup.saved_keys = true;
        setup.authentication = Authentication::SavedKeys;
        // This test substitutes the same already-probed fixture keys as a saved
        // binding, avoiding a write to the real credentials file.
        setup.checked = crate::cloud_setup::fingerprint(setup);
    }
    for _ in 0..3 {
        h.step();
    }
    confirm(&mut h, control);
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

#[test]
fn cloud_confirmation_errors_keep_values_on_both_routes() {
    let dialogs = vec![
        Dialog::CloudStorage {
            id: "personal".into(),
            bucket: "photos".into(),
            region: "us-east-1".into(),
            prefix: "library".into(),
            endpoint: "http://127.0.0.1:1".into(),
            profile: "acceptance".into(),
        },
        Dialog::CloudConnect {
            connection: "personal".into(),
            library: LIBRARY.into(),
            protocol: lightcraft_catalog::replication::SyncProtocol::AppendLog,
        },
        Dialog::CloudMigration { writers_stopped: true },
        Dialog::CloudImport { connection: "personal".into(), connections: vec!["personal".into()], adopt_ids: vec![1] },
    ];
    for control in [false, true] {
        for dialog in &dialogs {
            let services = Services {
                cloud_action: Some(Box::new(|_, id, _| match id {
                    "cloud.configure" | "cloud.syncEnable" | "cloud.upgradeLibrary" | "cloud.adopt" => {
                        Some(Err("fixture refusal; preserve the review".into()))
                    }
                    _ => None,
                })),
                ..Default::default()
            };
            let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
            h.app.ui.dialog = Some(dialog.clone());
            for _ in 0..4 {
                h.step();
            }
            if control {
                assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], false);
            } else {
                click(&mut h, "button:dialogOk");
            }
            assert_eq!(h.app.ui.dialog.as_ref(), Some(dialog), "control={control}");
        }
    }
}

#[test]
fn setup_failure_and_invalid_choice_keep_the_review_without_saving_keys() {
    for control in [false, true] {
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [1400., 1000.], 1.);
        let mut source = crate::cloud_setup::Setup {
            step: Step::Connect,
            endpoint: "not an endpoint".into(),
            key_id: "AKIDTEST".into(),
            secret: Secret("SECRETKEY".into()),
            ..Default::default()
        };
        h.app.ui.dialog = Some(Dialog::CloudSetup { setup: Box::new(source.clone()) });
        for _ in 0..3 {
            h.step();
        }
        if control {
            assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], false);
        } else {
            click(&mut h, "button:dialogOk");
        }
        let failed = setup(&h);
        assert!(matches!(failed.check, Check::Failed { .. }));
        assert_eq!((failed.endpoint, failed.key_id, failed.secret), (source.endpoint.clone(), source.key_id.clone(), source.secret.clone()));
        source.step = Step::Library;
        source.choice = Choice::ById;
        source.join_id = "invalid identifier".into();
        source.check = Check::Passed { found: Default::default() };
        source.checked = crate::cloud_setup::fingerprint(&source);
        h.app.ui.dialog = Some(Dialog::CloudSetup { setup: Box::new(source.clone()) });
        let before = h.app.ui.dialog.clone();
        assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], false);
        assert_eq!(h.app.ui.dialog, before, "invalid choice must never write credentials");
    }
}

#[test]
fn library_generation_discards_old_migration_consent_before_control_confirmation() {
    use lightcraft_engine::{Session, catalog::MemStore, library::LibraryStores};
    let mut h = Headless::new(LightcraftApp::new(Session::new(), Services::default()), [1400., 1000.], 1.);
    h.app.ui.dialog = Some(Dialog::CloudMigration { writers_stopped: true });
    h.app
        .session
        .open_library_in(
            LibraryStores { dir: "replacement".into(), catalog: Box::new(MemStore::new()), files: Box::new(MemStore::new()), on_disk: false },
            false,
        )
        .unwrap();
    assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], false);
    assert!(h.app.ui.dialog.is_none());
    assert!(!h.app.session.activity.list().iter().any(|task| task.kind == "cloudUpgrade"));
}

#[test]
fn confirmation_preserves_a_replacement_dialog() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [1400., 1000.], 1.);
    crate::panels::dialogs::prompt(&mut h.app, "About", "", "", "app.about", json!({}), "unused");
    assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], true);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::About)));
}

#[test]
fn asynchronous_probe_failure_keeps_the_same_entered_form_on_both_routes() {
    for control in [false, true] {
        let (release, wait) = mpsc::channel();
        let (endpoint, seen) = s3_response(1, wait, true);
        let source = crate::cloud_setup::Setup {
            step: Step::Connect,
            provider: lightcraft_engine::cloud_setup::Provider::Other,
            name: "fixture".into(),
            bucket: "photos".into(),
            endpoint,
            region: "us-east-1".into(),
            prefix: "library".into(),
            key_id: "AKIDTEST".into(),
            secret: Secret("SECRETKEY".into()),
            ..Default::default()
        };
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [1400., 1000.], 1.);
        h.app.ui.dialog = Some(Dialog::CloudSetup { setup: Box::new(source.clone()) });
        for _ in 0..3 {
            h.step();
        }
        confirm(&mut h, control);
        assert_eq!(setup(&h).check, Check::Running);
        release.send(()).unwrap();
        assert!(h.step_until(T, |h| matches!(setup(h).check, Check::Failed { .. })));
        let failed = setup(&h);
        assert_eq!(failed.step, Step::Connect);
        assert_eq!((failed.endpoint, failed.bucket, failed.key_id, failed.secret), (source.endpoint, source.bucket, source.key_id, source.secret));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}

#[test]
fn failed_final_setup_configuration_retains_the_successfully_checked_form() {
    use lightcraft_engine::{Session, catalog::MemStore, library::LibraryStores};
    for control in [false, true] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let services = Services {
            cloud_action: Some(Box::new(move |_, id, _| match id {
                "cloud.configure" | "cloud.syncEnable" | "cloud.sync" => {
                    recorded.lock().unwrap().push(id.to_string());
                    Some(Err("fixture settings write failed".into()))
                }
                _ => None,
            })),
            ..Default::default()
        };
        let mut session = Session::new();
        session
            .open_library_in(
                LibraryStores {
                    dir: "setup-final-error".into(),
                    catalog: Box::new(MemStore::new()),
                    files: Box::new(MemStore::new()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        let mut source = crate::cloud_setup::Setup {
            step: Step::Library,
            provider: lightcraft_engine::cloud_setup::Provider::Other,
            name: "fixture".into(),
            bucket: "photos".into(),
            endpoint: "http://127.0.0.1:1".into(),
            region: "us-east-1".into(),
            prefix: "library".into(),
            authentication: Authentication::SavedKeys,
            saved_keys: true,
            check: Check::Passed { found: Default::default() },
            ..Default::default()
        };
        source.checked = crate::cloud_setup::fingerprint(&source);
        let mut h = Headless::new(LightcraftApp::new(session, services), [1400., 1000.], 1.);
        let before = Dialog::CloudSetup { setup: Box::new(source) };
        h.app.ui.dialog = Some(before.clone());
        for _ in 0..3 {
            h.step();
        }
        if control {
            assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], false);
        } else {
            click(&mut h, "button:dialogOk");
        }
        assert_eq!(h.app.ui.dialog, Some(before));
        assert_eq!(*calls.lock().unwrap(), vec!["cloud.configure"]);
    }
}
