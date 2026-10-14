//! Immutable Merkle receipt forests. Each author has at most 64 roots, independent of
//! history length. Full causal contexts remain only on live register heads and pending work.
use super::{Clock, Portable, Receipt, SyncState, invalid, valid_clock};
use crate::{CatalogError, Result, assets::identifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub type Archives = BTreeMap<String, Prefix>;
pub type Witness = BTreeMap<String, Node>;
pub const MAX_NODE_BYTES: usize = 1024;
pub const MAX_WITNESS_NODES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Prefix {
    pub through: u64,
    /// Left to right, the descending powers of two in `through`'s binary expansion.
    pub roots: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Node {
    pub version: u32,
    pub library: String,
    pub author: String,
    pub first: u64,
    pub count: u64,
    pub value: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Value {
    Leaf { digest: String, context: String },
    Branch { left: String, right: String },
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(hash(&serde_json::to_vec(value).map_err(|e| invalid(&e.to_string()))?))
}
fn valid_hash(value: &str) -> bool {
    super::append::hash(value)
}
impl Node {
    pub fn validate(&self) -> Result<()> {
        let valid_value = match &self.value {
            Value::Leaf { digest, context } => self.count == 1 && valid_hash(digest) && valid_hash(context),
            Value::Branch { left, right } => self.count > 1 && valid_hash(left) && valid_hash(right),
        };
        if self.version != 1
            || !identifier(&self.library)
            || !identifier(&self.author)
            || self.author.len() > 96
            || self.first == 0
            || !self.count.is_power_of_two()
            || !(self.first - 1).is_multiple_of(self.count)
            || self.first.checked_add(self.count - 1).is_none()
            || !valid_value
            || serde_json::to_vec(self).map_err(|e| invalid(&e.to_string()))?.len() > MAX_NODE_BYTES
        {
            return Err(invalid("invalid receipt archive node; local work retained"));
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        digest(self)
    }
    pub fn decode(expected: &str, bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_NODE_BYTES || hash(bytes) != expected {
            return Err(invalid("receipt archive hash/size mismatch; local work retained"));
        }
        let node: Self = serde_json::from_slice(bytes).map_err(|_| invalid("malformed receipt archive node"))?;
        if node.digest()? != expected {
            return Err(invalid("noncanonical receipt archive node"));
        }
        Ok(node)
    }
}
fn ranges(prefix: &Prefix) -> Result<Vec<(u64, u64, &str)>> {
    if prefix.through == 0 || prefix.roots.len() != prefix.through.count_ones() as usize || prefix.roots.iter().any(|h| !valid_hash(h)) {
        return Err(invalid("invalid receipt archive coverage"));
    }
    let mut result = Vec::new();
    let mut first = 1u64;
    for bit in (0..64).rev() {
        let count = 1u64 << bit;
        if prefix.through & count != 0 {
            let root = prefix.roots.get(result.len()).ok_or_else(|| invalid("missing archive root"))?;
            result.push((first, count, root.as_str()));
            // The last range may end at u64::MAX; no following range uses this value.
            first = first.saturating_add(count);
        }
    }
    Ok(result)
}
pub fn validate(archives: &Archives, clock: &Clock) -> Result<()> {
    if archives.len() > super::MAX_DEVICES {
        return Err(invalid("too many receipt archive authors"));
    }
    for (author, prefix) in archives {
        if !valid_clock(&Clock::from([(author.clone(), prefix.through)])) || clock.get(author).copied().unwrap_or(0) < prefix.through {
            return Err(invalid("receipt archive exceeds causal clock"));
        }
        ranges(prefix)?;
    }
    Ok(())
}
pub fn validate_witness(proofs: &Witness) -> Result<()> {
    if proofs.len() > MAX_WITNESS_NODES {
        return Err(invalid("receipt archive proof budget exceeded; retry in smaller batches"));
    }
    for (hash, node) in proofs {
        if node.digest()? != *hash {
            return Err(invalid("receipt archive witness hash mismatch"));
        }
    }
    Ok(())
}
/// A content-addressed object to fetch, never a caller-supplied URL or path.
fn required(hash: &str) -> CatalogError {
    CatalogError::ArchiveRequired(hash.into())
}
fn subtree<'a>(library: &str, author: &str, prefix: &'a Prefix, first: u64, count: u64, proofs: &'a Witness) -> Result<&'a str> {
    let end = first.checked_add(count - 1).ok_or_else(|| invalid("archive range overflow"))?;
    let (mut start, mut width, mut root) = ranges(prefix)?
        .into_iter()
        .find(|(start, width, _)| first >= *start && end <= start.saturating_add(*width - 1))
        .ok_or_else(|| invalid("archive does not cover requested receipt"))?;
    for _ in 0..64 {
        if start == first && width == count {
            return Ok(root);
        }
        let node = proofs.get(root).ok_or_else(|| required(root))?;
        if node.digest()? != root || node.library != library || node.author != author || node.first != start || node.count != width {
            return Err(invalid("receipt archive node scope mismatch"));
        }
        let Value::Branch { left, right } = &node.value else { return Err(invalid("archive branch is a leaf")) };
        width /= 2;
        if first >= start.saturating_add(width) {
            start = start.saturating_add(width);
            root = right;
        } else {
            root = left;
        }
    }
    Err(invalid("receipt archive depth exceeded"))
}
pub fn verify_receipt(library: &str, archives: &Archives, id: &str, receipt: &Receipt, proofs: &Witness) -> Result<()> {
    let stamp = super::append::stamp(id)?;
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
    if subtree(library, &stamp.device, prefix, stamp.sequence, 1, proofs)? != leaf.digest()? {
        return Err(invalid("archived mutation identity/context collision; local work retained"));
    }
    Ok(())
}
/// Durable upload receipts retain the exact mutation digest, rather than its full context.
/// The authenticated leaf still commits both hashes; this never acknowledges a local outbox.
pub fn verify_digest(library: &str, archives: &Archives, id: &str, digest: &str, proofs: &Witness) -> Result<()> {
    let stamp = super::append::stamp(id)?;
    let prefix = archives.get(&stamp.device).ok_or_else(|| invalid("receipt has no archive"))?;
    let root = subtree(library, &stamp.device, prefix, stamp.sequence, 1, proofs)?;
    let node = proofs.get(root).ok_or_else(|| required(root))?;
    if node.digest()? != root || node.library != library || node.author != stamp.device || node.first != stamp.sequence || node.count != 1 {
        return Err(invalid("receipt archive leaf scope mismatch"));
    }
    if !matches!(&node.value, Value::Leaf { digest: stored, .. } if stored == digest) {
        return Err(invalid("archived publication digest collision; staged bytes retained"));
    }
    Ok(())
}
pub fn covers(archives: &Archives, id: &str) -> bool {
    super::SyncState::stamp(id).is_some_and(|stamp| archives.get(&stamp.device).is_some_and(|p| stamp.sequence <= p.through))
}

pub struct Plan {
    pub archives: Archives,
    pub objects: Witness,
}
/// Build only the new contiguous suffix, merging equal-size trees like binary addition.
fn plan(state: &Portable) -> Result<Plan> {
    let mut archives = state.archives.clone();
    let mut objects = Witness::new();
    let mut ordered: BTreeMap<String, BTreeMap<u64, &Receipt>> = BTreeMap::new();
    for (id, receipt) in &state.received {
        let stamp = super::append::stamp(id)?;
        ordered.entry(stamp.device).or_default().insert(stamp.sequence, receipt);
    }
    for (author, receipts) in ordered {
        let mut through = archives.get(&author).map_or(0, |p| p.through);
        let mut forest: Vec<(u64, u64, String)> = archives
            .get(&author)
            .map(ranges)
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|(first, count, root)| (first, count, root.to_owned()))
            .collect();
        while let Some(sequence) = through.checked_add(1) {
            let Some(receipt) = receipts.get(&sequence) else { break };
            let leaf = Node {
                version: 1,
                library: state.library.clone(),
                author: author.clone(),
                first: sequence,
                count: 1,
                value: Value::Leaf { digest: receipt.digest.clone(), context: digest(&receipt.context)? },
            };
            let mut current = (sequence, 1u64, leaf.digest()?);
            objects.insert(current.2.clone(), leaf);
            while forest.last().is_some_and(|old| old.1 == current.1) {
                let left = forest.pop().ok_or_else(|| invalid("missing archive forest root"))?;
                let count = current.1.checked_mul(2).ok_or_else(|| invalid("archive tree overflow"))?;
                let node = Node {
                    version: 1,
                    library: state.library.clone(),
                    author: author.clone(),
                    first: left.0,
                    count,
                    value: Value::Branch { left: left.2, right: current.2 },
                };
                current = (node.first, count, node.digest()?);
                objects.insert(current.2.clone(), node);
            }
            forest.push(current);
            through = sequence;
        }
        if through > 0 {
            archives.insert(author, Prefix { through, roots: forest.into_iter().map(|(_, _, hash)| hash).collect() });
        }
    }
    Ok(Plan { archives, objects })
}
/// Validate prefix compatibility even for incomparable checkpoints and cloned authors.
/// Locally reconstructable full-history trees avoid fetching one proof per historical ID.
pub(super) fn merge(local: &SyncState, remote: &Portable) -> Result<Archives> {
    validate_witness(&remote.proofs)?;
    if local.archives.is_empty() && remote.archives.is_empty() {
        return Ok(Archives::new());
    }
    let lp = plan(&local.portable())?;
    let rp = plan(remote)?;
    let mut proofs = remote.proofs.clone();
    proofs.extend(lp.objects);
    proofs.extend(rp.objects);
    // A newly claimed prefix must at least have a verified root in its exact scope.
    // Existing local commitments are already durable authority and need no refetch.
    for (author, prefix) in &remote.archives {
        let known = local.archives.get(author).map(ranges).transpose()?.unwrap_or_default();
        for (first, count, root) in ranges(prefix)? {
            if known.contains(&(first, count, root)) {
                continue;
            }
            let node = proofs.get(root).ok_or_else(|| required(root))?;
            if node.digest()? != root || node.library != local.library || node.author != *author || node.first != first || node.count != count {
                return Err(invalid("receipt archive root scope/coverage mismatch"));
            }
        }
    }
    for (author, left) in &lp.archives {
        if let Some(right) = rp.archives.get(author) {
            let (smaller, larger) = if left.through <= right.through { (left, right) } else { (right, left) };
            for (first, count, root) in ranges(smaller)? {
                if subtree(&local.library, author, larger, first, count, &proofs)? != root {
                    return Err(invalid("receipt archive prefix collision; both histories retained"));
                }
            }
        }
    }
    let mut result = local.archives.clone();
    for (author, prefix) in &remote.archives {
        if result.get(author).is_none_or(|old| old.through < prefix.through) {
            result.insert(author.clone(), prefix.clone());
        }
    }
    // Check sparse receipts and retained heads against the chosen authenticated coverage.
    for (id, receipt) in &local.received {
        let stamp = super::append::stamp(id)?;
        if covers(&remote.archives, id) && lp.archives.get(&stamp.device).is_none_or(|p| stamp.sequence > p.through) {
            verify_receipt(&local.library, &remote.archives, id, receipt, &proofs)?;
        }
    }
    for (id, receipt) in &remote.received {
        let stamp = super::append::stamp(id)?;
        let known = local.received.get(id) == Some(receipt) && lp.archives.get(&stamp.device).is_some_and(|p| stamp.sequence <= p.through);
        if covers(&result, id) && !known {
            verify_receipt(&local.library, &result, id, receipt, &proofs)?;
        }
    }
    Ok(result)
}
impl SyncState {
    pub fn archive_plan(&self) -> Result<Plan> {
        self.validate()?;
        plan(&self.portable())
    }
    /// Called only after every generated immutable object is durably verified by transport.
    pub fn compact_receipts(&mut self, plan: &Plan) -> Result<()> {
        let expected = self.archive_plan()?;
        if expected.archives != plan.archives {
            return Err(invalid("archive plan no longer matches state"));
        }
        let mut next = self.clone();
        next.version = 2;
        next.archives = plan.archives.clone();
        next.prune_archived_receipts();
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub(super) fn prune_archived_receipts(&mut self) {
        let heads: BTreeSet<_> = self.documents.values().flat_map(|fields| fields.values()).flat_map(|r| r.0.keys()).collect();
        self.received.retain(|id, _| !covers(&self.archives, id) || heads.contains(id) || self.pending.contains_key(id));
    }
    /// Hydrate an exact pending/replay proof without reapplying its historical effects.
    pub fn prove_archived(&mut self, id: &str, receipt: Receipt, proofs: &Witness) -> Result<()> {
        validate_witness(proofs)?;
        verify_receipt(&self.library, &self.archives, id, &receipt, proofs)?;
        if self.received.get(id).is_some_and(|old| old != &receipt) {
            return Err(invalid("archived receipt collision"));
        }
        let mut next = self.clone();
        next.received.insert(id.into(), receipt);
        next.validate()?;
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::{Change, Mutation, Register, Stamp};
    use serde_json::json;
    fn state(author: &str) -> SyncState {
        SyncState::new("library".into(), author.into(), "storage".into()).unwrap()
    }
    fn edit(s: &mut SyncState, field: &str, value: serde_json::Value) -> Mutation {
        s.enqueue(vec![Change { entity: "photo:one".into(), field: field.into(), base: None, value }]).unwrap().unwrap()
    }
    fn compact(s: &mut SyncState) -> Witness {
        let p = s.archive_plan().unwrap();
        s.compact_receipts(&p).unwrap();
        p.objects
    }
    #[test]
    fn compact_replay_fork_context_and_pending_ack_are_exact() {
        let mut a = state("a");
        let first = edit(&mut a, "rating", json!(1));
        let second = edit(&mut a, "rating", json!(4));
        let mut remote = state("remote");
        remote.receive(&first).unwrap();
        remote.receive(&second).unwrap();
        let proofs = compact(&mut remote);
        let mut corrupt = remote.portable();
        corrupt.proofs = proofs.clone();
        corrupt.received.get_mut(&second.revision.id()).unwrap().digest = "f".repeat(64);
        assert!(state("newpeer").merge(&corrupt).is_err(), "checkpoint heads must match their archive commitments");
        verify_digest(&remote.library, &remote.archives, &first.revision.id(), &first.digest().unwrap(), &proofs).unwrap();
        assert!(verify_digest(&remote.library, &remote.archives, &first.revision.id(), &"f".repeat(64), &proofs).is_err());
        assert!(matches!(state("newpeer").merge(&remote.portable()), Err(CatalogError::ArchiveRequired(_))), "new archive roots must be verified");
        assert_eq!(remote.received.len(), 1);
        assert!(remote.receive(&first).is_err(), "archive needs an exact proof");
        let receipt = a.received[&first.revision.id()].clone();
        remote.prove_archived(&first.revision.id(), receipt.clone(), &proofs).unwrap();
        assert!(!remote.receive(&first).unwrap());
        let mut fork = first.clone();
        fork.changes[0].value = json!(2);
        assert!(
            verify_receipt(
                &remote.library,
                &remote.archives,
                &fork.revision.id(),
                &Receipt { context: fork.context.clone(), digest: fork.digest().unwrap() },
                &proofs
            )
            .is_err()
        );
        let mut bad = receipt;
        bad.context.insert("forged".into(), 1);
        assert!(remote.prove_archived(&first.revision.id(), bad, &proofs).is_err());
        let mut p = remote.portable();
        p.proofs = proofs;
        a.merge(&p).unwrap();
        assert_eq!(a.pending.len(), 2, "merge cannot acknowledge unpublished local work");
        assert_eq!(a.received.len(), 2, "pending contexts cannot be demoted");
        a.acknowledge(&BTreeSet::from([first.revision.id(), second.revision.id()]), "a".repeat(64), 1).unwrap();
        assert_eq!(a.received.len(), 1);
        let next = edit(&mut a, "rating", json!(5));
        assert_eq!(next.revision.sequence, 3);
        assert_eq!(next.context.get("a"), Some(&2));
    }
    #[test]
    fn offline_delete_restore_conflicts_and_incomparable_checkpoints_survive() {
        let mut a = state("a");
        let first = edit(&mut a, "rating", json!(1));
        let mut b = state("b");
        b.receive(&first).unwrap();
        let delete = edit(&mut a, "deleted", json!(true));
        a.acknowledge(&a.pending.keys().cloned().collect(), "a".repeat(64), 1).unwrap();
        let mut proofs = compact(&mut a);
        let offline = edit(&mut b, "rating", json!(5));
        b.acknowledge(&b.pending.keys().cloned().collect(), "b".repeat(64), 1).unwrap();
        proofs.extend(compact(&mut b));
        let mut ap = a.portable();
        ap.proofs = proofs.clone();
        let mut bp = b.portable();
        bp.proofs = proofs.clone();
        a.merge(&bp).unwrap();
        b.merge(&ap).unwrap();
        assert_eq!(a.documents, b.documents);
        assert_eq!(a.winning("photo:one", "deleted").unwrap().1, &json!(true));
        assert_eq!(a.winning("photo:one", "rating").unwrap().1, &json!(5));
        assert_eq!(a.received[&delete.revision.id()].context, delete.context);
        assert_eq!(b.received[&offline.revision.id()].context, offline.context);
        let restore = edit(&mut b, "deleted", json!(false));
        a.receive(&restore).unwrap();
        assert_eq!(a.winning("photo:one", "deleted").unwrap().1, &json!(false));
        // Retired/restored authors keep their causal identity; no age-based clock removal.
        b.claim_owner("newowner".into(), "replacement".into()).unwrap();
        assert!(b.clock.contains_key("b"));
        let late = edit(&mut b, "rating", json!(3));
        assert_eq!(late.revision.sequence, 1);
        assert!(late.context.contains_key("a") && late.context.contains_key("b"));
    }
    #[test]
    fn missing_corrupt_cyclic_and_gapped_proofs_fail_without_state_changes() {
        let mut a = state("a");
        let first = edit(&mut a, "rating", json!(1));
        edit(&mut a, "rating", json!(2));
        a.pending.clear();
        let proofs = compact(&mut a);
        let receipt = Receipt { context: first.context.clone(), digest: first.digest().unwrap() };
        let before = a.clone();
        assert!(matches!(a.prove_archived("a-1", receipt.clone(), &Witness::new()), Err(CatalogError::ArchiveRequired(_))));
        let mut bad = proofs.clone();
        let node = bad.values_mut().find(|n| n.count == 2).unwrap();
        if let Value::Branch { left, .. } = &mut node.value {
            *left = "0".repeat(64);
        }
        assert!(a.prove_archived("a-1", receipt, &bad).is_err());
        assert_eq!(a, before);
        let root = a.archives["a"].roots[0].clone();
        let bytes = serde_json::to_vec(&proofs[&root]).unwrap();
        assert!(Node::decode(&root, &bytes[..bytes.len() - 1]).is_err());
        let mut p = a.portable();
        p.archives.get_mut("a").unwrap().through = 3;
        assert!(p.validate().is_err());
        let mut gap = state("gapped");
        gap.clock.insert("a".into(), 2);
        gap.received.insert("a-2".into(), before.received["a-2"].clone());
        assert!(gap.archive_plan().unwrap().archives.is_empty());
    }
    #[test]
    fn record_capacity_is_recovered_and_roots_remain_bounded_after_repeated_maintenance() {
        // Synthetic valid history at the actual production record bound; avoid quadratic
        // enqueue setup so the test measures maintenance rather than catalogue cloning.
        let mut a = state("a");
        for sequence in 1..=super::super::MAX_RECORDS as u64 {
            let context = if sequence == 1 { Clock::new() } else { Clock::from([("a".into(), sequence - 1)]) };
            let m = Mutation {
                version: 1,
                library: a.library.clone(),
                revision: Stamp { device: "a".into(), sequence },
                context,
                changes: vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(1) }],
            };
            a.received.insert(m.revision.id(), Receipt { context: m.context.clone(), digest: m.digest().unwrap() });
        }
        let last = super::super::MAX_RECORDS as u64;
        a.clock.insert("a".into(), last);
        a.documents.insert("photo:one".into(), BTreeMap::from([("rating".into(), Register(BTreeMap::from([(format!("a-{last}"), json!(1))])))]));
        a.validate().unwrap();
        let bytes = serde_json::to_vec(&a).unwrap().len();
        let change = vec![Change { entity: "photo:one".into(), field: "rating".into(), base: None, value: json!(2) }];
        let before = a.clone();
        assert!(a.enqueue(change).is_err());
        assert_eq!(a, before);
        compact(&mut a);
        assert_eq!(a.received.len(), 1);
        assert!(serde_json::to_vec(&a).unwrap().len() < bytes / 100);
        for n in 0..5 {
            let next = edit(&mut a, "rating", json!(n));
            a.acknowledge(&BTreeSet::from([next.revision.id()]), "a".repeat(64), n + 1).unwrap();
            compact(&mut a);
            assert!(a.archives["a"].roots.len() <= 64);
            assert_eq!(a.received.len(), 1);
        }
    }
    #[test]
    fn byte_capacity_is_recovered_without_discarding_author_clocks() {
        let mut a = state("a");
        let mut context = Clock::new();
        for n in 0..900 {
            context.insert(format!("retired_device_{n:04}"), 1);
        }
        a.clock = context.clone();
        let sample = Receipt { context: context.clone(), digest: "e".repeat(64) };
        let per_record = serde_json::to_vec(&sample).unwrap().len() + 32;
        let count = ((63 << 20) / per_record) as u64;
        for sequence in 1..=count {
            let mut context = context.clone();
            if sequence > 1 {
                context.insert("a".into(), sequence - 1);
            }
            a.received.insert(format!("a-{sequence}"), Receipt { context, digest: "e".repeat(64) });
        }
        a.clock.insert("a".into(), count);
        a.documents.insert("photo:one".into(), BTreeMap::from([("rating".into(), Register(BTreeMap::from([(format!("a-{count}"), json!(1))])))]));
        a.validate().unwrap();
        let changes = vec![Change { entity: "photo:one".into(), field: "meta.title".into(), base: None, value: json!("x".repeat(2_000_000)) }];
        let before = a.clone();
        let error = a.enqueue(changes.clone()).unwrap_err();
        assert!(error.to_string().contains("64 MiB"), "{error}");
        assert_eq!(a, before);
        compact(&mut a);
        assert!(serde_json::to_vec(&a).unwrap().len() < 100_000);
        assert_eq!(a.clock.len(), 901, "retired identities cannot be forgotten by receipt maintenance");
        assert!(a.enqueue(changes).unwrap().is_some());
    }
    #[test]
    fn different_prefix_lengths_prove_consistency_and_compacted_cloned_authors_fail_atomically() {
        let mut source = state("a");
        edit(&mut source, "rating", json!(1));
        edit(&mut source, "rating", json!(2));
        source.pending.clear();
        let mut early = source.clone();
        let mut proofs = compact(&mut early);
        let mut fork = source.clone();
        fork.received.get_mut("a-2").unwrap().digest = "f".repeat(64);
        proofs.extend(compact(&mut fork));
        let mut bad = fork.portable();
        bad.proofs = proofs.clone();
        let before = early.clone();
        assert!(early.merge(&bad).is_err());
        assert_eq!(early, before);
        edit(&mut source, "rating", json!(3));
        edit(&mut source, "rating", json!(4));
        source.pending.clear();
        proofs.extend(compact(&mut source));
        let mut later = source.portable();
        later.proofs = proofs.clone();
        early.merge(&later).unwrap();
        let mut old = before.portable();
        old.proofs = proofs;
        source.merge(&old).unwrap();
        assert_eq!(source.documents, early.documents);
        assert_eq!(source.archives, early.archives);
        assert_eq!(source.winning("photo:one", "rating").unwrap().1, &json!(4));
    }
}
