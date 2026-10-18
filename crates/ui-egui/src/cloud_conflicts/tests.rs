use crate::{LightcraftApp, Services, headless::Headless};
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
fn search(h: &mut Headless, key: &str, text: &str) {
    click(h, &format!("field:cloudPageSearch:{key}"));
    assert_eq!(h.request("ui.key", json!({"key":"A","cmd":true}), TIMEOUT)["ok"], true);
    assert_eq!(h.request("ui.text", json!({"text":text}), TIMEOUT)["ok"], true);
    frames(h);
}
fn widgets(h: &mut Headless, filter: &str) -> String {
    h.request("ui.widgets", json!({"filter":filter}), TIMEOUT)["result"].to_string()
}
fn fixture(data: Value) -> (Headless, Calls) {
    fixture_size(data, [1400., 1000.])
}
fn fixture_size(data: Value, size: [f32; 2]) -> (Headless, Calls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, params| {
            recorded.lock().unwrap().push((id.to_owned(), params.clone()));
            match id {
                "cloud.syncStatus" => Some(Ok(super::fixture_status(&data, params))),
                "cloud.resolveConflict" | "cloud.restoreDeleted" | "cloud.acceptStructure" => Some(Ok(json!({}))),
                _ => None,
            }
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), size, 1.);
    h.step();
    let state = lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
    h.app.session.apply_received(lightcraft_catalog::Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    (h, calls)
}

#[test]
fn recovery_ui_pages_to_seventeenth_conflict_and_ninth_choice_without_resolving_earlier_values() {
    let conflicts: Vec<_> = (1..=17)
        .map(|n| {
            json!({"entity":format!("photo:p{n}"),"field":"caption","name":format!("beach-{n:02}.jpg"),
            "choices":(1..=9).map(|v|json!({"revision":format!("device-{v}-1"),"preview":format!("value {v}"),"kind":"value"})).collect::<Vec<_>>()})
        })
        .collect();
    let (mut h, calls) = fixture(json!({"conflicts":conflicts}));
    h.app.run("dialog.cloudConflicts", json!({})).unwrap();
    frames(&mut h);
    assert!(!widgets(&mut h, "cloudKeepRevision:photo:p17:").contains("device-9-1"));
    let queries = || calls.lock().unwrap().iter().filter(|(id, p)| id == "cloud.syncStatus" && p["section"].is_string()).count();
    let cached = queries();
    frames(&mut h);
    assert_eq!(queries(), cached, "ordinary repaints reuse pages");
    click(&mut h, "button:cloudPageNext:conflicts");
    assert!(widgets(&mut h, "cloudKeepRevision:photo:p17:").contains("device-8-1"));
    click(&mut h, "button:cloudPageNext:choices:photo:p17:caption");
    assert!(widgets(&mut h, "cloudKeepRevision:photo:p17:").contains("device-9-1"));
    assert!(!widgets(&mut h, "cloudKeepRevision:photo:p17:").contains("device-8-1"));
    click(&mut h, "button:cloudPagePrev:choices:photo:p17:caption");
    search(&mut h, "choices:photo:p17:caption", "value 9");
    assert!(widgets(&mut h, "cloudKeepRevision:photo:p17:").contains("device-9-1"));
    search(&mut h, "conflicts", "beach-17");
    assert!(h.painted_text().iter().any(|text| text == "1–1 of 1"));
    assert!(widgets(&mut h, "cloudKeepRevision:photo:p17:").contains("device-9-1"));
    assert!(!calls.lock().unwrap().iter().any(|(id, _)| id == "cloud.resolveConflict"));
    click(&mut h, "button:cloudKeepRevision:photo:p17:caption:device-9-1");
    assert_eq!(
        calls.lock().unwrap().iter().filter(|(id, _)| id == "cloud.resolveConflict").map(|(_, p)| p.clone()).collect::<Vec<_>>(),
        vec![json!({"entity":"photo:p17","field":"caption","revision":"device-9-1"})]
    );
}

#[test]
fn recovery_ui_reaches_deleted_photo_201_and_structural_conflict_17_without_earlier_actions() {
    let deleted: Vec<_> = (1..=201).map(|n| json!({"entity":format!("photo:d{n}"),"name":format!("deleted-{n:03}.jpg")})).collect();
    let structural: Vec<_> = (1..=17)
        .map(|n| json!({"entity":format!("album:a{n}"),"field":"parent","name":format!("Folder {n}"),"projected":null,"proposed":"other"}))
        .collect();
    let (mut h, calls) = fixture(json!({"deletedPhotos":deleted,"deletedPhotoCount":201,"structuralConflicts":structural}));
    h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
    frames(&mut h);
    for _ in 0..4 {
        click(&mut h, "button:cloudPageNext:deleted");
    }
    assert!(widgets(&mut h, "cloudRecover:").contains("photo:d201"));
    click(&mut h, "button:cloudPagePrev:deleted");
    search(&mut h, "deleted", "deleted-201");
    assert!(widgets(&mut h, "cloudRecover:").contains("photo:d201"));
    assert!(h.painted_text().iter().any(|text| text == "1–1 of 1"));
    h.app.run("dialog.cloudConflicts", json!({})).unwrap();
    frames(&mut h);
    click(&mut h, "button:cloudPageNext:structural");
    assert!(widgets(&mut h, "cloudAcceptStructure:").contains("album:a17"));
    search(&mut h, "structural", "Folder 17");
    assert!(widgets(&mut h, "cloudAcceptStructure:").contains("album:a17"));
    assert!(!calls.lock().unwrap().iter().any(|(id, _)| matches!(id.as_str(), "cloud.restoreDeleted" | "cloud.acceptStructure")));
}

