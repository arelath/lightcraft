//! Runtime health belongs to the subsystem/resource that performed the work.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub message: String,
    pub task: Option<u64>,
}
impl Failure {
    pub fn new(message: &str, task: Option<u64>) -> Self {
        Self { message: lightcraft_catalog::uploads::UploadBlock::new(message, false).reason, task }
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
}
impl PollHealth {
    pub fn age_seconds(&self, now_unix_ms: Option<u64>) -> Option<u64> {
        Some(now_unix_ms?.checked_sub(self.last_successful_scan.as_ref()?.at_unix_ms?)? / 1000)
    }
    pub fn success(&mut self, at_unix_ms: Option<u64>, task: Option<u64>) {
        self.last_successful_scan = Some(Scan { at_unix_ms, task, via: "polling" });
        self.last_error = None;
    }
    pub fn publication_scan(&mut self, at_unix_ms: Option<u64>, task: Option<u64>) {
        self.last_successful_scan = Some(Scan { at_unix_ms, task, via: "publication" });
        // Publication scans do not restart a halted automatic polling scheduler.
    }
    pub fn fail(&mut self, message: &str, task: Option<u64>) {
        self.last_error = Some(Failure::new(message, task));
    }
    pub fn view(&self, enabled: bool, running: bool, user_paused: bool, migration_paused: bool, halted: bool) -> Value {
        #[cfg(not(target_arch = "wasm32"))]
        let age = self.age_seconds(now_ms());
        #[cfg(target_arch = "wasm32")]
        let age = None::<u64>;
        let state = if !enabled {
            "disabled"
        } else if user_paused {
            "userPaused"
        } else if migration_paused {
            "migrationPaused"
        } else if halted {
            "halted"
        } else if self.last_error.is_some() {
            "retrying"
        } else if running {
            "checking"
        } else if self.last_successful_scan.is_some() {
            "healthy"
        } else {
            "unverified"
        };
        json!({"state":state,"subsystem":"polling","userPaused":user_paused,"migrationPaused":migration_paused,
            "pollingHalted":enabled && halted && !user_paused && !migration_paused,"running":running,
            "lastSuccessfulRemoteScan":self.last_successful_scan,"secondsSinceLastRemoteScan":age,"lastError":self.last_error})
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
        assert_eq!(retry["state"], "retrying");
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
