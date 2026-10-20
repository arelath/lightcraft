//! Device-local remote-byte verification policy. Discovery and acknowledgement are
//! independent: a completed verification neither publishes nor acknowledges edits.
use crate::assets::{AssetError, Result};
use lightcraft_catalog::replication::{AppendAudit, AppendScan};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

pub const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;
pub const SLICE_MS: u64 = 15 * 60 * 1000;
pub const DISCOVERY_MS: u64 = 5 * 60 * 1000;
pub const REQUESTS: u64 = 128;
pub const BYTES: u64 = 128 << 20;
pub const SLICES_PER_DAY: u64 = 4;
pub const MAX_RECORD_BYTES: usize = 4096;
pub const DEFERRED: &str = "remote history verification budget reserved; continue verification later";
pub fn expected(local: &lightcraft_catalog::replication::SyncState) -> Result<std::collections::BTreeSet<String>> {
    let mut keys = std::collections::BTreeSet::new();
    if let Some(progress) = &local.append {
        progress.validate(&local.library).map_err(AssetError::from)?;
        keys.extend(progress.scan.checkpoints.iter().cloned());
        for (id, receipt) in &progress.remote.received {
            let (device, sequence) = id.rsplit_once('-').ok_or(AssetError::Integrity)?;
            let sequence = sequence.parse::<u64>().map_err(|_| AssetError::Integrity)?;
            keys.insert(format!("ops/{device}/{sequence:020}-{}.json", receipt.digest));
        }
        for root in progress.remote.archives.values().flat_map(|prefix| &prefix.roots) {
            keys.insert(format!("receipts/{root}.json"));
        }
    }
    Ok(keys)
}
/// A listing cannot silently remove an already acknowledged required record.
pub fn listing_gap(expected: &std::collections::BTreeSet<String>, namespace: &str, after: Option<&str>, before: Option<&str>) -> Result<()> {
    use std::ops::Bound::Excluded;
    let start = format!("{namespace}/");
    let end = format!("{namespace}0");
    let lower = after.filter(|key| *key > start.as_str()).unwrap_or(&start);
    let upper = before.unwrap_or(&end);
    if lower < upper && expected.range::<str, _>((Excluded(lower), Excluded(upper))).next().is_some() {
        return Err(AssetError::Missing);
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Schedule {
    version: u32,
    destination: String,
    pub cursor: AppendAudit,
    next_ms: u64,
    window_ms: u64,
    slices: u64,
    #[serde(default)]
    discovery_ms: u64,
    /// Persisted before I/O. Ordinary cached synchronization cannot clear a known
    /// remote integrity failure; only a completed fresh pass can do that.
    pub failed: bool,
}
impl Schedule {
    pub fn new(destination: String, now: u64) -> Self {
        Self {
            version: 1,
            destination,
            cursor: AppendAudit::default(),
            next_ms: now.saturating_add(WEEK_MS),
            window_ms: now,
            slices: 0,
            discovery_ms: 0,
            failed: false,
        }
    }
    pub fn decode(bytes: &[u8], destination: &str) -> Result<Self> {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(AssetError::Disk("remote verification schedule exceeds its limit".into()));
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| AssetError::Disk("remote verification schedule is damaged".into()))?;
        let mut cursor = value.cursor.clone();
        if let Some(key) = cursor.after.as_ref().and_then(|s| s.strip_prefix("receipts/")) {
            let digest = key.strip_suffix(".json").ok_or(AssetError::Integrity)?;
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return Err(AssetError::Integrity);
            }
            cursor.after = None;
        }
        let scan = AppendScan { audit: cursor, ..Default::default() };
        if value.version != 1 || value.destination != destination || value.slices > SLICES_PER_DAY || scan.validate().is_err() {
            return Err(AssetError::Disk("remote verification schedule has an unsupported identity or cursor".into()));
        }
        Ok(value)
    }
    /// Reserve an entire slice durably before starting I/O. Unused allowance is
    /// deliberately not refunded on success, failure, cancellation or restart.
    pub fn reserve(&mut self, now: u64, explicit: bool) -> Result<bool> {
        if now < self.window_ms || now.saturating_sub(self.window_ms) >= DAY_MS {
            // A backwards wall clock must not replenish allowance.
            if now < self.window_ms {
                return Ok(false);
            }
            self.window_ms = now;
            self.slices = 0;
        }
        if self.slices >= SLICES_PER_DAY || (!explicit && now < self.next_ms) {
            return Ok(false);
        }
        if self.cursor.complete {
            self.cursor.round = self.cursor.round.checked_add(1).ok_or_else(|| AssetError::Disk("remote verification round exhausted".into()))?;
            self.cursor.complete = false;
            self.cursor.after = None;
        }
        self.slices = self.slices.saturating_add(1);
        self.next_ms = now.saturating_add(SLICE_MS);
        Ok(true)
    }
    pub fn discover(&mut self, now: u64, manual: bool) -> bool {
        if !manual && now < self.discovery_ms {
            return false;
        }
        self.discovery_ms = now.saturating_add(DISCOVERY_MS);
        true
    }
    pub fn completed_slice(&mut self, cursor: AppendAudit, now: u64) {
        self.cursor = cursor;
        if self.cursor.complete {
            self.next_ms = now.saturating_add(WEEK_MS);
            self.failed = false;
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(|e| AssetError::Disk(e.to_string()))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(AssetError::Disk("remote verification schedule exceeds its limit".into()));
        }
        Ok(bytes)
    }
}

/// Worst-case admission before each request; nested receipt reads share this
/// allowance. Successful bodies refund only unused in-slice byte reservation.
pub struct Budget(Mutex<(u64, u64)>);
impl Default for Budget {
    fn default() -> Self {
        Self(Mutex::new((REQUESTS, BYTES)))
    }
}
impl Budget {
    pub fn admit(&self, max: usize) -> Result<()> {
        let mut remaining = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if remaining.0 == 0 || remaining.1 < max as u64 {
            return Err(AssetError::HistoryBudget);
        }
        remaining.0 -= 1;
        remaining.1 -= max as u64;
        Ok(())
    }
    pub fn read(&self, max: usize, actual: usize) {
        let mut remaining = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        remaining.1 = remaining.1.saturating_add(max.saturating_sub(actual) as u64);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native;
#[cfg(test)]
mod tests;
