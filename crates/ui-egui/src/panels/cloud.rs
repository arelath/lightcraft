//! Cloud library popup. Counts describe this catalog, never invented provider capacity.
use crate::{
    LightcraftApp,
    icons::{self, Icon},
    theme::Tokens,
    widgets::register,
};
use serde_json::{Value, json};

pub(crate) fn needs_attention(data: &Value) -> bool {
    if data["syncEnabled"] == false && !data["erasure"].is_object() {
        return false;
    }
    let summary = &data["summary"];
    let health = &data["health"];
    data["erasure"].is_object()
        || summary["issues"].as_u64().unwrap_or(0) > 0
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
            && data["enabled"] != false
            && data["syncEnabled"] != false
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
    ui.add_space(4.);
}
fn section(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin::same(14)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 4.;
        add(ui);
    });
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
            ui.label(egui::RichText::new(value).color(t.text));
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
        egui::Button::new(egui::RichText::new(crate::i18n::tr(label)).font(t.semibold(12.)).color(t.text))
            .frame(false)
            .min_size(egui::vec2(ui.available_width(), 22.))
            .right_text(""),
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
fn attention_row(app: &mut LightcraftApp, ui: &mut egui::Ui, icon: Icon, title: &str, detail: &str, link: Option<(&str, &str)>) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(16., 18.), egui::Sense::hover());
        icons::paint(ui.painter(), rect, icon, t.caution);
        register(ui.ctx(), format!("indicator:cloud{icon:?}"), rect);
        ui.vertical(|ui| {
            ui.horizontal_top(|ui| {
                let link_width = if link.is_some() { 44. + ui.spacing().item_spacing.x } else { 0. };
                let title_width = (ui.available_width() - link_width).max(50.);
                ui.allocate_ui_with_layout(egui::vec2(title_width, 0.), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(title_width);
                    ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(title)).font(t.semibold(13.)).color(t.text)).wrap());
                });
                if let Some((id, label)) = link {
                    let response = ui.link(crate::i18n::tr(label));
                    register(ui.ctx(), id, response.rect);
                    if response.clicked() {
                        let command = if id == "button:cloudChoose" {
                            "dialog.cloudConflicts"
                        } else if id == "button:cloudStorageDetails" || id == "button:cloudErrorDetails" {
                            "dialog.cloudError"
                        } else {
                            "dialog.cloudMaintenance"
                        };
                        let params = if command == "dialog.cloudError" { json!({"storage":id == "button:cloudStorageDetails"}) } else { json!({}) };
                        invoke(app, ui, command, params);
                    }
                }
            });
            if !detail.is_empty() {
                let width = (ui.available_width() - if link.is_some() { 44. } else { 0. }).max(50.);
                ui.allocate_ui_with_layout(egui::vec2(width, 0.), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(width);
                    ui.add(egui::Label::new(egui::RichText::new(detail).size(13.).color(t.text_dim)).wrap());
                });
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
    if !unsaved && data["erasure"].is_object() {
        return if data["erasure"]["complete"] == true { "Cloud library deleted" } else { "Deletion incomplete" };
    }
    if !unsaved && data["syncEnabled"] == false {
        return "Cloud sync is off";
    }
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
    let off = data["syncEnabled"] == false;
    let paused = data["paused"] == true;
    let controls = lightcraft_engine::cloud_status::Actions::from_overview(data);
    let waiting = data["waitingFiles"].as_u64().unwrap_or(0).saturating_add(data["preparingPhotos"].as_u64().unwrap_or(0));
    let active = data["uploadingFiles"].as_u64().unwrap_or(0);
    let changes = summary["pendingChanges"].as_u64().unwrap_or(0);
    let attention = needs_attention(data) || app.session.unsaved().is_some();
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    ui.set_width(214.);
    ui.style_mut().override_font_id = Some(t.font(12.));
    ui.spacing_mut().item_spacing.y = 0.;
    ui.spacing_mut().item_spacing.x = 6.;
    ui.spacing_mut().interact_size.y = 18.;
    let popup = ui.scope(|ui| {
        section(ui, |ui| {
            heading(ui, if off {"LOCAL LIBRARY"} else {"CLOUD LIBRARY"});
            let photos = if off {app.session.catalog.photos().filter(|p|!p.deleted&&!p.local).count() as u64} else {summary["cloudPhotos"].as_u64().unwrap_or(0)};
            let albums = summary["albums"].as_u64().unwrap_or(0);
            let photo_text = if photos == 1 { crate::i18n::tr_format!("{n} photo", n = photos) } else { crate::i18n::tr_format!("{n} photos", n = photos) };
            let album_text = if albums == 1 { crate::i18n::tr_format!("{n} album", n = albums) } else { crate::i18n::tr_format!("{n} albums", n = albums) };
            ui.label(egui::RichText::new(crate::i18n::tr_format!("{photos} in {albums}", photos = photo_text, albums = album_text)).font(t.semibold(12.)).color(t.text));
            ui.add_space(4.);
            let storage=if data["erasure"]["complete"]==true {bytes(0)} else if data["erasure"].is_object() {crate::i18n::tr("Unknown").to_owned()} else {bytes(summary["cloudOriginalBytes"].as_u64().unwrap_or(0))};
            metric(ui, if off {"Last known cloud storage"} else {"Storage space used"}, &storage).on_hover_text(crate::i18n::tr(
                if off {"Cloud sync is off. This is the last known size of cloud originals, not a current cloud scan or local disk usage."} else if data["erasure"].is_object() {"Library deletion retains local copies. Review deletion to inspect any remaining cloud data."} else {"Storage used by cloud originals represented in this catalog; shared originals count once. Previews, sidecars, sync metadata, and other libraries are not included."},
            ));
            if !off && !attention && !busy(data) && let Some(age) = data["health"]["sync"]["secondsSinceLastRemoteScan"].as_u64() {
                metric(ui, "Last checked", &ago(age));
            } else if !attention && !busy(data) && data["syncEnabled"] == true {
                metric(ui, "Last checked", crate::i18n::tr("Not yet checked"));
            }
        });
        if attention {
            divider(ui);
            section(ui, |ui| { heading(ui, if data["erasure"]["complete"]==true {"LIBRARY DELETED"} else {"NEEDS ATTENTION"}); attention_details(app, ui, data); });
        }
        if !off && (waiting > 0 || active > 0 || data["preparingFiles"].as_u64().unwrap_or(0) > 0) {
            divider(ui);
            section(ui, |ui| {
                heading(ui, if paused { "UPLOADS PAUSED" } else { "UPLOADING" });
                let done = data["doneBytes"].as_u64().unwrap_or(0);
                let total = data["totalBytes"].as_u64().unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr_format!("{active} of {total} photos", active = active, total = active.saturating_add(waiting)))
                        .on_hover_text(crate::i18n::tr("Photos currently uploading out of all active and waiting photos."));
                    if total > 0 { ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(crate::i18n::tr_format!("{done} of {total}", done = bytes(done.min(total)).replace(".0 ", " "), total = bytes(total).replace(".0 ", " "))).color(t.text_dim));
                    }); }
                });
                if total > 0 {
                    ui.add(egui::ProgressBar::new((done as f64 / total as f64).clamp(0., 1.) as f32).desired_height(6.).fill(t.accent))
                        .on_hover_text(crate::i18n::tr("Progress of current transfers; queued data includes originals, previews and sidecars."));
                } else if !paused && active > 0 {
                    ui.horizontal(|ui| { ui.spinner(); ui.label(crate::i18n::tr("Preparing transfer…")); });
                }
                if let Some(n) = data["preparingPhotos"].as_u64().filter(|n| *n > 0) { ui.label(count("Preparing photos", n)); }
                if changes > 0 { metric(ui, "Changes to publish", &changes.to_string()); }
                if active == 0 && let Some(bytes_waiting) = summary["queuedBytes"].as_u64().filter(|n| *n > 0) { metric(ui, "Queued data", &bytes(bytes_waiting)); }
            });
        } else if changes > 0 && !attention && !off {
            divider(ui);
            section(ui, |ui| { metric(ui, "Changes to publish", &changes.to_string()); });
        }
        divider(ui);
        section(ui, |ui| {
            if !off && !attention && (busy(data) || paused) {
                navigation(app, ui, "button:cloudShowWaiting", "Show waiting photos", "library.filter", json!({"syncStatus":"pending"}));
            }
            if off {
                navigation(app, ui, "button:cloudSetup", "Turn on cloud sync…", "dialog.cloudSync", json!({}));
            } else {
                let connection = app.session.catalog.sync_state().map(|s| s.connection.clone());
                navigation(app, ui, "button:cloudStorage", "Storage connection…", "dialog.cloudStorage", json!({"id":connection}));
                let more = ui.menu_button(crate::i18n::tr("More…"), |ui| {
                    ui.set_width(214.);
                    navigation(app, ui, "button:cloudMaintenance", "Storage tools…", "dialog.cloudMaintenance", json!({}));
                    if app.session.catalog.sync_state().is_some_and(|state| state.protocol.is_head()) {
                        navigation(app, ui, "button:cloudSyncMigrate", "Migrate library…", "dialog.cloudMigrate", json!({}));
                    }
                    navigation(app, ui, "button:cloudDeleteOpen", "Delete entire cloud library…", "dialog.cloudDelete", json!({}));
                });
                register(ui.ctx(), "button:cloudMore", more.response.rect);
            }
            if !off && data["syncEnabled"] == true && !data["erasure"].is_object() {
                #[cfg(not(target_arch = "wasm32"))]
                navigation(app, ui, "button:cloudDisableOpen", "Turn off cloud sync…", "dialog.cloudDisable", json!({}));
                #[cfg(target_arch = "wasm32")]
                {
                    let r = ui.add_enabled(false, egui::Button::new(crate::i18n::tr("Turn off cloud sync…")))
                        .on_disabled_hover_text(crate::i18n::tr("Use desktop LightCraft to keep permanent local originals. Pause is available below."));
                    register(ui.ctx(), "button:cloudDisableOpen", r.rect);
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            if !off && !busy(data) && !attention {
                navigation(app, ui, "button:cloudImport", "Import to cloud storage…", "file.importCloud", json!({}));
            }
        });
        divider(ui);
        section(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(20., 24.), egui::Sense::hover());
                let status = footer_text(data, app.session.unsaved().is_some());
                let icon = match status {
                    "Sync needs attention" | "Cloud library deleted" | "Deletion incomplete" => Icon::Warning,
                    "Syncing paused" => Icon::Pause,
                    "Synced and backed up" => Icon::CheckCircle,
                    "Local library" | "Cloud sync is off" => Icon::Cloud,
                    _ => Icon::Sync,
                };
                icons::paint(ui.painter(), rect.shrink(2.), icon, if attention { t.caution } else { t.accent });
                register(ui.ctx(), format!("indicator:cloudFooter{icon:?}"), rect);
                let footer_action = controls.footer;
                let reserve = if footer_action.is_some_and(|a|matches!(a.kind,lightcraft_engine::cloud_status::ActionKind::SyncNow|lightcraft_engine::cloud_status::ActionKind::ContinueWithoutMigration)) {74.} else {60.};
                let status_width = (ui.available_width() - reserve).max(50.);
                ui.allocate_ui_with_layout(egui::vec2(status_width, 24.), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(status_width);
                    ui.add(egui::Label::new(crate::i18n::tr(status)).wrap());
                });
                if data["erasure"].is_object() {
                    let response=ui.button(crate::i18n::tr(if data["erasure"]["complete"]==true {"Details"} else {"Review"}));
                    register(ui.ctx(),"button:cloudDeleteReview",response.rect);
                    if response.clicked(){invoke(app,ui,"dialog.cloudDelete",json!({}));}
                    return;
                }
                if let Some(action) = footer_action {
                    use lightcraft_engine::cloud_status::ActionKind;
                    let id = match action.kind {ActionKind::Retry=>"button:cloudRetry",ActionKind::Details=>"button:cloudStatusDetails",ActionKind::Enable=>"button:cloudTurnOn",_=>"button:cloudPauseResume"};
                    let enabled = action.enabled && (action.kind == ActionKind::Enable || app.session.library.is_some());
                    let mut response = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(action.kind.compact_label())).frame(true).stroke(egui::Stroke::new(1., t.button_border)).min_size(egui::vec2(54., 24.)));
                    register(ui.ctx(), id, response.rect);
                    if let Some(reason)=action.reason {response=response.on_hover_text(crate::i18n::tr(reason)).on_disabled_hover_text(crate::i18n::tr(reason));}
                    if response.clicked() && let Err(error) = app.run(action.kind.command(), action.kind.params()) { app.toast(ui.ctx(), error); }
                }
            }).response.on_hover_text(crate::i18n::tr(controls.footer.map_or("Cloud library upgrade is running.",|a|a.kind.description())));
            if controls.footer.is_some_and(|a|!a.enabled&&matches!(a.kind,lightcraft_engine::cloud_status::ActionKind::Resume|lightcraft_engine::cloud_status::ActionKind::ContinueWithoutMigration)) { ui.label(crate::i18n::tr("Stopping current transfers…")); }
        });
    });
    register(ui.ctx(), "popup:cloud", popup.response.rect);
}

