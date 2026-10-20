//! Modal application dialogs block background input while retaining their own controls.

use crate::headless::Headless;
use crate::{LightcraftApp, Services};
use serde_json::{Value, json};
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

#[test]
fn cloud_retry_headline_displays_bounded_countdown_and_manual_recovery_actions() {
    for (state, exhausted, expected) in [("retrying", false, "Retrying in 4 s"), ("halted", true, "Automatic retry limit reached.")] {
        let services = Services {
            cloud_action: Some(Box::new(move |_, id, _| {
                (id == "cloud.overview").then(|| {
                    Ok(json!({
                        "summary":{"cloudPhotos":1,"albums":0,"pendingChanges":1},"paused":false,"syncEnabled":true,"canResume":true,
                        "health":{"sync":{"state":state,"pollingHalted":state=="halted",
                            "retry":{"attempt":2,"maxAttempts":5,"secondsUntilRetry":4,"exhausted":exhausted},
                            "lastError":{"kind":"connectivity","message":"Storage unavailable","action":"Check connectivity, then Retry Syncing."}}}
                    }))
                })
            })),
            ..Default::default()
        };
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
        for _ in 0..4 {
            h.step();
        }
        assert_eq!(h.request("ui.clickWidget", json!({"id":"icon:cloud"}), T)["ok"], true);
        for _ in 0..4 {
            h.step();
        }
        let text = h.view.painted_text();
        for label in [expected, "Details", "Retry"] {
            assert!(text.iter().any(|value| value.contains(label)), "missing {label}: {text:?}");
        }
        assert!(!text.iter().any(|value| value == "Up to date"));
    }
}

#[test]
fn cloud_health_headline_covers_halted_polling_and_structural_only_conflicts() {
    for structural in [false, true] {
        let overview = std::sync::Arc::new(std::sync::Mutex::new(json!({
            "summary":{"cloudPhotos":0,"albums":2,"pendingChanges":0,"issues":0,"failedFiles":0},
            "paused":false,"syncEnabled":true,"canResume":true,"tasks":[],
            "health":{"sync":{"state":if structural {"healthy"}else{"halted"},"pollingHalted":!structural,
                "lastSuccessfulRemoteScan":{"atUnixMs":1000},"secondsSinceLastRemoteScan":20,
                "lastError":if structural {Value::Null}else{json!({"message":"Remote access failed; polling stopped"})}},
                "conflicts":{"structural":u64::from(structural)}},
            "structuralConflicts":if structural {json!([{"entity":"album:one","field":"parent"}])}else{json!([])},
            "error":if structural {Value::Null}else{json!("Remote access failed; polling stopped")}
        })));
        let data = overview.clone();
        let services = Services {
            cloud_action: Some(Box::new(move |_, id, _| (id == "cloud.overview").then(|| Ok(data.lock().unwrap().clone())))),
            ..Default::default()
        };
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
        for _ in 0..4 {
            h.step();
        }
        assert_eq!(h.request("ui.clickWidget", json!({"id":"icon:cloud"}), T)["ok"], true);
        for _ in 0..4 {
            h.step();
        }
        let text = h.view.painted_text();
        assert!(text.iter().any(|value| value.contains("Sync needs attention")), "{text:?}");
        assert!(!text.iter().any(|value| value == "Up to date"));
        let cause = if structural { "Album or stack choices need attention" } else { "Open Details for the diagnostic." };
        assert!(text.iter().any(|value| value.contains(cause)), "{text:?}");
        assert!(!text.iter().any(|value| value.contains("Last checked")), "{text:?}");
        let widgets = h.request("ui.widgets", json!({"filter":"indicator:cloudQueue"}), T);
        assert!(!widgets["result"].as_array().unwrap().is_empty());
        if !structural {
            assert_eq!(h.request("ui.clickWidget", json!({"id":"button:cloudErrorDetails"}), T)["ok"], true);
            for _ in 0..4 {
                h.step();
            }
            assert!(h.painted_text().iter().any(|value| value.contains("Remote access failed; polling stopped")));
            assert_eq!(h.request("ui.dialog.cancel", json!({}), T)["ok"], true);
            for _ in 0..4 {
                h.step();
            }
            assert_eq!(h.request("ui.clickWidget", json!({"id":"icon:cloud"}), T)["ok"], true);
            for _ in 0..4 {
                h.step();
            }
        }
        {
            let mut value = overview.lock().unwrap();
            value["health"]["sync"]["state"] = json!("healthy");
            value["health"]["sync"]["pollingHalted"] = json!(false);
            value["health"]["sync"]["lastError"] = Value::Null;
            value["health"]["conflicts"]["structural"] = json!(0);
            value["structuralConflicts"] = json!([]);
            value["error"] = Value::Null;
        }
        for _ in 0..4 {
            h.step();
        }
        assert!(h.view.painted_text().iter().any(|value| value == "Up to date"));
    }
}

