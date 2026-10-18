//! Native conditional-head library upgrades. Every transition is a CAS on the same head
//! that accepts ordinary commits, and is confirmed by rereading the control it wrote:
//! "Ok on conflict" is never enough for a maintenance step (see `cloud_control`).
//! Failures keep their typed kind (connectivity, contention, unsupported, …) so status
//! and retry policy describe what actually went wrong.
use crate::assets::AssetError;
use crate::assets::s3::S3Connection;
use crate::cloud_control::{Blocked, Boundary, Control, Plan, State, Successor};
use crate::cloud_outcome::{HeadPosition, Publication};
use crate::cloud_publication::{self as publication, Batch, CheckpointRef, Head};
use crate::cloud_replication::Outcome;
use crate::cloud_status::{CloudFailure, FailureKind};
use lightcraft_catalog::replication::SyncState;
use std::sync::atomic::AtomicBool;

type Result<T> = std::result::Result<T, CloudFailure>;
const ATTEMPTS: usize = 16;
mod lease;
use crate::cloud_control::lease::Lease;
use std::sync::Mutex;
fn invalid(message: impl Into<String>) -> CloudFailure {
    CloudFailure::new(FailureKind::InvalidHistory, message)
}
fn taken_over() -> CloudFailure {
    CloudFailure::new(FailureKind::Contention, "upgrade ownership was taken over by another device; this result was not activated")
}
/// A maintenance transition's failure. Storage that rejects conditional writes cannot
/// hold the barrier at all, which the append upgrade handles separately.
enum Failure {
    Unsupported(String),
    Other(CloudFailure),
}
impl From<CloudFailure> for Failure {
    fn from(error: CloudFailure) -> Self {
        Self::Other(error)
    }
}
impl From<Blocked> for Failure {
    fn from(blocked: Blocked) -> Self {
        Self::Other(blocked.into())
    }
}
impl From<AssetError> for Failure {
    fn from(error: AssetError) -> Self {
        match error {
            AssetError::Unsupported(why) => Self::Unsupported(why),
            error => Self::Other(error.into()),
        }
    }
}
impl From<Failure> for CloudFailure {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Unsupported(why) => AssetError::Unsupported(why).into(),
            Failure::Other(error) => error,
        }
    }
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
    control.may_read()?;
    Ok((head, etag, control))
}

/// One control transition carried by an empty marker batch (so the tip and generation
/// always advance together). Returns the confirmed head; a lost response is resolved
/// by rereading and checking for exactly the control that was written.
fn transition_control(
    connection: &S3Connection,
    library: &str,
    cancel: &AtomicBool,
    next: impl Fn(&Head, &Control) -> std::result::Result<Option<Control>, Failure>,
) -> std::result::Result<Head, Failure> {
    for _ in 0..ATTEMPTS {
        let (head, etag, control) = read(connection, library, cancel)?;
        let Some(wanted) = next(&head, &control)? else { return Ok(head) };
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
            Ok(_) => {}
            Err(error) if error.uncertain_write() => {}
            Err(error) => return Err(error.into()),
        }
        // `wanted` holds a fresh revision (and, while upgrading, our fencing token), so
        // observing it proves this transition was the one accepted.
        let (current, _, observed) = read(connection, library, cancel)?;
        if observed == wanted {
            return Ok(current);
        }
        // Another commit or transition won; recompute from the new head.
    }
    Err(CloudFailure::new(FailureKind::Contention, "cloud library head remained busy; upgrade not started or not confirmed").into())
}

fn transition(
    connection: &S3Connection,
    library: &str,
    cancel: &AtomicBool,
    next: impl Fn(&Head, &Control) -> std::result::Result<Control, Failure>,
) -> std::result::Result<Head, Failure> {
    transition_control(connection, library, cancel, |h, c| next(h, c).map(Some))
}

fn with_lease<T>(
    connection: &S3Connection,
    library: &str,
    acquired: Control,
    cancel: &AtomicBool,
    body: impl FnOnce(&AtomicBool, &Mutex<Lease>) -> Result<T>,
) -> Result<T> {
    let ownership = Mutex::new(Lease::new(acquired)?);
    lease::run(
        cancel,
        |io_cancel| {
            if !ownership.lock().unwrap_or_else(std::sync::PoisonError::into_inner).due(now_ms()) {
                return Ok(());
            }
            let head = transition_control(connection, library, io_cancel, |_, control| {
                ownership.lock().unwrap_or_else(std::sync::PoisonError::into_inner).renewal(control, now_ms()).map_err(Failure::from)
            })
            .map_err(CloudFailure::from)?;
            ownership.lock().unwrap_or_else(std::sync::PoisonError::into_inner).confirmed(&head.control().map_err(invalid)?);
            Ok(())
        },
        |work_cancel| body(work_cancel, &ownership),
    )
}

