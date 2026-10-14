//! Certified protocol outcomes shared by native and browser transports.
//! An unchanged scan retains evidence: the owner still decides whether its current
//! catalog can skip a write after edits made while the scan was in flight.
use lightcraft_catalog::replication::{AppendProgress, Portable};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::num::NonZeroU64;

/// A published position can be constructed only through validation.
/// ```compile_fail
/// use lightcraft_engine::cloud_outcome::HeadPosition;
/// let position = HeadPosition { cursor: String::new(), generation: 0 };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadPosition {
    cursor: String,
    generation: NonZeroU64,
}
impl HeadPosition {
    pub fn new(cursor: String, generation: u64) -> Result<Self, String> {
        let generation = NonZeroU64::new(generation).ok_or("published cloud head must have a nonzero generation")?;
        if cursor.len() != 64 || !cursor.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f')) {
            return Err("published cloud head must have a SHA-256 cursor".into());
        }
        Ok(Self { cursor, generation })
    }
    pub fn cursor(&self) -> &str {
        &self.cursor
    }
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }
}
#[derive(Clone)]
pub enum HeadOutcome {
    Empty { state: Box<Portable> },
    Published { state: Box<Portable>, position: HeadPosition, acknowledged: BTreeSet<String> },
}
#[derive(Clone)]
pub enum Outcome {
    Unchanged { state: Box<Portable>, position: HeadPosition },
    ConditionalHead(HeadOutcome),
    Append { progress: Box<AppendProgress>, acknowledged: BTreeSet<String> },
}
static NO_ACK: BTreeSet<String> = BTreeSet::new();
impl Outcome {
    pub fn unchanged(state: Portable, position: HeadPosition) -> Self {
        Self::Unchanged { state: Box::new(state), position }
    }
    pub fn empty(state: Portable) -> Self {
        Self::ConditionalHead(HeadOutcome::Empty { state: Box::new(state) })
    }
    pub fn head(state: Portable, position: HeadPosition, acknowledged: BTreeSet<String>) -> Self {
        Self::ConditionalHead(HeadOutcome::Published { state: Box::new(state), position, acknowledged })
    }
    pub fn append(progress: AppendProgress, acknowledged: BTreeSet<String>) -> Self {
        Self::Append { progress: Box::new(progress), acknowledged }
    }
    pub fn state(&self) -> &Portable {
        match self {
            Self::Unchanged { state, .. }
            | Self::ConditionalHead(HeadOutcome::Empty { state })
            | Self::ConditionalHead(HeadOutcome::Published { state, .. }) => state,
            Self::Append { progress, .. } => &progress.remote,
        }
    }
    pub fn acknowledged(&self) -> &BTreeSet<String> {
        match self {
            Self::ConditionalHead(HeadOutcome::Published { acknowledged, .. }) | Self::Append { acknowledged, .. } => acknowledged,
            Self::Unchanged { .. } | Self::ConditionalHead(HeadOutcome::Empty { .. }) => &NO_ACK,
        }
    }
    pub fn head_position(&self) -> Option<&HeadPosition> {
        match self {
            Self::Unchanged { position, .. } | Self::ConditionalHead(HeadOutcome::Published { position, .. }) => Some(position),
            _ => None,
        }
    }
    pub fn append_progress(&self) -> Option<&AppendProgress> {
        match self {
            Self::Append { progress, .. } => Some(progress),
            _ => None,
        }
    }
    pub fn report(&self) -> Value {
        let mut value = json!({"acknowledged":self.acknowledged().len()});
        match self {
            Self::Unchanged { .. } => value["outcome"] = json!("unchanged"),
            Self::ConditionalHead(HeadOutcome::Empty { .. }) => {
                value["outcome"] = json!("conditionalHead");
                value["head"] = json!("empty");
            }
            Self::ConditionalHead(HeadOutcome::Published { .. }) => value["outcome"] = json!("conditionalHead"),
            Self::Append { progress, .. } => {
                value["outcome"] = json!("append");
                value["frontiers"] = json!(&progress.frontiers);
                value["gaps"] = json!(&progress.gaps);
            }
        }
        if let Some(position) = self.head_position() {
            value["generation"] = json!(position.generation());
            value["cursor"] = json!(position.cursor());
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Session,
        catalog::{
            Op,
            replication::{AppendProgress, SyncState},
        },
    };
    #[test]
    fn cr32_outcomes_preserve_evidence_and_have_only_meaningful_protocol_fields() {
        let state = SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
        for (cursor, generation) in [(String::new(), 1), ("a".repeat(64), 0), ("A".repeat(64), 1), ("z".repeat(64), 1)] {
            assert!(HeadPosition::new(cursor, generation).is_err());
        }
        let position = HeadPosition::new("a".repeat(64), 11).unwrap();
        let unchanged = Outcome::unchanged(state.portable(), position.clone());
        assert!(matches!(unchanged, Outcome::Unchanged { .. }));
        assert!(unchanged.acknowledged().is_empty());
        assert_eq!(unchanged.head_position(), Some(&position));
        let empty = Outcome::empty(state.portable());
        assert!(empty.head_position().is_none());
        assert!(empty.acknowledged().is_empty());
        assert!(empty.report().get("generation").is_none());
        assert_eq!(empty.report()["head"], "empty");
        let progress = AppendProgress {
            version: 1,
            manifest: "b".repeat(64),
            frontiers: Default::default(),
            remote: state.portable(),
            gaps: Default::default(),
        };
        let append = Outcome::append(progress, BTreeSet::new());
        assert!(append.head_position().is_none());
        assert!(append.report().get("generation").is_none());
        assert!(append.report().get("cursor").is_none());
        assert!(std::ptr::eq(append.state(), &append.append_progress().unwrap().remote), "append has one authoritative remote state");
    }
    #[test]
    fn cr32_empty_head_is_distinct_from_rollback_and_cannot_acknowledge_work() {
        let mut s = Session::new();
        let state = SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
        s.apply_received(Op::SetSyncState { state: Some(Box::new(state.clone())) }).unwrap();
        let empty = Outcome::empty(state.portable());
        assert!(crate::cloud_sync::completed_outcome(&s.catalog, &empty, &BTreeSet::from(["device:1".into()]), 0).is_err());
        assert!(crate::cloud_sync::completed_outcome(&s.catalog, &empty, &BTreeSet::new(), 1).is_err());
        let op = crate::cloud_sync::completed_outcome(&s.catalog, &empty, &BTreeSet::new(), 0).unwrap();
        s.apply_received(op).unwrap();
        assert!(s.catalog.sync_state().unwrap().cursor.is_none());
        let mut newer = s.catalog.sync_state().unwrap().clone();
        newer.generation = 11;
        newer.cursor = Some("b".repeat(64));
        s.apply_received(Op::SetSyncState { state: Some(Box::new(newer)) }).unwrap();
        let op = crate::cloud_sync::completed_outcome(&s.catalog, &empty, &BTreeSet::new(), 0).unwrap();
        s.apply_received(op).unwrap();
        assert_eq!(s.catalog.sync_state().unwrap().generation, 11);
        assert_eq!(s.catalog.sync_state().unwrap().cursor.as_deref(), Some("b".repeat(64).as_str()));
    }
    #[test]
    fn cr32_unchanged_hint_keeps_evidence_when_owner_edits_during_the_scan() {
        let mut state = SyncState::new("library".into(), "device".into(), "personal".into()).unwrap();
        state.generation = 11;
        state.cursor = Some("a".repeat(64));
        let outcome = Outcome::unchanged(state.portable(), HeadPosition::new("a".repeat(64), 11).unwrap());
        let mut s = Session::new();
        s.apply_received(Op::SetSyncState { state: Some(Box::new(state.clone())) }).unwrap();
        assert!(crate::cloud_sync::unchanged_outcome(&s.catalog, &outcome));
        let mutation = state
            .enqueue(vec![crate::catalog::replication::Change {
                entity: "library:library".into(),
                field: "label.Green".into(),
                base: None,
                value: json!("Local green"),
            }])
            .unwrap()
            .unwrap();
        s.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
        assert!(!crate::cloud_sync::unchanged_outcome(&s.catalog, &outcome));
        let op = crate::cloud_sync::completed_outcome(&s.catalog, &outcome, outcome.acknowledged(), 11).unwrap();
        s.apply_received(op).unwrap();
        assert!(s.catalog.sync_state().unwrap().pending.contains_key(&mutation.revision.id()));
        assert_eq!(s.catalog.sync_state().unwrap().generation, 11);
        assert_eq!(s.catalog.custom_label_name(crate::catalog::ColorLabel::Green), Some("Local green"));
    }
}
