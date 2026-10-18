//! Shared manual cloud actions. A user's pause and migration preparation are
//! different intents even when both stop publication on this device.
use super::{Diagnostic, Recovery};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceMode {
    Off,
    Active,
    UserPaused,
    PreparingMigration,
    Migrating,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionKind {
    SyncNow,
    Retry,
    Resume,
    Pause,
    Enable,
    Details,
    ReviewDeletion,
    /// Explicitly leave local preparation, before any upgrade worker starts.
    ContinueWithoutMigration,
}
impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::SyncNow | Self::ContinueWithoutMigration => "Sync now",
            Self::Retry => "Retry Syncing",
            Self::Resume => "Resume",
            Self::Pause => "Pause",
            Self::Enable => "Turn on cloud sync…",
            Self::Details => "Details…",
            Self::ReviewDeletion => "Review deletion…",
        }
    }
    pub fn compact_label(self) -> &'static str {
        match self {
            Self::Retry => "Retry",
            Self::Enable => "Turn on",
            Self::Details => "Details",
            Self::ReviewDeletion => "Review",
            _ => self.label(),
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::SyncNow => "Check the cloud library and publish ready changes.",
            Self::Retry => "Retry pending cloud work on this device.",
            Self::Resume => "Resume syncing on this device. Pending work is kept.",
            Self::Pause => "Uploads and library publication pause on this device. Photos remain available for editing.",
            Self::Enable => "Turn on cloud sync explicitly to merge this local library with cloud storage.",
            Self::Details => "Review the original diagnostic and its recovery actions.",
            Self::ReviewDeletion => "Review the cloud deletion result and any remaining cloud data.",
            Self::ContinueWithoutMigration => "Stop preparing the migration and continue syncing without migrating.",
        }
    }
    pub fn command(self) -> &'static str {
        match self {
            Self::SyncNow => "cloud.sync",
            // Resume also restarts failed durable uploads/imports, unlike the
            // narrower catalog-only cloud.sync command.
            Self::Retry | Self::Resume | Self::ContinueWithoutMigration => "cloud.resume",
            Self::Pause => "cloud.pause",
            Self::Enable => "dialog.cloudSync",
            Self::Details => "dialog.cloudError",
            Self::ReviewDeletion => "dialog.cloudDelete",
        }
    }
    pub fn params(self) -> Value {
        if self == Self::SyncNow { json!({"wait":false}) } else { json!({}) }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualAction {
    pub kind: ActionKind,
    pub enabled: bool,
    pub reason: Option<&'static str>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actions {
    pub mode: DeviceMode,
    /// Info's explicit manual operation, also available as a secondary action.
    pub manual: Option<ManualAction>,
    /// The popup's compact primary operation (Pause while active).
    pub footer: Option<ManualAction>,
}
impl Actions {
    pub fn from_sync_status(status: &Value) -> Self {
        Self::from_overview(&json!({"syncEnabled":status["enabled"],"userPaused":status["userPaused"],
            "paused":status["paused"],"canResume":status["canResume"],"running":status["running"],
            "preparingMigration":status["preparingMigration"],"migrating":status["migrating"],
            "health":{"sync":status["health"]},"erasure":status["erasure"]}))
    }
    pub fn from_overview(data: &Value) -> Self {
        let health = &data["health"]["sync"];
        let running = data["running"] == true || health["running"] == true;
        let can_resume = data["canResume"].as_bool().unwrap_or(!running);
        let preparing = data["preparingMigration"] == true;
        let migrating = data["migrating"] == true || (health["migrationPaused"] == true && !preparing);
        let user_paused = data["userPaused"].as_bool().or_else(|| health["userPaused"].as_bool()).unwrap_or(data["paused"] == true && !preparing);
        let action = |kind, enabled, reason| ManualAction { kind, enabled, reason };
        let same = |mode, action| Self { mode, manual: action, footer: action };
        if data["erasure"].is_object() {
            return same(DeviceMode::Off, Some(action(ActionKind::ReviewDeletion, true, None)));
        }
        if data["syncEnabled"] == false {
            return same(DeviceMode::Off, Some(action(ActionKind::Enable, true, None)));
        }
        if migrating {
            return same(DeviceMode::Migrating, None);
        }
        let diagnostic = Diagnostic::from_overview(data, false);
        if diagnostic
            .as_ref()
            .is_some_and(|d| matches!(d.recovery(), Some(Recovery::Update | Recovery::Rejoin | Recovery::FinishUpgrade | Recovery::NewLibrary)))
        {
            let mode = if preparing {
                DeviceMode::PreparingMigration
            } else if user_paused {
                DeviceMode::UserPaused
            } else {
                DeviceMode::Active
            };
            return same(mode, Some(action(ActionKind::Details, true, None)));
        }
        if preparing {
            return same(
                DeviceMode::PreparingMigration,
                Some(action(
                    ActionKind::ContinueWithoutMigration,
                    can_resume,
                    Some(if can_resume {
                        "Stop preparing the migration and continue syncing without migrating."
                    } else {
                        "Wait for current cloud tasks to stop."
                    }),
                )),
            );
        }
        if user_paused {
            return same(
                DeviceMode::UserPaused,
                Some(action(ActionKind::Resume, can_resume, (!can_resume).then_some("Wait for current cloud tasks to stop."))),
            );
        }
        if diagnostic.is_some() || health["pollingHalted"] == true {
            let kind = if diagnostic.as_ref().is_some_and(|d| d.failure.kind.is_none()) { ActionKind::Details } else { ActionKind::Retry };
            let enabled = kind == ActionKind::Details || can_resume;
            return same(DeviceMode::Active, Some(action(kind, enabled, (!enabled).then_some("Wait for current cloud tasks to stop."))));
        }
        Self {
            mode: DeviceMode::Active,
            manual: Some(action(ActionKind::SyncNow, !running, running.then_some("Synchronization is already running."))),
            footer: Some(action(ActionKind::Pause, true, None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn overview() -> Value {
        json!({"syncEnabled":true,"paused":false,"userPaused":false,"canResume":true,
            "health":{"sync":{"state":"healthy","userPaused":false,"running":false}}})
    }
    fn info(data: &Value) -> Value {
        let mut status = data.clone();
        status["enabled"] = data["syncEnabled"].clone();
        status["health"] = data["health"]["sync"].clone();
        status
    }
    fn agree(data: &Value) -> Actions {
        let model = Actions::from_overview(data);
        assert_eq!(model, Actions::from_sync_status(&info(data)));
        model
    }
    #[test]
    fn surfaces_share_pause_retry_migration_and_local_actions() {
        let mut data = overview();
        let model = agree(&data);
        assert_eq!(model.manual.unwrap().kind, ActionKind::SyncNow);
        assert_eq!(model.footer.unwrap().kind, ActionKind::Pause);
        data["paused"] = json!(true);
        data["userPaused"] = json!(true);
        let model = agree(&data);
        assert_eq!(model.mode, DeviceMode::UserPaused);
        assert_eq!(model.manual.unwrap().kind, ActionKind::Resume);
        assert_eq!(model.manual, model.footer);
        data["canResume"] = json!(false);
        assert!(!agree(&data).manual.unwrap().enabled);
        assert!(agree(&data).manual.unwrap().reason.is_some());
        data["canResume"] = json!(true);
        data["health"]["sync"]["lastError"] = json!({"kind":"connectivity","message":"offline"});
        assert_eq!(agree(&data).manual.unwrap().kind, ActionKind::Resume, "user pause is an explicit Resume even after an error");
        data["userPaused"] = json!(false);
        data["paused"] = json!(false);
        let model = agree(&data);
        assert_eq!(model.manual.unwrap().kind, ActionKind::Retry);
        assert_eq!(model.manual, model.footer);
        assert_eq!(model.manual.unwrap().kind.command(), "cloud.resume");
        data["preparingMigration"] = json!(true);
        data["paused"] = json!(true);
        let model = agree(&data);
        assert_eq!(model.mode, DeviceMode::PreparingMigration);
        assert_eq!(model.manual.unwrap().kind, ActionKind::ContinueWithoutMigration);
        assert_ne!(model.footer.unwrap().kind, ActionKind::Resume);
        data["migrating"] = json!(true);
        assert_eq!(agree(&data).manual, None);
        assert_eq!(agree(&data).footer, None);
        data["syncEnabled"] = json!(false);
        assert_eq!(agree(&data).manual.unwrap().kind, ActionKind::Enable);
    }
    #[test]
    fn compatibility_and_unknown_diagnostics_never_offer_ordinary_retry() {
        for recovery in ["update", "rejoin", "finishUpgrade", "newLibrary"] {
            let mut data = overview();
            data["health"]["sync"]["lastError"] = json!({"kind":"unsupported","message":"original diagnosis","recovery":recovery});
            assert_eq!(agree(&data).manual.unwrap().kind, ActionKind::Details);
            data["preparingMigration"] = json!(true);
            assert_eq!(agree(&data).manual.unwrap().kind, ActionKind::Details, "local preparation cannot dismiss an unfinished remote upgrade");
            data["preparingMigration"] = json!(false);
            data["userPaused"] = json!(true);
            assert_eq!(agree(&data).footer.unwrap().kind, ActionKind::Details);
        }
        let mut data = overview();
        data["health"]["sync"]["lastError"] = json!({"kind":"futureFailure","message":"unknown diagnosis"});
        assert_eq!(agree(&data).manual.unwrap().kind, ActionKind::Details);
        data["health"]["sync"].as_object_mut().unwrap().remove("lastError");
        data["running"] = json!(true);
        assert!(!agree(&data).manual.unwrap().enabled);
        assert!(agree(&data).footer.unwrap().enabled, "Pause can stop running publication");
    }
    #[test]
    fn upload_diagnostics_share_retry_and_unknown_failure_review_actions() {
        let mut data = overview();
        data["error"] = json!("Exact upload failure");
        data["health"]["uploads"]["activity"]["failures"] =
            json!({"original":{"message":"Exact upload failure","kind":"connectivity","retryable":true}});
        let model = Actions::from_overview(&data);
        assert_eq!(model.manual.unwrap().kind, ActionKind::Retry);
        assert_eq!(model.footer, model.manual);
        data["health"]["uploads"]["activity"]["failures"] = json!({});
        assert_eq!(Actions::from_overview(&data).manual.unwrap().kind, ActionKind::Details, "unknown storage failure requires diagnosis");
    }
    #[test]
    fn info_action_actually_resumes_a_device_that_rejects_ordinary_sync() {
        use crate::{Session, library::LibraryStores};
        use lightcraft_catalog::{MemStore, Op, replication::SyncState};
        let mut session = Session::new();
        session
            .open_library_in(
                LibraryStores { dir: "shared-actions".into(), catalog: Box::new(MemStore::new()), files: Box::new(MemStore::new()), on_disk: false },
                false,
            )
            .unwrap();
        session
            .apply_received(Op::SetSyncState {
                state: Some(Box::new(SyncState::new("library".into(), "device".into(), "connection".into()).unwrap())),
            })
            .unwrap();
        session.execute("cloud.pause", &json!({})).unwrap();
        let info = session.execute("cloud.syncStatus", &json!({})).unwrap();
        let popup = session.execute("cloud.overview", &json!({})).unwrap();
        let action = Actions::from_sync_status(&info).manual.unwrap();
        assert_eq!(Some(action), Actions::from_overview(&popup).footer);
        assert_eq!(action.kind, ActionKind::Resume);
        assert!(session.execute("cloud.sync", &json!({"wait":false})).unwrap_err().to_string().contains("paused"));
        session.execute(action.kind.command(), &action.kind.params()).unwrap();
        assert_eq!(session.execute("cloud.overview", &json!({})).unwrap()["userPaused"], false);
    }
}
