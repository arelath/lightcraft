//! Incremental, exact archive certification. No document merge runs while a
//! witness is missing, and each path resumes at its missing immutable node.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    pub forest_builds: usize,
    pub witness_checks: usize,
    pub state_merges: usize,
}

pub struct MergeResult {
    pub states: Vec<SyncState>,
    pub remote: Portable,
    pub stats: MergeStats,
}

struct Target {
    state: SyncState,
    archives: Archives,
}

struct Step {
    author: String,
    start: u64,
    width: u64,
    root: String,
    first: u64,
    count: u64,
    expected: Option<String>,
    require_node: bool,
}

/// Owns the snapshots it certifies. An authenticated archive result cannot be
/// substituted onto another state or manufactured by a transport caller.
pub struct MergeSession {
    remote: Portable,
    remote_plan: Option<Plan>,
    nodes: Witness,
    supplied: BTreeSet<String>,
    targets: Vec<Target>,
    ready: Vec<Step>,
    waiting: BTreeMap<String, Vec<Step>>,
    stats: MergeStats,
    failed: bool,
}

impl MergeSession {
    pub fn new(local: &SyncState, remote: &Portable) -> Result<Self> {
        remote.validate_structure()?;
        if remote.proofs.len() > MAX_WITNESS_NODES {
            return Err(invalid("receipt archive proof budget exceeded; local work retained"));
        }
        let mut session = Self {
            remote: remote.clone(),
            remote_plan: None,
            nodes: Witness::new(),
            supplied: BTreeSet::new(),
            targets: Vec::new(),
            ready: Vec::new(),
            waiting: BTreeMap::new(),
            stats: MergeStats::default(),
            failed: false,
        };
        for (hash, node) in &remote.proofs {
            session.provide(hash, node.clone())?;
        }
        session.remote.proofs.clear();
        session.include(local)?;
        Ok(session)
    }

    /// Also certify retained append publication progress with the same remote
    /// forest and immutable node cache. This does not acknowledge either outbox.
    pub fn include(&mut self, local: &SyncState) -> Result<()> {
        if local.library != self.remote.library {
            return Err(invalid("checkpoint belongs to another library"));
        }
        if self.targets.len() >= 2 {
            return Err(invalid("too many archive certification targets"));
        }
        let mut result = local.archives.clone();
        if !local.archives.is_empty() || !self.remote.archives.is_empty() {
            if self.remote_plan.is_none() {
                self.stats.forest_builds += 1;
                let mut remote_plan = plan_parts(&self.remote.library, &self.remote.archives, &self.remote.received)?;
                for (hash, node) in std::mem::take(&mut remote_plan.objects) {
                    self.install(hash, node);
                }
                self.remote_plan = Some(remote_plan);
            }
            self.stats.forest_builds += 1;
            let lp = plan_parts(&local.library, &local.archives, &local.received)?;
            let rp = self.remote_plan.as_ref().ok_or_else(|| invalid("missing remote receipt forest"))?;
            let mut steps = Vec::new();
            for (author, prefix) in &self.remote.archives {
                let known = local.archives.get(author).map(ranges).transpose()?.unwrap_or_default();
                for (first, count, root) in ranges(prefix)? {
                    if !known.contains(&(first, count, root)) {
                        steps.push(Step {
                            author: author.clone(),
                            start: first,
                            width: count,
                            root: root.into(),
                            first,
                            count,
                            expected: None,
                            require_node: true,
                        });
                    }
                }
            }
            for (author, left) in &lp.archives {
                if let Some(right) = rp.archives.get(author) {
                    let (smaller, larger) = if left.through <= right.through { (left, right) } else { (right, left) };
                    for (first, count, root) in ranges(smaller)? {
                        steps.push(Self::path(author, larger, first, count, root.into())?);
                    }
                }
            }
            for (author, prefix) in &self.remote.archives {
                if result.get(author).is_none_or(|old| old.through < prefix.through) {
                    result.insert(author.clone(), prefix.clone());
                }
            }
            for (id, receipt) in &local.received {
                let stamp = super::super::append::stamp(id)?;
                if covers(&self.remote.archives, id) && lp.archives.get(&stamp.device).is_none_or(|p| stamp.sequence > p.through) {
                    steps.push(Self::receipt_path(&local.library, &self.remote.archives, id, receipt)?);
                }
            }
            for (id, receipt) in &self.remote.received {
                let stamp = super::super::append::stamp(id)?;
                let known = local.received.get(id) == Some(receipt) && lp.archives.get(&stamp.device).is_some_and(|p| stamp.sequence <= p.through);
                if covers(&result, id) && !known {
                    steps.push(Self::receipt_path(&local.library, &result, id, receipt)?);
                }
            }
            for (hash, node) in lp.objects {
                self.install(hash, node);
            }
            self.ready.extend(steps);
        }
        self.targets.push(Target { state: local.clone(), archives: result });
        Ok(())
    }

