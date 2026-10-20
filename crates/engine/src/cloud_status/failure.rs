//! Failure provenance is supplied by producers, never guessed from error text.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    Connectivity,
    Contention,
    Authentication,
    Integrity,
    InvalidHistory,
    LocalResource,
    Unsupported,
    Cancelled,
    /// A synchronization limit was reached: maintenance or a smaller change recovers it.
    Capacity,
}
/// Recovery provenance is independent of message wording and retry policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Recovery {
    Retry,
    EditConnection,
    RepairRemote,
    RepairLocal,
    ReviewCapacity,
    CheckCompatibility,
    Update,
    Rejoin,
    FinishUpgrade,
    NewLibrary,
}
impl Recovery {
    pub fn for_kind(kind: FailureKind) -> Self {
        match kind {
            FailureKind::Connectivity | FailureKind::Contention | FailureKind::Cancelled => Self::Retry,
            FailureKind::Authentication => Self::EditConnection,
            FailureKind::Integrity | FailureKind::InvalidHistory => Self::RepairRemote,
            FailureKind::LocalResource => Self::RepairLocal,
            FailureKind::Capacity => Self::ReviewCapacity,
            FailureKind::Unsupported => Self::CheckCompatibility,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct CloudFailure {
    pub kind: FailureKind,
    pub message: String,
    pub http_status: Option<u16>,
    /// What to do when the kind's general advice would mislead (update LightCraft, rejoin a
    /// moved library, fix the bucket or prefix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<Recovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}
const UPDATE: &str = "Update LightCraft on this device, then use Retry Syncing. Local edits are kept and publish after the update.";
const RETIRED: &str = "This cloud library moved to a new format. Use Migrate Cloud Library… to rejoin it; pending edits are kept and publish there.";
const MAINTENANCE: &str =
    "Another device is upgrading this cloud library. Changes from other devices still arrive; yours publish when the upgrade finishes.";
impl CloudFailure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            kind,
            message: lightcraft_catalog::uploads::UploadBlock::new(&message, false).reason,
            http_status: None,
            recovery: None,
            remedy: None,
            retry_after_ms: None,
        }
    }
    pub fn with_remedy(mut self, remedy: Recovery) -> Self {
        self.remedy = Some(remedy);
        self
    }
    /// Replace the kind's general advice with advice specific to this failure.
    pub fn with_recovery(mut self, recovery: impl Into<String>) -> Self {
        self.recovery = Some(recovery.into());
        self
    }
    pub fn connectivity(message: impl Into<String>) -> Self {
        Self::new(FailureKind::Connectivity, message)
    }
    pub fn local(message: impl Into<String>) -> Self {
        Self::new(FailureKind::LocalResource, message)
    }
    pub fn integrity(message: impl Into<String>) -> Self {
        Self::new(FailureKind::Integrity, message)
    }
    pub fn http(operation: &str, status: u16) -> Self {
        let kind = match status {
            401 | 403 => FailureKind::Authentication,
            409 | 412 => FailureKind::Contention,
            408 | 425 | 429 | 500 | 502 | 503 | 504 => FailureKind::Connectivity,
            // The request never reached readable history: wrong bucket, prefix, region or endpoint.
            301 | 307 | 400 | 404 | 413 | 501 | 505 => FailureKind::Unsupported,
            _ => FailureKind::InvalidHistory,
        };
        let mut error = Self::new(kind, format!("{operation} returned HTTP {status}; pending work retained"));
        error.http_status = Some(status);
        if matches!(status, 301 | 307 | 400 | 404 | 501 | 505) {
            error.remedy = Some(Recovery::EditConnection);
        }
        error.recovery = match status {
            301 | 307 => Some(
                "Storage redirected the request (wrong region or endpoint). Check the region and endpoint in Storage Connection, then use Retry Syncing.",
            ),
            400 => Some("Storage rejected the request. Check the endpoint and region in Storage Connection, then use Retry Syncing."),
            404 => Some(
                "Storage did not find the bucket or library. Check the bucket, endpoint, region and library prefix in Storage Connection, then use Retry Syncing.",
            ),
            413 => Some("Storage refused the request size. Check the provider's object and request size limits; pending work is kept."),
            _ => None,
        }
        .map(str::to_owned);
        error
    }
    pub fn retryable(&self) -> bool {
        matches!(self.kind, FailureKind::Connectivity | FailureKind::Contention)
    }
    /// What the person should do, worded for the Retry Syncing / Resume Syncing buttons.
    pub fn action(&self) -> &str {
        if let Some(recovery) = &self.recovery {
            return recovery;
        }
        match self.kind {
            FailureKind::Connectivity => {
                "Check your connection, the storage endpoint and signer availability. Automatic retries are limited; use Retry Syncing to try again now."
            }
            FailureKind::Contention => {
                "Another device changed the library at the same time. LightCraft retries with fresh remote data; use Retry Syncing after the retry limit."
            }
            FailureKind::Authentication => {
                "Reconnect or repair the storage credentials and permissions, then use Retry Syncing. Pending edits and originals are kept."
            }
            FailureKind::Integrity => "Verify or restore the damaged remote data, then use Retry Syncing. Pending edits and originals are kept.",
            FailureKind::InvalidHistory => {
                "The cloud library's history is not readable as expected. Check the library ID, storage prefix and LightCraft version on each device, then use Retry Syncing. Pending edits are kept."
            }
            FailureKind::LocalResource => {
                "Free disk space or memory, or repair this device's library storage or settings, then use Retry Syncing. Pending edits are kept."
            }
            FailureKind::Unsupported => {
                "Check the endpoint, TLS and protocol support in Storage Connection, then use Retry Syncing with a supported configuration."
            }
            FailureKind::Capacity => {
                "The library reached a synchronization limit; follow the diagnostic (Archive receipt history in Storage tools, finish pending sync, or split large albums). Pending work is kept."
            }
            FailureKind::Cancelled => "Use Retry Syncing when ready. Pending work is kept.",
        }
    }
}
impl From<String> for CloudFailure {
    fn from(message: String) -> Self {
        Self::new(FailureKind::InvalidHistory, message)
    }
}
impl From<&str> for CloudFailure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl From<&crate::assets::AssetError> for CloudFailure {
    fn from(error: &crate::assets::AssetError) -> Self {
        use crate::assets::AssetError;
        let kind = match error {
            AssetError::Http(status) => return Self::http("S3 request", *status),
            AssetError::Throttled { status, retry_after_ms } => {
                let mut failure = Self::http("Storage control request", *status);
                failure.retry_after_ms = Some((*retry_after_ms).min(15 * 60 * 1000));
                return failure;
            }
            AssetError::Blocked(blocked) => return Self::from(blocked.clone()),
            AssetError::Network(_) | AssetError::Pending => FailureKind::Connectivity,
            AssetError::Conflict => FailureKind::Contention,
            AssetError::Unauthorized | AssetError::Unconfigured(_) => FailureKind::Authentication,
            AssetError::Integrity => FailureKind::Integrity,
            AssetError::Missing | AssetError::Invalid(_) => FailureKind::InvalidHistory,
            AssetError::Disk(_) => FailureKind::LocalResource,
            AssetError::Unsupported(_) => FailureKind::Unsupported,
            AssetError::Capacity(_) => FailureKind::Capacity,
            AssetError::Moved(_) => {
                return Self::new(FailureKind::Unsupported, error.to_string()).with_recovery(RETIRED).with_remedy(Recovery::Rejoin);
            }
            AssetError::Cancelled => FailureKind::Cancelled,
        };
        Self::new(kind, error.to_string())
    }
}
impl From<crate::assets::AssetError> for CloudFailure {
    fn from(error: crate::assets::AssetError) -> Self {
        Self::from(&error)
    }
}
impl From<&crate::EngineError> for CloudFailure {
    fn from(error: &crate::EngineError) -> Self {
        match error {
            crate::EngineError::Cloud(error) => error.clone(),
            crate::EngineError::Catalog(error) => Self::from(error),
            _ => Self::local(error.to_string()),
        }
    }
}
impl From<crate::EngineError> for CloudFailure {
    fn from(error: crate::EngineError) -> Self {
        Self::from(&error)
    }
}
/// A full disk, a limit or a newer catalog is not damaged history.
impl From<&lightcraft_catalog::CatalogError> for CloudFailure {
    fn from(error: &lightcraft_catalog::CatalogError) -> Self {
        use lightcraft_catalog::CatalogError;
        match error {
            CatalogError::Capacity(_) => Self::new(FailureKind::Capacity, error.to_string()),
            CatalogError::Io(_) | CatalogError::Corrupt(_) => Self::local(error.to_string()),
            CatalogError::Newer(_) => Self::new(FailureKind::Unsupported, error.to_string()).with_recovery(UPDATE).with_remedy(Recovery::Update),
            _ => Self::new(FailureKind::InvalidHistory, error.to_string()),
        }
    }
}
impl From<lightcraft_catalog::CatalogError> for CloudFailure {
    fn from(error: lightcraft_catalog::CatalogError) -> Self {
        Self::from(&error)
    }
}
/// Why the control envelope stops this step: a newer format needs a newer build, another
/// device holds maintenance or wrote a competing record, or the library moved protocol.
impl From<crate::cloud_control::Blocked> for CloudFailure {
    fn from(blocked: crate::cloud_control::Blocked) -> Self {
        use crate::cloud_control::Blocked;
        let (kind, recovery) = match &blocked {
            Blocked::UpdateToRead(_) | Blocked::UpdateToWrite(_) | Blocked::UpdateToUpgrade(_) => (FailureKind::Unsupported, Some(UPDATE)),
            Blocked::Deleted => {
                (FailureKind::Unsupported, Some("This cloud library was deleted. Use a new library and storage folder; local copies remain."))
            }
            Blocked::Retired(_) => (FailureKind::Unsupported, Some(RETIRED)),
            Blocked::FinishUpgrade(_) => (
                FailureKind::Unsupported,
                Some(
                    "Successor data may already exist. Use Migrate Cloud Library… to finish the same upgrade. Keep both namespaces; do not erase successor data.",
                ),
            ),
            Blocked::Maintenance { .. } => (FailureKind::Contention, Some(MAINTENANCE)),
            Blocked::Conflict(_) => (FailureKind::Contention, None),
            Blocked::Invalid(_) => (FailureKind::InvalidHistory, None),
        };
        let remedy = match &blocked {
            Blocked::UpdateToRead(_) | Blocked::UpdateToWrite(_) | Blocked::UpdateToUpgrade(_) => Recovery::Update,
            Blocked::Deleted => Recovery::NewLibrary,
            Blocked::Retired(_) => Recovery::Rejoin,
            Blocked::FinishUpgrade(_) => Recovery::FinishUpgrade,
            _ => Recovery::for_kind(kind),
        };
        let failure = Self::new(kind, blocked.to_string()).with_remedy(remedy);
        match recovery {
            Some(recovery) => failure.with_recovery(recovery),
            None => failure,
        }
    }
}
impl From<serde_json::Error> for CloudFailure {
    fn from(error: serde_json::Error) -> Self {
        Self::from(error.to_string())
    }
}

#[derive(Default)]
pub struct RetryBudget {
    failures: u8,
    next_at: Option<u64>,
    scheduled: bool,
}
impl RetryBudget {
    pub const MAX: u8 = 5;
    pub fn reset(&mut self) {
        self.failures = 0;
        self.next_at = None;
        self.scheduled = false;
    }
    pub fn scheduled(&self) -> bool {
        self.scheduled
    }
    pub fn suspend(&mut self) {
        self.next_at = None;
        self.scheduled = false;
    }
    pub fn failed(&mut self, error: &CloudFailure, now: Option<u64>) -> Option<u64> {
        self.suspend();
        if !error.retryable() {
            return None;
        }
        self.failures = self.failures.saturating_add(1);
        let seconds = match self.failures {
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            5 => 30,
            _ => return None,
        };
        let delay_ms = (seconds * 1000).max(error.retry_after_ms.unwrap_or(0).min(15 * 60 * 1000));
        self.scheduled = true;
        self.next_at = now.map(|now| now.saturating_add(delay_ms));
        Some(delay_ms.saturating_add(999) / 1000)
    }
    pub fn view(&self, now: Option<u64>) -> serde_json::Value {
        serde_json::json!({"scheduled":self.scheduled,"attempt":self.failures.min(Self::MAX),"maxAttempts":Self::MAX,"exhausted":self.failures>Self::MAX,"nextAtUnixMs":self.next_at,"secondsUntilRetry":self.next_at.zip(now).map(|(at,now)|at.saturating_sub(now).saturating_add(999)/1000)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_budget_is_bounded_and_requires_explicit_reset_after_exhaustion() {
        let mut budget = RetryBudget::default();
        let error = CloudFailure::http("scan", 503);
        for (index, seconds) in [2, 4, 8, 16, 30].into_iter().enumerate() {
            assert_eq!(budget.failed(&error, Some(1000)), Some(seconds));
            assert_eq!(budget.view(Some(1000))["attempt"], index + 1);
            assert_eq!(budget.view(Some(1001))["secondsUntilRetry"], seconds);
        }
        assert_eq!(budget.failed(&error, Some(1000)), None);
        assert_eq!(budget.view(Some(1000))["exhausted"], true);
        assert!(budget.view(Some(1000))["nextAtUnixMs"].is_null());
        assert_eq!(budget.failed(&error, Some(1000)), None);
        budget.reset();
        assert_eq!(budget.failed(&error, Some(1000)), Some(2));
        assert_eq!(budget.failed(&error, Some(u64::MAX)), Some(4));
        assert_eq!(budget.view(Some(u64::MAX))["secondsUntilRetry"], 0);
    }
    #[test]
    fn control_blocks_keep_their_kind() {
        use crate::cloud_control::{Blocked, Plan, Successor};
        let maintenance = Blocked::Maintenance { migration: "m".into(), plan: Plan::Rebase, owner: "o".into() };
        let retired = Blocked::Retired(Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: "x".into() });
        for (blocked, kind) in [
            (Blocked::UpdateToRead("schema 9".into()), FailureKind::Unsupported),
            (Blocked::UpdateToWrite("schema 9".into()), FailureKind::Unsupported),
            (Blocked::UpdateToUpgrade("converter 1 to 2".into()), FailureKind::Unsupported),
            (Blocked::FinishUpgrade("m".into()), FailureKind::Unsupported),
            (retired, FailureKind::Unsupported),
            (maintenance, FailureKind::Contention),
            (Blocked::Conflict("revision 3".into()), FailureKind::Contention),
            (Blocked::Invalid("bad".into()), FailureKind::InvalidHistory),
        ] {
            let message = blocked.to_string();
            let failure = CloudFailure::from(blocked);
            assert_eq!((failure.kind, failure.message), (kind, message));
        }
    }
    #[test]
    fn only_explicit_transient_provenance_retries_and_manual_failures_have_recovery_actions() {
        for status in [408, 425, 429, 500, 502, 503, 504, 409, 412] {
            assert!(CloudFailure::http("scan", status).retryable(), "{status}");
        }
        for status in [301, 400, 401, 403, 404, 413, 501, 505, 599] {
            let error = CloudFailure::http("scan", status);
            assert!(!error.retryable(), "{status}");
            assert!(RetryBudget::default().failed(&error, Some(0)).is_none());
            assert!(!error.action().is_empty());
            assert_eq!(error.http_status, Some(status));
        }
        assert!(
            !CloudFailure::from("network timeout authentication failed integrity collision").retryable(),
            "error wording must never select retry policy"
        );
        for kind in [
            FailureKind::Authentication,
            FailureKind::Integrity,
            FailureKind::InvalidHistory,
            FailureKind::LocalResource,
            FailureKind::Unsupported,
            FailureKind::Cancelled,
        ] {
            assert!(!CloudFailure::new(kind, "message").retryable());
        }
    }
    #[test]
    fn recovery_advice_names_the_real_fix_not_the_kinds_general_advice() {
        use crate::cloud_control::{Blocked, Plan, Successor};
        let newer = CloudFailure::from(Blocked::UpdateToRead("schema 9".into()));
        assert!(newer.action().starts_with("Update LightCraft"), "{}", newer.action());
        assert!(!newer.action().contains("TLS"));
        let upgrade = CloudFailure::from(Blocked::UpdateToUpgrade("converter 1 to 2".into()));
        assert!(upgrade.action().starts_with("Update LightCraft"));
        let finish = CloudFailure::from(Blocked::FinishUpgrade("m".into()));
        assert!(finish.action().contains("finish the same upgrade"));
        let mut migration = super::super::PollHealth::default();
        for error in [&finish, &upgrade] {
            migration.fail_migration(error, Some(1));
            assert_eq!(migration.view_at(true, false, false, true, true, None)["lastError"]["action"], error.action());
        }
        let upgraded = CloudFailure::from(Blocked::Retired(Successor { protocol: "conditional-head-v1".into(), epoch: 2, manifest: "x".into() }));
        assert!(upgraded.action().contains("Migrate Cloud Library"));
        assert_eq!(upgraded.remedy, Some(Recovery::Rejoin));
        let moved = CloudFailure::from(Blocked::Retired(Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: "x".into() }));
        assert!(moved.action().contains("Migrate Cloud Library"), "{}", moved.action());
        let maintenance = CloudFailure::from(Blocked::Maintenance { migration: "m".into(), plan: Plan::Rebase, owner: "o".into() });
        assert!(maintenance.action().contains("upgrading"), "{}", maintenance.action());
        // the advice survives the trip through status JSON and the health view
        let json = serde_json::to_value(&moved).unwrap();
        let back: CloudFailure = serde_json::from_value(json).unwrap();
        assert_eq!(back.action(), moved.action());
        let mut health = super::super::PollHealth::default();
        health.fail_typed(&moved, None, None);
        assert_eq!(health.view_at(true, false, false, false, true, None)["lastError"]["action"], moved.action());
        // an old serialized failure without advice still reads, with the kind's advice
        let old: CloudFailure = serde_json::from_value(serde_json::json!({"kind":"connectivity","message":"m","httpStatus":null})).unwrap();
        assert!(old.action().contains("Retry Syncing"));
    }
    #[test]
    fn wrong_bucket_or_region_is_configuration_not_invalid_history() {
        for (status, words) in [(404, "library prefix"), (400, "endpoint and region"), (301, "region"), (413, "size")] {
            let error = CloudFailure::http("head read", status);
            assert_eq!(error.kind, FailureKind::Unsupported, "{status}");
            assert!(error.action().contains(words), "{status}: {}", error.action());
            assert!(!error.action().contains("history"), "{status}");
        }
        assert_eq!(CloudFailure::http("head read", 599).kind, FailureKind::InvalidHistory);
    }
    #[test]
    fn catalog_failures_keep_their_kind_through_assets_and_the_engine() {
        use crate::assets::AssetError;
        use lightcraft_catalog::CatalogError;
        let cases: [(fn() -> CatalogError, FailureKind); 5] = [
            (|| CatalogError::Capacity("synchronization receipt limit reached".into()), FailureKind::Capacity),
            (|| CatalogError::Io("disk full".into()), FailureKind::LocalResource),
            (|| CatalogError::Corrupt("log line 3".into()), FailureKind::LocalResource),
            (|| CatalogError::Newer("format 99".into()), FailureKind::Unsupported),
            (|| CatalogError::Invalid("pending mutation identity or receipt mismatch".into()), FailureKind::InvalidHistory),
        ];
        for (error, kind) in cases {
            let message = error().to_string();
            let via_asset = CloudFailure::from(AssetError::from(error()));
            let direct = CloudFailure::from(&error());
            let via_engine = CloudFailure::from(crate::EngineError::Catalog(error()));
            assert_eq!(direct.kind, kind, "{message}");
            assert_eq!(via_engine.kind, kind, "{message}");
            assert_eq!(via_asset.kind, kind, "{message}");
        }
        let capacity = CloudFailure::from(CatalogError::Capacity("x".into()));
        assert!(capacity.action().contains("Archive receipt history") && !capacity.retryable());
        assert!(CloudFailure::from(CatalogError::Newer("format 99".into())).action().starts_with("Update LightCraft"));
    }
}
