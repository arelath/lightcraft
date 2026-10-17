//! Library-wide error details; no selected photo or connection form is required.
use crate::{LightcraftApp, theme::Tokens, widgets::register};
use lightcraft_engine::cloud_status::{Diagnostic, Recovery};
use serde_json::json;

pub(crate) fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, diagnostic: &Diagnostic, action_error: &mut Option<String>) {
    let t = Tokens::get(ui.ctx());
    ui.strong(crate::i18n::tr(&diagnostic.operation));
    ui.add(egui::Label::new(&diagnostic.failure.message).wrap().selectable(true));
    if let Some(resource) = &diagnostic.resource {
        ui.label(super::cloud::count("Affected resource", resource));
    }
    if let Some(task) = diagnostic.failure.task {
        ui.label(super::cloud::count("Activity task", task));
    }
    if let Some(status) = diagnostic.failure.http_status {
        ui.label(format!("HTTP {status}"));
    }
    let state = if diagnostic.paused {
        "Paused on this device"
    } else if diagnostic.migration_paused {
        "Upgrade requires attention"
    } else if diagnostic.running {
        "Checking cloud library"
    } else if diagnostic.retry.exhausted {
        "Automatic retry limit reached"
    } else if diagnostic.retry.scheduled {
        "Automatic retry scheduled"
    } else {
        "No automatic retry scheduled"
    };
    ui.label(crate::i18n::tr(state));
    if diagnostic.retry.attempt > 0 {
        ui.label(format!("{}: {} / {}", crate::i18n::tr("Retry attempts"), diagnostic.retry.attempt, diagnostic.retry.max_attempts));
    }
    if diagnostic.retry.scheduled
        && let Some(seconds) = diagnostic.retry.seconds_until_retry
    {
        ui.label(super::cloud::count("Seconds until retry when opened", seconds));
    }
    ui.separator();
    ui.label(crate::i18n::tr(&diagnostic.advice()));
    ui.label(egui::RichText::new(crate::i18n::tr("Diagnostic captured when Details opened.")).small().color(t.text_dim));
    let recovery = match diagnostic.recovery() {
        Some(Recovery::EditConnection) => Some(("button:cloudErrorConnection", "Edit connection…", "dialog.cloudStorage")),
        Some(Recovery::ReviewCapacity) => Some(("button:cloudErrorCapacity", "Storage tools…", "dialog.cloudMaintenance")),
        _ => None,
    };
    if let Some((id, label, command)) = recovery {
        let response = ui.button(crate::i18n::tr(label));
        register(ui.ctx(), id, response.rect);
        if response.clicked() {
            *action_error = app.run(command, json!({"id":app.session.catalog.sync_state().map(|s| &s.connection)})).err();
        }
    }
    if diagnostic.can_retry() {
        let response = ui.button(crate::i18n::tr(if diagnostic.paused { "Resume Syncing" } else { "Retry Syncing" }));
        register(ui.ctx(), "button:cloudErrorRetry", response.rect);
        if response.clicked() {
            match app.run(diagnostic.retry_command(), json!({"wait":false})) {
                Ok(_) => {
                    *action_error = None;
                    app.toast(ui.ctx(), crate::i18n::tr("Synchronization requested."));
                }
                Err(error) => *action_error = Some(error),
            }
        }
    }
    if let Some(error) = action_error {
        ui.colored_label(t.caution, error.as_str());
    }
}