    fn path(author: &str, prefix: &Prefix, first: u64, count: u64, expected: String) -> Result<Step> {
        let end = first.checked_add(count - 1).ok_or_else(|| invalid("archive range overflow"))?;
        let (start, width, root) = ranges(prefix)?
            .into_iter()
            .find(|(start, width, _)| first >= *start && end <= start.saturating_add(*width - 1))
            .ok_or_else(|| invalid("archive does not cover requested receipt"))?;
        Ok(Step { author: author.into(), start, width, root: root.into(), first, count, expected: Some(expected), require_node: false })
    }

    fn receipt_path(library: &str, archives: &Archives, id: &str, receipt: &Receipt) -> Result<Step> {
        let stamp = super::super::append::stamp(id)?;
        if !valid_clock(&receipt.context)
            || !valid_hash(&receipt.digest)
            || receipt.context.get(&stamp.device).copied().unwrap_or(0) != stamp.sequence - 1
        {
            return Err(invalid("invalid archived receipt context/digest"));
        }
        let prefix = archives.get(&stamp.device).ok_or_else(|| invalid("receipt has no archive"))?;
        let leaf = Node {
            version: 1,
            library: library.into(),
            author: stamp.device.clone(),
            first: stamp.sequence,
            count: 1,
            value: Value::Leaf { digest: receipt.digest.clone(), context: digest(&receipt.context)? },
        };
        Self::path(&stamp.device, prefix, stamp.sequence, 1, leaf.digest()?)
    }

    fn install(&mut self, hash: String, node: Node) {
        if let Some(steps) = self.waiting.remove(&hash) {
            self.ready.extend(steps);
        }
        self.nodes.insert(hash, node);
    }

    /// Verify a newly supplied immutable object once per session, even when
    /// multiple paths and both main/publication states depend on it.
    pub fn provide(&mut self, hash: &str, node: Node) -> Result<()> {
        if let Some(old) = self.nodes.get(hash) {
            if old != &node {
                return Err(invalid("receipt archive witness collision"));
            }
            if !self.supplied.contains(hash) {
                if self.supplied.len() >= MAX_WITNESS_NODES {
                    return Err(invalid("receipt proof budget exceeded; local work retained"));
                }
                self.supplied.insert(hash.into());
            }
            return Ok(());
        }
        if self.supplied.len() >= MAX_WITNESS_NODES {
            return Err(invalid("receipt proof budget exceeded; local work retained"));
        }
        self.stats.witness_checks += 1;
        if node.digest()? != hash {
            return Err(invalid("receipt archive witness hash mismatch"));
        }
        self.supplied.insert(hash.into());
        self.install(hash.into(), node);
        Ok(())
    }

    /// Return the deduplicated missing frontier after advancing every ready path.
    /// Supplying one object only wakes the paths waiting for that exact hash.
    pub fn missing(&mut self) -> Result<Vec<String>> {
        if self.failed {
            return Err(invalid("archive certification previously failed; restart synchronization"));
        }
        let result = self.advance();
        self.failed = result.is_err();
        result
    }

