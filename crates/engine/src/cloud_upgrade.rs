//! Native conditional-head library upgrades. Every transition is a CAS on the same head
//! that accepts ordinary commits, and is confirmed by rereading the control it wrote:
//! "Ok on conflict" is never enough for a maintenance step (see `cloud_control`).
use crate::assets::s3::S3Connection;
use crate::assets::{AssetError, Result};
use crate::cloud_control::{Blocked, Boundary, Control, Plan, State, Successor};
use crate::cloud_outcome::HeadPosition;
use crate::cloud_publication::{self as publication, Batch, CheckpointRef, Head};
use crate::cloud_replication::Outcome;
use lightcraft_catalog::replication::SyncState;
use std::sync::atomic::AtomicBool;

const ATTEMPTS: usize = 16;
fn invalid(message: impl Into<String>) -> AssetError {
    AssetError::Invalid(message.into())
}
fn blocked(b: Blocked) -> AssetError {
    invalid(b.to_string())
}
fn now_ms() -> u64 {
    crate::cloud_status::now_ms().unwrap_or(0)
}
fn read(connection: &S3Connection, library: &str, cancel: &AtomicBool) -> Result<(Head, String, Control)> {
    let (head, etag) =
        publication::read_head(connection, cancel)?.ok_or_else(|| invalid("cloud library has no head to upgrade; synchronize first"))?;
    if head.library.as_deref() != Some(library) {
        return Err(invalid("storage contains another library; upgrade refused"));
    }
    let control = head.control().map_err(invalid)?;
    control.may_read().map_err(blocked)?;
    Ok((head, etag, control))
}

/// One control transition carried by an empty marker batch (so the tip and generation
/// always advance together). Returns the confirmed head; a lost response is resolved
/// by rereading and checking for exactly the control that was written.
fn transition(
    connection: &S3Connection,
    library: &str,
    cancel: &AtomicBool,
    next: impl Fn(&Head, &Control) -> std::result::Result<Control, Blocked>,
) -> Result<Head> {
    for _ in 0..ATTEMPTS {
        let (head, etag, control) = read(connection, library, cancel)?;
        let wanted = next(&head, &control).map_err(blocked)?;
        match crate::cloud_replication::commit(
            connection,
            Some((&head, &etag)),
            Some(head.tip.clone()),
            Vec::new(),
            head.checkpoint.clone(),
            None,
            wanted.clone(),
            library,
            cancel,
        ) {
            Ok(_) | Err(AssetError::Conflict | AssetError::Network(_)) => {}
            Err(error) => return Err(error),
        }
        // `wanted` holds a fresh revision (and, while upgrading, our fencing token), so
        // observing it proves this transition was the one accepted.
        let (current, _, observed) = read(connection, library, cancel)?;
        if observed == wanted {
            return Ok(current);
        }
        // Another commit or transition won; recompute from the new head.
    }
    Err(AssetError::Network("cloud library head remained busy; upgrade not started or not confirmed".into()))
}

/// Acquire maintenance: begin a new migration, resume our own (same installation) or
/// take over an expired one. Returns the head and our fresh token.
fn acquire(connection: &S3Connection, library: &str, plan: Plan, owner: &str, cancel: &AtomicBool) -> Result<(Head, String)> {
    let token = crate::cloud_control::token().map_err(invalid)?;
    let id = format!("{}-{}", plan.name(), token.get(..16).unwrap_or("migration"));
    let head = transition(connection, library, cancel, |head, control| match &control.migration {
        None => control.begin(&id, plan, Boundary { tip: head.tip.hash.0.clone(), generation: head.generation }, owner, &token, now_ms()),
        Some(m) if m.plan != plan => Err(Blocked::Maintenance { migration: m.id.clone(), plan: m.plan, owner: m.owner.clone() }),
        Some(m) => control.takeover(owner, &token, now_ms(), m.owner == owner),
    })?;
    Ok((head, token))
}

fn owned<'a>(head: &'a Head, token: &str) -> Result<&'a crate::cloud_control::Migration> {
    match &head.control {
        Some(Control { migration: Some(m), .. }) if m.token == token => Ok(m),
        _ => Err(invalid("upgrade ownership was taken over by another device; this result was not activated")),
    }
}