fn attention_details(app: &mut LightcraftApp, ui: &mut egui::Ui, data: &Value) {
    if data["erasure"].is_object() {
        attention_row(
            app,
            ui,
            Icon::Warning,
            if data["erasure"]["complete"] == true { "Cloud library deleted" } else { "Deletion incomplete" },
            crate::i18n::tr(if data["erasure"]["complete"] == true {
                "Cloud data was erased. Local copies remain on your devices."
            } else {
                "Some data may already be gone. Keep other devices stopped and review deletion to finish."
            }),
            None,
        );
        if app.session.unsaved().is_some() {
            attention_row(app, ui, Icon::Warning, "Local changes could not be saved", crate::i18n::tr("LightCraft is retrying."), None);
        }
        return;
    }
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
        attention_row(app, ui, Icon::Pause, "Publishing paused", &detail, None);
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
        attention_row(app, ui, Icon::Conflict, &title, &detail, Some(("button:cloudChoose", "Choose")));
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
            lightcraft_engine::cloud_status::Diagnostic::from_overview(data, false).map_or_else(
                || crate::i18n::tr("Automatic checking stopped after an error.").to_owned(),
                |diagnostic| crate::i18n::tr(diagnostic.short_cause()).to_owned(),
            )
        };
        attention_row(
            app,
            ui,
            if health["lastError"]["kind"] == "connectivity" { Icon::CloudOff } else { Icon::Warning },
            title,
            &detail,
            Some(("button:cloudErrorDetails", "Details")),
        );
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
        attention_row(app, ui, Icon::Warning, "Cloud storage needs attention", error, Some(("button:cloudStorageDetails", "Details")));
    }
    if data["health"]["offline"]["persistentCache"]["temporaryBlobs"].as_u64().unwrap_or(0) > 0 {
        attention_row(
            app,
            ui,
            Icon::Warning,
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
        attention_row(app, ui, Icon::Warning, "Local changes could not be saved", crate::i18n::tr("LightCraft is retrying."), None);
    }
    if data["capacityWarnings"].as_array().is_some_and(|warnings| !warnings.is_empty()) {
        capacity(app, ui, data, false);
    }
    if ui.cursor().top() <= start {
        attention_row(app, ui, Icon::Warning, "Cloud storage needs attention", "", Some(("button:cloudHealthDetails", "Details")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, headless::Headless};
    use std::time::Duration;
    const T: Duration = Duration::from_secs(30);
    fn overview() -> Value {
        json!({"summary":{"cloudPhotos":3,"albums":1,"originalBytes":1_200_000_000,"cloudOriginalBytes":1_200_000_000},"syncEnabled":true,"canResume":true,
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
            cloud_action: Some(Box::new(move |_, id, params| match id {
                "cloud.overview" => Some(Ok(data.clone())),
                "cloud.connections" => Some(Ok(json!({"connections":[]}))),
                "cloud.syncStatus" => {
                    let mut status = data.clone();
                    status["health"] = data["health"]["sync"].clone();
                    Some(Ok(crate::cloud_conflicts::fixture_status(&status, params)))
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
            ["CLOUD LIBRARY", "3 photos in 1 album", "Storage space used", "1.2 GB", "Last checked", "2 min ago", "Synced and backed up", "Pause"]
        {
            text(&h, expected);
        }
        for id in ["button:cloudStorage", "button:cloudImport", "button:cloudPauseResume"] {
            assert!(has(&h, id), "{id}");
        }
        assert!(!has(&h, "button:cloudShowWaiting"));
        assert!(!has(&h, "button:cloudSetup"), "no redundant synchronization dialog");
        assert!(!has(&h, "button:cloudRetry"));
        assert!(has(&h, "indicator:cloudFooterCheckCircle"));
        assert!(!h.painted_text().iter().any(|text| text == "UPLOADING" || text == "NEEDS ATTENTION"));
        capture(&mut h, "up-to-date");
        click(&mut h, "button:cloudStorage");
        assert!(h.app.ui.dialog.is_some());
        assert!(!has(&h, "popup:cloud"), "navigation closes the popup");
    }
    #[test]
    fn cloud_popup_keeps_guarded_library_deletion_in_more_menu() {
        let mut h = open(overview(), [1200., 900.]);
        click(&mut h, "button:cloudMore");
        assert!(has(&h, "button:cloudMaintenance"));
        assert!(has(&h, "button:cloudDeleteOpen"));
        capture(&mut h, "more-actions");
        click(&mut h, "button:cloudDeleteOpen");
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudDelete { .. })));
    }
    #[test]
    fn cloud_popup_shows_the_entire_preparing_batch_and_menu_opens_popup() {
        let mut data = overview();
        data["preparingPhotos"] = json!(20);
        data["preparingFiles"] = json!(1);
        let mut h = open(data, [1200., 900.]);
        text(&h, "0 of 20 photos");
        text(&h, "Preparing photos: 20");
        capture(&mut h, "preparing-twenty");
        h.app.run("dialog.cloudSync", json!({})).unwrap();
        for _ in 0..4 {
            h.step();
        }
        assert!(h.app.ui.dialog.is_none());
        assert!(has(&h, "popup:cloud"));
        assert!(!h.painted_text().iter().any(|text| text.contains("changed fields")));
    }
    #[test]
    fn cloud_popup_redesign_uploads_show_live_progress_and_waiting_filter() {
        let mut data = overview();
        data["summary"]["cloudPhotos"] = json!(15);
        data["summary"]["cloudOriginalBytes"] = json!(1_300_000_000);
        data["waitingFiles"] = json!(8);
        data["uploadingFiles"] = json!(4);
        data["doneBytes"] = json!(45_000_000);
        data["totalBytes"] = json!(120_000_000);
        data["summary"]["pendingChanges"] = json!(2);
        let mut h = open(data, [1200., 900.]);
        capture(&mut h, "uploading");
        for expected in ["UPLOADING", "4 of 12 photos", "45 MB of 120 MB", "Changes to publish", "Syncing library…"] {
            text(&h, expected);
        }
        assert!(!has(&h, "button:cloudImport"));
        assert!(has(&h, "indicator:cloudFooterSync"));
        assert!(!h.painted_text().iter().any(|text| text == "Last checked"));

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
            for icon in ["Pause", "Conflict", "CloudOff", "FooterWarning"] {
                assert!(has(&h, &format!("indicator:cloud{icon}")));
            }
            assert!(!h.painted_text().iter().any(|text| text == "Last checked"));
            assert!(!has(&h, "button:cloudShowWaiting"));
            assert!(!has(&h, "button:cloudImport"));

            click(&mut h, id);
            if id == "button:cloudChoose" {
                assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudConflicts)));
            } else {
                assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudError { .. })));
                text(&h, "Storage unavailable");
                text(&h, "Cloud synchronization");
                assert!(!has(&h, "button:cloudErrorConnection"));
            }
            assert!(!has(&h, "popup:cloud"));
        }
    }
    #[test]
    fn empty_library_details_show_original_error_and_only_relevant_repair_actions() {
        use lightcraft_engine::cloud_status::{CloudFailure, FailureKind, PollHealth};
        for (error, repair_widget) in [
            (CloudFailure::http("Read empty library", 403), Some("button:cloudErrorConnection")),
            (CloudFailure::integrity("Invalid checkpoint hash: exact expected digest"), None),
            (CloudFailure::new(FailureKind::Capacity, "Receipt capacity reached: exact bound"), Some("button:cloudErrorCapacity")),
            (CloudFailure::from(lightcraft_engine::cloud_control::Blocked::UpdateToRead("schema 9".into())), None),
            (CloudFailure::local("Cannot save catalog: disk full"), None),
        ] {
            let mut data = overview();
            data["summary"]["cloudPhotos"] = json!(0);
            let mut health = PollHealth::default();
            health.fail_typed(&error, Some(1000), Some(29));
            data["health"]["sync"] = health.view_at(true, false, false, false, true, Some(1000));
            let mut h = open(data, [1200., 900.]);
            click(&mut h, "button:cloudErrorDetails");
            text(&h, &error.message);
            text(&h, "Cloud synchronization");
            text(&h, "Activity task: 29");
            text(&h, "No automatic retry scheduled");
            for widget in ["button:cloudErrorConnection", "button:cloudErrorCapacity"] {
                assert_eq!(has(&h, widget), repair_widget == Some(widget), "{error}: {widget}");
            }
            if error.remedy == Some(lightcraft_engine::cloud_status::Recovery::Update) {
                assert!(!has(&h, "button:cloudErrorRetry"));
                text(&h, "Update LightCraft");
            }
            if let Some(widget) = repair_widget {
                click(&mut h, widget);
                if widget == "button:cloudErrorConnection" {
                    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudSetup { .. })));
                } else {
                    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudMaintenance)));
                }
            } else {
                let result = h.request("ui.dialog.confirm", json!({}), T);
                assert_eq!(result["ok"], true, "{result}");
                assert!(h.app.ui.dialog.is_none());
            }
        }
    }
    #[test]
    fn legacy_storage_error_details_do_not_guess_a_connection_problem() {
        let mut data = overview();
        data["error"] = json!("Authentication and disk keywords in an unclassified diagnostic");
        let mut h = open(data, [1200., 900.]);
        click(&mut h, "button:cloudStorageDetails");
        text(&h, "Authentication and disk keywords in an unclassified diagnostic");
        assert!(!has(&h, "button:cloudErrorConnection"));
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudError { .. })));
    }
    #[test]
    fn cloud_popup_deleted_library_shows_remaining_scope_without_resume_or_retry() {
        for complete in [false, true] {
            let mut data = overview();
            data["erasure"] = json!({"complete":complete});
            let mut h = open(data, [1200., 900.]);
            text(&h, if complete { "Cloud library deleted" } else { "Deletion incomplete" });
            text(&h, if complete { "0 B" } else { "Unknown" });
            assert!(has(&h, "button:cloudDeleteReview"));
            assert!(!has(&h, "button:cloudPauseResume"));
            assert!(!has(&h, "button:cloudRetry"));
            click(&mut h, "button:cloudDeleteReview");
            assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::CloudDelete { .. })));
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
