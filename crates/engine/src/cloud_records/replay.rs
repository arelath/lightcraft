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
