//! Observe an explicitly requested verification without keeping its popup open.
use crate::LightcraftApp;
use serde_json::{Value, json};

#[derive(Clone, PartialEq, Eq)]
struct Owner {
    generation: u64,
    connection: String,
    library: String,
}

#[derive(Default)]
pub(crate) struct Runtime {
    owner: Option<Owner>,
    task: Option<u64>,
    next_poll: f64,
}

fn observe_owner(app: &mut LightcraftApp) {
    let owner = app.session.catalog.sync_state().map(|state| Owner {
        generation: app.session.library_generation(),
        connection: state.connection.clone(),
        library: state.library.clone(),
    });
    if app.cloud_verification.owner != owner {
        app.cloud_verification = Runtime { owner, ..Default::default() };
    }
}

pub(crate) fn observe_command(app: &mut LightcraftApp, id: &str, result: &Result<Value, String>) {
    observe_owner(app);
    if id == "cloud.verifyHistory"
        && let Ok(result) = result
    {
        app.cloud_verification.task = result["task"].as_u64();
        app.cloud_verification.next_poll = 0.;
    }
}

pub(crate) fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    observe_owner(app);
    let Some(task) = app.cloud_verification.task else { return };
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let now = ctx.input(|i| i.time);
    if now < app.cloud_verification.next_poll {
        return;
    }
    app.cloud_verification.next_poll = now + 0.1;
    let owner = app.cloud_verification.owner.clone();
    // This reads completed local task reports; it does not synchronize or fetch.
    let Ok(status) = app.run("cloud.syncStatus", json!({})) else { return };
    if app.cloud_verification.owner != owner {
        return;
    }
    let Some(report) = status["reports"].as_array().and_then(|reports| reports.iter().find(|report| report["task"] == task)) else { return };
    app.cloud_verification.task = None;
    let text = if report["ok"] != true {
        report["error"].as_str().unwrap_or("Remote history verification did not complete.")
    } else if report["result"]["complete"] == true {
        "Remote history verified."
    } else {
        "Remote history verification is partial. Continue later."
    };
    app.toast_for(ctx, crate::i18n::tr(text), 10.);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::Services;

    #[test]
    fn explicit_completion_is_reported_once_and_discarded_when_library_changes() {
        for (report, expected) in [
            (json!({"task":42,"ok":true,"result":{"complete":true}}), "Remote history verified."),
            (json!({"task":42,"ok":true,"result":{"complete":false,"deferred":true}}), "Remote history verification is partial. Continue later."),
            (json!({"task":42,"ok":false,"error":"Damaged remote history"}), "Damaged remote history"),
        ] {
            let services = Services {
                cloud_action: Some(Box::new(move |_, id, _| match id {
                    "cloud.verifyHistory" => Some(Ok(json!({"task":42}))),
                    "cloud.syncStatus" => Some(Ok(json!({"reports":[report]}))),
                    _ => None,
                })),
                ..Default::default()
            };
            let mut session = lightcraft_engine::Session::with_demo();
            session
                .catalog
                .apply(lightcraft_catalog::Op::SetSyncState {
                    state: Some(Box::new(
                        lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "personal".into()).unwrap(),
                    )),
                })
                .unwrap();
            let mut app = LightcraftApp::new(session, services);
            let ctx = egui::Context::default();
            app.run("cloud.verifyHistory", json!({"wait":false})).unwrap();
            poll(&mut app, &ctx);
            assert_eq!(app.ui.toast.as_ref().unwrap().0, expected);
            app.ui.toast = None;
            poll(&mut app, &ctx);
            assert!(app.ui.toast.is_none());
            app.run("cloud.verifyHistory", json!({"wait":false})).unwrap();
            app.session.catalog.apply(lightcraft_catalog::Op::SetSyncState { state: None }).unwrap();
            poll(&mut app, &ctx);
            assert!(app.cloud_verification.task.is_none());
            assert!(app.ui.toast.is_none());
        }
    }
}
