//! Explicit retention review inside Cloud Synchronization. Preview state is ephemeral;
//! the engine repeats the full audit and validates its token before any deletion.
use crate::LightcraftApp;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Default)]
struct Review {
    task: Option<u64>,
    preview: Option<Value>,
    confirmed: bool,
    error: Option<String>,
}

pub(crate) fn body(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        ui.label(crate::i18n::tr("Use the desktop app to preview remote retention. Clearing this browser's cache never deletes bucket objects."));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(sync) = app.session.catalog.sync_state() else { return };
        let (connection, library) = (sync.connection.clone(), sync.library.clone());
        let id = egui::Id::new(("cloudRetentionReview", connection.as_str(), library.as_str()));
        let mut review = ui.ctx().data_mut(|data| data.get_temp::<Review>(id)).unwrap_or_default();
        let response=egui::CollapsingHeader::new(crate::i18n::tr("Cloud retention and orphan media")).id_salt("cloudRetentionTools").show(ui,|ui|{
            ui.label(crate::i18n::tr("Published originals, deleted photos, previous revisions, XMP and metadata history are retained indefinitely. These tools only reclaim unreferenced orphan media older than 30 days."));
            ui.label(crate::i18n::tr("Before applying, publish pending work on every device and pause all writers. Keep them paused until this operation finishes. Preview never deletes objects."));
            if let Some(task)=review.task {
                let status=app.run("cloud.uploadStatus",json!({})).unwrap_or_default();
                if let Some(report)=status["reports"].as_array().and_then(|reports|reports.iter().find(|report|report["task"]==task)) {
                    review.task=None;review.confirmed=false;
                    if report["ok"]==true {review.preview=Some(report["result"].clone());review.error=None;}
                    else {review.error=Some(report["error"].as_str().unwrap_or("Retention did not complete; review a new preview.").into());review.preview=None;}
                } else {
                    ui.label(crate::i18n::tr("Checking retained references or reclaiming reviewed objects… Editing is paused while applying."));
                    if ui.button(crate::i18n::tr("Cancel retention task")).clicked() {let _=app.run("activity.cancel",json!({"id":task}));}
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
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
                let result=app.run("cloud.pause",json!({})).and_then(|_|app.run("cloud.reclaim",json!({"connection":connection,"wait":false})));
                match result {Ok(value)=>{review.task=value["task"].as_u64();review.error=None;review.preview=None;review.confirmed=false;},Err(error)=>review.error=Some(error)}
            }
            let confirmation=ui.checkbox(&mut review.confirmed,crate::i18n::tr("Every device published its pending work and all writers will remain paused until completion."));
            crate::widgets::register(ui.ctx(),"checkbox:cloudRetentionConfirmed",confirmation.rect);
            let eligible=review.task.is_none()&&review.confirmed&&review.preview.as_ref().is_some_and(|p|p["applySupported"]==true&&p["applied"]==false&&p["candidates"].as_array().is_some_and(|v|!v.is_empty()));
            let button=ui.add_enabled(eligible,egui::Button::new(crate::i18n::tr("Reclaim reviewed orphan media")));
            crate::widgets::register(ui.ctx(),"button:cloudRetentionApply",button.rect);
            if button.clicked()&&let Some(preview)=&review.preview {
                match app.run("cloud.reclaim",json!({"connection":connection,"apply":true,"preview":preview["preview"],"allWritersPausedAndPublished":true,"wait":false})) {
                    Ok(value)=>{review.task=value["task"].as_u64();review.error=None;},Err(error)=>review.error=Some(error),
                }
            }
            if let Some(error)=&review.error {ui.colored_label(crate::theme::Tokens::get(ui.ctx()).caution,error);}
        });
        crate::widgets::register(ui.ctx(), "button:cloudRetentionTools", response.header_response.rect);
        ui.ctx().data_mut(|data| data.insert_temp(id, review));
    }
}
