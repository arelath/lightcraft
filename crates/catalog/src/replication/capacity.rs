//! Exact metadata utilization and recovery guidance. Counting serialization avoids
//! allocating a second synchronization-state buffer merely to check its byte budget.
use super::{MAX_DEVICES, MAX_FIELDS, MAX_PENDING, MAX_RECORDS, MAX_STATE_BYTES, MAX_VERSIONS, SyncState, invalid};
use crate::Result;
use serde::Serialize;
use std::{collections::BTreeSet, io::Write};

struct Counter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(next) = self.bytes.checked_add(bytes.len()) else { return Err(std::io::Error::other("serialized synchronization size overflow")) };
        if next > self.limit {
            self.exceeded = true;
            return Err(std::io::Error::other("synchronization byte limit"));
        }
        self.bytes = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn serialized_bytes(state: &SyncState) -> Result<usize> {
    let mut counter = Counter { bytes: 0, limit: MAX_STATE_BYTES, exceeded: false };
    if let Err(error) = serde_json::to_writer(&mut counter, state) {
        if counter.exceeded {
            return Err(invalid(state_size_recovery(state)));
        }
        return Err(invalid(&format!("cannot measure synchronization state: {error}")));
    }
    Ok(counter.bytes)
}
fn historical(state: &SyncState) -> usize {
    let heads: BTreeSet<_> = state.documents.values().flat_map(|f| f.values()).flat_map(|r| r.0.keys()).collect();
    state.received.keys().filter(|id| !heads.contains(id) && !state.pending.contains_key(*id)).count()
}
fn state_size_recovery(state: &SyncState) -> &'static str {
    if historical(state) > 0 {
        "synchronization state exceeds 64 MiB; current library and outbox retained; synchronize pending uploads/edits, then run cloud.sync with checkpoint:true to archive historical receipts; current documents, conflicts and retired clocks remain retained"
    } else if !state.pending.is_empty() {
        "synchronization state exceeds 64 MiB; current library and outbox retained; finish synchronizing pending uploads/edits and retry; checkpoint maintenance cannot remove current documents, conflicts or retired author clocks"
    } else {
        "synchronization state exceeds 64 MiB with no historical receipts to archive; current library retained; checkpoint maintenance cannot shrink current documents, conflicts or retired author clocks; reduce editable metadata/conflicts or use a separate library"
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    pub kind: &'static str,
    pub severity: &'static str,
    pub used: usize,
    pub limit: usize,
    pub message: &'static str,
    pub recovery: &'static str,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Utilization {
    pub state_bytes: usize,
    pub state_limit_bytes: usize,
    pub state_percent: f64,
    pub documents: usize,
    pub document_limit: usize,
    pub received: usize,
    pub receipt_limit: usize,
    pub historical_receipts: usize,
    pub pending: usize,
    pub pending_limit: usize,
    pub authors: usize,
    pub author_limit: usize,
    pub archived_authors: usize,
    pub archive_roots: usize,
    pub max_entity_fields: usize,
    pub entity_field_limit: usize,
    pub max_field_revisions: usize,
    pub field_revision_limit: usize,
    pub warnings: Vec<Warning>,
}
impl SyncState {
    pub fn utilization(&self) -> Result<Utilization> {
        let state_bytes = serialized_bytes(self)?;
        let historical_receipts = historical(self);
        let mut out = Utilization {
            state_bytes,
            state_limit_bytes: MAX_STATE_BYTES,
            state_percent: state_bytes as f64 * 100. / MAX_STATE_BYTES as f64,
            documents: self.documents.len(),
            document_limit: MAX_RECORDS,
            received: self.received.len(),
            receipt_limit: MAX_RECORDS,
            historical_receipts,
            pending: self.pending.len(),
            pending_limit: MAX_PENDING,
            authors: self.clock.len(),
            author_limit: MAX_DEVICES,
            archived_authors: self.archives.len(),
            archive_roots: self.archives.values().fold(0usize, |n, p| n.saturating_add(p.roots.len())),
            max_entity_fields: self.documents.values().map(|fields| fields.len()).max().unwrap_or(0),
            entity_field_limit: MAX_FIELDS,
            max_field_revisions: self.documents.values().flat_map(|fields| fields.values()).map(|r| r.0.len()).max().unwrap_or(0),
            field_revision_limit: MAX_VERSIONS,
            warnings: Vec::new(),
        };
        for (kind, used, limit, message, recovery) in [
            (
                "stateBytes",
                out.state_bytes,
                MAX_STATE_BYTES,
                "Synchronization metadata is nearing its byte limit.",
                if historical_receipts > 0 {
                    "Finish pending synchronization, then run checkpoint maintenance to archive historical receipts."
                } else {
                    "Finish pending synchronization and resolve conflicts. Checkpoints retain current documents and retired clocks."
                },
            ),
            (
                "receipts",
                out.received,
                MAX_RECORDS,
                "Full synchronization receipts are nearing their limit.",
                if historical_receipts > 0 {
                    "Run checkpoint maintenance to archive historical receipts; remote objects are retained."
                } else {
                    "Finish pending synchronization and resolve conflicts. Current field-head receipts cannot be archived away."
                },
            ),
            (
                "documents",
                out.documents,
                MAX_RECORDS,
                "Synchronization documents are nearing their limit.",
                "Checkpoint maintenance retains document identities and tombstones. Avoid adding documents; a smaller separate library may be needed.",
            ),
            (
                "pending",
                out.pending,
                MAX_PENDING,
                "Unsynchronized changes are nearing their limit.",
                "Finish uploads and synchronization before making more changes; keep the outbox intact.",
            ),
            (
                "authors",
                out.authors,
                MAX_DEVICES,
                "Causal author identities are nearing their limit.",
                "Avoid adding or restoring more devices. Retired author clocks are retained; safe author-slot retirement is unsupported.",
            ),
            (
                "entityFields",
                out.max_entity_fields,
                MAX_FIELDS,
                "One synchronized entity is nearing its field limit.",
                "Avoid adding fields or membership identities to this entity. Removed membership fields stay retained; use smaller separate albums.",
            ),
            (
                "fieldRevisions",
                out.max_field_revisions,
                MAX_VERSIONS,
                "One field has many unresolved causal revisions.",
                "Resolve retained revisions or make a new edit to this field before its revision limit is reached.",
            ),
        ] {
            if used.saturating_mul(100) >= limit.saturating_mul(80) {
                out.warnings.push(Warning {
                    kind,
                    used,
                    limit,
                    message,
                    recovery,
                    severity: if used.saturating_mul(100) >= limit.saturating_mul(95) { "critical" } else { "warning" },
                });
            }
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::{Change, Clock, Receipt, Register};
    #[test]
    fn counting_matches_canonical_unicode_serialization_and_warns_before_author_limit() {
        let mut s = SyncState::new("library".into(), "device".into(), "connection".into()).unwrap();
        s.enqueue(vec![Change { entity: "photo:one".into(), field: "title".into(), base: None, value: serde_json::json!("東京 🦀\n\"") }]).unwrap();
        assert_eq!(s.utilization().unwrap().state_bytes, serde_json::to_vec(&s).unwrap().len());
        for n in 0..(MAX_DEVICES * 4 / 5 + 1) {
            s.clock.insert(format!("retired_{n}"), 1);
        }
        let usage = s.utilization().unwrap();
        assert_eq!(usage.state_bytes, serde_json::to_vec(&s).unwrap().len());
        let warning = usage.warnings.iter().find(|w| w.kind == "authors").unwrap();
        assert_eq!(warning.severity, "warning");
        assert!(warning.recovery.contains("unsupported"));
        for n in 0..MAX_DEVICES {
            s.clock.insert(format!("retired_{n}"), 1);
        }
        assert_eq!(s.utilization().unwrap().warnings.iter().find(|w| w.kind == "authors").unwrap().severity, "critical");
    }
    #[test]
    fn historical_receipt_guidance_and_all_count_bounds_are_specific() {
        let mut s = SyncState::new("library".into(), "device".into(), "connection".into()).unwrap();
        for n in 1..=MAX_RECORDS * 4 / 5 {
            s.received.insert(
                format!("device-{n}"),
                Receipt { digest: "a".repeat(64), context: if n == 1 { Clock::new() } else { Clock::from([("device".into(), n as u64 - 1)]) } },
            );
        }
        s.clock.insert("device".into(), s.received.len() as u64);
        s.validate().unwrap();
        let usage = s.utilization().unwrap();
        assert_eq!(usage.historical_receipts, usage.received);
        assert!(usage.warnings.iter().any(|w| w.kind == "receipts" && w.recovery.contains("checkpoint")));
        for n in s.received.len() + 1..=MAX_RECORDS + 1 {
            s.received.insert(format!("device-{n}"), Receipt { digest: "a".repeat(64), context: Clock::new() });
        }
        assert!(s.validate().unwrap_err().to_string().contains("receipt limit"));
        s.received.clear();
        s.clock.clear();
        for n in 0..=MAX_RECORDS {
            s.documents.insert(format!("photo:{n}"), Default::default());
        }
        let error = s.validate().unwrap_err().to_string();
        assert!(error.contains("document limit") && error.contains("cannot remove"));
        s.documents.clear();
        for n in 0..=MAX_DEVICES {
            s.clock.insert(format!("device_{n}"), 1);
        }
        let error = s.validate().unwrap_err().to_string();
        assert!(error.contains("author limit") && error.contains("unsupported"));
        s.clock.clear();
        for n in 0..=MAX_FIELDS {
            s.documents.entry("album:large".into()).or_default().insert(format!("member:{n}"), Register::default());
        }
        let error = s.validate().unwrap_err().to_string();
        assert!(error.contains("entity field limit") && error.contains("removed membership fields stay retained"));
        s.documents.clear();
        let revisions = (0..=MAX_VERSIONS).map(|n| (format!("device_{n}-1"), serde_json::json!(n))).collect();
        s.documents.entry("photo:one".into()).or_default().insert("rating".into(), Register(revisions));
        assert!(s.validate().unwrap_err().to_string().contains("field revision limit"));
        s.documents.clear();
        let mutation =
            s.enqueue(vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: serde_json::json!(1) }]).unwrap().unwrap();
        for n in 0..=MAX_PENDING {
            s.pending.insert(format!("device-{n}"), mutation.clone());
        }
        let error = s.validate().unwrap_err().to_string();
        assert!(error.contains("pending-change limit") && error.contains("outbox retained"));
    }
}