/// Upgrading → Active at a new conditional-head epoch: the verified state at the frozen
/// boundary is written beside the old epoch, read back, then activated by token.
fn rebase(connection: &S3Connection, local: &SyncState, owner: &str, cancel: &AtomicBool) -> Result<Outcome> {
    let library = &local.library;
    let (head, token) = acquire(connection, library, Plan::Rebase, owner, cancel)?;
    let migration = owned(&head, &token)?.clone();
    // The maintenance head holds only marker batches after the frozen source, so
    // its full verified history is exactly the source state.
    let mut proofs = Default::default();
    let mut source = crate::cloud_replication::read_with_witness(connection, library, &mut proofs, cancel)?;
    let epoch = migration.target_epoch;
    let first = publication::encode(&Batch { version: 3, parent: None, imports: Vec::new(), mutations: Vec::new(), checkpoint_marker: true })?;
    let first_ref = crate::cloud_replication::batch_ref(connection, epoch, &first)?;
    crate::cloud_replication::write_immutable(connection, &first_ref.key, &first, cancel)?;
    let base = head.generation.checked_add(1).ok_or_else(|| invalid("generation exhausted"))?;
    let checkpoint: CheckpointRef = crate::cloud_replication::checkpoint(connection, &mut source.state, first_ref.clone(), base, epoch, cancel)?;
    // Verify the replacement before activation: exact bytes, structure and state.
    let written = crate::cloud_replication::read_checkpoint(connection, &checkpoint, cancel)?;
    if written.state != source.state.portable() {
        return Err(invalid("upgraded checkpoint does not reproduce the frozen source state; old epoch remains active"));
    }
    crate::cloud_control::translate_mutation(migration.target_schema, migration.target_schema).map_err(invalid)?;
    // A long build renews the lease before activation, so it is not taken over at the end.
    if now_ms() >= migration.lease_until_ms.saturating_sub(crate::cloud_control::LEASE_MS / 2) {
        transition(connection, library, cancel, |head, control| {
            owned(head, &token).map_err(|e| Blocked::Invalid(e.to_string()))?;
            control.renew(&token, now_ms())
        })?;
    }
    let mut activated = None;
    for _ in 0..ATTEMPTS {
        let (current, etag, control) = read(connection, library, cancel)?;
        // A lost activation response: the reread shows our epoch with our checkpoint.
        if control.state == State::Active && control.epoch == epoch && current.checkpoint.as_ref().map(|c| &c.object) == Some(&checkpoint.object) {
            activated = Some(current);
            break;
        }
        owned(&current, &token)?;
        let wanted = control.activate(&token, 1, 1).map_err(blocked)?;
        // The checkpoint records generation `base`; the activation head must follow both
        // it and any renewal/takeover heads written meanwhile.
        let generation = current.generation.max(base).checked_add(1).ok_or_else(|| invalid("generation exhausted"))?;
        match crate::cloud_replication::commit(
            connection,
            Some((&current, &etag)),
            Some(first_ref.clone()),
            Vec::new(),
            Some(checkpoint.clone()),
            Some(generation),
            wanted,
            library,
            cancel,
        ) {
            Ok(_) | Err(AssetError::Conflict | AssetError::Network(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let activated = activated.ok_or_else(|| AssetError::Network("activation not confirmed; reread before retrying".into()))?;
    let state = crate::cloud_history::native::certify(connection, local, &source.state.portable(), &mut proofs, cancel)?;
    let acknowledged = local.pending.keys().filter(|id| source.state.received.contains_key(*id)).cloned().collect();
    Ok(Outcome::head(state, HeadPosition::new(activated.tip.hash.0, activated.generation).map_err(invalid)?, acknowledged))
}

/// Seed the append protocol from the frozen head and retire the head, so conforming
/// head-mode writers stop at the CAS instead of relying on operator coordination.
/// A device that finds the head already retired rejoins without writing anything.
fn append_log(connection: &S3Connection, local: &SyncState, owner: &str, writers_stopped: bool, cancel: &AtomicBool) -> Result<Outcome> {
    let library = &local.library;
    let manifest = crate::assets::sha256(&crate::cloud_append::manifest(library)?).0;
    if publication::read_head(connection, cancel)?.is_none() {
        // Nothing was ever committed through a head (e.g. staged imports from a provider
        // without conditional writes), so there is no history to freeze or retire.
        if !writers_stopped {
            return Err(invalid("cloud library has no published head; stop every other device, then retry with writersStopped:true"));
        }
        return crate::cloud_append::migration::seed(connection, local, cancel).map_err(|e| invalid(e.to_string()));
    }
    let (_, _, control) = read(connection, library, cancel)?;
    if control.state != State::Retired {
        let token = match acquire(connection, library, Plan::AppendLog, owner, cancel) {
            Ok((_, token)) => token,
            // Without conditional writes no conforming device can commit to this head, so
            // it is already frozen; the operator's confirmation covers older binaries.
            Err(AssetError::Unsupported(_)) if writers_stopped => {
                return crate::cloud_append::migration::seed(connection, local, cancel).map_err(|e| invalid(e.to_string()));
            }
            Err(AssetError::Unsupported(why)) => {
                return Err(invalid(format!(
                    "{why}; this provider cannot hold the maintenance barrier: stop every other device, then retry with writersStopped:true"
                )));
            }
            Err(error) => return Err(error),
        };
        crate::cloud_append::migration::seed(connection, local, cancel).map_err(|e| invalid(e.to_string()))?;
        let successor = Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: manifest.clone() };
        transition(connection, library, cancel, |head, control| {
            owned(head, &token).map_err(|e| Blocked::Invalid(e.to_string()))?;
            control.retire(&token, successor.clone())
        })?;
    }
    rejoin(connection, local, &manifest, cancel)
}

/// Read-only discovery of the successor: the owner switches the local protocol and the
/// next ordinary poll publishes preserved pending mutations with unchanged identities.
fn rejoin(connection: &S3Connection, local: &SyncState, manifest: &str, cancel: &AtomicBool) -> Result<Outcome> {
    let (_, _, control) = read(connection, &local.library, cancel)?;
    match &control.successor {
        Some(s) if s.protocol == "append-log-v1" && s.manifest == manifest => {}
        _ => return Err(invalid("cloud library head is not retired to this append manifest")),
    }
    let mut probe = local.clone();
    probe.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
    probe.append = None;
    probe.cursor = None;
    probe.generation = 0;
    probe.pending.clear();
    probe.require_existing = true;
    crate::cloud_append::native::synchronize_verified(connection, &probe, false, true, cancel, &mut Default::default())
}

/// Abandon maintenance before activation, restoring the unchanged epoch. A different
/// device must wait for the lease to expire; the owning installation may cancel at once.
fn cancel_upgrade(connection: &S3Connection, local: &SyncState, owner: &str, cancel: &AtomicBool) -> Result<Outcome> {
    let token = crate::cloud_control::token().map_err(invalid)?;
    let head = transition(connection, &local.library, cancel, |_, control| {
        let Some(m) = &control.migration else { return Err(Blocked::Invalid("no upgrade in progress".into())) };
        control.takeover(owner, &token, now_ms(), m.owner == owner)?.cancel(&token)
    })?;
    let remote = crate::cloud_replication::read(connection, &local.library, cancel)?;
    let acknowledged = local.pending.keys().filter(|id| remote.state.received.contains_key(*id)).cloned().collect();
    let state = crate::cloud_history::native::certify(connection, local, &remote.state.portable(), &mut Default::default(), cancel)?;
    Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?, acknowledged))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// `writers_stopped`: operator confirmation, used only when the provider rejects
    /// conditional writes (the head then cannot accept any commit and is already frozen).
    Upgrade {
        plan: Plan,
        writers_stopped: bool,
    },
    Cancel,
}
pub fn run(connection: &S3Connection, local: &SyncState, owner: &str, request: Request, cancel: &AtomicBool) -> Result<Outcome> {
    if !local.protocol.is_head() {
        return Err(invalid("this library already uses append synchronization"));
    }
    match request {
        Request::Upgrade { plan: Plan::Rebase, .. } => rebase(connection, local, owner, cancel),
        Request::Upgrade { plan: Plan::AppendLog, writers_stopped } => append_log(connection, local, owner, writers_stopped, cancel),
        Request::Cancel => cancel_upgrade(connection, local, owner, cancel),
    }
}
