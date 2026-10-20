//! Elect a frozen append source through the native B2 authority, then reuse the
//! conditional rebase lease and epoch activation. Pending work is never seeded.
use super::*;
use crate::assets::sha256;
use crate::cloud_records::{Checkpoint, LegacyIntake, RecordRef, object_key};

pub(super) fn upgrade(connection: &S3Connection, local: &SyncState, owner: &str, cancel: &AtomicBool) -> Result<Outcome> {
    if crate::cloud_setup::Provider::of(connection) != crate::cloud_setup::Provider::Backblaze || connection.prefix.is_empty() {
        return Err(CloudFailure::new(FailureKind::Unsupported, "fenced append upgrades require Backblaze B2 and a dedicated storage folder"));
    }
    local.validate().map_err(AssetError::from)?;
    for _ in 0..ATTEMPTS {
        let descriptor = connection.b2_head_enabled(cancel)?;
        let observed = connection.b2_read_authority(cancel)?;
        observed.require_writable()?;
        if observed.head().is_some() {
            if !descriptor {
                return Err(invalid("B2 authority has no supported descriptor; repair its metadata before upgrading"));
            }
            let (head, _, _) = connection.b2_read_head(cancel)?;
            if head.library.as_deref() != Some(local.library.as_str()) {
                return Err(invalid("this storage folder already contains another B2 head library; use a separate folder"));
            }
            let control = head.control().map_err(invalid)?;
            control.may_read()?;
            fence(connection, &head, cancel)?;
            return match control.state {
                State::Active => rejoin(connection, local, cancel),
                State::Upgrading => rebase(connection, local, owner, cancel),
                _ => Err(invalid("B2 library authority is not active or upgrading")),
            };
        }
        let initial = initial(connection, local, owner, cancel)?;
        match connection.b2_initialize_head(&initial, cancel) {
            Ok(_) => {}
            Err(error) if error.uncertain_write() => {}
            Err(error) => return Err(error.into()),
        }
        // Reread even on success: another initializer may have won. Only its
        // immutable checkpoint/intake and lease token can be activated.
    }
    Err(CloudFailure::new(FailureKind::Contention, "B2 upgrade initialization remained busy; retry to resume it"))
}

fn catalog(connection: &S3Connection, kind: &str, value: &impl serde::Serialize, cancel: &AtomicBool) -> Result<RecordRef> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("could not encode the frozen upgrade source"))?;
    let hash = sha256(&bytes);
    let reference = RecordRef { key: object_key(&connection.prefix, 1, kind, &hash.0), size: bytes.len() as u64, hash };
    connection.b2_put_catalog_immutable(&reference.key, &bytes, cancel)?;
    Ok(reference)
}

fn initial(connection: &S3Connection, local: &SyncState, owner: &str, cancel: &AtomicBool) -> Result<Head> {
    let source_control = crate::cloud_append::native::library_control(connection, &local.library, cancel)?;
    source_control.may_publish()?;
    crate::cloud_control::MigrationSpec::resolve(Plan::Rebase, source_control.schema, crate::cloud_control::SCHEMA)?;
    let mut probe = local.clone();
    probe.require_existing = true;
    let source = crate::cloud_append::native::receive_only(connection, &probe, cancel)?;
    if let Some(failure) = source.publication().failure() {
        return Err(failure);
    }
    let progress = source.append_progress().ok_or_else(|| invalid("append upgrade source has no verified progress"))?;
    let intake = LegacyIntake::new(&local.library, progress.scan.clone())?;
    let first = catalog(
        connection,
        "changes",
        &Batch { version: 3, parent: None, imports: Vec::new(), mutations: Vec::new(), checkpoint_marker: true },
        cancel,
    )?;
    let object = catalog(
        connection,
        "checkpoints",
        &Checkpoint { version: 1, generation: 1, covers: first.clone(), state: source.state().clone(), imports: Default::default() },
        cancel,
    )?;
    let checkpoint = CheckpointRef { object, covers: first.clone(), generation: 1 };
    if crate::cloud_replication::read_checkpoint(connection, &checkpoint, cancel)?.state != *source.state() {
        return Err(CloudFailure::integrity("frozen append checkpoint could not be verified; source retained"));
    }
    let legacy = catalog(connection, "legacy", &intake, cancel)?;
    let token = crate::cloud_control::token().map_err(CloudFailure::local)?;
    let control = Control::initial(&local.library)
        .begin(
            &format!("b2-{}", token.get(..16).unwrap_or("upgrade")),
            Plan::Rebase,
            Boundary { tip: first.hash.0.clone(), generation: 1 },
            owner,
            &token,
            now_ms(),
        )?
        .begin_output(&token, now_ms())?;
    let tip = catalog(
        connection,
        "changes",
        &Batch { version: 3, parent: Some(first), imports: Vec::new(), mutations: Vec::new(), checkpoint_marker: true },
        cancel,
    )?;
    let mut head = Head::next(None, tip, &local.library, Some(checkpoint), control).map_err(invalid)?;
    head.version = 5;
    head.generation = 2;
    head.legacy_append = Some(legacy);
    head.validate_scope_typed(&connection.prefix, &local.library)?;
    Ok(head)
}

pub(super) fn fence(connection: &S3Connection, head: &Head, cancel: &AtomicBool) -> Result<()> {
    let intake = connection.b2_legacy_intake(head, cancel)?;
    let source = crate::cloud_append::native::library_control(connection, &intake.library, cancel)?;
    source.may_read()?;
    if source.state == State::Retired
        && source.successor.as_ref().is_some_and(|s| s.protocol == "conditional-head-v1" && s.epoch == 2 && s.manifest == intake.manifest)
    {
        return Ok(());
    }
    let retired = source.retire_append_source(&intake.manifest)?;
    let key = crate::cloud_append::control::key(&connection.prefix, &retired)?;
    let bytes = crate::cloud_append::control::encode(&retired)?;
    connection.put_immutable_verified(&key, &bytes, cancel)?;
    let confirmed = crate::cloud_append::native::library_control(connection, &intake.library, cancel)?;
    if confirmed != retired {
        return Err(invalid("append source retirement changed; retry to finish the elected B2 upgrade"));
    }
    Ok(())
}

fn rejoin(connection: &S3Connection, local: &SyncState, cancel: &AtomicBool) -> Result<Outcome> {
    let mut proofs = Default::default();
    let mut remote = crate::cloud_replication::read_with_witness(connection, &local.library, &mut proofs, cancel)?;
    let (head, _) = remote.head.ok_or_else(|| invalid("B2 successor disappeared while rejoining"))?;
    let capability = Publication::head(Some(&head), &local.library)?;
    crate::cloud_history::native::hydrate(connection, &mut remote.state, local, &mut proofs, cancel)?;
    let state = crate::cloud_history::native::certify(connection, local, &remote.state.portable(), &mut proofs, cancel)?;
    let acknowledged = local.pending.keys().filter(|id| state.received.contains_key(*id)).cloned().collect();
    Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation).map_err(invalid)?, acknowledged).with_publication(capability))
}
