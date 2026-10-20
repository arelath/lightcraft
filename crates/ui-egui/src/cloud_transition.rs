//! Turn cloud synchronization off only after durable local originals are verified.
//! The engine owns the persisted transition; this module only presents and polls it.

use crate::{LightcraftApp, state::Dialog, theme::Tokens};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Default)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct Runtime {
    pub status: Option<Value>,
    next_poll: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub preview: Option<Value>,
    pub status: Option<Value>,
    pub error: Option<String>,
    pub cancelling: bool,
    pub dismissed: bool,
}

impl Review {
    pub fn blocking(&self) -> bool {
        self.status.as_ref().is_some_and(blocking)
    }
}

fn blocking(status: &Value) -> bool {
    status["blocking"] == true && status["stage"] != "complete"
}

pub(crate) fn blocked(app: &LightcraftApp) -> bool {
    app.cloud_transition.status.as_ref().is_some_and(blocking) || matches!(&app.ui.dialog, Some(Dialog::CloudDisable { review }) if review.blocking())
}

pub(crate) fn local_only(app: &LightcraftApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.session.cloud_transition.local_only()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        false
    }
}

/// Native menu accelerators and control clients use the same guard as pointer input.
pub(crate) fn permitted(command: &str) -> bool {
    matches!(
        command,
        "app.quit"
            | "cloud.syncDisable"
            | "cloud.transitionStatus"
            | "cloud.overview"
            | "cloud.syncStatus"
            | "cloud.uploadStatus"
            | "cloud.connections"
    )
}

pub(crate) fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("Use desktop LightCraft to turn cloud sync off and keep permanent local originals. Pause is available in the browser.".into());
    }
    let review = match app.run("cloud.syncDisablePreview", json!({})) {
        Ok(preview) => Review { preview: Some(preview), ..Default::default() },
        Err(error) => Review { error: Some(error), ..Default::default() },
    };
    app.ui.dialog = Some(Dialog::CloudDisable { review });
    Ok(Value::Null)
}

fn transition(value: Value) -> Option<Value> {
    let value = value.get("transition").cloned().unwrap_or(value);
    value.is_object().then_some(value)
}

/// Commands issued by a menu or control client must block immediately, before the next frame.
pub(crate) fn observe(app: &mut LightcraftApp, command: &str, params: &Value, result: &Result<Value, String>) {
    if command != "cloud.syncDisable" || params["confirm"] != true {
        return;
    }
    if let Ok(value) = result {
        let status =
            transition(value.clone()).filter(|value| value["stage"].is_string()).unwrap_or_else(|| json!({"stage":"draining","blocking":true}));
        app.cloud_transition.status = Some(status);
        app.cloud_transition.next_poll = 0.;
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.session.catalog.sync_state().is_none() && !matches!(app.ui.dialog, Some(Dialog::CloudDisable { .. })) {
        app.cloud_transition = Runtime::default();
        return;
    }
    let now = ctx.input(|i| i.time);
    if blocked(app) {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
    if now < app.cloud_transition.next_poll {
        return;
    }
    app.cloud_transition.next_poll = now + 0.5;
    match app.run("cloud.transitionStatus", json!({})) {
        Ok(value) => {
            let status = transition(value);
            app.cloud_transition.status = status.clone();
            if let Some(Dialog::CloudDisable { review }) = &mut app.ui.dialog {
                if status.as_ref().is_some_and(|s| s["stage"] == "failed") {
                    review.cancelling = false;
                }
                if review.status.as_ref().and_then(|s| s["stage"].as_str()) != status.as_ref().and_then(|s| s["stage"].as_str()) {
                    review.error = None;
                }
                if review.status.is_some() && status.is_none() {
                    review.dismissed = true;
                }
                review.status = status;
            } else if status.as_ref().is_some_and(blocking) {
                app.ui.dialog = Some(Dialog::CloudDisable { review: Review { status, ..Default::default() } });
            }
        }
        Err(error) if blocked(app) => {
            if let Some(Dialog::CloudDisable { review }) = &mut app.ui.dialog {
                review.error = Some(error);
            }
        }
        Err(_) => {}
    }
}

fn button(ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    let response = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(label)));
    crate::widgets::register(ui.ctx(), id, response.rect);
    response.clicked()
}

