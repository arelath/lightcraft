//! Settings → Cache: the drive the cloud cache is on and what uses it, the cache's size limit,
//! Clear cache, library-wide offline copies and where the cache lives. Laid out like
//! Lightroom's Preferences → Cache, top to bottom: "Cache on" with the drive and a help link, a
//! usage bar with used / free figures, a four-entry legend, then Manage Storage.
//!
//! Everything goes through commands (`cloud.storage`, `cloud.cache`, `cloud.clearCache`,
//! `cloud.cacheLocation`), so the control channel and MCP drive the same settings.

use egui::{Align2, Rect, RichText, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets::register;

/// Size limits offered (GB); ones larger than the drive are left out.
const LIMITS_GB: [u64; 12] = [5, 10, 20, 30, 50, 70, 100, 150, 200, 300, 500, 1000];
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// "58.1 GB" (one decimal; less than 0.05 GB is "0 GB").
pub fn gb(bytes: u64) -> String {
    let g = bytes as f64 / GB;
    if g < 0.05 { "0 GB".into() } else { format!("{g:.1} GB") }
}

/// The usage bar's (used, total), as in the reference: "used" is what LightCraft keeps on the
/// drive (app & user data + photo cache + stored offline) and "total" is that plus the free
/// space, so the bar shows how much of the room left to it LightCraft takes. Unknown while any
/// part is unknown (app data is measured in the background).
pub fn bar_usage(app_data: Option<u64>, photo_cache: Option<u64>, offline: Option<u64>, free: Option<u64>) -> Option<(u64, u64)> {
    let used = app_data?.saturating_add(photo_cache?).saturating_add(offline?);
    Some((used, used.saturating_add(free?)))
}

/// What the page shows, fetched at most once a second (and right after a change).
#[derive(Clone)]
struct Fetched {
    at: f64,
    storage: Result<Value, String>,
    location: Value,
}

fn fetch(app: &mut LightcraftApp, ui: &egui::Ui) -> Fetched {
    let id = egui::Id::new("settings-cache-page");
    let now = ui.input(|i| i.time);
    if let Some(f) = ui.data(|d| d.get_temp::<Fetched>(id)).filter(|f| now - f.at < 1.0) {
        return f;
    }
    let storage = app.run("cloud.storage", json!({}));
    let location = app.run("cloud.cacheLocation", json!({})).unwrap_or(Value::Null);
    let f = Fetched { at: now, storage, location };
    ui.data_mut(|d| d.insert_temp(id, f.clone()));
    // the app-data size is measured in the background: look again shortly
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    f
}

fn refetch(ui: &egui::Ui) {
    ui.data_mut(|d| d.remove::<Fetched>(egui::Id::new("settings-cache-page")));
}

pub fn cache_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if crate::cloud_transition::local_only(app) {
        ui.heading(crate::i18n::tr("Local library storage"));
        ui.label(crate::i18n::tr("Cloud sync is off. Your library originals are permanent local files, separate from the disposable cloud cache."));
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = app.session.media.asset_resolver.as_ref().and_then(|r| r.permanent_root()) {
            ui.add(egui::Label::new(RichText::new(path.to_string_lossy()).monospace()).wrap());
        }
        ui.label(crate::i18n::tr(
            "Turn on cloud sync to manage cloud cache locations and offline-copy settings. Permanent local originals are retained.",
        ));
        let action = ui.button(crate::i18n::tr("Turn on cloud sync…"));
        register(ui.ctx(), "button:cacheLocalCloudEnable", action.rect);
        if action.clicked()
            && let Err(error) = app.run("dialog.cloudSync", json!({}))
        {
            app.toast(ui.ctx(), error);
        }
        return;
    }
    let f = fetch(app, ui);
    let storage = match f.storage {
        Ok(v) => v,
        Err(_) => {
            ui.add_space(8.0);
            let why = if cfg!(target_arch = "wasm32") {
                "In the browser, cloud photos are cached in the browser's own storage, which the browser manages."
            } else {
                "Open a library stored on this device to manage its cloud cache."
            };
            ui.label(RichText::new(crate::i18n::tr(why)).color(t.text_label));
            return;
        }
    };
    if storage["browser"] == true {
        browser_cache_tab(app, ui, t, &storage);
        return;
    }
    let width = ui.available_width();
    let u = |key: &str| storage[key].as_u64();
    let volume = &storage["volume"];
    let drive = volume["label"].as_str().unwrap_or("?").to_string();
    let free = volume["freeBytes"].as_u64();
    let usage = bar_usage(u("appDataBytes"), u("photoCacheBytes"), u("offlineBytes"), free);
    // the size limit can't exceed the drive
    let total = volume["totalBytes"].as_u64();

    // Cache on: C:                                                    Learn More ↗
    ui.horizontal(|ui| {
        ui.label(RichText::new(crate::i18n::tr("Cache on:")).font(t.font(12.0)).color(t.text_label));
        ui.add_space(10.0);
        ui.label(RichText::new(&drive).font(t.font(12.0)).color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = crate::i18n::tr("Learn More");
            let galley = ui.painter().layout_no_wrap(label.to_string(), t.semibold(13.0), t.text);
            let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 22.0, 22.0), Sense::click());
            register(ui.ctx(), "button:settingsCacheLearnMore", r);
            let c = if resp.hovered() { t.accent } else { t.text };
            ui.painter().galley_with_override_text_color(pos2(r.left(), r.center().y - galley.size().y / 2.0), galley, c);
            crate::icons::paint(ui.painter(), Rect::from_center_size(pos2(r.right() - 7.0, r.center().y), vec2(14.0, 14.0)), Icon::External, c);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                && let Err(e) = crate::links::open(app, crate::links::CACHE_HELP)
            {
                app.toast(ui.ctx(), e);
            }
        });
    });
    ui.add_space(4.0);

    // the drive's usage bar
    let (bar, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    ui.painter().rect_filled(bar, 2.0, t.track);
    if let Some((used, all)) = usage
        && all > 0
    {
        let w = bar.width() * (used as f64 / all as f64).clamp(0.0, 1.0) as f32;
        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(w, bar.height())), 2.0, t.accent);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let small = t.font(10.0);
        let used = match usage {
            Some((used, all)) => crate::i18n::tr_format!("{used} of {total} used", used = gb(used), total = gb(all)),
            None => crate::i18n::tr("Measuring…").to_string(),
        };
        ui.label(RichText::new(used).font(small.clone()).color(t.text_label));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let free = free.map(|b| crate::i18n::tr_format!("{free} free", free = format!("{:.2} GB", b as f64 / GB))).unwrap_or_default();
            ui.label(RichText::new(free).font(small).color(t.text_label));
        });
    });
    ui.add_space(10.0);

    // the legend: what uses the drive
    let unknown = || "—".to_string();
    let entries = [
        [
            (crate::i18n::tr("App & user data"), u("appDataBytes").map(gb).unwrap_or_else(unknown), t.accent),
            (crate::i18n::tr("Stored offline"), u("offlineBytes").map(gb).unwrap_or_else(unknown), t.accent),
        ],
        [
            (crate::i18n::tr("Photo cache"), u("photoCacheBytes").map(gb).unwrap_or_else(unknown), t.accent),
            (crate::i18n::tr("Free"), free.map(gb).unwrap_or_else(unknown), t.text_dim),
        ],
    ];
    let column = (width - 26.0) / 2.0;
    for (row, pair) in entries.iter().enumerate() {
        let (r, _) = ui.allocate_exact_size(vec2(width, 33.0), Sense::hover());
        for (i, (label, value, dot)) in pair.iter().enumerate() {
            let left = r.left() + i as f32 * (column + 26.0);
            let y = r.center().y;
            let p = ui.painter();
            p.circle_filled(pos2(left + 4.0, y), 3.5, *dot);
            p.text(pos2(left + 20.0, y), Align2::LEFT_CENTER, label, t.font(13.0), t.text);
            p.text(pos2(left + column, y), Align2::RIGHT_CENTER, value, t.font(13.0), t.text);
            register(
                ui.ctx(),
                format!("label:settingsCacheLegend-{}", row * 2 + i),
                Rect::from_min_max(pos2(left, r.top()), pos2(left + column, r.bottom())),
            );
        }
    }
    ui.add_space(14.0);
    if let Some(errors) = storage["offlineErrors"].as_object() {
        for message in errors.values().filter_map(Value::as_str).take(8) {
            ui.colored_label(t.caution, message);
        }
    }
    let (line, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter().hline(line.x_range(), line.center().y, Stroke::new(1.0, t.divider));
    ui.add_space(18.0);

    ui.label(RichText::new(crate::i18n::tr("MANAGE STORAGE")).font(t.semibold(10.5)).color(t.text));
    ui.add_space(14.0);
    // Cache size limit  70 GB ⌄                                  ( Clear cache )  ⓘ
    ui.horizontal(|ui| {
        ui.label(RichText::new(crate::i18n::tr("Cache size limit")).font(t.font(12.5)).color(t.text_label));
        ui.add_space(24.0);
        let budget = u("budgetBytes").unwrap_or(0);
        let resp = crate::widgets::dropdown(ui, "settingsCacheLimit", &gb_whole(budget), t.font(13.0), t.text);
        egui::Popup::menu(&resp).show(|ui| {
            let largest = total.map_or(u64::MAX, |t| t / (1 << 30));
            let mut options: Vec<u64> = LIMITS_GB.iter().copied().filter(|g| *g <= largest.max(LIMITS_GB[0])).collect();
            if budget.is_multiple_of(1 << 30) && !options.contains(&(budget >> 30)) {
                options.push(budget >> 30);
                options.sort_unstable();
            }
            for g in options {
                let r = ui.selectable_label(budget == g << 30, format!("{g} GB"));
                register(ui.ctx(), format!("button:settingsCacheLimit-{g}"), r.rect);
                if r.clicked() {
                    match app.run("cloud.cache", json!({"budgetMb": g * 1024})) {
                        Ok(_) => refetch(ui),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (info, info_resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            crate::icons::paint(ui.painter(), info, Icon::InfoFilled, t.text);
            register(ui.ctx(), "icon:settingsClearCacheInfo", info);
            info_resp.on_hover_text(crate::i18n::tr(
                "Clear cache removes the photos and previews downloaded to this device. They stay in the cloud and download again when you need them. Photos stored offline, photos not uploaded yet and files in use are kept.",
            ));
            ui.add_space(10.0);
            if pill_button(ui, t, "button:settingsClearCache", crate::i18n::tr("Clear cache")).clicked() {
                match app.run("cloud.clearCache", json!({})) {
                    Ok(_) => {
                        app.toast(ui.ctx(), crate::i18n::tr("Clearing the cache…"));
                        refetch(ui);
                    }
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
        });
    });
    ui.add_space(14.0);
    ui.allocate_ui(vec2((width * 0.78).max(200.0), 0.0), |ui| {
        ui.label(
            RichText::new(crate::i18n::tr(
                "Increase the cache size to retain more photos on your device. Decrease it if you prefer to save disk space by downloading content when you need it.",
            ))
            .font(t.font(12.0))
            .color(t.text_label),
        );
    });
    ui.add_space(18.0);

    let keep = &storage["keepAll"];
    let mut previews = keep["previews"].as_bool().unwrap_or(false);
    let label = crate::i18n::tr_format!("Store a copy of all Smart previews locally on “{drive}”", drive = drive);
    if super::settings::check(ui, "settingsKeepAllPreviews", &mut previews, &label) {
        keep_all(app, ui, json!({"keepAllPreviews": previews}));
    }
    ui.add_space(4.0);
    let mut originals = keep["originals"].as_bool().unwrap_or(false);
    if super::settings::check(ui, "settingsKeepAllOriginals", &mut originals, "Store a copy of all originals") {
        keep_all(app, ui, json!({"keepAllOriginals": originals}));
    }
    ui.add_space(18.0);

    ui.label(RichText::new(crate::i18n::tr("Storage location for originals:")).font(t.font(12.0)).color(t.text_label));
    ui.add_space(6.0);
    let path = f.location["path"].as_str().unwrap_or_default().to_string();
    let moving = f.location["moving"].as_u64().is_some();
    ui.horizontal(|ui| {
        let browse_w = 80.0;
        ui.add_sized(
            vec2((ui.available_width() - browse_w).max(60.0), 20.0),
            egui::Label::new(RichText::new(&path).font(t.font(12.0)).italics().color(t.text_label)).truncate(),
        )
        .on_hover_text(&path);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if moving { crate::i18n::tr("Moving…") } else { crate::i18n::tr("Browse…") };
            let galley = ui.painter().layout_no_wrap(label.to_string(), t.semibold(13.0), t.text);
            let (r, resp) = ui.allocate_exact_size(galley.size() + vec2(4.0, 6.0), Sense::click());
            register(ui.ctx(), "button:settingsCacheBrowse", r);
            let enabled = !moving && app.services.pick_folder.is_some();
            let c = if !enabled {
                t.text_disabled
            } else if resp.hovered() {
                t.accent
            } else {
                t.text
            };
            ui.painter().galley_with_override_text_color(r.left_center() - vec2(0.0, galley.size().y / 2.0), galley, c);
            if enabled
                && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                && let Some(dir) = app.services.pick_folder.as_mut().and_then(|pick| pick())
            {
                match app.run("cloud.cacheLocation", json!({"path": dir})) {
                    Ok(v) if v["background"] == true => app.toast(ui.ctx(), crate::i18n::tr("Moving the cache; Activity shows the progress")),
                    Ok(_) => {}
                    Err(e) => app.toast(ui.ctx(), e),
                }
                refetch(ui);
            }
        });
    });
    if let Some(error) = f.location["lastMove"]["error"].as_str() {
        ui.label(RichText::new(error).font(t.font(11.0)).color(t.caution));
    }
    for key in ["error", "stagingError"] {
        if let Some(error) = storage[key].as_str() {
            ui.label(RichText::new(error).font(t.font(11.0)).color(t.caution));
        }
    }
}

/// "70 GB" for the size-limit menu (whole gigabytes when it is one).
fn gb_whole(bytes: u64) -> String {
    if bytes.is_multiple_of(1 << 30) { format!("{} GB", bytes >> 30) } else { gb(bytes) }
}

fn keep_all(app: &mut LightcraftApp, ui: &egui::Ui, params: Value) {
    if let Err(e) = app.run("cloud.cache", params) {
        app.toast(ui.ctx(), e);
    }
    refetch(ui);
}

fn browser_cache_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, storage: &Value) {
    ui.heading(crate::i18n::tr("Browser storage"));
    ui.label(crate::i18n::tr(
        "Cloud downloads are disposable unless stored offline. Imported originals, catalog changes and offline copies are protected.",
    ));
    let cache = &storage["persistentCache"];
    if let (Some(usage), Some(quota)) = (cache["usageBytes"].as_u64(), cache["quotaBytes"].as_u64()) {
        ui.label(format!("{:.1} / {:.1} MB", usage as f64 / (1 << 20) as f64, quota as f64 / (1 << 20) as f64));
    }
    ui.label(crate::i18n::tr("Media reserves space for catalog saves. Browser quota estimates can change."));
    if let Some(reserve) = cache["reservedHeadroomBytes"].as_u64() {
        ui.label(format!("{} MB", reserve >> 20));
    }
    let budget = storage["budgetBytes"].as_u64().unwrap_or(0) >> 20;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Cache size limit"));
        egui::ComboBox::from_id_salt("browserCacheLimit").selected_text(format!("{budget} MB")).show_ui(ui, |ui| {
            for mb in [0u64, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096] {
                let r = ui.selectable_label(budget == mb, format!("{mb} MB"));
                register(ui.ctx(), format!("button:settingsCacheLimitMb-{mb}"), r.rect);
                if r.clicked() {
                    match app.run("cloud.cache", json!({"budgetMb":mb})) {
                        Ok(_) => refetch(ui),
                        Err(error) => app.toast(ui.ctx(), error),
                    }
                }
            }
        });
    });
    if cache["temporaryBlobs"].as_u64().unwrap_or(0) > 0 {
        ui.colored_label(
            t.caution,
            crate::i18n::tr("Some verified photos are available only for this session. Store them offline to retain them after reload."),
        );
    }
    if let Some(error) = cache["error"]
        .as_str()
        .filter(|error| !storage["offlineErrors"].as_object().is_some_and(|errors| errors.values().any(|value| value.as_str() == Some(*error))))
    {
        ui.colored_label(t.caution, error);
    }
    if let Some(errors) = storage["offlineErrors"].as_object() {
        for error in errors.values().filter_map(Value::as_str).take(8) {
            ui.colored_label(t.caution, error);
        }
    }
    ui.label(crate::i18n::tr(if storage["offlineReady"] == true {
        "Offline copies are ready."
    } else {
        "Offline copies are waiting or need attention."
    }));
    ui.horizontal(|ui| {
        if pill_button(ui, t, "button:settingsClearCache", crate::i18n::tr("Clear cache")).clicked() {
            match app.run("cloud.clearCache", json!({})) {
                Ok(_) => refetch(ui),
                Err(error) => app.toast(ui.ctx(), error),
            }
        }
        if pill_button(ui, t, "button:settingsCacheRetry", crate::i18n::tr("Retry downloads")).clicked() {
            match app.run("cloud.reconnect", json!({})) {
                Ok(_) => refetch(ui),
                Err(error) => app.toast(ui.ctx(), error),
            }
        }
    });
    let mut previews = storage["keepAll"]["previews"] == true;
    if super::settings::check(ui, "settingsKeepAllPreviews", &mut previews, "Store a copy of all Smart previews") {
        keep_all(app, ui, json!({"keepAllPreviews":previews}));
    }
    let mut originals = storage["keepAll"]["originals"] == true;
    if super::settings::check(ui, "settingsKeepAllOriginals", &mut originals, "Store a copy of all originals") {
        keep_all(app, ui, json!({"keepAllOriginals":originals}));
    }
    ui.label(crate::i18n::tr("Offline retention prevents LightCraft cache eviction. Keep backups: clearing site data or browser eviction can still remove browser storage."));
}

/// An outlined, fully rounded button ("Clear cache").
fn pill_button(ui: &mut egui::Ui, t: &Tokens, id: &str, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), t.semibold(13.0), t.text);
    let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 34.0, 28.0), Sense::click());
    register(ui.ctx(), id, r);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, 14.0, t.hover);
    }
    p.rect_stroke(r.shrink(1.0), 13.0, Stroke::new(1.5, t.text), egui::StrokeKind::Middle);
    p.galley_with_override_text_color(r.center() - galley.size() / 2.0, galley, t.text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's figures: 7.9 + 58.1 + 0 GB used and 109.73 GB free read "66.0 GB of 175.7 GB used".
    #[test]
    fn the_bar_counts_lightcrafts_usage_against_the_room_left_to_it() {
        let g = |x: f64| (x * GB) as u64;
        let (used, total) = bar_usage(Some(g(7.9)), Some(g(58.1)), Some(0), Some(g(109.73))).unwrap();
        assert_eq!((gb(used), gb(total)), ("66.0 GB".to_string(), "175.7 GB".to_string()));
        assert!((used as f64 / total as f64 - 0.3756).abs() < 0.001, "about 38% full");
        assert_eq!(bar_usage(None, Some(1), Some(0), Some(1)), None, "unknown until app data is measured");
        assert_eq!(bar_usage(Some(0), Some(0), Some(0), None), None);
        assert_eq!(bar_usage(Some(u64::MAX), Some(1), Some(1), Some(1)), Some((u64::MAX, u64::MAX)));
        assert_eq!(gb(0), "0 GB");
    }
}
