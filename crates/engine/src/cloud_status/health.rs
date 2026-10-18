//! Runtime health belongs to the subsystem/resource that performed the work.
use super::{CloudFailure, FailureKind, Recovery, RetryBudget};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub message: String,
    #[serde(default)]
    pub task: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FailureKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Recovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}
impl Failure {
    /// Normalize producer-supplied provenance for status and readable publication blocks.
    pub fn classified(error: &CloudFailure, task: Option<u64>) -> Self {
        let mut failure = Self::new(&error.message, task);
        failure.kind = Some(error.kind);
        failure.http_status = error.http_status;
        failure.action = Some(error.action().into());
        failure.retryable = error.retryable();
        failure.recovery = Some(error.remedy.unwrap_or_else(|| Recovery::for_kind(error.kind)));
        failure
    }
    pub fn new(message: &str, task: Option<u64>) -> Self {
        Self {
            message: lightcraft_catalog::uploads::UploadBlock::new(message, false).reason,
            task,
            kind: None,
            http_status: None,
            action: None,
            retryable: false,
            recovery: None,
            operation: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scan {
    pub at_unix_ms: Option<u64>,
    pub task: Option<u64>,
    pub via: &'static str,
}

#[derive(Default)]
pub struct PollHealth {
    last_successful_scan: Option<Scan>,
    last_error: Option<Failure>,
    retry: RetryBudget,
}
impl PollHealth {
    pub fn age_seconds(&self, now_unix_ms: Option<u64>) -> Option<u64> {
        Some(now_unix_ms?.checked_sub(self.last_successful_scan.as_ref()?.at_unix_ms?)? / 1000)
    }
    pub fn success(&mut self, at_unix_ms: Option<u64>, task: Option<u64>) {
        self.last_successful_scan = Some(Scan { at_unix_ms, task, via: "polling" });
        self.last_error = None;
        self.retry.reset();
    }
    pub fn publication_scan(&mut self, at_unix_ms: Option<u64>, task: Option<u64>) {
        self.last_successful_scan = Some(Scan { at_unix_ms, task, via: "publication" });
        // Publication scans do not restart a halted automatic polling scheduler.
    }
    pub fn fail(&mut self, message: &str, task: Option<u64>) {
        let _ = self.fail_typed(&CloudFailure::local(message), None, task);
    }
    pub fn fail_typed(&mut self, error: &CloudFailure, now: Option<u64>, task: Option<u64>) -> Option<u64> {
        self.last_error = Some(Failure::classified(error, task));
        self.retry.failed(error, now)
    }
    /// Migration never falls back to ordinary automatic polling after a failed cutover.
    pub fn fail_migration(&mut self, error: &CloudFailure, task: Option<u64>) {
        self.last_error = Some(Failure::classified(error, task));
        self.retry.suspend();
        if let Some(failure) = &mut self.last_error {
            failure.operation = Some("Cloud library upgrade".into());
            failure.retryable = false;
            if error.recovery.is_none() {
                failure.action = Some(format!(
                    "{} Then explicitly retry the upgrade; cancellation is available only before successor publication. Local work is retained.",
                    error.action()
                ));
            }
        }
    }
    pub fn reset_retries(&mut self) {
        self.retry.reset();
    }
    pub fn view(&self, enabled: bool, running: bool, user_paused: bool, migration_paused: bool, halted: bool) -> Value {
        #[cfg(not(target_arch = "wasm32"))]
        let now = now_ms();
        #[cfg(target_arch = "wasm32")]
        let now = None;
        self.view_at(enabled, running, user_paused, migration_paused, halted, now)
    }
    pub fn view_at(&self, enabled: bool, running: bool, user_paused: bool, migration_paused: bool, halted: bool, now: Option<u64>) -> Value {
        let age = self.age_seconds(now);
        let state = if !enabled {
            "disabled"
        } else if user_paused {
            "userPaused"
        } else if migration_paused {
            "migrationPaused"
        } else if halted {
            "halted"
        } else if running {
            "checking"
        } else if self.retry.scheduled() {
            "retrying"
        } else if self.last_error.is_some() {
            "unverified"
        } else if self.last_successful_scan.is_some() {
            "healthy"
        } else {
            "unverified"
        };
        json!({"state":state,"subsystem":"polling","userPaused":user_paused,"migrationPaused":migration_paused,
            "pollingHalted":enabled && halted && !user_paused && !migration_paused,"running":running,
            "lastSuccessfulRemoteScan":self.last_successful_scan,"secondsSinceLastRemoteScan":age,"lastError":self.last_error,"retry":self.retry.view(now)})
    }
}

/// Successful work clears only failures for the same resource, never another import.
#[derive(Default)]
pub struct ResourceHealth {
    pub(crate) failures: BTreeMap<String, Failure>,
    failed_resources: BTreeSet<String>,
    overflow: bool,
}
impl ResourceHealth {
    pub fn fail(&mut self, resource: String, message: &str, task: Option<u64>) {
        if resource.is_empty()
            || resource.len() > 256
            || resource.contains('\0')
            || (self.failed_resources.len() >= 100_256 && !self.failed_resources.contains(&resource))
        {
            self.overflow = true;
            return;
        }
        self.failed_resources.insert(resource.clone());
        // Bound runtime diagnostics independently of activity-history retention.
        if self.failures.len() < 32 || self.failures.contains_key(&resource) {
            self.failures.insert(resource, Failure::new(message, task));
        }
    }
    pub fn success(&mut self, resource: &str) {
        self.failures.remove(resource);
        self.failed_resources.remove(resource);
    }
    pub fn view(&self) -> Value {
        json!({"failures":self.failures,"failedResources":self.failed_resources.len().saturating_add(usize::from(self.overflow)),"firstFailedResource":self.failed_resources.first(),"detailsTruncated":self.failed_resources.len()>self.failures.len(),"overflow":self.overflow})
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn now_ms() -> Option<u64> {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|elapsed| elapsed.as_millis().try_into().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_retries_preserve_failure_and_scan_evidence_until_polling_recovers() {
        let mut health = PollHealth::default();
        health.success(Some(1000), Some(1));
        assert_eq!(health.fail_typed(&CloudFailure::http("scan", 503), Some(2000), Some(2)), Some(2));
        health.publication_scan(Some(2500), Some(3));
        let view = health.view_at(true, false, false, false, false, Some(3000));
        assert_eq!(view["state"], "retrying");
        assert_eq!(view["retry"]["secondsUntilRetry"], 1);
        assert_eq!(view["lastError"]["httpStatus"], 503);
        assert_eq!(view["lastSuccessfulRemoteScan"]["task"], 3);
        assert_eq!(health.fail_typed(&CloudFailure::connectivity("offline"), None, Some(4)), Some(4));
        let view = health.view_at(true, false, false, false, false, None);
        assert_eq!(view["state"], "retrying");
        assert!(view["retry"]["secondsUntilRetry"].is_null());
        health.success(Some(5000), Some(5));
        let view = health.view_at(true, false, false, false, false, Some(5000));
        assert_eq!(view["state"], "healthy");
        assert!(view["lastError"].is_null());
        assert_eq!(view["retry"]["attempt"], 0);
    }
    #[test]
    fn permanent_and_migration_failures_require_explicit_recovery() {
        let mut health = PollHealth::default();
        assert_eq!(health.fail_typed(&CloudFailure::http("scan", 401), Some(0), None), None);
        let view = health.view_at(true, false, false, false, true, Some(0));
        assert_eq!(view["state"], "halted");
        assert_eq!(view["lastError"]["kind"], "authentication");
        assert_eq!(view["lastError"]["retryable"], false);
        health.reset_retries();
        health.fail_migration(&CloudFailure::connectivity("offline during migration"), Some(2));
        let view = health.view_at(true, false, false, true, true, Some(0));
        assert_eq!(view["state"], "migrationPaused");
        assert_eq!(view["retry"]["scheduled"], false);
        assert_eq!(view["retry"]["attempt"], 0);
        assert_eq!(view["lastError"]["retryable"], false);
    }
    #[test]
    fn recovery_and_scan_evidence_are_subsystem_and_resource_specific() {
        let mut polling = PollHealth::default();
        assert_eq!(polling.view(true, false, false, false, false)["state"], "unverified");
        polling.success(Some(100), Some(1));
        polling.fail("remote authentication rejected", Some(2));
        let mut uploads = ResourceHealth::default();
        uploads.fail("first".into(), "manifest unavailable", Some(3));
        uploads.success("second");
        assert_eq!(uploads.view()["failedResources"], 1);
        assert_eq!(polling.view(true, false, false, false, true)["state"], "halted");
        polling.publication_scan(Some(150), Some(8));
        assert_eq!(polling.view(true, false, false, false, true)["state"], "halted");
        let retry = polling.view(true, true, false, false, false);
        assert_eq!(retry["state"], "checking");
        assert_eq!(retry["lastSuccessfulRemoteScan"]["atUnixMs"], 150);
        assert_eq!(retry["lastSuccessfulRemoteScan"]["via"], "publication");
        assert_eq!(polling.age_seconds(Some(149)), None);
        assert_eq!(polling.view(true, false, true, false, true)["state"], "userPaused");
        assert_eq!(polling.view(true, false, false, true, true)["state"], "migrationPaused");
        polling.success(Some(200), Some(4));
        assert_eq!(polling.view(true, false, false, false, false)["state"], "healthy");
        uploads.success("first");
        assert_eq!(uploads.view()["failedResources"], 0);
    }
    #[test]
    fn bounded_details_do_not_hide_failures_after_other_resources_recover() {
        let mut health = ResourceHealth::default();
        for n in 0..33 {
            health.fail(format!("import-{n}"), "verification failed", None);
        }
        for n in 0..32 {
            health.success(&format!("import-{n}"));
        }
        assert_eq!(health.view()["failedResources"], 1);
        health.success("import-32");
        assert_eq!(health.view()["failedResources"], 0);
    }
}
