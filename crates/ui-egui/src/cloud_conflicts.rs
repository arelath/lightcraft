//! Choosing between competing cloud edits and recovering deleted cloud photos. Both read the
//! `cloud.syncStatus` view, whose lists the engine bounds and caches per catalog revision.
use crate::{LightcraftApp, theme::Tokens, widgets::register};
use serde_json::{Value, json};

/// Conflicts listed in Cloud Synchronization; the rest are counted.
const LISTED: usize = 16;

/// A synchronized field's name as people know it.
pub(crate) fn field_label(field: &str) -> &str {
    crate::i18n::tr(match field {
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
    let t = Tokens::get(ui.ctx());
    let (Some(entity), Some(field)) = (conflict["entity"].as_str(), conflict["field"].as_str()) else { return };
    for choice in conflict["choices"].as_array().into_iter().flatten() {
        let Some(revision) = choice["revision"].as_str() else { continue };
        ui.horizontal(|ui| {
            let origin = if choice["thisDevice"] == true { "This device" } else { "Another device" };
            let mut text = format!("{} · {}", choice_text(field, choice), crate::i18n::tr(origin));
            if choice["current"] == true {
                text = format!("{text} · {}", crate::i18n::tr("shown now"));
            }
            ui.label(egui::RichText::new(text).color(if choice["current"] == true { t.text } else { t.text_dim }));
            let button = ui.small_button(crate::i18n::tr("Keep"));
            register(ui.ctx(), format!("button:cloudKeepRevision:{entity}:{field}:{revision}"), button.rect);
            if button.clicked()
                && let Err(error) = app.run("cloud.resolveConflict", json!({"entity":entity,"field":field,"revision":revision}))
            {
                app.toast(ui.ctx(), error);
            }
        });
    }
    if let Some(more) = conflict["moreChoices"].as_u64().filter(|n| *n > 0) {
        ui.label(egui::RichText::new(crate::panels::cloud::count("More retained values", more)).small().color(t.text_dim));
    }
}

/// Every conflicting field in the library (Cloud Synchronization).
pub(crate) fn list(app: &mut LightcraftApp, ui: &mut egui::Ui, status: &Value) {
    let Some(conflicts) = status["conflicts"].as_array().filter(|conflicts| !conflicts.is_empty()) else { return };
    let t = Tokens::get(ui.ctx());
    ui.separator();
    ui.strong(crate::i18n::tr("Conflicting edits"));
    ui.label(crate::i18n::tr(
        "Two devices changed the same thing before seeing each other's change. LightCraft shows one and keeps the others; choose which to keep.",
    ));
    for conflict in conflicts.iter().take(LISTED) {
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
    if conflicts.len() > LISTED {
        ui.label(crate::panels::cloud::count("More conflicts", conflicts.len() - LISTED));
    }
}

/// Deleted cloud photos that can still be recovered with their retained edits.
pub(crate) fn deleted(app: &mut LightcraftApp, ui: &mut egui::Ui, status: &Value) {
    let Some(photos) = status["deletedPhotos"].as_array().filter(|photos| !photos.is_empty()) else { return };
    let total = status["deletedPhotoCount"].as_u64().unwrap_or(photos.len() as u64);
    ui.separator();
    ui.label(crate::i18n::tr("Recover deleted cloud photos and their retained edits:"));
    ui.label(egui::RichText::new(crate::panels::cloud::count("Deleted cloud photos", total)).small());
    egui::ScrollArea::vertical().id_salt("cloudDeletedPhotos").max_height(180.).show(ui, |ui| {
        for photo in photos {
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
    if total > photos.len() as u64 {
        ui.label(
            egui::RichText::new(crate::panels::cloud::count("Not listed here", total - photos.len() as u64))
                .small()
                .color(Tokens::get(ui.ctx()).text_dim),
        );
    }
}
