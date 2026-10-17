//! A local, bounded recovery-page cache. Changes to the library, catalog or query
//! invalidate the cursor; ordinary repaint never repeats the query.
use crate::{LightcraftApp, theme::Tokens, widgets::register};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Browse {
    owner: Option<(u64, u64)>,
    search: String,
    cursor: Option<String>,
    request: Option<Value>,
    page: Option<Value>,
    error: Option<String>,
}

pub(super) fn rows(app: &mut LightcraftApp, ui: &mut egui::Ui, section: &str, field: Option<(&str, &str)>) -> Vec<Value> {
    rows_in(app, ui, section, field, None)
}

pub(super) fn rows_in(app: &mut LightcraftApp, ui: &mut egui::Ui, section: &str, field: Option<(&str, &str)>, surface: Option<&str>) -> Vec<Value> {
    let key = field.map_or_else(|| section.to_owned(), |(entity, field)| format!("{section}:{entity}:{field}"));
    let key = surface.map_or_else(|| key.clone(), |surface| format!("{surface}:{key}"));
    let id = egui::Id::new(("cloud-recovery-page", &key));
    let mut browse = ui.data_mut(|data| data.get_temp::<Browse>(id)).unwrap_or_default();
    let owner = (app.session.library_generation(), app.session.catalog.revision);
    if browse.owner.is_some_and(|old| old.0 != owner.0) {
        browse = Browse::default();
    }
    if browse.owner != Some(owner) {
        browse.cursor = None;
        browse.request = None;
        browse.page = None;
        browse.owner = Some(owner);
    }
    let hint = match section {
        "choices" => "Search values or devices",
        "deleted" => "Search deleted photos",
        "structural" => "Search folder or stack conflicts",
        _ => "Search photo names or conflicting fields",
    };
    let response = crate::text_field::TextField::singleline(&format!("field:cloudPageSearch:{key}"), &mut browse.search)
        .hint(crate::i18n::tr(hint))
        .width(f32::INFINITY)
        .show(ui);
    if response.response.changed() {
        browse.cursor = None;
        browse.request = None;
    }
    let mut request = json!({"section":section,"search":browse.search,"cursor":browse.cursor});
    if let Some((entity, field)) = field {
        request["entity"] = json!(entity);
        request["field"] = json!(field);
    }
    if browse.request.as_ref() != Some(&request) {
        match app.run("cloud.syncStatus", request.clone()) {
            Ok(value) if value["page"]["items"].as_array().is_some_and(|items| items.len() <= 200) => {
                browse.page = Some(value["page"].clone());
                browse.error = None;
            }
            Ok(_) => {
                browse.page = None;
                browse.error = Some(crate::i18n::tr("Recovery paging is unavailable. Update LightCraft and reopen this view.").into());
            }
            Err(error) => {
                browse.page = None;
                browse.error = Some(error);
            }
        }
        browse.request = Some(request);
    }
    let t = Tokens::get(ui.ctx());
    if let Some(error) = &browse.error {
        ui.colored_label(t.caution, error);
        let response = ui.button(crate::i18n::tr("Refresh list"));
        register(ui.ctx(), format!("button:cloudPageRefresh:{key}"), response.rect);
        if response.clicked() {
            browse.cursor = None;
            browse.request = None;
        }
    }
    let mut rows = Vec::new();
    if let Some(page) = &browse.page {
        rows = page["items"].as_array().cloned().unwrap_or_default();
        let total = page["total"].as_u64().unwrap_or(0);
        let offset = page["offset"].as_u64().unwrap_or(0);
        let previous = page["previousCursor"].as_str().map(str::to_owned);
        let next = page["nextCursor"].as_str().map(str::to_owned);
        ui.horizontal(|ui| {
            let response = ui.add_enabled(previous.is_some(), egui::Button::new(crate::i18n::tr("Previous")));
            register(ui.ctx(), format!("button:cloudPagePrev:{key}"), response.rect);
            if response.clicked() {
                browse.cursor = previous;
                browse.request = None;
            }
            if rows.is_empty() {
                ui.label(crate::i18n::tr("No matching items"));
            } else {
                ui.label(crate::i18n::tr_format!(
                    "{first}–{last} of {total}",
                    first = offset.saturating_add(1),
                    last = offset.saturating_add(rows.len() as u64),
                    total = total
                ));
            }
            let response = ui.add_enabled(next.is_some(), egui::Button::new(crate::i18n::tr("Next")));
            register(ui.ctx(), format!("button:cloudPageNext:{key}"), response.rect);
            if response.clicked() {
                browse.cursor = next;
                browse.request = None;
            }
        });
    }
    ui.data_mut(|data| data.insert_temp(id, browse));
    rows
}