    fn advance(&mut self) -> Result<Vec<String>> {
        while let Some(mut step) = self.ready.pop() {
            loop {
                if step.start == step.first && step.width == step.count {
                    if step.expected.as_ref().is_some_and(|expected| *expected != step.root) {
                        return Err(invalid("receipt archive commitment collision; local work retained"));
                    }
                    if !step.require_node {
                        break;
                    }
                }
                let Some(node) = self.nodes.get(&step.root) else {
                    self.waiting.entry(step.root.clone()).or_default().push(step);
                    break;
                };
                // A different target can verify this path from generated local
                // nodes. The later owner/publication merge does not share that
                // forest, so carry every used path node, not just fetched ones.
                if !self.supplied.contains(&step.root) {
                    if self.supplied.len() >= MAX_WITNESS_NODES {
                        return Err(invalid("receipt proof budget exceeded; local work retained"));
                    }
                    self.supplied.insert(step.root.clone());
                }
                if node.library != self.remote.library || node.author != step.author || node.first != step.start || node.count != step.width {
                    return Err(invalid("receipt archive node scope/coverage mismatch"));
                }
                if step.require_node {
                    break;
                }
                if step.width <= step.count {
                    return Err(invalid("invalid receipt archive descent"));
                }
                let Value::Branch { left, right } = &node.value else {
                    return Err(invalid("archive branch is a leaf"));
                };
                step.width /= 2;
                if step.first >= step.start.saturating_add(step.width) {
                    step.start = step.start.saturating_add(step.width);
                    step.root = right.clone();
                } else {
                    step.root = left.clone();
                }
            }
        }
        Ok(self.waiting.keys().cloned().collect())
    }

    pub fn stats(&self) -> &MergeStats {
        &self.stats
    }

