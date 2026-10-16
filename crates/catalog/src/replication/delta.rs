//! Local journal deltas, independent of the remote wire format. Every edited value
//! carries its expected predecessor; replay and batch rollback are atomic.
use super::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Edit<T> {
    before: T,
    after: T,
}
impl<T: Clone> Edit<T> {
    fn inverse(&self) -> Self {
        Self { before: self.after.clone(), after: self.before.clone() }
    }
}
type MapDelta<T> = BTreeMap<String, Edit<Option<T>>>;
fn difference<T: Clone + PartialEq>(before: &BTreeMap<String, T>, after: &BTreeMap<String, T>) -> MapDelta<T> {
    after
        .iter()
        .filter(|(key, value)| before.get(*key) != Some(*value))
        .map(|(key, value)| (key.clone(), Edit { before: before.get(key).cloned(), after: Some(value.clone()) }))
        .chain(
            before
                .iter()
                .filter(|(key, _)| !after.contains_key(*key))
                .map(|(key, value)| (key.clone(), Edit { before: Some(value.clone()), after: None })),
        )
        .collect()
}
fn check<T: PartialEq>(base: &BTreeMap<String, T>, delta: &MapDelta<T>) -> Result<()> {
    if delta.iter().any(|(key, edit)| base.get(key) != edit.before.as_ref()) {
        return Err(invalid("synchronization delta does not match its durable base; state retained"));
    }
    Ok(())
}
fn apply<T: Clone>(base: &mut BTreeMap<String, T>, delta: &MapDelta<T>) {
    for (key, edit) in delta {
        if let Some(value) = &edit.after {
            base.insert(key.clone(), value.clone());
        } else {
            base.remove(key);
        }
    }
}
fn inverse<T: Clone>(delta: &MapDelta<T>) -> MapDelta<T> {
    delta.iter().map(|(key, edit)| (key.clone(), edit.inverse())).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DocumentDelta {
    present: Edit<bool>,
    fields: MapDelta<Register>,
}
type Documents = BTreeMap<String, BTreeMap<String, Register>>;
fn documents(before: &Documents, after: &Documents) -> BTreeMap<String, DocumentDelta> {
    let empty = BTreeMap::new();
    after
        .iter()
        .filter(|(key, fields)| before.get(*key) != Some(*fields))
        .map(|(key, fields)| {
            (
                key.clone(),
                DocumentDelta {
                    present: Edit { before: before.contains_key(key), after: true },
                    fields: difference(before.get(key).unwrap_or(&empty), fields),
                },
            )
        })
        .chain(
            before.iter().filter(|(key, _)| !after.contains_key(*key)).map(|(key, fields)| {
                (key.clone(), DocumentDelta { present: Edit { before: true, after: false }, fields: difference(fields, &empty) })
            }),
        )
        .collect()
}
fn update_documents(base: &mut Documents, delta: &BTreeMap<String, DocumentDelta>) -> Result<()> {
    if delta.len() > MAX_RECORDS {
        return Err(invalid("synchronization delta exceeds document limit"));
    }
    let empty = BTreeMap::new();
    for (key, edit) in delta {
        if base.contains_key(key) != edit.present.before || edit.fields.len() > MAX_FIELDS {
            return Err(invalid("synchronization document delta does not match its base"));
        }
        let fields = base.get(key).unwrap_or(&empty);
        check(fields, &edit.fields)?;
        if !edit.present.after && (fields.len() != edit.fields.len() || edit.fields.values().any(|field| field.after.is_some())) {
            return Err(invalid("synchronization document removal omits retained fields"));
        }
    }
    for (key, edit) in delta {
        if edit.present.after {
            apply(base.entry(key.clone()).or_default(), &edit.fields);
        } else {
            base.remove(key);
        }
    }
    Ok(())
}
fn inverse_documents(delta: &BTreeMap<String, DocumentDelta>) -> BTreeMap<String, DocumentDelta> {
    delta.iter().map(|(key, edit)| (key.clone(), DocumentDelta { present: edit.present.inverse(), fields: inverse(&edit.fields) })).collect()
}

// Small metadata headers deliberately exclude documents, receipts and pending
// payloads. Construct explicitly rather than cloning the large state to clear it.
fn header(state: &SyncState) -> SyncState {
    SyncState {
        version: state.version,
        library: state.library.clone(),
        device: state.device.clone(),
        connection: state.connection.clone(),
        protocol: state.protocol,
        append: None,
        local_owner: state.local_owner.clone(),
        previous_authors: state.previous_authors.clone(),
        migrated_from_head: state.migrated_from_head,
        require_existing: state.require_existing,
        clock: state.clock.clone(),
        cursor: state.cursor.clone(),
        generation: state.generation,
        documents: BTreeMap::new(),
        received: BTreeMap::new(),
        archives: BTreeMap::new(),
        pending: BTreeMap::new(),
    }
}
fn portable_header(state: &Portable) -> Portable {
    Portable {
        version: state.version,
        library: state.library.clone(),
        clock: state.clock.clone(),
        documents: BTreeMap::new(),
        received: BTreeMap::new(),
        archives: BTreeMap::new(),
        proofs: BTreeMap::new(),
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableDelta {
    header: Edit<Portable>,
    documents: BTreeMap<String, DocumentDelta>,
    received: MapDelta<Receipt>,
    archives: MapDelta<archive::Prefix>,
    proofs: MapDelta<archive::Node>,
}
impl PortableDelta {
    fn between(before: &Portable, after: &Portable) -> Self {
        Self {
            header: Edit { before: portable_header(before), after: portable_header(after) },
            documents: documents(&before.documents, &after.documents),
            received: difference(&before.received, &after.received),
            archives: difference(&before.archives, &after.archives),
            proofs: difference(&before.proofs, &after.proofs),
        }
    }
    fn updated(&self, base: &Portable) -> Result<Portable> {
        if portable_header(base) != self.header.before || portable_header(&self.header.after) != self.header.after {
            return Err(invalid("append delta does not match its publication base"));
        }
        check(&base.received, &self.received)?;
        check(&base.archives, &self.archives)?;
        check(&base.proofs, &self.proofs)?;
        let mut next = self.header.after.clone();
        next.documents = base.documents.clone();
        update_documents(&mut next.documents, &self.documents)?;
        next.received = base.received.clone();
        next.archives = base.archives.clone();
        next.proofs = base.proofs.clone();
        apply(&mut next.received, &self.received);
        apply(&mut next.archives, &self.archives);
        apply(&mut next.proofs, &self.proofs);
        Ok(next)
    }
    fn inverse(&self) -> Self {
        Self {
            header: self.header.inverse(),
            documents: inverse_documents(&self.documents),
            received: inverse(&self.received),
            archives: inverse(&self.archives),
            proofs: inverse(&self.proofs),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppendHeader {
    version: u32,
    manifest: String,
    frontiers: Clock,
    gaps: BTreeSet<String>,
}
impl From<&AppendProgress> for AppendHeader {
    fn from(value: &AppendProgress) -> Self {
        Self { version: value.version, manifest: value.manifest.clone(), frontiers: value.frontiers.clone(), gaps: value.gaps.clone() }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum AppendDelta {
    Replace { value: Edit<Option<Box<AppendProgress>>> },
    Patch { header: Edit<AppendHeader>, remote: Box<PortableDelta> },
}
impl AppendDelta {
    fn between(before: &Option<Box<AppendProgress>>, after: &Option<Box<AppendProgress>>) -> Self {
        match (before.as_deref(), after.as_deref()) {
            (Some(before), Some(after)) => Self::Patch {
                header: Edit { before: before.into(), after: after.into() },
                remote: Box::new(PortableDelta::between(&before.remote, &after.remote)),
            },
            _ => Self::Replace { value: Edit { before: before.clone(), after: after.clone() } },
        }
    }
    fn updated(&self, base: &Option<Box<AppendProgress>>) -> Result<Option<Box<AppendProgress>>> {
        match self {
            Self::Replace { value } if base == &value.before => Ok(value.after.clone()),
            Self::Patch { header, remote } => {
                let base = base
                    .as_deref()
                    .filter(|base| AppendHeader::from(*base) == header.before)
                    .ok_or_else(|| invalid("append delta has no matching base"))?;
                let next = &header.after;
                Ok(Some(Box::new(AppendProgress {
                    version: next.version,
                    manifest: next.manifest.clone(),
                    frontiers: next.frontiers.clone(),
                    gaps: next.gaps.clone(),
                    remote: remote.updated(&base.remote)?,
                })))
            }
            _ => Err(invalid("append delta does not match its base")),
        }
    }
    fn inverse(&self) -> Self {
        match self {
            Self::Replace { value } => Self::Replace { value: value.inverse() },
            Self::Patch { header, remote } => Self::Patch { header: header.inverse(), remote: Box::new(remote.inverse()) },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncDelta {
    header: Edit<SyncState>,
    documents: BTreeMap<String, DocumentDelta>,
    received: MapDelta<Receipt>,
    archives: MapDelta<archive::Prefix>,
    pending: MapDelta<Mutation>,
    append: AppendDelta,
}
impl SyncDelta {
    pub fn between(before: &SyncState, after: &SyncState) -> Self {
        Self {
            header: Edit { before: header(before), after: header(after) },
            documents: documents(&before.documents, &after.documents),
            received: difference(&before.received, &after.received),
            archives: difference(&before.archives, &after.archives),
            pending: difference(&before.pending, &after.pending),
            append: AppendDelta::between(&before.append, &after.append),
        }
    }
    /// Validate on a private copy. A failed precondition or final state validation
    /// cannot alter the caller, including when nested in a partially applied batch.
    pub fn updated(&self, base: &SyncState) -> Result<SyncState> {
        if header(base) != self.header.before || header(&self.header.after) != self.header.after {
            return Err(invalid("synchronization delta does not match its base metadata"));
        }
        check(&base.received, &self.received)?;
        check(&base.archives, &self.archives)?;
        check(&base.pending, &self.pending)?;
        let mut next = self.header.after.clone();
        next.documents = base.documents.clone();
        update_documents(&mut next.documents, &self.documents)?;
        next.received = base.received.clone();
        next.archives = base.archives.clone();
        next.pending = base.pending.clone();
        apply(&mut next.received, &self.received);
        apply(&mut next.archives, &self.archives);
        apply(&mut next.pending, &self.pending);
        next.append = self.append.updated(&base.append)?;
        next.validate()?;
        Ok(next)
    }
    pub fn inverse(&self) -> Self {
        Self {
            header: self.header.inverse(),
            documents: inverse_documents(&self.documents),
            received: inverse(&self.received),
            archives: inverse(&self.archives),
            pending: inverse(&self.pending),
            append: self.append.inverse(),
        }
    }
}

impl crate::Catalog {
    /// Preserve full producer states through writer reservation, then use deltas
    /// for apply/log. Multiple state operations in a batch each use their own base.
    pub fn compact_sync_op(&self, op: crate::Op) -> Result<crate::Op> {
        use std::borrow::Cow;
        fn compact<'a>(op: crate::Op, base: &mut Option<Cow<'a, SyncState>>, depth: usize) -> Result<crate::Op> {
            use crate::Op;
            if depth > 64 {
                return Err(invalid("catalog synchronization batch exceeds 64 nesting levels"));
            }
            match op {
                Op::Batch { ops } => Ok(Op::Batch { ops: ops.into_iter().map(|op| compact(op, base, depth + 1)).collect::<Result<_>>()? }),
                Op::SetSyncState { state } => {
                    let op = match (base.as_deref(), state.as_deref()) {
                        (Some(before), Some(after)) => Op::PatchSyncState { delta: Box::new(SyncDelta::between(before, after)) },
                        _ => Op::SetSyncState { state: state.clone() },
                    };
                    *base = state.map(|state| Cow::Owned(*state));
                    Ok(op)
                }
                Op::PatchSyncState { delta } => {
                    *base = Some(Cow::Owned(delta.updated(base.as_deref().ok_or_else(|| invalid("synchronization delta has no base"))?)?));
                    Ok(Op::PatchSyncState { delta })
                }
                op => Ok(op),
            }
        }
        compact(op, &mut self.sync_state().map(Cow::Borrowed), 0)
    }
}

#[cfg(test)]
mod tests;
