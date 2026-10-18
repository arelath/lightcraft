//! Read-only retained values and independently fitted revision renders.
use crate::{LightcraftApp, render::Slot, state::Dialog, theme::Tokens, widgets::register};
use lightcraft_catalog::{PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    library: String,
    library_generation: u64,
    entity: String,
    field: String,
    revision: String,
    name: Option<String>,
    value: Value,
    device: Option<String>,
    sequence: Option<u64>,
    photo_id: Option<u64>,
    deleted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Details,
    Preview,
    Compare,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    choice: Choice,
    pub mode: Mode,
    pub return_to_conflicts: bool,
    selected: Option<PhotoId>,
    background_view: crate::state::ViewMode,
    source: Option<Source>,
    develop_hash: Option<u64>,
    #[serde(skip)]
    settings: Option<DevelopSettings>,
    #[serde(skip)]
    text: String,
}
impl View {
    fn refresh_photo(&mut self, app: &LightcraftApp) {
        let photo = self.choice.photo_id.and_then(|id| app.session.catalog.photo(PhotoId(id)));
        self.selected = app.session.selection.active;
        self.background_view = app.ui.view;
        self.source = photo.map(|photo| photo.source.clone());
        self.develop_hash = photo.map(|photo| photo.develop.hash64());
    }
    fn can_preview(&self, app: &LightcraftApp) -> bool {
        self.choice.field == "develop"
            && !self.choice.deleted
            && self.settings.is_some()
            && self.choice.photo_id.is_some_and(|id| app.session.catalog.photo(PhotoId(id)).is_some_and(|photo| !photo.deleted))
    }
    fn valid(&self, app: &LightcraftApp) -> bool {
        let Some(state) = app.session.catalog.sync_state() else { return false };
        if self.choice.library != state.library
            || self.choice.library_generation != app.session.library_generation()
            || state
                .documents
                .get(&self.choice.entity)
                .and_then(|fields| fields.get(&self.choice.field))
                .and_then(|register| register.0.get(&self.choice.revision))
                != Some(&self.choice.value)
        {
            return false;
        }
        if self.mode == Mode::Details {
            return true;
        }
        self.background_view == app.ui.view
            && self.selected == app.session.selection.active
            && self.choice.photo_id.is_some_and(|id| {
                app.session.catalog.photo(PhotoId(id)).is_some_and(|photo| {
                    !photo.deleted && self.source.as_ref() == Some(&photo.source) && self.develop_hash == Some(photo.develop.hash64())
                })
            })
    }
    fn prepare(&mut self) {
        if self.choice.field == "develop" && self.settings.is_none() {
            self.settings = serde_json::from_value(self.choice.value.clone()).ok();
        }
        if self.text.is_empty() {
            self.text = match &self.choice.value {
                Value::String(text) => text.clone(),
                value => serde_json::to_string_pretty(value).unwrap_or_else(|error| format!("Cannot display this value: {error}")),
            };
        }
    }
}
pub(crate) fn open(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    let mode = match params.get("mode").and_then(Value::as_str).unwrap_or("details") {
        "details" => Mode::Details,
        "preview" => Mode::Preview,
        "compare" => Mode::Compare,
        _ => return Err("Choose details, preview or compare.".into()),
    };
    let return_to_conflicts = matches!(app.ui.dialog, Some(Dialog::CloudConflicts));
    let library = app.session.catalog.sync_state().map(|state| state.library.clone());
    let result = app.run(
        "cloud.syncStatus",
        json!({"section":"choice","entity":params["entity"],"field":params["field"],
        "revision":params["revision"],"library":library,"libraryGeneration":app.session.library_generation()}),
    )?;
    let choice: Choice = serde_json::from_value(result).map_err(|error| format!("Cannot inspect this retained value: {error}"))?;
    let mut view = View {
        choice,
        mode,
        return_to_conflicts,
        selected: None,
        background_view: app.ui.view,
        source: None,
        develop_hash: None,
        settings: None,
        text: String::new(),
    };
    view.prepare();
    view.refresh_photo(app);
    if mode != Mode::Details && !view.can_preview(app) {
        return Err("Restore the photo and choose a supported retained develop revision to preview it.".into());
    }
    app.renderer.forget_cloud_choices();
    app.ui.dialog = Some(Dialog::CloudChoice { view: Box::new(view) });
    Ok(Value::Null)
}
pub(crate) fn observe(app: &mut LightcraftApp) {
    if matches!(&app.ui.dialog,Some(Dialog::CloudChoice{view}) if !view.valid(app)) {
        app.ui.dialog = None;
    }
    if !matches!(app.ui.dialog, Some(Dialog::CloudChoice { .. }))
        && [Slot::CloudChoice(0), Slot::CloudChoice(1)]
            .iter()
            .any(|slot| app.renderer.wanted(*slot).is_some() || app.renderer.failure(*slot).is_some())
    {
        app.renderer.forget_cloud_choices();
    }
}
fn tile(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, settings: &DevelopSettings, slot: Slot, label: &str) {
    let t = Tokens::get(ui.ctx());
    ui.strong(crate::i18n::tr(label));
    let height = (ui.ctx().content_rect().height() * 0.40).clamp(120., 320.);
    let (area, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.), height), egui::Sense::hover());
    ui.painter().rect_filled(area, 2., t.canvas);
    let ppp = ui.ctx().pixels_per_point();
    let width = (area.width() * ppp).ceil().clamp(1., 2048.) as usize;
    let height = (area.height() * ppp).ceil().clamp(1., 2048.) as usize;
    let Some(job) = app.session.exact_preview_job(id, width.max(1), height.max(1), true, settings) else {
        ui.label(crate::i18n::tr("This photo is unavailable."));
        return;
    };
    let key = job.key;
    app.renderer.request(slot, job, 0);
    if let Some(texture) = app.renderer.textures.get(&slot).filter(|texture| texture.photo == id && texture.key == key) {
        let aspect = texture.size.first().copied().unwrap_or(1).max(1) as f32 / texture.size.get(1).copied().unwrap_or(1).max(1) as f32;
        let rect = crate::panels::detail::fit_rect(area, aspect, crate::state::Zoom::Fit, texture.size, ppp, (0., 0.));
        ui.painter_at(area).image(texture.tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.)), egui::Color32::WHITE);
        register(ui.ctx(), if slot == Slot::CloudChoice(0) { "image:cloudChoiceCurrent" } else { "image:cloudChoiceRetained" }, rect);
    } else if !app.renderer.is_pending(slot)
        && let Some(error) = app.renderer.failure(slot)
    {
        ui.label(egui::RichText::new(error).color(t.caution));
        ui.label(crate::i18n::tr("Fix the connection or local original, then reopen this preview."));
    } else {
        ui.painter_at(area).text(area.center(), egui::Align2::CENTER_CENTER, crate::i18n::tr("Rendering preview…"), t.font(12.), t.text_dim);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
    }
}
pub(crate) fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, view: &mut View) {
    view.prepare();
    let name: String = view.choice.name.as_deref().unwrap_or("Cloud item").chars().take(160).collect();
    ui.strong(format!("{name} · {}", super::field_label(&view.choice.field)));
    let writer: String = view.choice.device.as_deref().unwrap_or("?").chars().take(12).collect();
    ui.label(format!(
        "{} {writer} · {} {}",
        crate::i18n::tr("Writer"),
        crate::i18n::tr("Revision"),
        view.choice.sequence.map_or_else(|| "?".into(), |n| n.to_string())
    ));
    ui.collapsing(crate::i18n::tr("Revision details"), |ui| {
        ui.add(egui::Label::new(format!("{}: {}", crate::i18n::tr("Writer"), view.choice.device.as_deref().unwrap_or("?"))).wrap());
        ui.add(egui::Label::new(format!("{}: {}", crate::i18n::tr("Revision"), view.choice.revision)).wrap());
    });
    let can_preview = view.can_preview(app);
    ui.horizontal_wrapped(|ui| {
        for (mode, label, id) in [
            (Mode::Details, "Full value", "button:cloudChoiceDetails"),
            (Mode::Preview, "Preview", "button:cloudChoicePreview"),
            (Mode::Compare, "Compare", "button:cloudChoiceCompare"),
        ] {
            let response =
                ui.add_enabled(mode == Mode::Details || can_preview, egui::Button::new(crate::i18n::tr(label)).selected(view.mode == mode));
            register(ui.ctx(), id, response.rect);
            if response.clicked() {
                app.renderer.forget_cloud_choices();
                view.refresh_photo(app);
                view.mode = mode;
            }
        }
    });
    if view.mode != Mode::Details
        && let (Some(id), Some(settings)) = (view.choice.photo_id, view.settings.clone())
    {
        ui.label(crate::i18n::tr("Read-only preview. Your edits and conflict choices are unchanged."));
        if view.mode == Mode::Compare {
            let current = app.session.catalog.photo(PhotoId(id)).map(|photo| photo.develop.clone());
            ui.columns(2, |columns| {
                for (index, column) in columns.iter_mut().enumerate() {
                    if index == 0 {
                        if let Some(current) = &current {
                            tile(app, column, PhotoId(id), current, Slot::CloudChoice(0), "Current edit");
                        }
                    } else {
                        tile(app, column, PhotoId(id), &settings, Slot::CloudChoice(1), "Retained edit");
                    }
                }
            });
        } else {
            tile(app, ui, PhotoId(id), &settings, Slot::CloudChoice(1), "Retained edit");
        }
    } else {
        if view.choice.field == "develop" && !can_preview {
            ui.label(crate::i18n::tr("Restore this photo to preview its retained edits."));
        }
        egui::ScrollArea::both().id_salt("cloud-choice-value").max_height(260.).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&view.text).monospace()).wrap());
        });
        let response = ui.small_button(crate::i18n::tr("Copy value"));
        register(ui.ctx(), "button:cloudChoiceCopy", response.rect);
        if response.clicked() {
            ui.ctx().copy_text(view.text.clone());
        }
    }
}

#[cfg(test)]
mod tests;