/// Acquire maintenance: begin a new migration, resume our own (same installation) or
/// take over an expired one. Returns the head and our fresh token.
fn acquire(connection: &S3Connection, library: &str, plan: Plan, owner: &str, cancel: &AtomicBool) -> std::result::Result<(Head, String), Failure> {
    let token = crate::cloud_control::token().map_err(CloudFailure::local)?;
    let id = format!("{}-{}", plan.name(), token.get(..16).unwrap_or("migration"));
    let head = transition(connection, library, cancel, |head, control| {
        Ok(match &control.migration {
            None => control.begin(&id, plan, Boundary { tip: head.tip.hash.0.clone(), generation: head.generation }, owner, &token, now_ms()),
            Some(m) if m.plan != plan => Err(Blocked::Maintenance { migration: m.id.clone(), plan: m.plan, owner: m.owner.clone() }),
            Some(m) => control.takeover(owner, &token, now_ms(), m.owner == owner),
        }?)
    })?;
    Ok((head, token))
}

fn owned<'a>(head: &'a Head, token: &str) -> Result<&'a crate::cloud_control::Migration> {
    match &head.control {
        Some(Control { migration: Some(m), .. }) if m.token == token => Ok(m),
        _ => Err(taken_over()),
    }
}

/// Upgrading → Active at a new conditional-head epoch: the verified state at the frozen
/// boundary is written beside the old epoch, read back, then activated by token.
fn rebase(connection: &S3Connection, local: &SyncState, owner: &str, cancel: &AtomicBool) -> Result<Outcome> {
    let library = &local.library;
    let (head, token) = acquire(connection, library, Plan::Rebase, owner, cancel)?;
    with_lease(connection, library, head.control().map_err(invalid)?, cancel, |cancel, ownership| {
        let migration = owned(&head, &token)?.clone();
        let spec = head.control().map_err(invalid)?.migration_spec()?;
        match spec.converter() {
            crate::cloud_control::Converter::Identity => crate::cloud_control::translate_mutation(spec.source_schema(), spec.target_schema())
                .map_err(|why| CloudFailure::new(FailureKind::Unsupported, why))?,
        }
        // The maintenance head holds only marker batches after the frozen source, so
        // its full verified history is exactly the source state.
        let mut proofs = Default::default();
        let mut source = crate::cloud_replication::read_with_witness(connection, library, &mut proofs, cancel)?;
        let publishing = transition(connection, library, cancel, |head, control| {
            owned(head, &token)?;
            Ok(control.begin_output(&token, now_ms())?)
        })?;
        let epoch = migration.target_epoch;
        let first = publication::encode(&Batch { version: 3, parent: None, imports: Vec::new(), mutations: Vec::new(), checkpoint_marker: true })?;
        let first_ref = crate::cloud_replication::batch_ref(connection, epoch, &first)?;
        crate::cloud_replication::write_immutable(connection, &first_ref.key, &first, cancel)?;
        let base = publishing.generation.checked_add(1).ok_or_else(|| invalid("generation exhausted"))?;
        let checkpoint: CheckpointRef = crate::cloud_replication::checkpoint(connection, &mut source.state, first_ref.clone(), base, epoch, cancel)?;
        // Verify the replacement before activation: exact bytes, structure and state.
        let written = crate::cloud_replication::read_checkpoint(connection, &checkpoint, cancel)?;
        if written.state != source.state.portable() {
            return Err(CloudFailure::integrity("upgraded checkpoint does not reproduce the frozen source state; old epoch remains active"));
        }
        ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .expect_completion(publishing.control().map_err(invalid)?.activate(&token)?)?;
        let mut activated = None;
        for _ in 0..ATTEMPTS {
            let (current, etag, control) = read(connection, library, cancel)?;
            // A lost activation response: the reread shows our epoch with our checkpoint.
            if control.state == State::Active && control.epoch == epoch && current.checkpoint.as_ref().map(|c| &c.object) == Some(&checkpoint.object)
            {
                activated = Some(current);
                break;
            }
            owned(&current, &token)?;
            let wanted = control.activate(&token)?;
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
                Ok(_) => {}
                Err(error) if error.uncertain_write() => {}
                Err(error) => return Err(error.into()),
            }
        }
        activated.ok_or_else(|| CloudFailure::connectivity("activation not confirmed; reread before retrying"))?;
        // Another device may publish after activation, even before its response or
        // confirmation GET reaches us. Never pair the frozen source with that newer
        // cursor: replay one head snapshot and save only its matching state/position.
        let confirmed = crate::cloud_replication::read_with_witness(connection, library, &mut proofs, cancel)?;
        let (head, _) = confirmed.head.ok_or_else(|| invalid("activated cloud library head disappeared"))?;
        let control = head.control().map_err(invalid)?;
        control.may_read()?;
        if control.state != State::Active || control.epoch != epoch || head.checkpoint.as_ref().map(|c| &c.object) != Some(&checkpoint.object) {
            return Err(taken_over());
        }
        let capability = Publication::head(Some(&head), library)?;
        let state = crate::cloud_history::native::certify(connection, local, &confirmed.state.portable(), &mut proofs, cancel)?;
        let acknowledged = local.pending.keys().filter(|id| confirmed.state.received.contains_key(*id)).cloned().collect();
        Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?, acknowledged).with_publication(capability))
    })
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
        return crate::cloud_append::migration::seed(connection, local, cancel).map_err(CloudFailure::from);
    }
    let (_, _, control) = read(connection, library, cancel)?;
    if control.state != State::Retired {
        let (acquired, token) = match acquire(connection, library, Plan::AppendLog, owner, cancel) {
            Ok(value) => value,
            // Without conditional writes no conforming device can commit to this head, so
            // it is already frozen; the operator's confirmation covers older binaries.
            Err(Failure::Unsupported(_)) if writers_stopped => {
                return crate::cloud_append::migration::seed(connection, local, cancel).map_err(CloudFailure::from);
            }
            Err(Failure::Unsupported(why)) => {
                return Err(CloudFailure::new(
                    FailureKind::Unsupported,
                    format!("{why}; this provider cannot hold the maintenance barrier: stop every other device, then retry with writersStopped:true"),
                ));
            }
            Err(Failure::Other(error)) => return Err(error),
        };
        with_lease(connection, library, acquired.control().map_err(invalid)?, cancel, |cancel, ownership| {
            let publishing = transition(connection, library, cancel, |head, control| {
                owned(head, &token)?;
                Ok(control.begin_output(&token, now_ms())?)
            })?;
            crate::cloud_append::migration::seed(connection, local, cancel).map_err(CloudFailure::from)?;
            let successor = Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: manifest.clone() };
            ownership
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect_completion(publishing.control().map_err(invalid)?.retire(&token, successor.clone())?)?;
            transition(connection, library, cancel, |head, control| {
                owned(head, &token)?;
                Ok(control.retire(&token, successor.clone())?)
            })?;
            Ok(())
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
    Ok(crate::cloud_append::native::synchronize_verified(connection, &probe, false, true, cancel, &mut Default::default())?)
}

/// Abandon maintenance before any successor publication, restoring the unchanged epoch. A different
/// device must wait for the lease to expire; the owning installation may cancel at once.
fn cancel_upgrade(connection: &S3Connection, local: &SyncState, owner: &str, cancel: &AtomicBool) -> Result<Outcome> {
    let token = crate::cloud_control::token().map_err(CloudFailure::local)?;
    transition(connection, &local.library, cancel, |_, control| {
        let Some(m) = &control.migration else { return Err(invalid("no upgrade in progress; nothing to cancel").into()) };
        Ok(control.takeover(owner, &token, now_ms(), m.owner == owner)?.cancel(&token)?)
    })?;
    let remote = crate::cloud_replication::read(connection, &local.library, cancel)?;
    let (head, _) = remote.head.ok_or_else(|| invalid("cloud library has no head after cancellation"))?;
    let capability = Publication::head(Some(&head), &local.library)?;
    let acknowledged = local.pending.keys().filter(|id| remote.state.received.contains_key(*id)).cloned().collect();
    let state = crate::cloud_history::native::certify(connection, local, &remote.state.portable(), &mut Default::default(), cancel)?;
    Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?, acknowledged).with_publication(capability))
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
        return Err(CloudFailure::new(FailureKind::Unsupported, "this library already uses append synchronization"));
    }
    match request {
        Request::Upgrade { plan: Plan::Rebase, .. } => rebase(connection, local, owner, cancel),
        Request::Upgrade { plan: Plan::AppendLog, writers_stopped } => append_log(connection, local, owner, writers_stopped, cancel),
        Request::Cancel => cancel_upgrade(connection, local, owner, cancel),
    }
}