#[test]
fn recovery_pages_reset_after_revision_or_library_changes() {
    use lightcraft_engine::{
        catalog::{MemStore, Op, PhotoId},
        library::LibraryStores,
    };
    let deleted: Vec<_> = (1..=201).map(|n| json!({"entity":format!("photo:d{n}"),"name":format!("deleted-{n:03}.jpg")})).collect();
    let (mut h, calls) = fixture(json!({"deletedPhotos":deleted,"deletedPhotoCount":201}));
    h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
    frames(&mut h);
    click(&mut h, "button:cloudPageNext:deleted");
    h.app.session.commit("revision change", Op::SetRating { id: PhotoId(1), rating: 3 }).unwrap();
    frames(&mut h);
    let last = calls.lock().unwrap().iter().rev().find(|(_, p)| p["section"] == "deleted").unwrap().1.clone();
    assert!(last["cursor"].is_null(), "a changed revision restarts the list");
    search(&mut h, "deleted", "deleted-201");
    h.app
        .session
        .open_library_in(
            LibraryStores { dir: "second".into(), catalog: Box::new(MemStore::new()), files: Box::new(MemStore::new()), on_disk: false },
            false,
        )
        .unwrap();
    frames(&mut h);
    assert!(h.app.ui.dialog.is_none(), "switching libraries dismisses the old cloud review");
    h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
    frames(&mut h);
    let last = calls.lock().unwrap().iter().rev().find(|(_, p)| p["section"] == "deleted").unwrap().1.clone();
    assert_eq!(last["search"], "", "queries belong to the opened library");
    assert!(last["cursor"].is_null());
}

#[test]
fn selected_managed_photo_info_reaches_ninth_retained_choice() {
    use lightcraft_catalog::{
        Op, Source,
        assets::{Asset, AssetId, Blob, BlobId, RemoteLocation},
    };
    let choices: Vec<_> = (1..=9).map(|n| json!({"revision":format!("device-{n}-1"),"preview":format!("Caption {n}"),"kind":"value"})).collect();
    let (mut h, calls) =
        fixture_size(json!({"enabled":true,"conflicts":[{"entity":"photo:selected","field":"caption","choices":choices}]}), [1400., 1800.]);
    let id = h.app.session.active().unwrap();
    let asset_id = AssetId("info-original".into());
    h.app
        .session
        .catalog
        .apply(Op::SetAsset {
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
        })
        .unwrap();
    h.app
        .session
        .catalog
        .apply(Op::Relink { id, file_name: "info.png".into(), source: Source::Managed { asset_id }, format: Some("PNG".into()) })
        .unwrap();
    h.app.session.catalog.apply(Op::SetCloudIdentity { entity: format!("photo:{}", id.0), id: Some("selected".into()) }).unwrap();
    h.app.run("panel.info", json!({})).unwrap();
    frames(&mut h);
    click(&mut h, "button:cloudPageNext:info:choices:photo:selected:caption");
    assert!(widgets(&mut h, "cloudKeepRevision:photo:selected:").contains("device-9-1"));
    search(&mut h, "info:choices:photo:selected:caption", "Caption 9");
    assert!(widgets(&mut h, "cloudKeepRevision:photo:selected:").contains("device-9-1"));
    h.app.run("dialog.cloudConflicts", json!({})).unwrap();
    frames(&mut h);
    search(&mut h, "choices:photo:selected:caption", "Caption 1");
    assert!(widgets(&mut h, "cloudKeepRevision:photo:selected:").contains("device-1-1"));
    let ids = widgets(&mut h, "cloudKeepRevision:photo:selected:");
    assert!(ids.contains("button:info:cloudKeepRevision:photo:selected:caption:device-9-1"));
    assert!(ids.contains("button:cloudKeepRevision:photo:selected:caption:device-1-1"));
    h.request("ui.dialog.cancel", json!({}), TIMEOUT);
    frames(&mut h);
    assert!(widgets(&mut h, "cloudKeepRevision:photo:selected:").contains("device-9-1"), "dialog search must not change Info's search/page state");
    assert!(!calls.lock().unwrap().iter().any(|(id, _)| id == "cloud.resolveConflict"));
}
