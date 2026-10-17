//! Whole-library erasure uses a fresh, explicit dialog; dismissal drops typed consent.
use crate::LightcraftApp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(any(test, not(target_arch = "wasm32")))]
use serde_json::json;
#[cfg(any(test, not(target_arch = "wasm32")))]
const PHRASE: &str = "DELETE CLOUD LIBRARY";
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub library: String,
    task: Option<u64>,
    preview: Option<Value>,
    phrase: String,
    stopped: bool,
    error: Option<String>,
}
impl Review {
    pub fn new(library: String) -> Self {
        Self { library, ..Default::default() }
    }
    #[cfg(any(test, not(target_arch = "wasm32")))]
    fn can_apply(&self) -> bool {
        self.task.is_none()
            && self.phrase == PHRASE
            && self.stopped
            && self.preview.as_ref().is_some_and(|v| v["applied"] == false && v["preview"].is_string())
    }
}
pub(crate) fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, review: &mut Review) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, review);
        ui.label(crate::i18n::tr(
            "Use the desktop app to permanently delete a cloud library. This browser connection does not grant remote deletion.",
        ));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t = crate::theme::Tokens::get(ui.ctx());
        ui.colored_label(
            t.reject,
            crate::i18n::tr("Permanently delete every cloud photo, edit, album, previous version and library history. This cannot be undone."),
        );
        ui.label(crate::i18n::tr(
            "The storage bucket is kept. Local copies on every device remain. A small deletion marker prevents this library from syncing again.",
        ));
        ui.label(crate::i18n::tr("Close LightCraft on all other devices before continuing. This device will pause synchronization. Keep other devices stopped if deletion is interrupted."));
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Library"));
            ui.add(egui::Label::new(egui::RichText::new(&review.library).monospace()).wrap());
        });
        if app.session.catalog.sync_state().is_none_or(|s| s.library != review.library) {
            ui.colored_label(t.reject, crate::i18n::tr("The selected library changed. Close this dialog and review the current library."));
            return;
        }
        if review.task.is_none()
            && review.preview.is_none()
            && app.run("cloud.overview", json!({})).ok().is_some_and(|v| v["erasure"]["complete"] == true)
        {
            review.preview = Some(json!({"applied":true}));
        }
        if let Some(task) = review.task {
            let status = app.run("cloud.uploadStatus", json!({})).unwrap_or_default();
            if let Some(report) = status["reports"].as_array().and_then(|v| v.iter().find(|r| r["task"] == task)) {
                review.task = None;
                review.phrase.clear();
                review.stopped = false;
                if report["ok"] == true {
                    review.preview = Some(report["result"].clone());
                    review.error = None;
                } else {
                    review.error =
                        Some(report["error"].as_str().unwrap_or("Deletion did not complete. Review again to inspect remaining data.").into());
                    review.preview = None;
                }
            } else {
                ui.label(crate::i18n::tr("Reviewing storage or deleting cloud library data…"));
                ui.label(crate::i18n::tr("Stopping only halts remaining deletions. Deleted data cannot be restored."));
                let r = ui.button(crate::i18n::tr("Stop deletion task"));
                crate::widgets::register(ui.ctx(), "button:cloudDeleteStop", r.rect);
                if r.clicked() {
                    let _ = app.run("activity.cancel", json!({"id":task}));
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if let Some(preview) = &review.preview {
            if preview["applied"] == true {
                ui.strong(crate::i18n::tr(
                    "Cloud library deleted. Local copies remain on your devices. Use a new library and storage folder to synchronize again.",
                ));
                return;
            }
            for (key, label) in [("bucket", "Bucket"), ("prefix", "Folder")] {
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr(label));
                    ui.add(egui::Label::new(egui::RichText::new(preview[key].as_str().unwrap_or_default()).monospace()).wrap());
                });
            }
            ui.label(format!(
                "{}: {} · {}: {}",
                crate::i18n::tr("Stored object versions"),
                preview["versions"],
                crate::i18n::tr("Storage space used"),
                crate::panels::cloud::bytes(preview["bytes"].as_u64().unwrap_or(0))
            ));
        }
        let r = ui.add_enabled(review.task.is_none(), egui::Button::new(crate::i18n::tr("Pause and review library deletion")));
        crate::widgets::register(ui.ctx(), "button:cloudDeletePreview", r.rect);
        if r.clicked() {
            review.phrase.clear();
            review.stopped = false;
            review.preview = None;
            match app.run("cloud.pause", json!({})).and_then(|_| app.run("cloud.deleteLibrary", json!({"library":review.library,"wait":false}))) {
                Ok(value) => {
                    review.task = value["task"].as_u64();
                    review.error = None;
                }
                Err(error) => review.error = Some(error),
            }
        }
        ui.add_enabled_ui(review.task.is_none() && review.preview.is_some(), |ui| {
            let r = ui.checkbox(
                &mut review.stopped,
                crate::i18n::tr("LightCraft is closed on all other devices, and I understand that cloud deletion is permanent."),
            );
            crate::widgets::register(ui.ctx(), "checkbox:cloudDeleteStopped", r.rect);
            ui.label(crate::i18n::tr("Type DELETE CLOUD LIBRARY to confirm:"));
            let r = ui.add(egui::TextEdit::singleline(&mut review.phrase).id_salt("cloudDeletePhrase").hint_text(PHRASE));
            crate::widgets::register(ui.ctx(), "field:cloudDeletePhrase", r.rect);
        });
        let r = ui.add_enabled(
            review.can_apply(),
            egui::Button::new(egui::RichText::new(crate::i18n::tr("Permanently delete cloud library")).color(t.reject)),
        );
        crate::widgets::register(ui.ctx(), "button:cloudDeleteApply", r.rect);
        if r.clicked()
            && let Some(preview) = &review.preview
        {
            match app.run("cloud.deleteLibrary",json!({"library":review.library,"apply":true,"confirmation":review.phrase,"preview":preview["preview"],"allDevicesStopped":review.stopped,"wait":false})) {
                Ok(value)=>{review.task=value["task"].as_u64();review.error=None;review.phrase.clear();review.stopped=false;},Err(error)=>review.error=Some(error),
            }
        }
        if let Some(error) = &review.error {
            ui.colored_label(t.reject, error);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_needs_exact_phrase_review_and_stopped_devices() {
        let mut review = Review::new("library".into());
        review.preview = Some(json!({"preview":"a".repeat(64),"applied":false}));
        review.stopped = true;
        for phrase in ["", "DELETE", "delete cloud library", "DELETE CLOUD LIBRARY "] {
            review.phrase = phrase.into();
            assert!(!review.can_apply());
        }
        review.phrase = PHRASE.into();
        assert!(review.can_apply());
        review.stopped = false;
        assert!(!review.can_apply());
    }
    #[test]
    fn cloud_library_delete_dialog_requires_typed_phrase_and_drops_consent_on_close() {
        use crate::{Services, headless::Headless};
        use std::{
            sync::{Arc, Mutex},
            time::Duration,
        };
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let recorded = calls.clone();
        let mut session = lightcraft_engine::Session::new();
        session
            .open_library_in(
                lightcraft_engine::library::LibraryStores {
                    dir: "erase-ui-fixture".into(),
                    catalog: Box::new(lightcraft_catalog::MemStore::new()),
                    files: Box::new(lightcraft_catalog::MemStore::new()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        session
            .apply_received(lightcraft_catalog::Op::SetSyncState {
                state: Some(Box::new(lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap())),
            })
            .unwrap();
        let services = Services {
            cloud_action: Some(Box::new(move |_, id, p| match id {
                "cloud.pause" => Some(Ok(json!({"paused":true}))),
                "cloud.deleteLibrary" => {
                    recorded.lock().unwrap().push(p.clone());
                    Some(Ok(json!({"task":if p["apply"]==true {8}else{7}})))
                }
                "cloud.uploadStatus" => Some(Ok(
                    json!({"reports":[{"task":7,"ok":true,"result":{"preview":"a".repeat(64),"bucket":"photos","prefix":"lightcraft/personal","versions":12,"bytes":1_200_000_000,"applied":false}},{"task":8,"ok":true,"result":{"applied":true}}]}),
                )),
                _ => None,
            })),
            write: Some(Box::new(|p, b| std::fs::write(p, b).map_err(|e| e.to_string()))),
            png: Some(Box::new(|i| lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(i), &Default::default()).unwrap())),
            ..Default::default()
        };
        let mut h = Headless::new(LightcraftApp::new(session, services), [1200., 1000.], 1.);
        let ask = |h: &mut Headless, method: &str, p: Value| {
            let v = h.request(method, p, Duration::from_secs(30));
            assert_eq!(v["ok"], true, "{v}");
            for _ in 0..4 {
                h.step();
            }
        };
        let click = |h: &mut Headless, id: &str| ask(h, "ui.clickWidget", json!({"id":id}));
        let capture = |h: &mut Headless, name: &str| {
            if let Ok(dir) = std::env::var("LIGHTCRAFT_ERASE_SNAPSHOTS") {
                std::fs::create_dir_all(&dir).unwrap();
                ask(h, "ui.screenshot", json!({"path":std::path::Path::new(&dir).join(format!("{name}.png")).to_string_lossy()}));
            }
        };
        for _ in 0..4 {
            h.step();
        }
        h.app.run("dialog.cloudDelete", json!({})).unwrap();
        for _ in 0..4 {
            h.step();
        }
        click(&mut h, "button:cloudDeleteApply");
        assert!(calls.lock().unwrap().is_empty());
        click(&mut h, "button:cloudDeletePreview");
        capture(&mut h, "review");
        click(&mut h, "checkbox:cloudDeleteStopped");
        click(&mut h, "field:cloudDeletePhrase");
        ask(&mut h, "ui.text", json!({"text":"wrong"}));
        click(&mut h, "button:cloudDeleteApply");
        assert!(calls.lock().unwrap().iter().all(|p| p["apply"] != true));
        if let Some(crate::state::Dialog::CloudDelete { review }) = &mut h.app.ui.dialog {
            review.phrase.clear();
        }
        for _ in 0..4 {
            h.step();
        }
        click(&mut h, "field:cloudDeletePhrase");
        ask(&mut h, "ui.text", json!({"text":PHRASE}));
        assert!(matches!(&h.app.ui.dialog,Some(crate::state::Dialog::CloudDelete{review}) if review.can_apply()));
        capture(&mut h, "confirmed");
        click(&mut h, "button:dialogOk");
        assert!(h.app.ui.dialog.is_none());
        h.app.run("dialog.cloudDelete", json!({})).unwrap();
        for _ in 0..4 {
            h.step();
        }
        assert!(
            matches!(&h.app.ui.dialog,Some(crate::state::Dialog::CloudDelete{review}) if review.phrase.is_empty() && !review.stopped && review.preview.is_none())
        );
        click(&mut h, "button:cloudDeletePreview");
        click(&mut h, "checkbox:cloudDeleteStopped");
        click(&mut h, "field:cloudDeletePhrase");
        ask(&mut h, "ui.text", json!({"text":PHRASE}));
        click(&mut h, "button:cloudDeleteApply");
        let calls = calls.lock().unwrap();
        let applied: Vec<_> = calls.iter().filter(|p| p["apply"] == true).collect();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["confirmation"], PHRASE);
        assert_eq!(applied[0]["allDevicesStopped"], true);
        capture(&mut h, "complete");
    }
}
