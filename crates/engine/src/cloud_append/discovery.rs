//! Shared device-local checkpoint consumption and bounded, resumable historical audits.
use super::{MAX_RECORDS, Planner, Result, encode, invalid};
use lightcraft_catalog::replication::{AppendAudit, AppendProgress, AppendScan};

pub const AUDIT_RECORDS: usize = 64;
pub const AUDIT_BYTES: usize = 8 << 20;
pub const AUDIT_PAGES: usize = 16;

pub fn relative<'a>(root: &str, key: &'a str) -> Result<&'a str> {
    key.strip_prefix(root).and_then(|s| s.strip_prefix('/')).ok_or_else(|| invalid("out-of-scope append discovery key"))
}
pub fn consumed(planner: &Planner, root: &str, key: &str) -> Result<bool> {
    Ok(planner.scan.checkpoints.contains(relative(root, key)?))
}
/// Call only after hash verification, causal certification and successful merge.
pub fn consume(planner: &mut Planner, root: &str, key: &str) -> Result<()> {
    if planner.scan.checkpoints.len() >= MAX_RECORDS && !consumed(planner, root, key)? {
        return Err(invalid("consumed checkpoint limit reached; local work retained"));
    }
    planner.scan.checkpoints.insert(relative(root, key)?.into());
    Ok(())
}
/// Local consumption/audit hints are never transmitted as shared evidence.
pub fn checkpoint_bytes(progress: &AppendProgress) -> Result<Vec<u8>> {
    let mut checkpoint = progress.clone();
    checkpoint.scan = AppendScan::default();
    encode(&checkpoint)
}

pub struct Audit {
    state: AppendAudit,
    records: usize,
    bytes: usize,
    pages: usize,
}
impl Audit {
    pub fn new(scan: &AppendScan, requested: bool) -> Result<Self> {
        let mut state = scan.audit.clone();
        if requested && state.complete {
            state.round = state.round.checked_add(1).ok_or_else(|| invalid("append audit epoch exhausted"))?;
            state.complete = false;
            state.after = None;
        }
        Ok(Self { state, records: 0, bytes: 0, pages: 0 })
    }
    pub fn available(&self) -> bool {
        !self.state.complete && self.records < AUDIT_RECORDS && self.bytes < AUDIT_BYTES
    }
    pub fn start_page(&mut self) -> Result<bool> {
        if self.pages >= AUDIT_PAGES {
            if self.records == 0 {
                return Err(invalid("append audit listing made no progress within its 16-page budget; retry or check the storage provider"));
            }
            return Ok(false);
        }
        self.pages = self.pages.saturating_add(1);
        Ok(true)
    }
    pub fn request(&self, root: &str, namespace: &str) -> Option<super::listing::ListRequest> {
        if !self.available() {
            return None;
        }
        let prefix = format!("{namespace}/");
        let after = self.state.after.as_ref();
        if after.is_some_and(|s| s.as_str() >= prefix.as_str() && !s.starts_with(&prefix)) {
            return None;
        }
        Some(super::listing::ListRequest {
            prefix: format!("{root}/{prefix}"),
            delimiter: false,
            continuation: None,
            start_after: after.filter(|s| s.starts_with(&prefix)).map(|s| format!("{root}/{s}")),
        })
    }
    pub fn verified(&mut self, root: &str, key: &str, bytes: usize) -> Result<()> {
        let key = relative(root, key)?;
        if self.state.after.as_ref().is_some_and(|after| key <= after.as_str()) {
            return Err(invalid("append audit listing did not advance"));
        }
        self.records = self.records.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.state.after = Some(key.into());
        Ok(())
    }
    pub fn finished(&mut self, namespace: &str) {
        if namespace == "ops" {
            self.state.complete = true;
        }
    }
    pub fn save(self, scan: &mut AppendScan) {
        scan.audit = self.state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_checkpoint_hints_never_become_local_consumption_evidence() {
        let local = lightcraft_catalog::replication::SyncState::new("library".into(), "a".into(), "storage".into()).unwrap();
        let mut planner = Planner::new(&local).unwrap();
        let mut remote = planner.progress().unwrap();
        remote.scan.checkpoints.insert(format!("checkpoints/a/{}.json", "a".repeat(64)));
        planner.checkpoint(&encode(&remote).unwrap()).unwrap();
        assert!(planner.scan.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&checkpoint_bytes(&remote).unwrap()).unwrap();
        assert!(value.get("scan").is_none());
    }
    #[test]
    fn audits_resume_with_key_cursors_and_respect_record_and_byte_budgets() {
        let mut scan = AppendScan::default();
        let mut audit = Audit::new(&scan, true).unwrap();
        for n in 1..=AUDIT_RECORDS {
            assert!(audit.available());
            audit.verified("root", &format!("root/ops/a/{n:020}-{}.json", "a".repeat(64)), 100).unwrap();
        }
        assert!(!audit.available());
        audit.save(&mut scan);
        let loaded = serde_json::from_slice::<AppendScan>(&serde_json::to_vec(&scan).unwrap()).unwrap();
        let mut resumed = Audit::new(&loaded, true).unwrap();
        assert!(resumed.request("root", "checkpoints").is_none());
        assert_eq!(resumed.request("root", "ops").unwrap().start_after, scan.audit.after.as_ref().map(|s| format!("root/{s}")));
        resumed.verified("root", &format!("root/ops/b/{:020}-{}.json", 1, "b".repeat(64)), AUDIT_BYTES).unwrap();
        assert!(!resumed.available());
        resumed.finished("ops");
        resumed.save(&mut scan);
        assert!(!Audit::new(&scan, false).unwrap().available());
        let next = Audit::new(&scan, true).unwrap();
        assert!(next.request("root", "checkpoints").unwrap().start_after.is_none());
    }
    #[test]
    fn empty_page_chains_cannot_exhaust_an_unbounded_request_budget() {
        let mut audit = Audit::new(&AppendScan::default(), true).unwrap();
        for _ in 0..AUDIT_PAGES {
            assert!(audit.start_page().unwrap());
        }
        assert!(audit.start_page().is_err());
        audit.verified("root", &format!("root/ops/a/{:020}-{}.json", 1, "a".repeat(64)), 1).unwrap();
        assert!(!audit.start_page().unwrap());
    }
}
