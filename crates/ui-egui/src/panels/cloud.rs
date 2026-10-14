//! Cloud library popup. Counts describe this catalog, never invented provider capacity.
use crate::{LightcraftApp, theme::Tokens, widgets::register};
use serde_json::{Value, json};

pub fn bytes(n: u64) -> String {
    if n >= 1_000_000_000_000 {
        format!("{:.1} TB", n as f64 / 1_000_000_000_000.)
    } else if n >= 1_000_000_000 {
        format!("{:.1} GB", n as f64 / 1_000_000_000.)
    } else if n >= 1_000_000 {
        format!("{:.1} MB", n as f64 / 1_000_000.)
    } else if n >= 1_000 {
        format!("{:.1} KB", n as f64 / 1_000.)
    } else {
        format!("{n} B")
    }
}
fn action(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, label: &str, enabled: bool, command: &str, params: Value) {
    let r = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(label)));
    register(ui.ctx(), id, r.rect);
    if r.clicked()
        && let Err(e) = app.run(command, params)
    {
        app.toast(ui.ctx(), e);
    }
}
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, data: &Value) {
    let t = Tokens::get(ui.ctx());
    let summary = &data["summary"];
    register(ui.ctx(), "popup:cloud", ui.max_rect());
    ui.set_min_width(290.);
    ui.set_max_width(320.);
    ui.label(egui::RichText::new(crate::i18n::tr("CLOUD LIBRARY")).font(t.semibold(11.)).color(t.text_label));
    ui.label(format!("{} cloud photos · {} albums", summary["cloudPhotos"].as_u64().unwrap_or(0), summary["albums"].as_u64().unwrap_or(0)));
    ui.label(format!("{} of managed originals", bytes(summary["originalBytes"].as_u64().unwrap_or(0)))).on_hover_text(crate::i18n::tr(
        "Known original data in this catalog; shared originals count once. This is not the storage provider's quota or total bucket usage.",
    ));
    let paused = data["paused"] == true;
    let migrating = data["migrating"] == true;
    let waiting = data["waitingFiles"].as_u64().unwrap_or(0);
    let active = data["uploadingFiles"].as_u64().unwrap_or(0);
    let failed = summary["failedFiles"].as_u64().unwrap_or(0);
    ui.separator();
    ui.label(
        egui::RichText::new(crate::i18n::tr(if migrating {
            "Migrating cloud library…"
        } else if paused {
            "Syncing paused"
        } else if summary["issues"].as_u64().unwrap_or(0) > 0 || failed > 0 || data["error"].is_string() {
            "Sync needs attention"
        } else if waiting > 0 || active > 0 || summary["pendingChanges"].as_u64().unwrap_or(0) > 0 {
            "Syncing library"
        } else if data["syncEnabled"] == true {
            "Up to date"
        } else {
            "Local library"
        }))
        .font(t.semibold(14.)),
    );
    ui.label(format!("{waiting} files waiting · {active} uploading"));
    if failed > 0 {
        ui.label(egui::RichText::new(format!("{failed} files need upload retry")).color(t.caution));
    }
    if summary["issues"].as_u64().unwrap_or(0) > 0 {
        ui.label(format!("{} photos with sync issues", summary["issues"].as_u64().unwrap_or(0)));
    }
    ui.label(format!("{} queued data", bytes(summary["queuedBytes"].as_u64().unwrap_or(0)))).on_hover_text(crate::i18n::tr(
        "Originals, previews and sidecars awaiting verification. Existing multipart transfers resume from saved parts.",
    ));
    let changes = summary["pendingChanges"].as_u64().unwrap_or(0);
    ui.label(format!("{changes} library changes waiting to sync"));
    action(
        app,
        ui,
        "button:cloudPauseResume",
        if paused { "Resume Syncing" } else { "Pause Syncing" },
        app.session.library.is_some() && (!paused || data["canResume"] == true),
        if paused { "cloud.resume" } else { "cloud.pause" },
        json!({}),
    );
    if !paused && (failed > 0 || data["error"].is_string()) {
        action(app, ui, "button:cloudRetry", "Retry Syncing", data["canResume"] == true, "cloud.resume", json!({}));
    }
    if paused && data["canResume"] != true {
        ui.label(crate::i18n::tr("Stopping current transfers…"));
    }
    ui.label(
        egui::RichText::new(crate::i18n::tr("Uploads and library publication pause on this device. Photos remain available for editing."))
            .size(11.)
            .color(t.text_dim),
    );
    ui.separator();
    if let Some(tasks) = data["tasks"].as_array() {
        for task in tasks.iter().take(4) {
            ui.label(crate::i18n::tr(task["label"].as_str().unwrap_or("Cloud transfer")));
            if let Some(detail) = task["detail"].as_str().filter(|s| !s.is_empty()) {
                ui.label(egui::RichText::new(detail.chars().take(80).collect::<String>()).size(11.).color(t.text_dim));
            }
            let done = task["done"].as_u64().unwrap_or(0);
            let total = task["total"].as_u64().unwrap_or(0);
            if total > 0 {
                ui.add(egui::ProgressBar::new((done as f64 / total as f64).clamp(0., 1.) as f32).text(if task["unit"] == "bytes" {
                    format!("{} / {}", bytes(done.min(total)), bytes(total))
                } else {
                    format!("{done} / {total}")
                }));
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(crate::i18n::tr("Working…"));
                });
            }
        }
    }
    if let Some(error) = data["error"].as_str().filter(|_| !paused) {
        ui.label(egui::RichText::new(error).size(11.).color(t.caution));
    }
    if app.session.unsaved().is_some() {
        ui.label(egui::RichText::new(crate::i18n::tr("Local changes could not be saved. LightCraft is retrying.")).color(t.caution));
    }
    ui.separator();
    ui.horizontal(|ui| {
        action(app, ui, "button:cloudShowWaiting", "Show waiting", true, "library.filter", json!({"syncStatus":"pending"}));
        action(app, ui, "button:cloudShowIssues", "Show issues", true, "library.filter", json!({"syncStatus":"issues"}));
    });
    action(app, ui, "button:cloudStorage", "Storage Connection…", app.session.library.is_some(), "dialog.cloudStorage", json!({}));
    action(
        app,
        ui,
        "button:cloudSetup",
        if data["syncEnabled"] == true { "Cloud Synchronization…" } else { "Set Up Cloud Sync…" },
        app.session.library.is_some(),
        "dialog.cloudSync",
        json!({}),
    );
    #[cfg(not(target_arch = "wasm32"))]
    action(app, ui, "button:cloudImport", "Import to Cloud Storage…", app.session.library.is_some(), "file.importCloud", json!({}));
    if let Some(state) = app.session.catalog.sync_state().filter(|s| s.protocol.is_head()) {
        let _ = state;
        action(app, ui, "button:cloudMigrate", "Migrate Cloud Library…", !migrating, "dialog.cloudMigrate", json!({}));
    }
}
