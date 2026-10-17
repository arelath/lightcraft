//! Info and the popup must dispatch the same operation for the same device state.
use crate::{LightcraftApp, Services, headless::Headless};
use lightcraft_catalog::{
    MemStore, Op, Photo, PhotoId, Source,
    assets::{Asset, AssetId, Blob, BlobId, RemoteLocation},
    replication::SyncState,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
const TIMEOUT: Duration = Duration::from_secs(20);
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

fn frames(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}
fn click(h: &mut Headless, id: &str) {
    assert_eq!(h.request("ui.clickWidget", json!({"id":id}), TIMEOUT)["ok"], true, "{id}");
    frames(h);
}
fn fixture(data: Value) -> (Headless, Calls) {
    let mut session = lightcraft_engine::Session::new();
    session
        .open_library_in(
            lightcraft_engine::library::LibraryStores {
                dir: "cloud-shared-actions".into(),
                catalog: Box::new(MemStore::new()),
                files: Box::new(MemStore::new()),
                on_disk: false,
            },
            false,
        )
        .unwrap();
    let asset_id = AssetId("original".into());
    session
        .apply_received(Op::Batch {
            ops: vec![
                Op::SetAsset {
                    id: asset_id.clone(),
                    asset: Some(Box::new(Asset {
                        original: Blob {
                            id: BlobId("a".repeat(64)),
                            size: 5,
                            remote: RemoteLocation { connection: "personal".into(), key: "original".into(), version: None },
                        },
                        smart_preview: None,
                        thumbnail: None,
                        associated: Default::default(),
                    })),
                },
                Op::AddPhoto { photo: Box::new(Photo::new(PhotoId(1), Source::Managed { asset_id }, "fixture.png", "PNG", 4, 3, "")) },
                Op::SetSyncState { state: Some(Box::new(SyncState::new("library".into(), "device".into(), "personal".into()).unwrap())) },
            ],
        })
        .unwrap();
    session.selection = lightcraft_engine::Selection::single(PhotoId(1));
    let calls: Calls = Default::default();
    let recorded = calls.clone();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, params| match id {
            "cloud.overview" => Some(Ok(data.clone())),
            "cloud.syncStatus" => {
                let mut status = data.clone();
                status["enabled"] = data["syncEnabled"].clone();
                status["health"] = data["health"]["sync"].clone();
                Some(Ok(status))
            }
            "cloud.resume" | "cloud.sync" | "cloud.pause" => {
                recorded.lock().unwrap().push((id.into(), params.clone()));
                Some(Ok(json!({})))
            }
            _ => None,
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(session, services), [1400., 1200.], 1.);
    frames(&mut h);
    (h, calls)
}
fn overview() -> Value {
    json!({"syncEnabled":true,"canResume":true,"userPaused":false,"paused":false,
        "summary":{"cloudPhotos":1,"albums":0},"protocol":"conditional-head-v1",
        "health":{"sync":{"state":"healthy","lastSuccessfulRemoteScan":{"atUnixMs":1}}}})
}

#[test]
fn popup_and_info_resume_user_pause_and_retry_the_same_durable_work() {
    for paused in [true, false] {
        let mut data = overview();
        data["userPaused"] = json!(paused);
        data["paused"] = json!(paused);
        data["health"]["sync"]["state"] = json!(if paused { "userPaused" } else { "halted" });
        data["health"]["sync"]["lastError"] = json!({"kind":"connectivity","message":"Storage unavailable"});
        let (mut h, calls) = fixture(data);
        click(&mut h, "icon:cloud");
        click(&mut h, if paused { "button:cloudPauseResume" } else { "button:cloudRetry" });
        click(&mut h, "icon:cloud");
        h.app.run("panel.info", json!({})).unwrap();
        frames(&mut h);
        let expected = if paused { "Resume" } else { "Retry Syncing" };
        assert!(h.painted_text().iter().any(|text| text == expected), "{:?}", h.painted_text());
        click(&mut h, "button:cloudSyncNow");
        assert_eq!(*calls.lock().unwrap(), vec![("cloud.resume".into(), json!({})), ("cloud.resume".into(), json!({}))]);
    }
}

#[test]
fn both_surfaces_disable_resume_until_tasks_drain_and_do_not_offer_it_during_upgrade() {
    for migrating in [false, true] {
        let mut data = overview();
        data["userPaused"] = json!(true);
        data["paused"] = json!(true);
        data["canResume"] = json!(false);
        data["migrating"] = json!(migrating);
        let (mut h, calls) = fixture(data);
        click(&mut h, "icon:cloud");
        if migrating {
            assert!(!h.app.widgets.iter().any(|(id, _)| id == "button:cloudPauseResume"));
        } else {
            click(&mut h, "button:cloudPauseResume");
        }
        click(&mut h, "icon:cloud");
        h.app.run("panel.info", json!({})).unwrap();
        frames(&mut h);
        if migrating {
            assert!(!h.app.widgets.iter().any(|(id, _)| id == "button:cloudSyncNow"));
        } else {
            click(&mut h, "button:cloudSyncNow");
        }
        assert!(calls.lock().unwrap().is_empty(), "disabled controls must not dispatch");
    }
}

#[test]
fn migration_preparation_offers_explicit_sync_without_an_ordinary_resume() {
    let mut data = overview();
    data["paused"] = json!(true);
    data["preparingMigration"] = json!(true);
    let (mut h, calls) = fixture(data);
    click(&mut h, "icon:cloud");
    assert!(!h.painted_text().iter().any(|text| text == "Resume"));
    click(&mut h, "button:cloudPauseResume");
    click(&mut h, "icon:cloud");
    h.app.run("panel.info", json!({})).unwrap();
    frames(&mut h);
    assert!(!h.painted_text().iter().any(|text| text == "Resume"));
    click(&mut h, "button:cloudSyncNow");
    assert_eq!(calls.lock().unwrap().iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["cloud.resume", "cloud.resume"]);
}
