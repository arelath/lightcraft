use super::*;
use crate::{Services, headless::Headless, state::Dialog};
use lightcraft_catalog::{MemStore, Op, replication::SyncState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(15);
#[derive(Default)]
struct Fixture {
    calls: Vec<Value>,
    reports: Vec<Value>,
}
fn open(session: &mut lightcraft_engine::Session) {
    session
        .open_library_in(
            lightcraft_engine::library::LibraryStores {
                dir: "retention-consent".into(),
                catalog: Box::new(MemStore::new()),
                files: Box::new(MemStore::new()),
                on_disk: false,
            },
            false,
        )
        .unwrap();
    session
        .apply_received(Op::SetSyncState { state: Some(Box::new(SyncState::new("library".into(), "device".into(), "personal".into()).unwrap())) })
        .unwrap();
}
fn frames(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}
fn click(h: &mut Headless, id: &str) {
    assert_eq!(h.request("ui.clickWidget", json!({"id":id}), TIMEOUT)["ok"], true, "{id}");
    frames(h);
}
fn fixture() -> (Headless, Arc<Mutex<Fixture>>) {
    let mut session = lightcraft_engine::Session::new();
    open(&mut session);
    let data = Arc::new(Mutex::new(Fixture::default()));
    let worker = data.clone();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, params| {
            let mut data = worker.lock().unwrap();
            match id {
                "cloud.reclaim" => {
                    data.calls.push(params.clone());
                    Some(Ok(json!({"task":if params["apply"]==true {2} else {1}})))
                }
                "cloud.uploadStatus" => Some(Ok(json!({"reports":data.reports}))),
                _ => None,
            }
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(session, services), [1400., 1200.], 1.);
    frames(&mut h);
    h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
    frames(&mut h);
    click(&mut h, "button:cloudRetentionTools");
    click(&mut h, "button:cloudRetentionPreview");
    assert_eq!(h.app.cloud_retention.review.task, Some(1));
    (h, data)
}
fn ready(h: &mut Headless, data: &Arc<Mutex<Fixture>>) {
    data.lock().unwrap().reports.push(json!({"task":1,"ok":true,"result":{
        "preview":"audited-token","applySupported":true,"applied":false,
        "candidates":[{"key":"orphan"}],"candidateBytes":7,"objects":1
    }}));
    assert!(h.step_until(TIMEOUT, |h| h.app.cloud_retention.review.task.is_none()));
    frames(h);
}

#[test]
fn every_dialog_exit_requires_fresh_consent_but_keeps_the_audit() {
    for exit in ["done", "escape", "cancel", "confirm", "replacement", "reopen"] {
        let (mut h, data) = fixture();
        ready(&mut h, &data);
        click(&mut h, "checkbox:cloudRetentionConfirmed");
        assert!(h.app.cloud_retention.review.confirmed, "{exit}");
        match exit {
            "done" => click(&mut h, "button:dialogOk"),
            "escape" => {
                h.request("ui.key", json!({"key":"Escape"}), TIMEOUT);
                frames(&mut h);
            }
            "cancel" => {
                assert_eq!(h.request("ui.dialog.cancel", json!({}), TIMEOUT)["ok"], true);
            }
            "confirm" => {
                assert_eq!(h.request("ui.dialog.confirm", json!({}), TIMEOUT)["ok"], true);
            }
            "replacement" => {
                h.app.run("dialog.cloudConflicts", json!({})).unwrap();
            }
            _ => {
                assert_eq!(exit, "reopen");
                h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
            }
        }
        assert!(!h.app.cloud_retention.review.confirmed, "{exit} kept stale consent");
        h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
        frames(&mut h);
        assert_eq!(h.app.cloud_retention.review.preview.as_ref().unwrap()["preview"], "audited-token");
        click(&mut h, "button:cloudRetentionApply");
        assert_eq!(data.lock().unwrap().calls.len(), 1, "{exit} applied without fresh consent");
        click(&mut h, "checkbox:cloudRetentionConfirmed");
        click(&mut h, "button:cloudRetentionApply");
        assert_eq!(h.app.cloud_retention.review.task, Some(2));
        assert!(!h.app.cloud_retention.review.confirmed, "apply consumes consent immediately");
        let data = data.lock().unwrap();
        assert_eq!(data.calls.len(), 2);
        assert_eq!(data.calls[1]["preview"], "audited-token");
        assert_eq!(data.calls[1]["allWritersPausedAndPublished"], true);
    }
}

#[test]
fn a_retention_task_completes_while_closed_and_is_still_reviewable() {
    let (mut h, data) = fixture();
    click(&mut h, "checkbox:cloudRetentionConfirmed");
    assert!(h.app.cloud_retention.review.confirmed);
    h.request("ui.dialog.cancel", json!({}), TIMEOUT);
    assert_eq!(h.app.cloud_retention.review.task, Some(1));
    assert!(!h.app.cloud_retention.review.confirmed);
    ready(&mut h, &data);
    assert!(h.app.ui.dialog.is_none(), "completion must not reopen the view");
    h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
    frames(&mut h);
    assert!(h.app.cloud_retention.review.preview.is_some());
    assert!(!h.app.cloud_retention.review.confirmed);
    assert!(h.view.painted_text().iter().any(|t| t.contains("Eligible orphan objects")));
    assert_eq!(data.lock().unwrap().calls.len(), 1);
}

#[test]
fn successful_resume_resets_consent_on_engine_and_intercepted_routes() {
    let (mut h, data) = fixture();
    ready(&mut h, &data);
    click(&mut h, "checkbox:cloudRetentionConfirmed");
    assert!(h.app.session.cloud_paused);
    h.app.run("cloud.resume", json!({})).unwrap();
    assert!(!h.app.session.cloud_paused);
    assert!(!h.app.cloud_retention.review.confirmed);
    assert!(h.app.cloud_retention.review.preview.is_some());
    assert!(matches!(h.app.ui.dialog, Some(Dialog::CloudMaintenance)));
    for succeeded in [false, true] {
        h.app.cloud_retention.review.confirmed = true;
        h.app.services.cloud_action = Some(Box::new(move |_, id, _| {
            (id == "cloud.resume").then(|| if succeeded { Ok(json!({})) } else { Err("wait for current cloud tasks".into()) })
        }));
        assert_eq!(h.app.run("cloud.resume", json!({})).is_ok(), succeeded);
        assert_eq!(h.app.cloud_retention.review.confirmed, !succeeded);
    }
}

#[test]
fn reopening_the_same_library_identity_invalidates_old_tasks_and_audits() {
    let (mut h, data) = fixture();
    ready(&mut h, &data);
    click(&mut h, "checkbox:cloudRetentionConfirmed");
    let generation = h.app.session.library_generation();
    h.app.cloud_retention.review.task = Some(1);
    open(&mut h.app.session);
    assert_ne!(h.app.session.library_generation(), generation);
    // Command guards run before another frame can offer the old Apply button.
    h.app.run("cloud.syncStatus", json!({})).unwrap();
    let runtime = &h.app.cloud_retention;
    assert_eq!(runtime.owner.as_ref().unwrap().library, "library");
    assert_eq!(runtime.owner.as_ref().unwrap().connection, "personal");
    assert!(!runtime.review.confirmed);
    assert!(runtime.review.preview.is_none() && runtime.review.task.is_none());
    frames(&mut h);
    assert!(h.app.cloud_retention.review.preview.is_none(), "old task report was applied to a new generation");
}
