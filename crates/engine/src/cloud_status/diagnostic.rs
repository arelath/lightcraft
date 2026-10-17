//! Typed presentation at the command JSON boundary, shared by all cloud surfaces.
use super::{CloudFailure, Failure, FailureKind, Recovery};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RetryState {
    pub scheduled: bool,
    pub attempt: u64,
    pub max_attempts: u64,
    pub exhausted: bool,
    pub seconds_until_retry: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub failure: Failure,
    pub operation: String,
    pub resource: Option<String>,
    pub state: String,
    pub retry: RetryState,
    pub paused: bool,
    pub running: bool,
    pub enabled: bool,
    pub migration_paused: bool,
}
impl Diagnostic {
    pub fn from_sync_status(status: &Value) -> Option<Self> {
        Self::from_overview(
            &serde_json::json!({"health":{"sync":status["health"]},"syncEnabled":status["enabled"],
            "preparingMigration":status["preparingMigration"],"migrating":status["migrating"]}),
            false,
        )
    }
    /// `storage` selects the separately displayed non-polling error. Unknown legacy
    /// diagnostics remain unclassified: their words never select a connection editor.
    pub fn from_overview(data: &Value, storage: bool) -> Option<Self> {
        let sync = &data["health"]["sync"];
        let (failure, operation, resource) = if !storage && sync["lastError"].is_object() {
            (decode_failure(&sync["lastError"])?, "Cloud synchronization", None)
        } else {
            let message = data["error"].as_str()?;
            let resource = data["health"]["uploads"]["activity"]["failures"]
                .as_object()
                .and_then(|items| items.iter().find(|(_, failure)| failure["message"].as_str() == Some(message)));
            if let Some((resource, failure)) = resource {
                (decode_failure(failure)?, "Cloud upload or verification", Some(resource.clone()))
            } else {
                (Failure::new(message, None), "Cloud storage", None)
            }
        };
        let operation = failure.operation.clone().unwrap_or_else(|| operation.into());
        Some(Self {
            failure,
            operation,
            resource,
            state: sync["state"].as_str().unwrap_or("unverified").into(),
            retry: if storage { RetryState::default() } else { serde_json::from_value(sync["retry"].clone()).unwrap_or_default() },
            paused: data["userPaused"] == true || sync["userPaused"] == true,
            running: sync["running"] == true,
            enabled: data["syncEnabled"] != false,
            migration_paused: data["preparingMigration"] == true || data["migrating"] == true || sync["migrationPaused"] == true,
        })
    }
    pub fn recovery(&self) -> Option<Recovery> {
        self.failure.recovery.or_else(|| self.failure.kind.map(Recovery::for_kind))
    }
    pub fn advice(&self) -> String {
        self.failure.action.clone().unwrap_or_else(|| {
            self.failure.kind.map_or_else(
                || "Review the diagnostic and the affected operation before retrying. Pending work is kept.".into(),
                |kind| CloudFailure::new(kind, &self.failure.message).action().into(),
            )
        })
    }
    /// Retrying cannot repair compatibility, a moved library or an unfinished upgrade.
    pub fn can_retry(&self) -> bool {
        self.enabled
            && self.failure.kind.is_some()
            && self.resource.is_none()
            && !self.running
            && !self.migration_paused
            && !matches!(self.recovery(), Some(Recovery::Update | Recovery::Rejoin | Recovery::FinishUpgrade | Recovery::NewLibrary))
    }
    pub fn retry_command(&self) -> &'static str {
        if self.paused { "cloud.resume" } else { "cloud.sync" }
    }
    pub fn short_cause(&self) -> &'static str {
        match self.failure.kind {
            Some(FailureKind::Connectivity) => "Storage is unavailable.",
            Some(FailureKind::Contention) => "Cloud library is busy.",
            Some(FailureKind::Authentication) => "Storage access was refused.",
            Some(FailureKind::Integrity | FailureKind::InvalidHistory) => "Cloud data needs repair.",
            Some(FailureKind::LocalResource) => "Local storage needs attention.",
            Some(FailureKind::Capacity) => "Synchronization limit reached.",
            Some(FailureKind::Unsupported) => "Compatibility needs attention.",
            Some(FailureKind::Cancelled) => "Operation stopped.",
            None => "Open Details for the diagnostic.",
        }
    }
}

