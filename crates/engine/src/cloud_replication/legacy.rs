//! Adopt verified legacy input only through the current conditional authority.
use super::*;

pub(super) fn due(connection: &S3Connection, head: Option<&Head>, requested: bool, cancel: &AtomicBool) -> Result<bool> {
    let Some(head) = head.filter(|head| head.legacy_append.is_some()) else { return Ok(false) };
    Ok(requested || connection.b2_legacy_pending(head, cancel)?)
}

pub(super) fn admit(connection: &S3Connection, remote: &Remote, audit: bool, cancel: &AtomicBool) -> Result<bool> {
    let Some((head, token)) = &remote.head else { return Ok(false) };
    if head.legacy_append.is_none() {
        return Ok(false);
    }
    let old = connection.b2_legacy_intake(head, cancel)?;
    let committed = remote.state.portable();
    let received = crate::cloud_append::native::receive_legacy(connection, head, &committed, audit, cancel)?;
    let changed = received.state != committed;
    if !changed && received.intake == old {
        return Ok(false);
    }
    let bytes = serde_json::to_vec(&received.intake).map_err(|_| invalid("could not encode legacy recovery progress"))?;
    let hash = sha256(&bytes);
    let reference = RecordRef { key: crate::cloud_records::object_key(&connection.prefix, 1, "legacy", &hash.0), size: bytes.len() as u64, hash };
    connection.b2_put_catalog_immutable(&reference.key, &bytes, cancel)?;
    connection.remember_verified_metadata(&reference, &bytes, cancel)?;
    let mut next = if changed {
        let control = head.control().map_err(invalid)?;
        let mut state = SyncState::restore_durable(&received.state, "legacy-recovery".into(), connection.id.clone()).map_err(catalog_error)?;
        let checkpoint = checkpoint(connection, &mut state, head.tip.clone(), head.generation, control.epoch, cancel)?;
        let bytes = publication::encode(&Batch {
            version: 3,
            parent: Some(head.tip.clone()),
            imports: Vec::new(),
            mutations: Vec::new(),
            checkpoint_marker: true,
        })?;
        let tip = batch_ref(connection, control.epoch, &bytes)?;
        connection.b2_put_catalog_immutable(&tip.key, &bytes, cancel)?;
        Head::next(Some(head), tip, &remote.state.library, Some(checkpoint), control).map_err(invalid)?
    } else {
        head.clone()
    };
    next.legacy_append = Some(reference);
    publication::write_head(connection, Some(token), &next, cancel)?;
    Ok(true)
}
