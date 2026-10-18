//! Choosing between competing cloud edits and recovering deleted cloud photos. Both read the
//! `cloud.syncStatus` view, whose lists the engine bounds and caches per catalog revision.
use crate::{LightcraftApp, theme::Tokens, widgets::register};
use serde_json::{Value, json};
pub(crate) mod choice;
mod pages;
#[cfg(test)]
mod tests;

/// A synchronized field's name as people know it.
pub(crate) fn field_label(field: &str) -> &str {
    crate::i18n::tr(match field.strip_prefix("meta.").unwrap_or(field) {
        "rating" => "Rating",
        "flag" => "Flag",
        "label" => "Color label",
        "develop" => "Develop settings",
        "title" => "Title",
        "caption" => "Caption",
        "keywords" => "Keywords",
        "file_name" => "File name",
        "name" => "Name",
        "deleted" => "Deleted",
        _ => return field,
    })
}

/// One retained value: a short preview, or a word for values too large to show.
fn choice_text(field: &str, choice: &Value) -> String {
    match choice["kind"].as_str() {
        Some("none") => crate::i18n::tr("None").to_owned(),
        Some("settings") if field == "develop" => crate::i18n::tr("Edit settings").to_owned(),
        Some("settings") => crate::i18n::tr("Settings").to_owned(),
        _ => {
            let preview = choice["preview"].as_str().unwrap_or_default();
            match (field, preview.parse::<u8>()) {
                ("rating", Ok(0)) => crate::i18n::tr("No rating").to_owned(),
                ("rating", Ok(n)) if n <= 5 => "★".repeat(usize::from(n)),
                _ if preview.is_empty() => crate::i18n::tr("(empty)").to_owned(),
                _ => preview.to_owned(),
            }
        }
    }
}

/// The retained values of one conflicting field, each with a button that keeps it. The value
/// shown now is marked; keeping it still records the choice so other devices stop showing the
/// conflict.
pub(crate) fn choices(app: &mut LightcraftApp, ui: &mut egui::Ui, conflict: &Value) {
    choices_at(app, ui, conflict, None);
}

/// Info may remain visible behind the recovery dialog. Its text fields and page
/// state need a separate identity so the dialog cannot consume its input.
pub(crate) fn choices_in_info(app: &mut LightcraftApp, ui: &mut egui::Ui, conflict: &Value) {
    choices_at(app, ui, conflict, Some("info"));
}

fn choices_at(app: &mut LightcraftApp, ui: &mut egui::Ui, conflict: &Value, surface: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    let (Some(entity), Some(field)) = (conflict["entity"].as_str(), conflict["field"].as_str()) else { return };
    for choice in &pages::rows_in(app, ui, "choices", Some((entity, field)), surface) {
        let Some(revision) = choice["revision"].as_str() else { continue };
        ui.horizontal(|ui| {
            let origin = if choice["thisDevice"] == true { "This device" } else { "Another device" };
            let mut text = format!("{} · {}", choice_text(field, choice), crate::i18n::tr(origin));
            if choice["current"] == true {
                text = format!("{text} · {}", crate::i18n::tr("shown now"));
            }
            ui.label(egui::RichText::new(text).color(if choice["current"] == true { t.text } else { t.text_dim }));
            let button = ui.small_button(crate::i18n::tr("Keep"));
            let prefix = surface.map_or_else(String::new, |surface| format!("{surface}:"));
            register(ui.ctx(), format!("button:{prefix}cloudKeepRevision:{entity}:{field}:{revision}"), button.rect);
            if button.clicked()
                && let Err(error) = app.run("cloud.resolveConflict", json!({"entity":entity,"field":field,"revision":revision}))
            {
                app.toast(ui.ctx(), error);
            }
        });
        ui.horizontal(|ui| {
            let prefix = surface.map_or_else(String::new, |surface| format!("{surface}:"));
            let writer = revision.rsplit_once('-');
            if let Some((device, sequence)) = writer {
                let short: String = device.chars().take(12).collect();
                ui.small(format!("{short} · {sequence}"));
            }
            for (mode, label) in [("details", "Full value"), ("preview", "Preview"), ("compare", "Compare")] {
                if mode != "details" && field != "develop" {
                    continue;
                }
                let response = ui.small_button(crate::i18n::tr(label));
                register(ui.ctx(), format!("button:{prefix}cloudChoice:{mode}:{entity}:{field}:{revision}"), response.rect);
                if response.clicked()
                    && let Err(error) = app.run("dialog.cloudChoice", json!({"entity":entity,"field":field,"revision":revision,"mode":mode}))
                {
                    app.toast(ui.ctx(), error);
                }
            }
        });
        ui.add_space(4.);
    }
}

/// Every conflicting field in the library (Cloud Synchronization).
pub(crate) fn list(app: &mut LightcraftApp, ui: &mut egui::Ui, status: &Value) {
    let Some(_conflicts) = status["conflicts"].as_array().filter(|conflicts| !conflicts.is_empty()) else { return };
    let t = Tokens::get(ui.ctx());
    ui.separator();
    ui.strong(crate::i18n::tr("Conflicting edits"));
    ui.label(crate::i18n::tr(
        "Two devices changed the same thing before seeing each other's change. LightCraft shows one and keeps the others; choose which to keep.",
    ));
    for conflict in &pages::rows(app, ui, "conflicts", None) {
        let kind = conflict["entity"].as_str().and_then(|entity| entity.split_once(':')).map(|(kind, _)| kind);
        let fallback = match kind {
            Some("photo") => crate::i18n::tr("Photo"),
            Some("album") => crate::i18n::tr("Album"),
            _ => crate::i18n::tr("Library item"),
        };
        let name = conflict["name"].as_str().unwrap_or(fallback);
        ui.label(egui::RichText::new(format!("{name} · {}", field_label(conflict["field"].as_str().unwrap_or_default()))).strong());
        if conflict["field"] == "develop" {
            ui.label(egui::RichText::new(crate::i18n::tr("The other edit is also kept in the photo's Versions.")).small().color(t.text_dim));
        }
        choices(app, ui, conflict);
    }
}

