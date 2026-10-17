//! Retention tasks outlive their view; operator consent never does.
//! The engine repeats the full audit and validates its token before any deletion.
use crate::LightcraftApp;
use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Default)]
struct Review {
    task: Option<u64>,
    preview: Option<Value>,
    confirmed: bool,
    error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, PartialEq, Eq)]
struct Owner {
    generation: u64,
    connection: String,
    library: String,
}

#[derive(Default)]
pub(crate) struct Runtime {
    #[cfg(not(target_arch = "wasm32"))]
    owner: Option<Owner>,
    #[cfg(not(target_arch = "wasm32"))]
    review: Review,
    #[cfg(not(target_arch = "wasm32"))]
    visible: bool,
    #[cfg(not(target_arch = "wasm32"))]
    next_poll: f64,
}

/// Public UI state for control clients; never expose audit tokens, keys or dialog fields.
pub(crate) fn inspect(app: &LightcraftApp) -> Value {
    #[cfg(not(target_arch = "wasm32"))]
    {
        serde_json::json!({"confirmed":app.cloud_retention.review.confirmed,
            "task":app.cloud_retention.review.task,"hasPreview":app.cloud_retention.review.preview.is_some()})
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        Value::Null
    }
}

/// Call at command and view boundaries, including replacement by another dialog.
pub(crate) fn observe_view(app: &mut LightcraftApp) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let owner = app.session.catalog.sync_state().map(|sync| Owner {
            generation: app.session.library_generation(),
            connection: sync.connection.clone(),
            library: sync.library.clone(),
        });
        if app.cloud_retention.owner != owner {
            app.cloud_retention = Runtime { owner, ..Default::default() };
        }
        let visible = matches!(app.ui.dialog, Some(crate::state::Dialog::CloudMaintenance));
        if app.cloud_retention.visible != visible {
            app.cloud_retention.review.confirmed = false;
            app.cloud_retention.visible = visible;
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = &app.cloud_retention;
}

pub(crate) fn observe_command(app: &mut LightcraftApp, id: &str, result: &Result<Value, String>) {
    observe_view(app);
    #[cfg(not(target_arch = "wasm32"))]
    if result.is_ok() && matches!(id, "cloud.resume" | "dialog.cloudMaintenance") {
        app.cloud_retention.review.confirmed = false;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (id, result);
}

/// Harvest completion even with Storage tools closed, retaining no operator assertion.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    observe_view(app);
    let Some(task) = app.cloud_retention.review.task else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let now = ctx.input(|i| i.time);
    if now < app.cloud_retention.next_poll {
        return;
    }
    app.cloud_retention.next_poll = now + 0.1;
    let owner = app.cloud_retention.owner.clone();
    let Ok(status) = app.run("cloud.uploadStatus", json!({})) else { return };
    if app.cloud_retention.owner != owner {
        return;
    }
    let Some(report) = status["reports"].as_array().and_then(|reports| reports.iter().find(|report| report["task"] == task)) else { return };
    let review = &mut app.cloud_retention.review;
    review.task = None;
    review.confirmed = false;
    if report["ok"] == true {
        review.preview = Some(report["result"].clone());
        review.error = None;
    } else {
        review.error = Some(report["error"].as_str().unwrap_or("Retention did not complete; review a new preview.").into());
        review.preview = None;
    }
}

