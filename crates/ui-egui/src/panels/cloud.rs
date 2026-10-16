//! Cloud library popup. Counts describe this catalog, never invented provider capacity.
use crate::{LightcraftApp, theme::Tokens, widgets::register};
use serde_json::{Value, json};

pub(crate) fn needs_attention(data: &Value) -> bool {
    let summary = &data["summary"];
    let health = &data["health"];
    summary["issues"].as_u64().unwrap_or(0) > 0
        || summary["failedFiles"].as_u64().unwrap_or(0) > 0
        || summary["blockedImportCount"].as_u64().unwrap_or(0) > 0
        || data["error"].is_string()
        || health["sync"]["pollingHalted"] == true
        || health["sync"]["lastError"].is_object()
        || health["uploads"]["failedImports"].as_u64().unwrap_or(0) > 0
        || health["uploads"]["activity"]["failedResources"].as_u64().unwrap_or(0) > 0
        || health["media"]["failedResources"].as_u64().unwrap_or(0) > 0
        || health["offline"]["offlineErrors"].as_object().is_some_and(|errors| !errors.is_empty())
        || health["receipts"]["failed"].as_u64().unwrap_or(0) > 0
        || health["conflicts"]["fields"].as_u64().unwrap_or(0) > 0
        || health["conflicts"]["structural"].as_u64().unwrap_or(0) > 0
        || data["conflicts"].as_array().is_some_and(|items| !items.is_empty())
        || data["structuralConflicts"].as_array().is_some_and(|items| !items.is_empty())
        || data["capacityWarnings"].as_array().is_some_and(|items| !items.is_empty())
}
fn verified_scan(data: &Value) -> bool {
    data["health"]["sync"]["lastSuccessfulRemoteScan"].is_object() && matches!(data["health"]["sync"]["state"].as_str(), Some("healthy" | "checking"))
}
pub(crate) fn retry_text(health: &Value) -> Option<String> {
    (health["state"] == "retrying").then(|| {
        health["retry"]["secondsUntilRetry"]
            .as_u64()
            .map_or_else(|| crate::i18n::tr("Retrying synchronization…").to_owned(), |n| crate::i18n::tr_format!("Retrying in {n} s", n = n))
    })
}

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
    let halted = data["health"]["sync"]["pollingHalted"] == true;
    let receipts = data["health"]["receipts"]["pending"].as_u64().unwrap_or(0);
    let structural = data["structuralConflicts"].as_array().map_or(0, Vec::len);
    ui.separator();
    ui.label(
        egui::RichText::new(crate::i18n::tr(if migrating {
            "Migrating cloud library…"
        } else if paused {
            "Syncing paused"
        } else if data["health"]["sync"]["state"] == "retrying" {
            "Retrying synchronization…"
        } else if needs_attention(data) {
            "Sync needs attention"
        } else if waiting > 0
            || active > 0
            || data["preparingFiles"].as_u64().unwrap_or(0) > 0
            || receipts > 0
            || data["health"]["remoteHistoryWaiting"] == true
            || summary["pendingChanges"].as_u64().unwrap_or(0) > 0
        {
            "Syncing library"
        } else if data["syncEnabled"] == true && verified_scan(data) {
            "Up to date"
        } else if data["syncEnabled"] == true {
            "Checking cloud library"
        } else {
            "Local library"
        }))
        .font(t.semibold(14.)),
    );
    ui.label(format!("{waiting} files waiting · {active} uploading"));
    if let Some(n) = data["preparingFiles"].as_u64().filter(|n| *n > 0) {
        ui.label(format!("{n} import batches preparing"));
    }
    if halted {
        ui.label(egui::RichText::new(crate::i18n::tr("Automatic checking stopped after an error.")).color(t.caution));
    }
    if let Some(text) = retry_text(&data["health"]["sync"]) {
        ui.label(text);
        ui.label(crate::i18n::tr_format!(
            "Automatic retry {attempt} of {max}",
            attempt = data["health"]["sync"]["retry"]["attempt"].as_u64().unwrap_or(0),
            max = data["health"]["sync"]["retry"]["maxAttempts"].as_u64().unwrap_or(0)
        ));
    }
    if data["health"]["sync"]["retry"]["exhausted"] == true {
        ui.colored_label(t.caution, crate::i18n::tr("Automatic retry limit reached. Resolve the error, then Retry Syncing."));
    }
    if let Some(action) = data["health"]["sync"]["lastError"]["action"].as_str() {
        ui.label(egui::RichText::new(action).size(11.).color(t.caution));
    }
    if data["syncEnabled"] == true && !data["health"]["sync"]["lastSuccessfulRemoteScan"].is_object() {
        ui.label(egui::RichText::new(crate::i18n::tr("No successful remote check in this session.")).size(11.).color(t.text_dim));
    }
    if let Some(n) = data["health"]["sync"]["secondsSinceLastRemoteScan"].as_u64() {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("Last remote check: {n} s ago", n = n)).size(11.).color(t.text_dim));
    }
    if data["health"]["offline"]["persistentCache"]["temporaryBlobs"].as_u64().unwrap_or(0) > 0 {
        ui.colored_label(t.caution, crate::i18n::tr("Some downloads are available for this session only."));
    }
    if let Some(errors) = data["health"]["offline"]["offlineErrors"].as_object() {
        for error in errors.values().filter_map(Value::as_str).take(3) {
            ui.colored_label(t.caution, error);
        }
    }
    if receipts > 0 {
        ui.label(format!("{}: {receipts}", crate::i18n::tr("Local upload records waiting")));
    }
    if structural > 0 {
        ui.label(egui::RichText::new(format!("{}: {structural}", crate::i18n::tr("Album or stack choices need attention"))).color(t.caution));
    }
    if failed > 0 {
        ui.label(egui::RichText::new(format!("{failed} files need upload retry")).color(t.caution));
    }
    if let Some(issues) = data["health"]["imports"]["failed"].as_array() {
        for issue in issues.iter().take(3) {
            ui.label(egui::RichText::new(issue["name"].as_str().unwrap_or("Staged import")).size(11.));
            ui.label(egui::RichText::new(issue["reason"].as_str().unwrap_or("Import recovery needs attention")).size(11.).color(t.caution));
            if let Some(command @ ("cloud.import" | "cloud.adopt" | "cloud.recoverImports")) = issue["command"].as_str() {
                action(
                    app,
                    ui,
                    &format!("button:cloudRecoverImport:{}", issue["id"].as_str().unwrap_or("unknown")),
                    "Retry import",
                    data["canResume"] == true,
                    command,
                    issue["retry"].clone(),
                );
            }
        }
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
    if let Some(resource) = data["health"]["uploads"]["activity"]["firstFailedResource"].as_str()
        && let Some((kind, id)) = resource.split_once(':')
        && matches!(kind, "upload" | "verify")
    {
        if let Some(message) = data["health"]["uploads"]["activity"]["failures"][resource]["message"].as_str() {
            ui.label(egui::RichText::new(message).size(11.).color(t.caution));
        }
        action(
            app,
            ui,
            &format!("button:cloudRetryWork:{resource}"),
            "Retry upload",
            !paused && data["canResume"] == true,
            "cloud.upload",
            json!({"imports":[id],"verify":kind=="verify","wait":false}),
        );
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
    if !paused && needs_attention(data) {
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
    if let Some(error) = data["error"]
        .as_str()
        .filter(|error| !paused && !blocked.is_some_and(|items| items.iter().any(|item| item["reason"].as_str() == Some(error))))
    {
        ui.label(egui::RichText::new(error).size(11.).color(t.caution));
    }
    if let Some(error) = summary["firstUploadError"].as_str().filter(|error| {
        data["error"].as_str() != Some(error) && !blocked.is_some_and(|items| items.iter().any(|item| item["reason"].as_str() == Some(error)))
    }) {
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
    ui.label(
        egui::RichText::new(crate::i18n::tr(
            "Deleting photos retains uploaded originals and history in the bucket. Retention tools are in Cloud Synchronization.",
        ))
        .size(11.)
        .color(t.text_dim),
    );
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