pub(crate) fn structure(app: &mut LightcraftApp, ui: &mut egui::Ui, status: &Value) {
    if status["structuralConflicts"].as_array().is_none_or(Vec::is_empty) {
        return;
    }
    ui.separator();
    ui.strong(crate::i18n::tr("Conflicting folder or stack moves"));
    ui.label(crate::i18n::tr("Move the affected folder or regroup a stack to resolve. The original choices remain retained."));
    for conflict in pages::rows(app, ui, "structural", None) {
        if conflict["field"] == "parent" {
            ui.strong(conflict["name"].as_str().unwrap_or_else(|| crate::i18n::tr("Folder")));
            for (label, key) in [("Requested parent", "proposed"), ("Currently shown under", "projected")] {
                let parent = conflict[key].as_str().and_then(|global| {
                    app.session
                        .catalog
                        .sync_state()
                        .and_then(|state| state.winning(&format!("album:{global}"), "name"))
                        .and_then(|(_, name)| name.as_str())
                });
                let fallback = if conflict[key].is_null() { crate::i18n::tr("Library root") } else { crate::i18n::tr("Unavailable folder") };
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr(label));
                    ui.label(parent.unwrap_or(fallback));
                });
            }
        } else {
            ui.label(crate::i18n::tr("Stack membership"));
        }
        if let Some(entity) = conflict["entity"].as_str() {
            let response = ui.button(crate::i18n::tr("Keep current structure"));
            register(ui.ctx(), format!("button:cloudAcceptStructure:{entity}"), response.rect);
            if response.clicked()
                && let Err(error) = app.run("cloud.acceptStructure", json!({"entity":entity}))
            {
                app.toast(ui.ctx(), error);
            }
        }
    }
}

/// Deleted cloud photos that can still be recovered with their retained edits.
pub(crate) fn deleted(app: &mut LightcraftApp, ui: &mut egui::Ui, status: &Value) {
    let Some(photos) = status["deletedPhotos"].as_array().filter(|photos| !photos.is_empty()) else { return };
    let total = status["deletedPhotoCount"].as_u64().unwrap_or(photos.len() as u64);
    ui.separator();
    ui.label(crate::i18n::tr("Recover deleted cloud photos and their retained edits:"));
    ui.label(egui::RichText::new(crate::panels::cloud::count("Deleted cloud photos", total)).small());
    let rows = pages::rows(app, ui, "deleted", None);
    egui::ScrollArea::vertical().id_salt("cloudDeletedPhotos").max_height(180.).show(ui, |ui| {
        for photo in rows {
            let Some(entity) = photo["entity"].as_str() else { continue };
            ui.horizontal(|ui| {
                ui.label(photo["name"].as_str().unwrap_or_else(|| crate::i18n::tr("Deleted cloud photo")));
                let response = ui.button(crate::i18n::tr("Recover"));
                register(ui.ctx(), format!("button:cloudRecover:{entity}"), response.rect);
                if response.clicked()
                    && let Err(error) = app.run("cloud.restoreDeleted", json!({"entity":entity}))
                {
                    app.toast(ui.ctx(), error);
                }
            });
        }
    });
}

/// UI fixtures supply complete retained rows, while engine tests independently
/// verify real revision/query-bound cursors and omitted values.
#[cfg(test)]
pub(crate) fn fixture_status(data: &Value, params: &Value) -> Value {
    let Some(section) = params["section"].as_str() else { return data.clone() };
    let source = match section {
        "conflicts" => &data["conflicts"],
        "deleted" => &data["deletedPhotos"],
        "structural" => &data["structuralConflicts"],
        "choices" => data["conflicts"]
            .as_array()
            .and_then(|items| items.iter().find(|c| c["entity"] == params["entity"] && c["field"] == params["field"]))
            .map(|conflict| &conflict["choices"])
            .unwrap_or(&Value::Null),
        _ => &Value::Null,
    };
    let search = params["search"].as_str().unwrap_or_default().trim().to_lowercase();
    let rows: Vec<_> = source.as_array().into_iter().flatten().filter(|value| value.to_string().to_lowercase().contains(&search)).cloned().collect();
    let offset = params["cursor"]
        .as_str()
        .and_then(|cursor| cursor.strip_prefix("fixture-"))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0)
        .min(rows.len());
    let limit = match section {
        "choices" => 8,
        "deleted" => 50,
        _ => 16,
    };
    let items: Vec<_> = rows.iter().skip(offset).take(limit).cloned().collect();
    let next = offset.saturating_add(items.len());
    json!({"page":{"items":items,"total":rows.len(),"offset":offset,"limit":limit,
        "nextCursor":(next<rows.len()).then(||format!("fixture-{next}")),
        "previousCursor":(offset>0).then(||format!("fixture-{}",offset.saturating_sub(limit)))}})
}
