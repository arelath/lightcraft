//! Bounded provider-independent history windows; decoded batches are never retained.
use super::RecordRef;
use lightcraft_catalog::assets::BlobId;
use std::collections::VecDeque;

pub const AUTO_CHECKPOINT_BATCHES: u64 = 1024;
pub const WINDOW_BATCHES: u64 = 4096;
pub const MAX_REFERENCE_BYTES: usize = 4096;

/// Backward traversal retains only the oldest window reached so far. Replay pops
/// oldest-first; a longer tail is handled through further authenticated windows.
pub struct Window {
    references: VecDeque<RecordRef>,
    count: u64,
}
impl Default for Window {
    fn default() -> Self {
        Self { references: VecDeque::with_capacity(WINDOW_BATCHES as usize), count: 0 }
    }
}
impl Window {
    pub fn push(&mut self, reference: &RecordRef) -> Result<(), String> {
        if serde_json::to_vec(reference).map_err(|e| e.to_string())?.len() > MAX_REFERENCE_BYTES {
            return Err("history reference exceeds its bounded size".into());
        }
        self.count = self.count.checked_add(1).ok_or("history length exhausted")?;
        if self.references.len() >= WINDOW_BATCHES as usize {
            self.references.pop_front();
        }
        self.references.push_back(reference.clone());
        Ok(())
    }
    pub fn pop_oldest(&mut self) -> Option<RecordRef> {
        self.references.pop_back()
    }
    pub fn count(&self) -> u64 {
        self.count
    }
    /// Allocated reference storage, excluding allocator bookkeeping.
    pub fn resident_bytes(&self) -> usize {
        self.references.capacity().saturating_mul(std::mem::size_of::<RecordRef>()).saturating_add(
            self.references.iter().map(|reference| reference.key.capacity().saturating_add(reference.hash.0.capacity())).sum::<usize>(),
        )
    }
}
/// Brent cycle detection requires one hash instead of a history-sized seen set.
#[derive(Default)]
pub struct Cycle {
    anchor: Option<BlobId>,
    power: u64,
    steps: u64,
}
impl Cycle {
    pub fn visit(&mut self, hash: &BlobId) -> Result<(), String> {
        if self.anchor.as_ref() == Some(hash) {
            return Err("remote catalog chain contains a cycle".into());
        }
        self.steps = self.steps.saturating_add(1);
        if self.anchor.is_none() || self.steps == self.power {
            self.anchor = Some(hash.clone());
            self.power = self.power.saturating_mul(2).max(1);
            self.steps = 0;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_histories_keep_a_constant_reference_window_and_reverse_the_oldest_prefix() {
        let mut window = Window::default();
        for n in 0..100_000 {
            window
                .push(&RecordRef { key: format!("library/catalog/changes/{n:064x}.json"), hash: BlobId(format!("{n:064x}")), size: 900 << 10 })
                .unwrap();
            if matches!(n, 4095 | 99999) {
                assert!(window.resident_bytes() <= WINDOW_BATCHES as usize * (MAX_REFERENCE_BYTES + std::mem::size_of::<RecordRef>()));
            }
        }
        assert_eq!(window.count(), 100_000);
        for n in (100_000 - WINDOW_BATCHES..100_000).rev() {
            assert_eq!(window.pop_oldest().unwrap().hash.0, format!("{n:064x}"));
        }
        assert!(window.pop_oldest().is_none());
    }
    #[test]
    fn overlong_reference_is_rejected_without_consuming_window_space() {
        let mut window = Window::default();
        assert!(window.push(&RecordRef { key: "x".repeat(MAX_REFERENCE_BYTES + 1), hash: BlobId("a".repeat(64)), size: 1 }).is_err());
        assert_eq!(window.count(), 0);
        assert!(window.pop_oldest().is_none());
    }
    #[test]
    fn cycle_detection_is_constant_space_and_does_not_reject_long_acyclic_histories() {
        let mut cycle = Cycle::default();
        for n in 0..100_000 {
            cycle.visit(&BlobId(format!("{n:064x}"))).unwrap();
        }
        for period in [1, 2, 17, 4097] {
            let mut cycle = Cycle::default();
            assert!((0..period * 4).any(|n| cycle.visit(&BlobId(format!("{:064x}", n % period))).is_err()));
        }
    }
}

/// Browser replay retains the oldest verified prefix in a bounded byte window.
/// No second download is needed, including explicit fresh verification. A longer
/// tail uses the existing authenticated prefix-checkpoint recovery path.
pub struct BodyWindow {
    records: VecDeque<(RecordRef, Vec<u8>)>,
    bytes: usize,
    count: u64,
    limit: usize,
}
impl Default for BodyWindow {
    fn default() -> Self {
        Self { records: VecDeque::new(), bytes: 0, count: 0, limit: 64 << 20 }
    }
}
impl BodyWindow {
    pub fn push(&mut self, reference: &RecordRef, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.len() > 2 << 20
            || reference.size != bytes.len() as u64
            || serde_json::to_vec(reference).map_err(|e| e.to_string())?.len() > MAX_REFERENCE_BYTES
        {
            return Err("history body/reference exceeds its bounded size".into());
        }
        self.count = self.count.checked_add(1).ok_or("history length exhausted")?;
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or("history byte window exhausted")?;
        self.records.push_back((reference.clone(), bytes));
        while self.records.len() > WINDOW_BATCHES as usize || self.bytes > self.limit {
            let Some((_, bytes)) = self.records.pop_front() else { break };
            self.bytes = self.bytes.saturating_sub(bytes.len());
        }
        if self.records.is_empty() {
            return Err("history object cannot fit its replay window".into());
        }
        Ok(())
    }
    pub fn pop_oldest(&mut self) -> Option<(RecordRef, Vec<u8>)> {
        let value = self.records.pop_back()?;
        self.bytes = self.bytes.saturating_sub(value.1.len());
        Some(value)
    }
    pub fn count(&self) -> u64 {
        self.count
    }
    pub fn retained(&self) -> u64 {
        self.records.len() as u64
    }
}
#[cfg(test)]
mod body_tests {
    use super::*;
    #[test]
    fn byte_and_record_limits_retain_the_oldest_replay_prefix_without_duplicate_reads() {
        let mut window = BodyWindow { limit: 10, ..Default::default() };
        for n in 0..100 {
            window.push(&RecordRef { key: format!("library/{n}"), hash: BlobId(format!("{n:064x}")), size: 3 }, vec![n; 3]).unwrap();
        }
        assert_eq!(window.count(), 100);
        assert_eq!(window.retained(), 3);
        assert_eq!(window.bytes, 9);
        for n in (97..100).rev() {
            let (r, b) = window.pop_oldest().unwrap();
            assert_eq!(r.key, format!("library/{n}"));
            assert_eq!(b, vec![n; 3]);
        }
        assert_eq!(window.bytes, 0);
        assert!(window.pop_oldest().is_none());
        let mut records = BodyWindow::default();
        for n in 0..10_000 {
            records.push(&RecordRef { key: format!("library/{n}"), hash: BlobId(format!("{n:064x}")), size: 0 }, Vec::new()).unwrap();
        }
        assert_eq!(records.retained(), WINDOW_BATCHES);
        assert_eq!(records.count(), 10_000);
        assert!(records.push(&RecordRef { key: "x".into(), hash: BlobId("a".repeat(64)), size: 1 }, vec![]).is_err());
    }
}
