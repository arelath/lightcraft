//! One dialog lifecycle for pointer/keyboard input and control clients.
use crate::{LightcraftApp, state::Dialog};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Refresh,
    Confirm,
    #[cfg(not(target_arch = "wasm32"))]
    Back,
    Cancel,
}

fn is_cloud(dialog: &Dialog) -> bool {
    match dialog {
        #[cfg(not(target_arch = "wasm32"))]
        Dialog::CloudSetup { .. } => true,
        Dialog::CloudStorage { .. }
        | Dialog::CloudConnect { .. }
        | Dialog::CloudMigration { .. }
        | Dialog::CloudImport { .. }
        | Dialog::CloudConflicts
        | Dialog::CloudMaintenance
        | Dialog::CloudError { .. }
        | Dialog::CloudDisable { .. }
        | Dialog::CloudDelete { .. } => true,
        _ => false,
    }
}

pub(crate) fn observe(app: &mut LightcraftApp) {
    let generation = app.session.library_generation();
    if app.dialog_library_generation != generation {
        app.dialog_library_generation = generation;
        if app.ui.dialog.as_ref().is_some_and(is_cloud) {
            app.ui.dialog = None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        crate::cloud_setup::cancel_check(app);
    }
    crate::cloud_retention::observe_view(app);
}

pub(crate) fn confirm_current(app: &mut LightcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    observe(app);
    let dialog = app.ui.dialog.clone().ok_or_else(|| "no dialog open".to_string())?;
    update(app, ctx, dialog.clone(), dialog, Action::Confirm)
}
pub(crate) fn cancel_current(app: &mut LightcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    observe(app);
    let Some(dialog) = app.ui.dialog.clone() else { return Ok(Value::Null) };
    update(app, ctx, dialog.clone(), dialog, Action::Cancel)
}

pub(super) fn update(app: &mut LightcraftApp, ctx: &egui::Context, at_start: Dialog, mut dialog: Dialog, action: Action) -> Result<Value, String> {
    observe(app);
    // A replaced or library-invalidated dialog must not execute its old action.
    if app.ui.dialog.as_ref() != Some(&at_start) {
        return Ok(Value::Null);
    }
    if action == Action::Cancel && matches!(&dialog,Dialog::CloudDisable{review} if review.blocking()) {
        return Err("Local preparation is blocking this library. Finish or cancel turning cloud sync off before closing this review.".into());
    }
    let mut close = action == Action::Cancel;
    let mut confirm = action == Action::Confirm;
    let mut result = Ok(Value::Null);
    #[cfg(not(target_arch = "wasm32"))]
    if let Dialog::CloudSetup { setup } = &mut dialog {
        if close {
            crate::cloud_setup::cancel_check(app);
        }
        if action == Action::Back {
            crate::cloud_setup::cancel_check(app);
            if setup.check == crate::cloud_setup::Check::Running {
                setup.check = crate::cloud_setup::Check::Untested;
            }
            setup.back();
        }
        if confirm && !setup.can_confirm() {
            result = Err(crate::i18n::tr("Wait for the connection check or complete the current library choice.").into());
            confirm = false;
        }
        if confirm && !setup.finishes() {
            crate::cloud_setup::next(app, setup);
            confirm = false;
            if let crate::cloud_setup::Check::Failed { error } = &setup.check {
                result = Err(error.clone());
            }
        }
        if !close {
            confirm |= std::mem::take(&mut setup.finish);
        }
    }
    if confirm {
        result = super::confirm_dialog(app, &dialog);
    }
    if result.is_err() {
        if let Dialog::SamModel { error, .. } = &mut dialog {
            *error = result.as_ref().err().cloned();
        } else if retains_error(&dialog) {
            if let Err(error) = &result {
                app.toast(ctx, error.clone());
            }
        } else {
            close = true;
        }
    } else if confirm {
        if matches!(dialog, Dialog::NewAlbum { .. }) && result.as_ref().is_ok_and(|v| v.get("skipped").and_then(Value::as_u64).is_some_and(|n| n > 0))
        {
            if let Ok(value) = &result {
                crate::panels::grid::toast_skipped(app, ctx, value);
            }
            close = true;
        } else if super::keeps_open(app, &dialog) {
            if let Dialog::SamModel { error, .. } = &mut dialog {
                *error = None;
            }
        } else {
            close = true;
        }
    }
    // Commands can replace the view (or switch libraries); preserve that result.
    observe(app);
    if app.ui.dialog.as_ref() != Some(&at_start) {
        return result;
    }
    if let Dialog::CloudDisable { review } = &dialog {
        close = review.dismissed || close && !review.blocking();
    }
    app.ui.dialog = if close { None } else { Some(dialog) };
    observe(app);
    result
}

fn retains_error(dialog: &Dialog) -> bool {
    match dialog {
        #[cfg(not(target_arch = "wasm32"))]
        Dialog::CloudSetup { .. } => true,
        Dialog::Import { .. }
        | Dialog::CloudStorage { .. }
        | Dialog::CloudConnect { .. }
        | Dialog::CloudMigration { .. }
        | Dialog::CloudImport { .. }
        | Dialog::Export { .. }
        | Dialog::ContactSheet { .. }
        | Dialog::NewAlbum { .. }
        | Dialog::NewSmartAlbum { .. }
        | Dialog::SmartRules { .. }
        | Dialog::KeywordTag { .. }
        | Dialog::KeywordSet { .. }
        | Dialog::SynchronizeFolder { .. } => true,
        _ => false,
    }
}
