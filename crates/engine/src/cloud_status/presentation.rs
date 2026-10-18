//! One cloud status projection at the JSON boundary for popup and Info.
use super::{Actions, DeviceMode, Diagnostic};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncStatus {
    Local,
    Off,
    Erased,
    DeletionIncomplete,
    Migrating,
    PreparingMigration,
    UserPaused,
    Retrying,
    PublishingPaused,
    NeedsAttention,
    Syncing,
    UpToDate,
    Checking,
}
impl SyncStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local library",
            Self::Off => "Cloud sync is off",
            Self::Erased => "Cloud library deleted",
            Self::DeletionIncomplete => "Deletion incomplete",
            Self::Migrating => "Migrating cloud library…",
            Self::PreparingMigration => "Preparing cloud migration…",
            Self::UserPaused => "Syncing paused",
            Self::Retrying => "Retrying synchronization…",
            Self::PublishingPaused => "Publishing paused",
            Self::NeedsAttention => "Sync needs attention",
            Self::Syncing => "Syncing library…",
            Self::UpToDate => "Up to date",
            Self::Checking => "Checking cloud library…",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Presentation {
    pub status: SyncStatus,
    pub actions: Actions,
    pub local_saved: bool,
    pub pending_changes: u64,
    pub last_scan_at_unix_ms: Option<u64>,
    pub seconds_since_scan: Option<u64>,
    pub publication_blocked: Option<String>,
    pub diagnostic: Option<Diagnostic>,
    pub local_destination: Option<String>,
    pub needs_attention: bool,
    pub busy: bool,
}
impl Presentation {
    pub fn from_overview(data: &Value, unsaved: bool) -> Self {
        let actions = Actions::from_overview(data);
        let attention = needs_attention(data) || unsaved;
        let busy = busy(data);
        let health = &data["health"]["sync"];
        let publication_blocked = data["publicationBlocked"].as_str().map(str::to_owned);
        let status = if !unsaved && data["erasure"].is_object() {
            if data["erasure"]["complete"] == true { SyncStatus::Erased } else { SyncStatus::DeletionIncomplete }
        } else if !unsaved && data["syncEnabled"] == false {
            SyncStatus::Off
        } else if unsaved {
            SyncStatus::NeedsAttention
        } else if actions.mode == DeviceMode::Migrating {
            SyncStatus::Migrating
        } else if actions.mode == DeviceMode::PreparingMigration {
            SyncStatus::PreparingMigration
        } else if actions.mode == DeviceMode::UserPaused {
            SyncStatus::UserPaused
        } else if health["state"] == "retrying" {
            SyncStatus::Retrying
        } else if publication_blocked.is_some() {
            SyncStatus::PublishingPaused
        } else if attention {
            SyncStatus::NeedsAttention
        } else if busy || health["running"] == true || data["running"] == true {
            SyncStatus::Syncing
        } else if data["syncEnabled"] == true && verified_scan(data) {
            SyncStatus::UpToDate
        } else if data["syncEnabled"] == true {
            SyncStatus::Checking
        } else {
            SyncStatus::Local
        };
        Self {
            status,
            actions,
            local_saved: !unsaved,
            pending_changes: data["summary"]["pendingChanges"].as_u64().unwrap_or(0),
            last_scan_at_unix_ms: health["lastSuccessfulRemoteScan"]["atUnixMs"].as_u64(),
            seconds_since_scan: health["secondsSinceLastRemoteScan"].as_u64(),
            publication_blocked,
            diagnostic: Diagnostic::from_overview(data, false),
            local_destination: data["localDestination"].as_str().map(str::to_owned),
            needs_attention: attention,
            busy,
        }
    }
}
fn needs_attention(data: &Value) -> bool {
    if data["syncEnabled"] == false && !data["erasure"].is_object() {
        return false;
    }
    let summary = &data["summary"];
    let health = &data["health"];
    data["erasure"].is_object()
        || summary["issues"].as_u64().unwrap_or(0) > 0
        || summary["failedFiles"].as_u64().unwrap_or(0) > 0
        || summary["blockedImportCount"].as_u64().unwrap_or(0) > 0
        || data["error"].is_string()
        || health["imports"]["failed"].as_array().is_some_and(|items| !items.is_empty())
        || health["offline"]["persistentCache"]["temporaryBlobs"].as_u64().unwrap_or(0) > 0
        || health["sync"]["pollingHalted"] == true
        || health["sync"]["lastError"].is_object()
        || health["uploads"]["failedImports"].as_u64().unwrap_or(0) > 0
        || health["uploads"]["activity"]["failedResources"].as_u64().unwrap_or(0) > 0
        || health["media"]["failedResources"].as_u64().unwrap_or(0) > 0
        || health["offline"]["offlineErrors"].as_object().is_some_and(|errors| !errors.is_empty())
        || health["receipts"]["failed"].as_u64().unwrap_or(0) > 0
        || health["conflicts"]["fields"].as_u64().unwrap_or(0) > 0
        || health["conflicts"]["structural"].as_u64().unwrap_or(0) > 0
        || data["conflicts"].as_array().is_some_and(|items| !items.is_empty())
        || data["structuralConflicts"].as_array().is_some_and(|items| !items.is_empty())
        || data["capacityWarnings"].as_array().is_some_and(|items| !items.is_empty())
        || data["publicationBlocked"].is_string()
}
fn verified_scan(data: &Value) -> bool {
    data["health"]["sync"]["lastSuccessfulRemoteScan"].is_object() && matches!(data["health"]["sync"]["state"].as_str(), Some("healthy" | "checking"))
}
fn busy(data: &Value) -> bool {
    data["waitingFiles"].as_u64().unwrap_or(0) > 0
        || data["uploadingFiles"].as_u64().unwrap_or(0) > 0
        || data["preparingFiles"].as_u64().unwrap_or(0) > 0
        || data["health"]["receipts"]["pending"].as_u64().unwrap_or(0) > 0
        || data["health"]["remoteHistoryWaiting"] == true
        || data["summary"]["pendingChanges"].as_u64().unwrap_or(0) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn overview() -> Value {
        json!({"syncEnabled":true,"summary":{"pendingChanges":0},"health":{"sync":{"state":"healthy","lastSuccessfulRemoteScan":{"atUnixMs":1000}}}})
    }
    #[test]
    fn status_distinguishes_successful_scan_work_pause_and_publication_restrictions() {
        let mut data = overview();
        let model = Presentation::from_overview(&data, false);
        assert_eq!(model.status, SyncStatus::UpToDate);
        assert_eq!(model.last_scan_at_unix_ms, Some(1000));
        assert!(model.local_saved);
        data["health"]["sync"]["lastSuccessfulRemoteScan"] = Value::Null;
        assert_eq!(Presentation::from_overview(&data, false).status, SyncStatus::Checking, "configured empty storage has not been checked yet");
        data = overview();
        data["summary"]["pendingChanges"] = json!(2);
        let model = Presentation::from_overview(&data, false);
        assert_eq!(model.status, SyncStatus::Syncing);
        assert_eq!(model.pending_changes, 2);
        data["userPaused"] = json!(true);
        assert_eq!(Presentation::from_overview(&data, false).status, SyncStatus::UserPaused);
        data["userPaused"] = json!(false);
        data["publicationBlocked"] = json!("Another device requires a newer writer.");
        let model = Presentation::from_overview(&data, false);
        assert_eq!(model.status, SyncStatus::PublishingPaused);
        assert_eq!(model.publication_blocked.as_deref(), Some("Another device requires a newer writer."));
        data["publicationBlocked"] = Value::Null;
        data["health"]["sync"]["state"] = json!("retrying");
        data["health"]["sync"]["lastError"] = json!({"kind":"connectivity","message":"Exact cause","httpStatus":503});
        let model = Presentation::from_overview(&data, false);
        assert_eq!(model.status, SyncStatus::Retrying);
        assert_eq!(model.diagnostic.unwrap().failure.http_status, Some(503));
        assert_eq!(Presentation::from_overview(&data, true).status, SyncStatus::NeedsAttention);
        data["syncEnabled"] = json!(false);
        data["localOnly"] = json!(true);
        data["localDestination"] = json!("D:/Library/Local Originals");
        let model = Presentation::from_overview(&data, false);
        assert_eq!(model.status, SyncStatus::Off);
        assert_eq!(model.local_destination.as_deref(), Some("D:/Library/Local Originals"));
        data["erasure"] = json!({"complete":false});
        assert_eq!(Presentation::from_overview(&data, false).status, SyncStatus::DeletionIncomplete);
        data["erasure"]["complete"] = json!(true);
        let model = Presentation::from_overview(&data, false);
        assert_eq!(model.status, SyncStatus::Erased);
        assert_eq!(model.actions.manual.unwrap().kind, crate::cloud_status::ActionKind::ReviewDeletion);
        assert_eq!(model.actions.manual.unwrap().kind.command(), "dialog.cloudDelete");
    }
}
