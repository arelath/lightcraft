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
        || health["imports"]["failed"].as_array().is_some_and(|items| !items.is_empty())
        || health["offline"]["persistentCache"]["temporaryBlobs"].as_u64().unwrap_or(0) > 0
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
        || data["publicationBlocked"].is_string()
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

/// How long ago, in the largest whole unit: "just now", "40 s ago", "5 min ago", "3 h ago", "2 d ago".
pub(crate) fn ago(seconds: u64) -> String {
    match seconds {
        0..5 => crate::i18n::tr("just now").to_owned(),
        5..60 => crate::i18n::tr_format!("{n} s ago", n = seconds),
        60..3600 => crate::i18n::tr_format!("{n} min ago", n = seconds / 60),
        3600..86400 => crate::i18n::tr_format!("{n} h ago", n = seconds / 3600),
        _ => crate::i18n::tr_format!("{n} d ago", n = seconds / 86400),
    }
}
/// "Label: value", with the label translated; counts read the same in every language this way.
pub(crate) fn count(label: &str, value: impl std::fmt::Display) -> String {
    format!("{}: {value}", crate::i18n::tr(label))
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
        "{}: {:.1} / {:.0} MiB ({:.1}%)",
        crate::i18n::tr("Metadata"),
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
            "{} · {}",
            count("Historical full receipts", usage["historicalReceipts"].as_u64().unwrap_or(0)),
            count("Archive roots", usage["archiveRoots"].as_u64().unwrap_or(0))
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

fn heading(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(text)).font(t.font(11.)).color(t.text_dim));
    ui.add_space(6.);
}
fn section(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin::same(16)).show(ui, add);
}
fn divider(ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.), egui::Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, egui::Stroke::new(1., t.field_border));
}
fn metric(ui: &mut egui::Ui, label: &str, value: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let response = ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(value);
        });
        response
    })
    .inner
}
fn invoke(app: &mut LightcraftApp, ui: &mut egui::Ui, command: &str, params: Value) {
    if let Err(error) = app.run(command, params) {
        app.toast(ui.ctx(), error);
    }
    ui.close();
}
/// Full-width menu rows keep the target generous and their chevrons aligned.
fn navigation(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, label: &str, command: &str, params: Value) {
    let t = Tokens::get(ui.ctx());
    let response = ui.add_enabled(
        app.session.library.is_some(),
        egui::Button::new(crate::i18n::tr(label)).frame(false).min_size(egui::vec2(ui.available_width(), 30.)).right_text(""),
    );
    register(ui.ctx(), id, response.rect);
    crate::icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(response.rect.right_center() - egui::vec2(8., 0.), egui::vec2(12., 12.)),
        crate::icons::Icon::ChevronRight,
        t.text_dim,
    );
    if response.clicked() {
        invoke(app, ui, command, params);
    }
}
fn attention_row(app: &mut LightcraftApp, ui: &mut egui::Ui, title: &str, detail: &str, link: Option<(&str, &str)>) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(18., 20.), egui::Sense::hover());
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "!", t.semibold(15.), t.caution);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr(title)).size(14.));
                if let Some((id, label)) = link {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let response = ui.link(crate::i18n::tr(label));
                        register(ui.ctx(), id, response.rect);
                        if response.clicked() {
                            invoke(app, ui, "dialog.cloudSync", json!({}));
                        }
                    });
                }
            });
            if !detail.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(detail).color(t.text_dim)).wrap());
            }
        });
    });
    ui.add_space(12.);
}
fn busy(data: &Value) -> bool {
    data["waitingFiles"].as_u64().unwrap_or(0) > 0
        || data["uploadingFiles"].as_u64().unwrap_or(0) > 0
        || data["preparingFiles"].as_u64().unwrap_or(0) > 0
        || data["health"]["receipts"]["pending"].as_u64().unwrap_or(0) > 0
        || data["health"]["remoteHistoryWaiting"] == true
        || data["summary"]["pendingChanges"].as_u64().unwrap_or(0) > 0
}
fn footer_text(data: &Value, unsaved: bool) -> &'static str {
    if unsaved || needs_attention(data) {
        "Sync needs attention"
    } else if data["migrating"] == true || data["preparingMigration"] == true {
        "Migrating cloud library…"
    } else if data["paused"] == true {
        "Syncing paused"
    } else if busy(data) {
        "Syncing library…"
    } else if data["syncEnabled"] == true && verified_scan(data) {
        "Synced and backed up"
    } else if data["syncEnabled"] == true {
        "Checking cloud library…"
    } else {
        "Local library"
    }
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, data: &Value) {
    let t = Tokens::get(ui.ctx());
    let summary = &data["summary"];
    let paused = data["paused"] == true;
    let waiting = data["waitingFiles"].as_u64().unwrap_or(0);
    let active = data["uploadingFiles"].as_u64().unwrap_or(0);
    let changes = summary["pendingChanges"].as_u64().unwrap_or(0);
    let attention = needs_attention(data) || app.session.unsaved().is_some();
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    ui.set_width(320.);
    ui.spacing_mut().item_spacing.y = 6.;
    let popup = ui.scope(|ui| {
        section(ui, |ui| {
            heading(ui, "CLOUD LIBRARY");
            let photos = summary["cloudPhotos"].as_u64().unwrap_or(0);
            let albums = summary["albums"].as_u64().unwrap_or(0);
            let photo_text = if photos == 1 { crate::i18n::tr_format!("{n} photo", n = photos) } else { crate::i18n::tr_format!("{n} photos", n = photos) };
            let album_text = if albums == 1 { crate::i18n::tr_format!("{n} album", n = albums) } else { crate::i18n::tr_format!("{n} albums", n = albums) };
            ui.label(crate::i18n::tr_format!("{photos} in {albums}", photos = photo_text, albums = album_text));
            ui.add_space(6.);
            metric(ui, if waiting > 0 || active > 0 || summary["queuedFiles"].as_u64().unwrap_or(0) > 0 { "Managed originals" } else { "Originals in the cloud" }, &bytes(summary["originalBytes"].as_u64().unwrap_or(0))).on_hover_text(crate::i18n::tr(
                "Known original data in this catalog; shared originals count once. This is not the storage provider's quota or total bucket usage. Pending uploads may not yet be in the cloud.",
            ));
            if let Some(age) = data["health"]["sync"]["secondsSinceLastRemoteScan"].as_u64() {
                metric(ui, "Last checked", &ago(age));
            } else if data["syncEnabled"] == true {
                metric(ui, "Last checked", crate::i18n::tr("Not yet checked"));
            }
        });
        if attention {
            divider(ui);
            section(ui, |ui| { heading(ui, "NEEDS ATTENTION"); attention_details(app, ui, data); });
        }
        if waiting > 0 || active > 0 || data["preparingFiles"].as_u64().unwrap_or(0) > 0 {
            divider(ui);
            section(ui, |ui| {
                heading(ui, if paused { "UPLOADS PAUSED" } else { "UPLOADING" });
                metric(ui, &crate::i18n::tr_format!("{n} uploading", n = active), &crate::i18n::tr_format!("{n} waiting", n = waiting));
                let done = data["doneBytes"].as_u64().unwrap_or(0);
                let total = data["totalBytes"].as_u64().unwrap_or(0);
                if total > 0 {
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("{done} of {total}", done = bytes(done.min(total)), total = bytes(total))).color(t.text_dim));
                    ui.add(egui::ProgressBar::new((done as f64 / total as f64).clamp(0., 1.) as f32).desired_height(6.).fill(t.accent))
                        .on_hover_text(crate::i18n::tr("Progress of current transfers; queued data includes originals, previews and sidecars."));
                } else if !paused && active > 0 {
                    ui.horizontal(|ui| { ui.spinner(); ui.label(crate::i18n::tr("Preparing transfer…")); });
                }
                if let Some(n) = data["preparingFiles"].as_u64().filter(|n| *n > 0) { ui.label(count("Import batches preparing", n)); }
                metric(ui, "Changes to publish", &changes.to_string());
                if active == 0 { metric(ui, "Queued data", &bytes(summary["queuedBytes"].as_u64().unwrap_or(0))); }
            });
        } else if changes > 0 && !attention {
            divider(ui);
            section(ui, |ui| { metric(ui, "Changes to publish", &changes.to_string()); });
        }
        divider(ui);
        section(ui, |ui| {
            if !attention && (busy(data) || paused) {
                navigation(app, ui, "button:cloudShowWaiting", "Show waiting photos", "library.filter", json!({"syncStatus":"pending"}));
            }
            navigation(app, ui, "button:cloudSetup", "Cloud synchronization…", "dialog.cloudSync", json!({}));
            if !busy(data) || attention {
                navigation(app, ui, "button:cloudStorage", "Storage connection…", "dialog.cloudStorage", json!({}));
            }
            #[cfg(not(target_arch = "wasm32"))]
            if !busy(data) && !attention {
                navigation(app, ui, "button:cloudImport", "Import to cloud storage…", "file.importCloud", json!({}));
            }
        });
        divider(ui);
        section(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(20., 24.), egui::Sense::hover());
                if attention {
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "!", t.semibold(16.), t.caution);
                } else {
                    crate::icons::paint(ui.painter(), rect.shrink(2.), crate::icons::Icon::Cloud, t.accent);
                }
                let status_width = (ui.available_width() - 70.).max(80.);
                ui.allocate_ui_with_layout(egui::vec2(status_width, 36.), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(status_width);
                    ui.add(egui::Label::new(crate::i18n::tr(footer_text(data, app.session.unsaved().is_some()))).wrap());
                });
                let retry = attention && !paused;
                let label = if retry { "Retry" } else if paused { "Resume" } else { "Pause" };
                let id = if retry { "button:cloudRetry" } else { "button:cloudPauseResume" };
                let response = ui.add_enabled(app.session.library.is_some() && (!(retry || paused) || data["canResume"] == true), egui::Button::new(crate::i18n::tr(label)).frame(true).stroke(egui::Stroke::new(1., t.button_border)).min_size(egui::vec2(64., 30.)));
                register(ui.ctx(), id, response.rect);
                if response.clicked() && let Err(error) = app.run(if retry || paused { "cloud.resume" } else { "cloud.pause" }, json!({})) { app.toast(ui.ctx(), error); }
            }).response.on_hover_text(crate::i18n::tr("Uploads and library publication pause on this device. Photos remain available for editing."));
            if paused && data["canResume"] != true { ui.label(crate::i18n::tr("Stopping current transfers…")); }
        });
    });
    register(ui.ctx(), "popup:cloud", popup.response.rect);
}