    pub fn finish(mut self) -> Result<MergeResult> {
        if let Some(hash) = self.missing()?.first() {
            return Err(required(hash));
        }
        let mut states = Vec::new();
        for target in self.targets {
            self.stats.state_merges += 1;
            states.push(target.state.merged_verified(&self.remote, target.archives)?);
        }
        for hash in self.supplied {
            let node = self.nodes.remove(&hash).ok_or_else(|| invalid("missing certified receipt witness"))?;
            self.remote.proofs.insert(hash, node);
        }
        Ok(MergeResult { states, remote: self.remote, stats: self.stats })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::{Change, Mutation, Register, Stamp};
    use serde_json::json;

    fn history(count: u64) -> (SyncState, Witness) {
        let mut state = SyncState::new("library".into(), "source".into(), "storage".into()).unwrap();
        for author in ["a", "b"] {
            for sequence in 1..=count {
                let mutation = Mutation {
                    version: 2,
                    library: state.library.clone(),
                    revision: Stamp { device: author.into(), sequence },
                    context: if sequence == 1 { Clock::new() } else { Clock::from([(author.into(), sequence - 1)]) },
                    changes: vec![Change { entity: "photo:one".into(), field: author.into(), base: None, value: json!(sequence) }],
                };
                mutation.validate().unwrap();
                let id = mutation.revision.id();
                state.received.insert(id.clone(), Receipt { context: mutation.context.clone(), digest: mutation.digest().unwrap() });
                state.documents.entry("photo:one".into()).or_default().insert(author.into(), Register(BTreeMap::from([(id, json!(sequence))])));
            }
            state.clock.insert(author.into(), count);
        }
        state.validate().unwrap();
        let plan = state.archive_plan().unwrap();
        state.compact_receipts(&plan).unwrap();
        (state, plan.objects)
    }

    #[test]
    fn small_and_large_histories_build_forests_once_and_merge_only_after_batched_proofs() {
        for count in [64u64, 4096] {
            let (remote, objects) = history(count);
            let local = SyncState::new("library".into(), "peer".into(), "storage".into()).unwrap();
            let saved = local.clone();
            let mut session = MergeSession::new(&local, &remote.portable()).unwrap();
            // Append progress is an independent target, sharing the remote
            // forest, nodes and missing frontier with the main catalog.
            session.include(&local).unwrap();
            let mut gets = BTreeSet::new();
            let mut rounds = 0;
            loop {
                let missing = session.missing().unwrap();
                assert_eq!(session.stats().state_merges, 0);
                assert_eq!(session.stats().forest_builds, 3);
                if missing.is_empty() {
                    break;
                }
                if rounds == 0 {
                    assert_eq!(missing.len(), 2, "both authors must be planned before fetching");
                }
                rounds += 1;
                for hash in missing {
                    assert!(gets.insert(hash.clone()), "immutable nodes are fetched once");
                    session.provide(&hash, objects[&hash].clone()).unwrap();
                    session.provide(&hash, objects[&hash].clone()).unwrap();
                }
            }
            let result = session.finish().unwrap();
            assert_eq!(result.stats.forest_builds, 3);
            assert_eq!(result.stats.state_merges, 2);
            assert_eq!(result.stats.witness_checks, gets.len());
            assert_eq!(gets.len(), 2 * count.ilog2() as usize);
            assert_eq!(rounds, count.ilog2());
            assert_eq!(result.remote.proofs.len(), gets.len());
            assert_eq!(local, saved);
            for merged in result.states {
                assert_eq!(merged.documents, remote.documents);
            }
            println!(
                "receipts={} forests={} full_merges={} witness_checks={} archive_GETs={} rounds={}",
                count * 2,
                result.stats.forest_builds,
                result.stats.state_merges,
                result.stats.witness_checks,
                gets.len(),
                rounds
            );
        }
    }

    #[test]
    fn prefix_comparison_and_sparse_receipts_resume_without_rebuilding_or_losing_pending() {
        let (mut old, mut objects) = history(32);
        old.device = "offline".into();
        old.enqueue(vec![Change { entity: "photo:one".into(), field: "caption".into(), base: None, value: json!("offline edit") }]).unwrap();
        let saved = old.clone();
        let (remote, more) = history(4096);
        objects.extend(more);
        let mut session = MergeSession::new(&old, &remote.portable()).unwrap();
        let mut fetched = BTreeSet::new();
        loop {
            let missing = session.missing().unwrap();
            if missing.is_empty() {
                break;
            }
            for hash in missing {
                assert!(fetched.insert(hash.clone()));
                session.provide(&hash, objects[&hash].clone()).unwrap();
            }
        }
        let result = session.finish().unwrap();
        let merged = result.states.first().unwrap();
        assert_eq!(merged.pending, saved.pending);
        assert_eq!(merged.winning("photo:one", "caption").unwrap().1, &json!("offline edit"));
        assert_eq!(merged.winning("photo:one", "a").unwrap().1, &json!(4096));
        assert_eq!(old, saved);
        assert_eq!(result.stats.forest_builds, 2);
        assert_eq!(result.stats.state_merges, 1);
        assert_eq!(result.stats.witness_checks, fetched.len());
    }

    #[test]
    fn missing_corrupt_and_unrelated_witnesses_fail_before_any_state_application() {
        let (remote, objects) = history(64);
        let local = SyncState::new("library".into(), "peer".into(), "storage".into()).unwrap();
        let mut incomplete = MergeSession::new(&local, &remote.portable()).unwrap();
        assert!(!incomplete.missing().unwrap().is_empty());
        assert!(matches!(incomplete.finish(), Err(CatalogError::ArchiveRequired(_))));
        let mut corrupt = MergeSession::new(&local, &remote.portable()).unwrap();
        let hash = corrupt.missing().unwrap().remove(0);
        let mut node = objects[&hash].clone();
        node.library = "another".into();
        assert!(corrupt.provide(&hash, node).is_err());
        assert_eq!(corrupt.stats().state_merges, 0);
        let mut bad = local.portable();
        bad.proofs.insert("f".repeat(64), objects[&hash].clone());
        assert!(MergeSession::new(&local, &bad).is_err(), "unused input witnesses still require hash validation");
        // A caller cannot ignore a failed path check and then finish after the
        // offending obligation has been removed from the ready queue.
        let mut wrong_scope = remote.portable();
        let mut node = objects[&hash].clone();
        node.library = "another".into();
        let forged = node.digest().unwrap();
        for prefix in wrong_scope.archives.values_mut() {
            for root in &mut prefix.roots {
                if *root == hash {
                    *root = forged.clone();
                }
            }
        }
        wrong_scope.proofs.insert(forged, node);
        let mut poisoned = MergeSession::new(&local, &wrong_scope).unwrap();
        assert!(poisoned.missing().is_err());
        assert!(poisoned.finish().is_err());
    }
}
