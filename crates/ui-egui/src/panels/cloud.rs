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
pub(super) fn capacity(app: &mut LightcraftApp, ui: &mut egui::Ui, data: &Value, detailed: bool) {
    let Some(usage) = data.get("utilization").filter(|v| v.is_object()) else { return };
    let t = Tokens::get(ui.ctx());
    ui.separator();
    ui.strong(crate::i18n::tr("Synchronization capacity"));
    let used = usage["stateBytes"].as_u64().unwrap_or(0);
    let limit = usage["stateLimitBytes"].as_u64().unwrap_or(64 << 20);
    ui.label(format!(
        "Metadata: {:.1} / {:.0} MiB ({:.1}%)",
        used as f64 / 1_048_576.,
        limit as f64 / 1_048_576.,
        usage["statePercent"].as_f64().unwrap_or(0.)
    ));
    if detailed {
        for (label, count, max) in [
            ("Documents", "documents", "documentLimit"),
            ("Full receipts", "received", "receiptLimit"),
            ("Pending changes", "pending", "pendingLimit"),
            ("Author identities", "authors", "authorLimit"),
            ("Largest entity fields", "maxEntityFields", "entityFieldLimit"),
            ("Most field revisions", "maxFieldRevisions", "fieldRevisionLimit"),
        ] {
            ui.label(format!("{}: {} / {}", crate::i18n::tr(label), usage[count].as_u64().unwrap_or(0), usage[max].as_u64().unwrap_or(0)));
        }
        ui.label(format!(
            "{} historical full receipts · {} archive roots",
            usage["historicalReceipts"].as_u64().unwrap_or(0),
            usage["archiveRoots"].as_u64().unwrap_or(0)
        ));
    }
    if let Some(warnings) = data.get("capacityWarnings").and_then(Value::as_array) {
        for warning in warnings.iter().take(if detailed { 7 } else { 1 }) {
            if let Some(message) = warning["message"].as_str() {
                ui.label(egui::RichText::new(crate::i18n::tr(message)).color(t.caution));
            }
            if detailed && let Some(recovery) = warning["recovery"].as_str() {
                ui.label(crate::i18n::tr(recovery));
            }
        }
    }
    if detailed {
        let enabled = app.session.catalog.sync_state().is_some()
            && data["running"] != true
            && data["paused"] != true
            && data["preparingMigration"] != true
            && data["migrating"] != true;
        action(app, ui, "button:cloudCheckpoint", "Archive receipt history", enabled, "cloud.sync", json!({"wait":false,"checkpoint":true}));
        ui.label(crate::i18n::tr("Current edits, tombstones, author clocks and remote recovery records stay retained."));
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
    let blocked = summary["blockedImports"].as_array();
    ui.separator();
    ui.label(
        egui::RichText::new(crate::i18n::tr(if migrating {
            "Migrating cloud library…"
        } else if paused {
            "Syncing paused"
        } else if summary["issues"].as_u64().unwrap_or(0) > 0
            || failed > 0
            || blocked.is_some_and(|items| !items.is_empty())
            || data["error"].is_string()
            || data["capacityWarnings"].as_array().is_some_and(|w| !w.is_empty())
        {
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
    if let Some(blocked) = blocked {
        for item in blocked.iter().take(3) {
            ui.label(egui::RichText::new(item["name"].as_str().unwrap_or("Cloud import")).size(11.));
            ui.label(egui::RichText::new(item["reason"].as_str().unwrap_or("Upload preparation failed")).size(11.).color(t.caution));
            if let Some(id) = item["id"].as_str() {
                action(
                    app,
                    ui,
                    &format!("button:cloudRetryImport:{id}"),
                    "Retry upload",
                    !paused && data["canResume"] == true,
                    "cloud.upload",
                    json!({"imports":[id],"verify":item["verify"] == true,"wait":false}),
                );
            }
        }
        let count = summary["blockedImportCount"].as_u64().unwrap_or(blocked.len() as u64);
        if count > 3 {
            ui.label(format!("{} more blocked imports; select a photo for details", count.saturating_sub(3)));
        }
    }
    if summary["issues"].as_u64().unwrap_or(0) > 0 {
        ui.label(format!("{} photos with sync issues", summary["issues"].as_u64().unwrap_or(0)));
    }
    ui.label(format!("{} queued data", bytes(summary["queuedBytes"].as_u64().unwrap_or(0)))).on_hover_text(crate::i18n::tr(
        "Originals, previews and sidecars awaiting verification. Existing multipart transfers resume from saved parts.",
    ));
    let changes = summary["pendingChanges"].as_u64().unwrap_or(0);
    ui.label(format!("{changes} library changes waiting to sync"));
    capacity(app, ui, data, false);
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