pub(crate) fn body(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    observe_view(app);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        ui.label(crate::i18n::tr("Use the desktop app to preview remote retention. Clearing this browser's cache never deletes bucket objects."));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(sync) = app.session.catalog.sync_state() else { return };
        let connection = sync.connection.clone();
        let owner = app.cloud_retention.owner.clone();
        let mut review = app.cloud_retention.review.clone();
        let response=egui::CollapsingHeader::new(crate::i18n::tr("Cloud retention and orphan media")).id_salt("cloudRetentionTools").show(ui,|ui|{
            ui.label(crate::i18n::tr("Published originals, deleted photos, previous revisions, XMP and metadata history are retained indefinitely. These tools only reclaim unreferenced orphan media older than 30 days."));
            ui.label(crate::i18n::tr("Before applying, publish pending work on every device and pause all writers. Keep them paused until this operation finishes. Preview never deletes objects."));
            if let Some(task)=review.task {
                ui.label(crate::i18n::tr("Checking retained references or reclaiming reviewed objects… Editing is paused while applying."));
                if ui.button(crate::i18n::tr("Cancel retention task")).clicked() {let _=app.run("activity.cancel",json!({"id":task}));}
            }
            if let Some(preview)=&review.preview {
                let count=preview["candidates"].as_array().map_or(0,Vec::len);
                ui.label(format!("{}: {count}. {}: {}. {}: {}.",crate::i18n::tr("Eligible orphan objects"),crate::i18n::tr("Bytes"),preview["candidateBytes"].as_u64().unwrap_or(0),crate::i18n::tr("Objects inspected"),preview["objects"].as_u64().unwrap_or(0)));
                if let Some(reasons)=preview["retained"].as_object(){for (reason,count) in reasons {
                    let label=match reason.as_str(){"referencedIncludingHistoryAndLocalStaging"=>"Referenced by catalog, history or staging","historySupportingOrUnknown"=>"History and supporting files","versioned"=>"Versioned objects","youngerThanRetention"=>"Within retention period",_=>"Unrecognized size"};
                    ui.label(format!("{}: {}",crate::i18n::tr(label),count));
                }}
                if let Some(deleted)=preview["deleted"].as_array(){ui.strong(format!("{}: {}. {}",crate::i18n::tr("Reclaimed orphan objects"),deleted.len(),crate::i18n::tr("Published history is retained.")));}
                if preview["applySupported"]==false {ui.label(crate::i18n::tr("This endpoint is preview-only. Conditional orphan deletion is supported on AWS S3."));}
                if let Some(candidates)=preview["candidates"].as_array(){for candidate in candidates.iter().take(8){ui.small(candidate["key"].as_str().unwrap_or_default());}}
            }
            let button=ui.add_enabled(review.task.is_none(),egui::Button::new(crate::i18n::tr("Pause this device and preview retention")));
            crate::widgets::register(ui.ctx(),"button:cloudRetentionPreview",button.rect);
            if button.clicked(){
                review.confirmed=false;
                let result=app.run("cloud.pause",json!({})).and_then(|_|app.run("cloud.reclaim",json!({"connection":connection,"wait":false})));
                match result {Ok(value)=>{review.task=value["task"].as_u64();review.error=None;review.preview=None;app.cloud_retention.next_poll=0.;},Err(error)=>review.error=Some(error)}
            }
            let confirmation=ui.checkbox(&mut review.confirmed,crate::i18n::tr("Every device published its pending work and all writers will remain paused until completion."));
            crate::widgets::register(ui.ctx(),"checkbox:cloudRetentionConfirmed",confirmation.rect);
            let eligible=review.task.is_none()&&review.confirmed&&review.preview.as_ref().is_some_and(|p|p["applySupported"]==true&&p["applied"]==false&&p["candidates"].as_array().is_some_and(|v|!v.is_empty()));
            let button=ui.add_enabled(eligible,egui::Button::new(crate::i18n::tr("Reclaim reviewed orphan media")));
            crate::widgets::register(ui.ctx(),"button:cloudRetentionApply",button.rect);
            if button.clicked()&&let Some(preview)=&review.preview {
                // Consume the assertion on the attempt, including a refused apply.
                review.confirmed=false;
                match app.run("cloud.reclaim",json!({"connection":connection,"apply":true,"preview":preview["preview"],"allWritersPausedAndPublished":true,"wait":false})) {
                    Ok(value)=>{review.task=value["task"].as_u64();review.error=None;app.cloud_retention.next_poll=0.;},Err(error)=>review.error=Some(error),
                }
            }
            if let Some(error)=&review.error {ui.colored_label(crate::theme::Tokens::get(ui.ctx()).caution,error);}
        });
        crate::widgets::register(ui.ctx(), "button:cloudRetentionTools", response.header_response.rect);
        if app.cloud_retention.owner == owner {
            app.cloud_retention.review = review;
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used)]
mod tests;