fn begin(app: &mut LightcraftApp, review: &mut Review) {
    match app.run("cloud.syncDisable", json!({"confirm":true,"wait":false})) {
        Ok(value) => {
            // A task acknowledgement can precede the first status poll: block immediately.
            let status = transition(value).filter(|v| v["stage"].is_string()).unwrap_or_else(|| {
                let mut status = review.preview.clone().filter(Value::is_object).unwrap_or_else(|| json!({}));
                status["stage"] = json!("draining");
                status["blocking"] = json!(true);
                status
            });
            app.cloud_transition.status = Some(status.clone());
            app.cloud_transition.next_poll = 0.;
            review.status = Some(status);
            review.error = None;
            review.cancelling = false;
        }
        Err(error) => review.error = Some(error),
    }
}

pub(crate) fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, review: &mut Review) {
    let t = Tokens::get(ui.ctx());
    if let Some(error) = &review.error {
        ui.colored_label(t.reject, error);
    }
    if let Some(status) = review.status.clone() {
        let complete = status["stage"] == "complete" && status["localOnly"] == true;
        let failed = status["stage"] == "failed";
        let label = if review.cancelling {
            "Finishing cancellation safely."
        } else {
            match status["stage"].as_str() {
                Some("draining") => "Finishing current cloud work.",
                Some("reading") => "Reading the latest cloud library.",
                Some("downloading") => "Saving originals and sidecars on this device.",
                Some("checking") => "Verifying local files and checking for new cloud photos.",
                Some("failed") => "Cloud sync is still on. Local preparation needs attention.",
                Some("complete") if complete => "Cloud sync is off. Your library is ready on this device.",
                _ => "Preparing this library for local use.",
            }
        };
        ui.strong(crate::i18n::tr(label));
        progress(ui, &status);
        if let Some(error) = status["error"].as_str() {
            ui.colored_label(t.reject, error);
        }
        if !complete {
            ui.label(crate::i18n::tr("Editing and navigation wait until this finishes or you cancel. Other devices can keep editing; new remote files are checked again before completion."));
            ui.label(crate::i18n::tr("You can quit safely. Reopen this library and choose Retry to resume; verified local files are reused."));
        }
        ui.horizontal(|ui| {
            if complete {
                review.dismissed |= button(ui, "button:cloudDisableDone", "Done", true);
            } else {
                if failed && button(ui, "button:cloudDisableRetry", "Retry", !review.cancelling) {
                    begin(app, review);
                }
                if button(ui, "button:cloudDisableCancel", "Cancel", !review.cancelling) {
                    match app.run("cloud.syncDisable", json!({"cancel":true,"wait":false})) {
                        Ok(_) => {
                            review.cancelling = true;
                            review.error = None;
                            app.cloud_transition.next_poll = 0.;
                        }
                        Err(error) => review.error = Some(error),
                    }
                }
                if button(ui, "button:cloudDisableQuit", "Quit LightCraft", true)
                    && let Err(error) = app.run("app.quit", json!({}))
                {
                    review.error = Some(error);
                }
            }
        });
        if !complete {
            ui.label(crate::i18n::tr("Cancel keeps cloud sync on and retains any files already saved locally."));
        }
    } else {
        ui.strong(crate::i18n::tr("Turn off cloud sync on this device?"));
        ui.label(crate::i18n::tr("LightCraft will finish current cloud work, read the latest library, and save and verify permanent local copies of live and Recently Deleted library originals and sidecars before turning sync off."));
        ui.label(crate::i18n::tr("This does not delete anything from cloud storage. Recently Deleted cloud photos and retained conflict originals are included. Other devices can keep editing; new remote files are checked again before completion."));
        if let Some(preview) = &review.preview {
            destination(ui, preview);
            ui.horizontal_wrapped(|ui| {
                ui.label(crate::i18n::tr("Recently Deleted photos:"));
                ui.label(number(preview, "recentlyDeletedPhotos", "estimatedRecentlyDeletedPhotos").to_string());
                ui.label("·");
                ui.label(crate::i18n::tr("Additional space:"));
                ui.label(crate::panels::cloud::bytes(number(preview, "recentlyDeletedBytes", "estimatedRecentlyDeletedBytes")));
            });
            ui.label(crate::i18n::tr_format!(
                "Estimated: {files} files · {bytes}",
                files = number(preview, "totalFiles", "estimatedFiles"),
                bytes = crate::panels::cloud::bytes(number(preview, "totalBytes", "estimatedBytes"))
            ));
        }
        ui.label(crate::i18n::tr(
            "Editing and navigation will wait while local copies are prepared. You can cancel safely or quit and resume after reopening.",
        ));
        ui.horizontal(|ui| {
            review.dismissed |= button(ui, "button:cloudDisableCancel", "Cancel", true);
            if review.preview.is_none() && button(ui, "button:cloudDisablePreviewRetry", "Check again", true) {
                match app.run("cloud.syncDisablePreview", json!({})) {
                    Ok(preview) => {
                        review.preview = Some(preview);
                        review.error = None;
                    }
                    Err(error) => review.error = Some(error),
                }
            }
            if button(ui, "button:cloudDisableConfirm", "Save locally and turn off sync", review.preview.is_some()) {
                begin(app, review);
            }
        });
    }
}