#[test]
fn cloud_popup_exposes_preflight_reason_and_dispatches_exact_import_retry() {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, params| match id {
            "cloud.overview" => Some(Ok(json!({"summary":{"cloudPhotos":1,"albums":0,"blockedImportCount":1,
                "blockedImports":[{"id":"blocked-import","name":"camera.dng","reason":"invalid revision manifest; restore it and retry","verify":true}]},
                "syncEnabled":true,"canResume":true,"paused":false,"tasks":[]}))),
            "cloud.upload" => {
                recorded.lock().unwrap().push(params.clone());
                Some(Ok(json!({"tasks":[]})))
            }
            _ => None,
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.request("ui.clickWidget", json!({"id":"icon:cloud"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    let text = h.view.painted_text();
    for label in ["Sync needs attention", "camera.dng", "invalid revision manifest", "Retry upload"] {
        assert!(text.iter().any(|value| value.contains(label)), "missing {label}: {text:?}");
    }
    assert!(!text.iter().any(|value| value == "Up to date"));
    assert_eq!(h.request("ui.clickWidget", json!({"id":"button:cloudRetryImport:blocked-import"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(*calls.lock().unwrap(), vec![json!({"imports":["blocked-import"],"verify":true,"wait":false})]);
}

#[test]
fn cloud_capacity_warning_explains_recovery_and_dispatches_checkpoint_maintenance() {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, params| match id {
            "cloud.connections" => Some(Ok(json!({"connections":[]}))),
            "cloud.syncStatus" => Some(Ok(json!({
                "utilization":{"stateBytes":53_687_091,"stateLimitBytes":67_108_864,"statePercent":80.,
                    "documents":21,"documentLimit":100_000,"received":80_000,"receiptLimit":100_000,
                    "pending":2,"pendingLimit":10_000,"authors":2,"authorLimit":4096,"historicalReceipts":79_979,"archiveRoots":4},
                "capacityWarnings":[{"message":"Full synchronization receipts are nearing their limit.",
                    "recovery":"Run checkpoint maintenance to archive historical receipts; remote objects are retained."}]
            }))),
            "cloud.sync" => {
                recorded.lock().unwrap().push(params.clone());
                Some(Ok(json!({"task":1})))
            }
            _ => None,
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
    h.step();
    let state = lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
    h.app.session.apply_received(lightcraft_catalog::Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.request("ui.clickWidget", json!({"id":"button:cloudCapacity"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    let text = h.view.painted_text();
    for label in
        ["Synchronization capacity", "80.0%", "80000 / 100000", "nearing their limit", "remote objects are retained", "Archive receipt history"]
    {
        assert!(text.iter().any(|value| value.contains(label)), "missing {label}: {text:?}");
    }
    assert_eq!(h.request("ui.clickWidget", json!({"id":"button:cloudCheckpoint"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(*calls.lock().unwrap(), vec![json!({"wait":false,"checkpoint":true})]);
}

#[test]
fn cloud_setup_explains_retained_structural_conflicts() {
    let services = Services {
        cloud_action: Some(Box::new(|_, id, params| {
            if id == "cloud.connections" {
                return Some(Ok(json!({"connections": []})));
            }
            (id=="cloud.syncStatus").then(||Ok(crate::cloud_conflicts::fixture_status(&json!({
            "structuralConflicts":[{"entity":"album:A","field":"parent","name":"Folder A","proposed":"B","projected":null},{"entity":"stack:one","field":"photos","proposed":["a","b"],"projected":[]}]
        }), params)))
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
    h.step(); // Finish initial library/UI setup before opening a dialog.
    // conflicts come from synchronization: an unsynchronized library opens cloud setup instead
    let state = lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
    h.app.session.apply_received(lightcraft_catalog::Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    h.app.run("dialog.cloudConflicts", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let text = h.view.painted_text();
    for label in ["Conflicting folder or stack moves", "Folder A", "Requested parent", "Currently shown under", "Stack membership"] {
        assert!(text.iter().any(|value| value.contains(label)), "missing {label}: {text:?}");
    }
    assert!(text.iter().any(|value| value.contains("original choices remain retained")));
    assert!(text.iter().any(|value| value.contains("Keep current structure")));
}

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(120));
    h
}

/// Application dialogs block the toolbar, including the gear that would reopen Copy Settings
/// and discard its choices. Their own controls work, and closing restores background input.
#[test]
fn modal_dialogs_block_background_clicks_and_preserve_choices() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    let rating = h.app.session.catalog.photo(active).unwrap().rating;
    h.app.run("dialog.copySettings", json!({})).unwrap();
    // Also exercise the first frame of a newly opened dialog.
    let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating, "the dialog blocks background ratings");
    let r = h.request("ui.clickWidget", json!({"id": "copyGroup:none"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let choices = h.app.ui.dialog.clone();
    assert!(matches!(&choices, Some(crate::state::Dialog::CopySettings { groups }) if groups.is_empty()));
    let r = h.request("ui.clickWidget", json!({"id": "icon:copyGear"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.dialog, choices, "a background click cannot reopen the dialog and reset its choices");
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores toolbar input");
    // A different dialog uses the same barrier.
    h.app.run("app.settings", json!({})).unwrap();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "star:bottom:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5);
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
}

#[test]
fn modal_dialogs_block_background_shortcuts_and_keep_escape() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let active = h.app.session.active().unwrap();
    let rating = h.app.session.catalog.photo(active).unwrap().rating;
    h.app.run("dialog.copySettings", json!({})).unwrap();
    h.step();
    for key in ["5", "ArrowRight", "J"] {
        let r = h.request("ui.key", json!({"key": key}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating);
    assert_eq!(h.app.session.active(), Some(active));
    assert!(!h.app.ui.show_clipping);
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    let r = h.request("ui.key", json!({"key": "5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores shortcuts");
}

#[test]
fn modal_dialogs_close_background_menus() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    let r = h.request("ui.clickWidget", json!({"id": "menu:Photo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(egui::Popup::is_any_open(&h.view.ctx));
    h.app.run("dialog.copySettings", json!({})).unwrap();
    h.step();
    assert!(!egui::Popup::is_any_open(&h.view.ctx), "background menus cannot remain above the modal window");
}

#[test]
fn cloud_migration_dialog_relies_on_library_maintenance_and_cancel_keeps_pause() {
    use std::{cell::RefCell, rc::Rc};
    let calls = Rc::new(RefCell::new(Vec::new()));
    let output = calls.clone();
    let paused = Rc::new(RefCell::new(false));
    let control = paused.clone();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, p| match id {
            "cloud.prepareMigration" => {
                *control.borrow_mut() = true;
                Some(Ok(json!({"paused":true})))
            }
            "cloud.syncStatus" => Some(Ok(json!({"running":false,"preparingMigration":*control.borrow()}))),
            "cloud.upgradeLibrary" => {
                output.borrow_mut().push(p.clone());
                Some(Ok(json!({"task":1,"background":true})))
            }
            "cloud.sync" => {
                *control.borrow_mut() = false;
                Some(Ok(json!({"task":2,"background":true})))
            }
            _ => None,
        })),
        ..Default::default()
    };
    let mut session = lightcraft_engine::Session::with_demo();
    session
        .catalog
        .apply(lightcraft_engine::catalog::Op::SetSyncState {
            state: Some(Box::new(
                lightcraft_engine::catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap(),
            )),
        })
        .unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, services), [1400., 1000.], 1.);
    h.settle(Duration::from_secs(120));
    h.app.run("dialog.cloudMigrate", json!({})).unwrap();
    h.step();
    h.step();
    assert!(*paused.borrow());
    let r = h.request("ui.clickWidget", json!({"id":"button:dialogCancel"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert!(*paused.borrow());
    h.app.run("cloud.sync", json!({"wait":false})).unwrap();
    assert!(!*paused.borrow());
    assert!(calls.borrow().is_empty(), "cancel never migrates");
    // The head's maintenance state is the barrier: no peer confirmation is required.
    h.app.run("dialog.cloudMigrate", json!({})).unwrap();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id":"button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.dialog.is_none());
    // The optional confirmation covers storage that cannot hold the barrier.
    h.app.run("dialog.cloudMigrate", json!({})).unwrap();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id":"checkbox:cloudMigrationStopped"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let r = h.request("ui.clickWidget", json!({"id":"button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(calls.borrow().as_slice(), &[json!({"writersStopped":false,"wait":false}), json!({"writersStopped":true,"wait":false})]);
}

#[test]
fn browser_upgrade_menu_uses_cached_provider_capability() {
    use lightcraft_engine::catalog::replication::{SyncProtocol, SyncState};
    let mut session = lightcraft_engine::Session::with_demo();
    let mut state = SyncState::new("library".into(), "browser".into(), "personal".into()).unwrap();
    state.protocol = SyncProtocol::AppendLog;
    session.catalog.apply(lightcraft_engine::catalog::Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    let mut app = LightcraftApp::new(session, Services { cloud_can_upgrade: Some(Box::new(|_| true)), ..Default::default() });
    assert!(crate::menus::ui_enabled(&app, "dialog.cloudMigrate"));
    app.services.cloud_can_upgrade = Some(Box::new(|_| false));
    assert!(!crate::menus::ui_enabled(&app, "dialog.cloudMigrate"));
    app.services.cloud_can_upgrade = None;
    assert!(!crate::menus::ui_enabled(&app, "dialog.cloudMigrate"), "native fallback does not invent a B2 connection");
}

#[test]
fn cloud_popup_says_publishing_is_blocked_and_counts_field_conflicts() {
    let services = Services {
        cloud_action: Some(Box::new(|_, id, _| {
            (id == "cloud.overview").then(|| {
                Ok(json!({"summary":{"cloudPhotos":3,"albums":1,"pendingChanges":2},"paused":false,"syncEnabled":true,"canResume":true,"tasks":[],
                    "health":{"sync":{"state":"healthy","lastSuccessfulRemoteScan":{"atUnixMs":1000},"secondsSinceLastRemoteScan":7200},
                        "conflicts":{"fields":1}},
                    "conflicts":[{"entity":"photo:a","field":"rating"}],
                    "publicationBlocked":"another device is upgrading this cloud library; receiving continues and local changes wait"}))
            })
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.request("ui.clickWidget", json!({"id":"icon:cloud"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    let text = h.view.painted_text();
    for label in ["Publishing paused", "Open Details for the diagnostic.", "Your 2 changes wait here.", "1 conflicting edit", "3 photos in 1 album"] {
        assert!(text.iter().any(|value| value.contains(label)), "missing {label}: {text:?}");
    }
    assert!(!text.iter().any(|value| value == "Up to date"));
    assert!(!text.iter().any(|value| value.contains("another device is upgrading this cloud library")));
    assert_eq!(h.request("ui.clickWidget", json!({"id":"button:cloudPublicationDetails"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    assert!(h.painted_text().iter().any(|value| value.contains("another device is upgrading this cloud library")));
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "button:cloudErrorConnection"));
}

#[test]
fn cloud_sync_dialog_keeps_a_chosen_conflict_value_and_bounds_the_recover_list() {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let deleted: Vec<Value> = (0..250).map(|n| json!({"entity":format!("photo:d{n}"),"name":format!("gone-{n}.jpg")})).collect();
    let services = Services {
        cloud_action: Some(Box::new(move |_, id, params| match id {
            "cloud.connections" => Some(Ok(json!({"connections":[]}))),
            "cloud.syncStatus" => Some(Ok(crate::cloud_conflicts::fixture_status(
                &json!({
                    "conflicts":[{"entity":"photo:p1","field":"rating","name":"beach.jpg","choices":[
                        {"revision":"dev-a-3","current":true,"thisDevice":true,"kind":"value","preview":"2"},
                        {"revision":"dev-b-4","current":false,"thisDevice":false,"kind":"value","preview":"5"}]}],
                    "deletedPhotos":deleted,"deletedPhotoCount":250
                }),
                params,
            ))),
            "cloud.resolveConflict" => {
                recorded.lock().unwrap().push(params.clone());
                Some(Ok(json!({})))
            }
            _ => None,
        })),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1400., 1000.], 1.);
    h.step();
    let state = lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
    h.app.session.apply_received(lightcraft_catalog::Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    h.app.run("dialog.cloudConflicts", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let text = h.view.painted_text();
    for label in ["Conflicting edits", "beach.jpg · Rating", "★★ · This device · shown now", "★★★★★ · Another device"] {
        assert!(text.iter().any(|value| value.contains(label)), "missing {label}: {text:?}");
    }
    assert_eq!(h.request("ui.clickWidget", json!({"id":"button:cloudKeepRevision:photo:p1:rating:dev-b-4"}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(*calls.lock().unwrap(), vec![json!({"entity":"photo:p1","field":"rating","revision":"dev-b-4"})]);
    h.app.run("dialog.cloudMaintenance", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let text = h.view.painted_text();
    assert!(text.iter().any(|value| value.contains("Deleted cloud photos: 250")));
    let painted = text.iter().filter(|value| value.starts_with("gone-")).count();
    assert!(painted > 0 && painted < 200, "the Recover list scrolls: {painted}");
}