fn attention_details(app: &mut LightcraftApp, ui: &mut egui::Ui, data: &Value) {
    let start = ui.cursor().top();
    let t = Tokens::get(ui.ctx());
    let summary = &data["summary"];
    let paused = data["paused"] == true;
    let blocked = summary["blockedImports"].as_array();
    if let Some(reason) = data["publicationBlocked"].as_str() {
        let changes = summary["pendingChanges"].as_u64().unwrap_or(0);
        let waiting = if changes == 1 {
            crate::i18n::tr("Your change waits here.").to_owned()
        } else if changes > 0 {
            crate::i18n::tr_format!("Your {n} changes wait here.", n = changes)
        } else {
            String::new()
        };
        let detail = format!("{reason} {waiting}");
        attention_row(app, ui, "Publishing paused", &detail, None);
    }
    let fields = data["health"]["conflicts"]["fields"].as_u64().unwrap_or_else(|| data["conflicts"].as_array().map_or(0, |v| v.len() as u64));
    let structural =
        data["health"]["conflicts"]["structural"].as_u64().unwrap_or_else(|| data["structuralConflicts"].as_array().map_or(0, |v| v.len() as u64));
    if fields > 0 || structural > 0 {
        let title = if fields == 1 && structural == 0 {
            crate::i18n::tr("1 conflicting edit").to_owned()
        } else {
            count("Conflicting edits", fields.saturating_add(structural))
        };
        let detail = data["conflicts"]
            .as_array()
            .and_then(|items| items.first())
            .map(|item| {
                format!(
                    "{} · {}",
                    item["name"].as_str().unwrap_or("Library item"),
                    crate::cloud_conflicts::field_label(item["field"].as_str().unwrap_or_default())
                )
            })
            .unwrap_or_else(|| crate::i18n::tr("Album or stack choices need attention").to_owned());
        attention_row(app, ui, &title, &detail, Some(("button:cloudChoose", "Choose")));
    }
    let health = &data["health"]["sync"];
    let sync_error = health["lastError"]["message"].as_str();
    if sync_error.is_some() || health["pollingHalted"] == true {
        let title = if health["lastError"]["kind"] == "connectivity" { "Can't reach storage" } else { "Synchronization needs attention" };
        let detail = if let Some(retry) = retry_text(health) {
            format!(
                "{retry} ({} {} {})",
                health["retry"]["attempt"].as_u64().unwrap_or(0),
                crate::i18n::tr("of"),
                health["retry"]["maxAttempts"].as_u64().unwrap_or(0)
            )
        } else if health["retry"]["exhausted"] == true {
            crate::i18n::tr("Automatic retry limit reached.").to_owned()
        } else {
            health["lastError"]["action"].as_str().or(sync_error).unwrap_or(crate::i18n::tr("Automatic checking stopped after an error.")).to_owned()
        };
        attention_row(app, ui, title, &detail, Some(("button:cloudErrorDetails", "Details")));
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
        let all = summary["blockedImportCount"].as_u64().unwrap_or(blocked.len() as u64);
        if all > 3 {
            ui.label(format!("{} {}", count("More blocked imports", all.saturating_sub(3)), crate::i18n::tr("Select a photo for details.")));
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

    if let Some(error) = data["error"].as_str().filter(|error| {
        sync_error != Some(*error)
            && !blocked.is_some_and(|items| items.iter().any(|item| item["reason"].as_str() == Some(error)))
            && !data["health"]["imports"]["failed"].as_array().is_some_and(|items| items.iter().any(|item| item["reason"].as_str() == Some(error)))
    }) {
        attention_row(app, ui, "Cloud storage needs attention", error, Some(("button:cloudStorageDetails", "Details")));
    }
    if data["health"]["offline"]["persistentCache"]["temporaryBlobs"].as_u64().unwrap_or(0) > 0 {
        attention_row(
            app,
            ui,
            "Some downloads are available for this session only.",
            crate::i18n::tr("These downloads are not saved for offline use."),
            Some(("button:cloudOfflineDetails", "Details")),
        );
    }
    if let Some(errors) = data["health"]["offline"]["offlineErrors"].as_object() {
        for error in errors.values().filter_map(Value::as_str).take(3) {
            ui.colored_label(t.caution, error);
        }
    }
    if data["health"]["receipts"]["pending"].as_u64().unwrap_or(0) > 0 {
        ui.label(crate::i18n::tr("Local upload records waiting"));
    }
    if app.session.unsaved().is_some() {
        attention_row(app, ui, "Local changes could not be saved", crate::i18n::tr("LightCraft is retrying."), None);
    }
    if data["capacityWarnings"].as_array().is_some_and(|warnings| !warnings.is_empty()) {
        capacity(app, ui, data, false);
    }
    if ui.cursor().top() <= start {
        attention_row(
            app,
            ui,
            "Cloud storage needs attention",
            crate::i18n::tr("Open Cloud Synchronization for recovery details."),
            Some(("button:cloudHealthDetails", "Details")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, headless::Headless};
    use std::time::Duration;
    const T: Duration = Duration::from_secs(30);
    fn overview() -> Value {
        json!({"summary":{"cloudPhotos":3,"albums":1,"originalBytes":1_200_000_000},"syncEnabled":true,"canResume":true,
            "health":{"sync":{"state":"healthy","lastSuccessfulRemoteScan":{"atUnixMs":1000},"secondsSinceLastRemoteScan":120}}})
    }
    fn open(data: Value, size: [f32; 2]) -> Headless {
        let mut session = lightcraft_engine::Session::new();
        session
            .open_library_in(
                lightcraft_engine::library::LibraryStores {
                    dir: "popup-redesign-test".into(),
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
            cloud_action: Some(Box::new(move |_, id, _| match id {
                "cloud.overview" => Some(Ok(data.clone())),
                "cloud.connections" => Some(Ok(json!({"connections":[]}))),
                "cloud.syncStatus" => {
                    let mut status = data.clone();
                    status["health"] = data["health"]["sync"].clone();
                    Some(Ok(status))
                }
                _ => None,
            })),
            write: Some(Box::new(|path, bytes| std::fs::write(path, bytes).map_err(|e| e.to_string()))),
            png: Some(Box::new(|image| {
                lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(image), &lightcraft_codecs::EncodeMeta::default()).unwrap()
            })),
            ..Default::default()
        };
        let mut h = Headless::new(LightcraftApp::new(session, services), size, 1.);
        for _ in 0..4 {
            h.step();
        }
        click(&mut h, "icon:cloud");
        h
    }
    fn click(h: &mut Headless, id: &str) {
        let response = h.request("ui.clickWidget", json!({"id":id}), T);
        assert_eq!(response["ok"], true, "{id}: {response}");
        for _ in 0..4 {
            h.step();
        }
    }
    fn has(h: &Headless, id: &str) -> bool {
        h.app.widgets.iter().any(|(key, _)| key == id)
    }
    fn text(h: &Headless, expected: &str) {
        assert!(h.painted_text().iter().any(|text| text.contains(expected)), "missing {expected}: {:?}", h.painted_text());
    }
    fn capture(h: &mut Headless, name: &str) {
        if let Ok(dir) = std::env::var("LIGHTCRAFT_POPUP_SNAPSHOTS") {
            std::fs::create_dir_all(&dir).unwrap();
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            let result = h.request("ui.screenshot", json!({"path":path.to_string_lossy()}), T);
            assert_eq!(result["ok"], true, "{result}");
        }
    }
    #[test]
    fn cloud_popup_redesign_idle_has_summary_navigation_and_one_footer_action() {
        let mut h = open(overview(), [1200., 900.]);
        for expected in
            ["CLOUD LIBRARY", "3 photos in 1 album", "Originals in the cloud", "1.2 GB", "Last checked", "2 min ago", "Synced and backed up", "Pause"]
        {
            text(&h, expected);
        }
        for id in ["button:cloudSetup", "button:cloudStorage", "button:cloudImport", "button:cloudPauseResume"] {
            assert!(has(&h, id), "{id}");
        }
        assert!(!has(&h, "button:cloudShowWaiting"));
        assert!(!has(&h, "button:cloudRetry"));
        assert!(!h.painted_text().iter().any(|text| text == "UPLOADING" || text == "NEEDS ATTENTION"));
        capture(&mut h, "up-to-date");
        click(&mut h, "button:cloudStorage");
        assert!(h.app.ui.dialog.is_some());
        assert!(!has(&h, "popup:cloud"), "navigation closes the popup");
    }
    #[test]
    fn cloud_popup_redesign_uploads_show_live_progress_and_waiting_filter() {
        let mut data = overview();
        data["waitingFiles"] = json!(8);
        data["uploadingFiles"] = json!(4);
        data["doneBytes"] = json!(45_000_000);
        data["totalBytes"] = json!(120_000_000);
        data["summary"]["pendingChanges"] = json!(2);
        let mut h = open(data, [1200., 900.]);
        capture(&mut h, "uploading");
        for expected in ["UPLOADING", "4 uploading", "8 waiting", "45.0 MB of 120.0 MB", "Changes to publish", "Syncing library…"] {
            text(&h, expected);
        }
        assert!(!has(&h, "button:cloudImport"));

        click(&mut h, "button:cloudShowWaiting");
        assert_eq!(h.app.session.filter.sync_status, Some(lightcraft_catalog::sync_status::SyncStatus::Pending));
        assert!(!has(&h, "popup:cloud"));
    }
    #[test]
    fn cloud_popup_redesign_attention_groups_causes_and_routes_choose_and_details() {
        let mut data = overview();
        data["summary"]["pendingChanges"] = json!(2);
        data["publicationBlocked"] = json!("Another device is upgrading this library.");
        data["conflicts"] = json!([{"entity":"photo:a","name":"beach.jpg","field":"rating"}]);
        data["health"]["sync"]["state"] = json!("retrying");
        data["health"]["sync"]["lastError"] = json!({"kind":"connectivity","message":"Storage unavailable"});
        data["health"]["sync"]["retry"] = json!({"secondsUntilRetry":8,"attempt":2,"maxAttempts":5});
        for id in ["button:cloudChoose", "button:cloudErrorDetails"] {
            let mut h = open(data.clone(), [1200., 900.]);
            capture(&mut h, "needs-attention");
            for expected in [
                "NEEDS ATTENTION",
                "Publishing paused",
                "Your 2 changes wait here.",
                "1 conflicting edit",
                "beach.jpg · Rating",
                "Can't reach storage",
                "Retrying in 8 s (2 of 5)",
                "Sync needs attention",
            ] {
                text(&h, expected);
            }
            assert!(has(&h, "button:cloudRetry"));
            assert!(!has(&h, "button:cloudShowWaiting"));
            assert!(!has(&h, "button:cloudImport"));

            click(&mut h, id);
            assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudSync { .. })));
            assert!(!has(&h, "popup:cloud"));
        }
    }
    #[test]
    fn cloud_popup_redesign_paused_and_small_window_keep_footer_reachable() {
        let mut data = overview();
        data["paused"] = json!(true);
        data["waitingFiles"] = json!(12);
        let mut h = open(data, [900., 600.]);
        capture(&mut h, "paused-small-window");
        text(&h, "UPLOADS PAUSED");
        text(&h, "Syncing paused");
        text(&h, "Resume");
        let rect = h.app.widgets.iter().find(|(id, _)| id == "button:cloudPauseResume").unwrap().1;
        assert!(rect.bottom() <= 600., "footer outside window: {rect:?}");
    }
}