/// Forward-compatible diagnostics keep their message even when a newer producer
/// supplies an unknown classification. Unknown provenance never grants a repair action.
fn decode_failure(value: &Value) -> Option<Failure> {
    if let Ok(failure) = serde_json::from_value(value.clone()) {
        return Some(failure);
    }
    let mut failure = Failure::new(value["message"].as_str()?, value["task"].as_u64());
    failure.http_status = value["httpStatus"].as_u64().and_then(|status| status.try_into().ok());
    failure.action = value["action"].as_str().map(str::to_owned);
    Some(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_status::PollHealth;
    use serde_json::json;

    #[test]
    fn empty_library_diagnostics_preserve_provenance_and_select_cause_specific_recovery() {
        for (error, expected, connection) in [
            (CloudFailure::http("GET library head", 403), Recovery::EditConnection, true),
            (CloudFailure::http("GET library head", 404), Recovery::EditConnection, true),
            (CloudFailure::integrity("checkpoint hash mismatch: expected a, read b"), Recovery::RepairRemote, false),
            (CloudFailure::new(FailureKind::Capacity, "entity photo:a exceeds field bound"), Recovery::ReviewCapacity, false),
            (CloudFailure::from(crate::cloud_control::Blocked::UpdateToRead("reader schema 9".into())), Recovery::Update, false),
            (CloudFailure::local("write catalog: disk full"), Recovery::RepairLocal, false),
            (CloudFailure::http("PUT object", 413), Recovery::CheckCompatibility, false),
        ] {
            let mut health = PollHealth::default();
            health.fail_typed(&error, Some(1000), Some(23));
            let data = json!({"summary":{"cloudPhotos":0},"syncEnabled":true,
                "health":{"sync":health.view_at(true,false,false,false,true,Some(1000))}});
            let diagnostic = Diagnostic::from_overview(&data, false).unwrap();
            assert_eq!(diagnostic.failure.message, error.message);
            assert_eq!(diagnostic.failure.http_status, error.http_status);
            assert_eq!(diagnostic.failure.task, Some(23));
            assert_eq!(diagnostic.operation, "Cloud synchronization");
            assert_eq!(diagnostic.recovery(), Some(expected));
            assert_eq!(diagnostic.recovery() == Some(Recovery::EditConnection), connection);
            assert_eq!(diagnostic.advice(), error.action());
            assert_eq!(diagnostic.can_retry(), expected != Recovery::Update);
            let encoded = serde_json::to_value(&diagnostic).unwrap();
            assert_eq!(serde_json::from_value::<Diagnostic>(encoded).unwrap(), diagnostic);
        }
    }

    #[test]
    fn retry_snapshot_and_info_adapter_agree_and_legacy_words_cannot_select_actions() {
        let mut health = PollHealth::default();
        health.fail_typed(&CloudFailure::http("scan", 503), Some(1000), Some(4));
        let view = health.view_at(true, false, false, false, false, Some(2000));
        let data = json!({"syncEnabled":true,"health":{"sync":view}});
        let diagnostic = Diagnostic::from_overview(&data, false).unwrap();
        assert_eq!(diagnostic.retry.seconds_until_retry, Some(1));
        assert_eq!(diagnostic.retry.attempt, 1);
        assert_eq!(diagnostic.retry.max_attempts, 5);
        assert!(diagnostic.retry.scheduled);
        assert_eq!(diagnostic, Diagnostic::from_sync_status(&json!({"enabled":true,"health":view})).unwrap());
        let unknown = Diagnostic::from_overview(&json!({"error":"authentication credentials timeout disk schema capacity"}), true).unwrap();
        assert_eq!(unknown.recovery(), None);
        assert_eq!(unknown.failure.kind, None);
        let future = Diagnostic::from_overview(
            &json!({"health":{"sync":{"lastError":{
            "kind":"futureClassification", "message":"original future error", "httpStatus":418, "task":6,
            "action":"Use the newer reader."}}}}),
            false,
        )
        .unwrap();
        assert_eq!(future.failure.message, "original future error");
        assert_eq!(future.failure.http_status, Some(418));
        assert_eq!(future.advice(), "Use the newer reader.");
        assert_eq!(future.recovery(), None);
        assert!(!future.can_retry());
        let mut paused = diagnostic;
        paused.paused = true;
        assert_eq!(paused.retry_command(), "cloud.resume");
    }

    #[test]
    fn distinct_storage_error_keeps_its_resource_and_upgrade_does_not_offer_ordinary_retry() {
        let mut health = PollHealth::default();
        health.fail_migration(&CloudFailure::connectivity("upgrade interrupted"), Some(3));
        let data = json!({"health":{"sync":health.view_at(true,false,false,true,true,None),
            "uploads":{"activity":{"failures":{"upload:import-a":{"message":"missing local original","task":12}}}}},
            "error":"missing local original"});
        let upgrade = Diagnostic::from_overview(&data, false).unwrap();
        assert_eq!(upgrade.operation, "Cloud library upgrade");
        assert!(!upgrade.can_retry());
        let storage = Diagnostic::from_overview(&data, true).unwrap();
        assert_eq!(storage.failure.message, "missing local original");
        assert_eq!(storage.resource.as_deref(), Some("upload:import-a"));
        assert_eq!(storage.failure.task, Some(12));
        assert_eq!(storage.recovery(), None);
    }
}
