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
}
#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct CloudFailure {
    pub kind: FailureKind,
    pub message: String,
    pub http_status: Option<u16>,
}
impl CloudFailure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        let message = message.into();
        Self { kind, message: lightcraft_catalog::uploads::UploadBlock::new(&message, false).reason, http_status: None }
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
            501 | 505 => FailureKind::Unsupported,
            _ => FailureKind::InvalidHistory,
        };
        let mut error = Self::new(kind, format!("{operation} returned HTTP {status}; pending work retained"));
        error.http_status = Some(status);
        error
    }
    pub fn retryable(&self) -> bool {
        matches!(self.kind, FailureKind::Connectivity | FailureKind::Contention)
    }
    pub fn action(&self) -> &'static str {
        match self.kind {
            FailureKind::Connectivity => {
                "Check connectivity, the storage endpoint and signer availability; automatic retries are bounded. Use Resume to retry now."
            }
            FailureKind::Contention => "Another writer changed metadata. Retrying with fresh remote evidence; use Resume after the retry limit.",
            FailureKind::Authentication => "Sign in again or repair storage permissions, then Resume. Pending edits and originals are retained.",
            FailureKind::Integrity => "Verify or restore the damaged remote bytes/history before Resume. Pending edits and originals are retained.",
            FailureKind::InvalidHistory => "Check endpoint, library identity, protocol and software version; repair invalid history before Resume.",
            FailureKind::LocalResource => {
                "Review the local error, free space or memory, or repair storage/configuration, then Resume. Pending edits are retained."
            }
            FailureKind::Unsupported => "Check endpoint, TLS and protocol support, then Resume with a supported configuration.",
            FailureKind::Cancelled => "Resume when ready. Pending work is retained.",
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
            AssetError::Blocked(blocked) => return Self::from(blocked.clone()),
            AssetError::Network(_) | AssetError::Pending => FailureKind::Connectivity,
            AssetError::Conflict => FailureKind::Contention,
            AssetError::Unauthorized | AssetError::Unconfigured(_) => FailureKind::Authentication,
            AssetError::Integrity => FailureKind::Integrity,
            AssetError::Missing | AssetError::Invalid(_) => FailureKind::InvalidHistory,
            AssetError::Disk(_) => FailureKind::LocalResource,
            AssetError::Unsupported(_) => FailureKind::Unsupported,
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
            crate::EngineError::Catalog(_) => Self::from(error.to_string()),
            _ => Self::local(error.to_string()),
        }
    }
}
impl From<crate::EngineError> for CloudFailure {
    fn from(error: crate::EngineError) -> Self {
        Self::from(&error)
    }
}
impl From<lightcraft_catalog::CatalogError> for CloudFailure {
    fn from(error: lightcraft_catalog::CatalogError) -> Self {
        Self::from(error.to_string())
    }
}
/// Why the control envelope stops this step: a newer format needs a newer build, another
/// device holds maintenance or wrote a competing record, or the library moved protocol.
impl From<crate::cloud_control::Blocked> for CloudFailure {
    fn from(blocked: crate::cloud_control::Blocked) -> Self {
        use crate::cloud_control::Blocked;
        let kind = match &blocked {
            Blocked::UpdateToRead(_) | Blocked::UpdateToWrite(_) | Blocked::Retired(_) => FailureKind::Unsupported,
            Blocked::Maintenance { .. } | Blocked::Conflict(_) => FailureKind::Contention,
            Blocked::Invalid(_) => FailureKind::InvalidHistory,
        };
        Self::new(kind, blocked.to_string())
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
        self.scheduled = true;
        self.next_at = now.map(|now| now.saturating_add(seconds * 1000));
        Some(seconds)
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
}