fn number(value: &Value, key: &str, alias: &str) -> u64 {
    value[key].as_u64().or_else(|| value[alias].as_u64()).unwrap_or(0)
}

fn destination(ui: &mut egui::Ui, value: &Value) {
    ui.label(crate::i18n::tr("Permanent local destination"));
    ui.add(egui::Label::new(egui::RichText::new(value["destination"].as_str().unwrap_or("—")).monospace()).wrap());
}

fn progress(ui: &mut egui::Ui, status: &Value) {
    destination(ui, status);
    if let (Some(done), Some(total)) = (status["currentDownloadBytes"].as_u64(), status["currentDownloadTotal"].as_u64()) {
        ui.label(crate::i18n::tr_format!(
            "Downloading current file: {done} of {total}",
            done = crate::panels::cloud::bytes(done),
            total = crate::panels::cloud::bytes(total)
        ));
    }
    let files = number(status, "totalFiles", "estimatedFiles");
    let done_files = number(status, "doneFiles", "doneFiles").min(files);
    let total = number(status, "totalBytes", "estimatedBytes");
    let done = number(status, "doneBytes", "doneBytes").min(total);
    ui.label(crate::i18n::tr_format!(
        "Files: {done} of {total} · {remaining} remaining",
        done = done_files,
        total = files,
        remaining = files.saturating_sub(done_files)
    ));
    ui.label(crate::i18n::tr_format!(
        "Data: {done} of {total} · {remaining} remaining",
        done = crate::panels::cloud::bytes(done),
        total = crate::panels::cloud::bytes(total),
        remaining = crate::panels::cloud::bytes(total.saturating_sub(done))
    ));
    if total > 0 {
        ui.add(egui::ProgressBar::new((done as f64 / total as f64).clamp(0., 1.) as f32).show_percentage());
    } else if status["stage"] != "failed" && status["stage"] != "complete" {
        ui.spinner();
    }
    if let Some(file) = status["currentFile"].as_str().filter(|file| !file.is_empty()) {
        ui.add(egui::Label::new(file).wrap());
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::{Services, headless::Headless};
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    const WAIT: Duration = Duration::from_secs(20);
    type Shared = Arc<Mutex<Value>>;
    type Calls = Arc<Mutex<Vec<(String, Value)>>>;

    fn status(stage: &str) -> Value {
        json!({"stage":stage,"destination":"C:/Photos/My Library/originals", "totalFiles":8,"doneFiles":if stage=="complete"{8}else{3},
            "totalBytes":80_000_000,"doneBytes":if stage=="complete"{80_000_000}else{30_000_000},"currentFile":"2026/beach.raw",
            "blocking":stage!="complete","localOnly":stage=="complete",
            "error":if stage=="failed" {json!("Preparation was interrupted. Retry to resume.")}else{Value::Null}})
    }

    fn fixture(initial: Value, size: [f32; 2]) -> (Headless, Shared, Calls) {
        let state = Arc::new(Mutex::new(initial));
        let calls: Calls = Default::default();
        let (read, recorded) = (state.clone(), calls.clone());
        let services = Services {
            cloud_action: Some(Box::new(move |_, command, params| match command {
                "cloud.transitionStatus" => Some(Ok(read.lock().unwrap().clone())),
                "cloud.syncDisablePreview" => Some(Ok(json!({"destination":"C:/Photos/My Library/originals","totalFiles":8,"totalBytes":80_000_000,
                        "recentlyDeletedPhotos":3,"recentlyDeletedBytes":12_000_000}))),
                "cloud.syncDisable" => {
                    recorded.lock().unwrap().push((command.into(), params.clone()));
                    if let Some(error) = read.lock().unwrap()["actionError"].as_str() {
                        return Some(Err(error.to_owned()));
                    }
                    let value = if params["cancel"] == true { Value::Null } else { status("downloading") };
                    *read.lock().unwrap() = value.clone();
                    Some(Ok(value))
                }
                "cloud.connections" => Some(Ok(json!({"connections":[{"id":"another"},{"id":"personal"}]}))),
                "cloud.overview" => {
                    let local_only = read.lock().unwrap()["localOnly"] == true;
                    Some(Ok(json!({"syncEnabled":!local_only,"localOnly":local_only,"summary":{}})))
                }
                "cloud.syncStatus" => Some(Ok(json!({"enabled":read.lock().unwrap()["localOnly"] != true,"protocol":"conditional-head-v1"}))),
                "cloud.syncEnable" | "cloud.sync" => {
                    recorded.lock().unwrap().push((command.into(), params.clone()));
                    Some(Ok(json!({})))
                }
                _ => None,
            })),
            write: Some(Box::new(|path, bytes| std::fs::write(path, bytes).map_err(|error| error.to_string()))),
            png: Some(Box::new(|image| lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(image), &Default::default()).unwrap())),
            ..Default::default()
        };
        let mut session = lightcraft_engine::Session::with_demo();
        session
            .catalog
            .apply(lightcraft_catalog::Op::SetSyncState {
                state: Some(Box::new(lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap())),
            })
            .unwrap();
        let mut headless = Headless::new(LightcraftApp::new(session, services), size, 1.);
        frames(&mut headless);
        (headless, state, calls)
    }

    fn frames(headless: &mut Headless) {
        for _ in 0..4 {
            headless.step();
        }
    }

    fn click(headless: &mut Headless, widget: &str) {
        let response = headless.request("ui.clickWidget", json!({"id":widget}), WAIT);
        assert_eq!(response["ok"], true, "{widget}: {response}");
        frames(headless);
    }

    fn capture(headless: &mut Headless, name: &str) {
        if let Ok(dir) = std::env::var("LIGHTCRAFT_TRANSITION_SNAPSHOTS") {
            std::fs::create_dir_all(&dir).unwrap();
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            assert_eq!(headless.request("ui.screenshot", json!({"path":path.to_string_lossy()}), WAIT)["ok"], true);
        }
    }

    #[test]
    fn cloud_disable_requires_confirmation_then_blocks_navigation_and_escape_until_safe_cancel() {
        let (mut headless, _, calls) = fixture(Value::Null, [1200., 900.]);
        headless.app.run("dialog.cloudDisable", json!({})).unwrap();
        frames(&mut headless);
        let text = headless.painted_text();
        for label in [
            "does not delete anything",
            "Other devices can keep editing",
            "8 files",
            "80.0 MB",
            "Permanent local destination",
            "Recently Deleted photos:",
            "Additional space:",
            "12.0 MB",
            "Recently Deleted cloud photos and retained conflict originals are included",
        ] {
            assert!(text.iter().any(|text| text.contains(label)), "{label}: {text:?}");
        }
        assert!(text.iter().any(|text| text == "3"), "Recently Deleted count is visible: {text:?}");
        assert!(calls.lock().unwrap().is_empty());
        capture(&mut headless, "confirmation");
        click(&mut headless, "button:cloudDisableConfirm");
        assert!(blocked(&headless.app));
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(calls.lock().unwrap()[0].1["confirm"], true);
        for command in ["view.grid", "library.filter", "photo.rating", "app.openLibrary", "dialog.cloudSync"] {
            assert!(headless.app.run(command, json!({})).is_err(), "{command}");
        }
        for method in ["ui.dialog.cancel", "ui.dialog.confirm", "ui.set"] {
            assert_eq!(headless.request(method, json!({"view":"grid"}), WAIT)["ok"], false, "{method}");
        }
        let active = headless.app.session.active();
        for key in ["Escape", "ArrowRight", "5", "G"] {
            assert_eq!(headless.request("ui.key", json!({"key":key}), WAIT)["ok"], true);
            frames(&mut headless);
        }
        assert!(matches!(headless.app.ui.dialog, Some(Dialog::CloudDisable { .. })));
        assert_eq!(headless.app.session.active(), active);
        capture(&mut headless, "downloading");
        click(&mut headless, "button:cloudDisableCancel");
        assert!(!blocked(&headless.app));
        assert!(headless.app.ui.dialog.is_none());
        assert_eq!(calls.lock().unwrap().last().unwrap().1["cancel"], true);
    }

    #[test]
    fn cloud_disable_restores_interrupted_modal_with_retry_and_finishes_only_when_local() {
        let (mut headless, state, calls) = fixture(status("failed"), [900., 600.]);
        assert!(blocked(&headless.app));
        let text = headless.painted_text();
        for label in ["interrupted", "Retry", "Cancel", "Quit LightCraft", "5 remaining", "50.0 MB remaining"] {
            assert!(text.iter().any(|text| text.contains(label)), "{label}: {text:?}");
        }
        capture(&mut headless, "interrupted-small-window");
        click(&mut headless, "button:cloudDisableRetry");
        assert_eq!(calls.lock().unwrap().last().unwrap().1["confirm"], true);
        *state.lock().unwrap() = status("complete");
        headless.app.cloud_transition.next_poll = 0.;
        frames(&mut headless);
        assert!(!blocked(&headless.app));
        assert!(headless.painted_text().iter().any(|text| text.contains("Cloud sync is off")));
        capture(&mut headless, "complete-small-window");
        click(&mut headless, "button:cloudDisableDone");
        assert!(headless.app.ui.dialog.is_none());
        headless.app.run("dialog.cloudSync", json!({})).unwrap();
        let dialog = headless.app.ui.dialog.clone().unwrap();
        assert!(matches!(&dialog, Dialog::CloudConnect { connection, library, .. } if connection == "personal" && library == "library"));
        frames(&mut headless);
        crate::panels::dialogs::confirm_dialog(&mut headless.app, &dialog).unwrap();
        let calls = calls.lock().unwrap();
        let enable = calls.iter().find(|(id, _)| id == "cloud.syncEnable").unwrap();
        assert_eq!(enable.1["connection"], "personal");
        assert_eq!(enable.1["library"], "library");
    }

    #[test]
    fn cloud_disable_action_errors_remain_visible_across_status_polls() {
        let mut initial = status("failed");
        initial["actionError"] = json!("Could not save recovery state: disk full");
        let (mut headless, state, _) = fixture(initial, [900., 600.]);
        for button in ["button:cloudDisableRetry", "button:cloudDisableCancel"] {
            click(&mut headless, button);
            headless.app.cloud_transition.next_poll = 0.;
            frames(&mut headless);
            assert!(headless.painted_text().iter().any(|text| text.contains("disk full")));
            assert!(blocked(&headless.app));
        }
        state.lock().unwrap().as_object_mut().unwrap().remove("actionError");
        click(&mut headless, "button:cloudDisableRetry");
        assert!(!headless.painted_text().iter().any(|text| text.contains("disk full")));
    }

    #[test]
    fn local_only_info_offers_explicit_enable_and_hides_cloud_maintenance() {
        use lightcraft_catalog::{
            Op, Source,
            assets::{Asset, AssetId, Blob, BlobId, RemoteLocation},
        };
        let (mut headless, _, calls) = fixture(status("complete"), [1400., 1600.]);
        let id = headless.app.session.active().unwrap();
        let asset_id = AssetId("local-mode-original".into());
        headless
            .app
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
        headless
            .app
            .session
            .catalog
            .apply(Op::Relink { id, file_name: "local.png".into(), source: Source::Managed { asset_id }, format: Some("PNG".into()) })
            .unwrap();
        headless.app.run("panel.info", json!({})).unwrap();
        frames(&mut headless);
        let text = headless.painted_text();
        assert!(text.iter().any(|s| s.contains("Cloud sync is off")), "{text:?}");
        assert!(text.iter().any(|s| s.contains("Turn on cloud sync")), "{text:?}");
        for label in ["Sync now", "Reconnect and retry", "Remove local original copy", "Keep original offline", "Migrate library"] {
            assert!(!text.iter().any(|s| s.contains(label)), "{label}: {text:?}");
        }
        capture(&mut headless, "off-info");
        click(&mut headless, "button:cloudSyncTurnOn");
        assert!(matches!(headless.app.ui.dialog, Some(Dialog::CloudConnect { .. })));
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn cloud_disable_stage_views_include_verified_progress_and_safe_quit() {
        for stage in ["draining", "reading", "downloading", "checking"] {
            let (mut headless, _, _) = fixture(status(stage), [1100., 760.]);
            assert!(blocked(&headless.app));
            assert!(headless.painted_text().iter().any(|text| text.contains("3 of 8")));
            assert!(headless.painted_text().iter().any(|text| text.contains("30.0 MB of 80.0 MB")));
            capture(&mut headless, stage);
            click(&mut headless, "button:cloudDisableQuit");
            assert!(headless.quit_requested(), "{stage}");
        }
    }
    #[test]
    fn local_only_reenable_explains_a_merge_with_or_without_a_local_edit_baseline() {
        use lightcraft_catalog::{Op, local_sync::Baseline};
        for (local_only, baseline) in [(true, false), (true, true), (false, true)] {
            let initial = if local_only { status("complete") } else { Value::Null };
            let (mut h, _, calls) = fixture(initial, [1100., 760.]);
            if baseline {
                h.app
                    .session
                    .catalog
                    .apply(Op::SetLocalSyncBaseline {
                        baseline: Some(Box::new(Baseline {
                            library: "library".into(),
                            connection: "personal".into(),
                            clock: Default::default(),
                            documents: Default::default(),
                            imports: Default::default(),
                        })),
                    })
                    .unwrap();
            }
            h.app.run("dialog.cloudSync", json!({})).unwrap();
            frames(&mut h);
            let text = h.painted_text();
            if !local_only {
                assert!(h.app.ui.dialog.is_none(), "active sync opens the popup");
                assert!(
                    !text.iter().any(|s| s.contains("local edits and new photos will merge")),
                    "an edit baseline alone must not select the local-only copy"
                );
                assert!(calls.lock().unwrap().is_empty());
                continue;
            }
            assert!(matches!(&h.app.ui.dialog,Some(Dialog::CloudConnect {connection,library,..}) if connection=="personal" && library=="library"));
            for phrase in
                ["local edits and new photos will merge", "Conflicting edits are kept for review", "Permanent local originals stay on this device"]
            {
                assert!(text.iter().any(|s| s.contains(phrase)), "{phrase}: {text:?}");
            }
            click(&mut h, "button:dialogOk");
            let calls = calls.lock().unwrap();
            let enabled = calls.iter().find(|(id, _)| id == "cloud.syncEnable").unwrap();
            assert_eq!(enabled.1, json!({"connection":"personal","library":"library","protocol":"conditional-head-v1"}));
            assert!(calls.iter().any(|(id, _)| id == "cloud.sync"));
        }
    }
}
