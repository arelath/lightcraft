//! A publication permit is acquired before asking the owner for its latest
//! durable outbox. This snapshot never substitutes for a remote publication ACK.
use super::*;
use lightcraft_catalog::replication::SyncState;
pub(super) struct Snapshot {
    pub state: Arc<SyncState>,
    pub complete: BTreeSet<String>,
    pub receipts: Vec<finalize::Ready>,
}
pub(super) fn owner(s: &mut Session, pending: &Pending, connection: &S3Connection, expected: &SyncState) -> Result<Snapshot> {
    transitions::Ownership::new(
        Arc::ptr_eq(&pending.library, &s.library_token()),
        pending.connection_revision == pending.resolver.connection_revision(),
    )
    .publication()?;
    if s.cloud_paused || pending.guard.is_cancelled() {
        return Err(error("publication paused or cancelled; staged data retained"));
    }
    let state = s.catalog.sync_state().ok_or_else(|| error("cloud publication was disabled; staged data retained"))?;
    if state.library != expected.library || state.protocol != expected.protocol || state.connection != expected.connection {
        return Err(error("publication identity changed while media transferred; staged data retained"));
    }
    let configured = s
        .library
        .as_mut()
        .ok_or_else(|| error("publication library was closed"))?
        .cloud_connections()?
        .into_iter()
        .find(|current| current.id == state.connection)
        .ok_or_else(|| error("publication connection was removed"))?;
    if !configured.same_destination(connection) {
        return Err(error("publication destination changed; staged data retained"));
    }
    s.persist()?;
    let state = Arc::new(s.catalog.sync_state().cloned().ok_or_else(|| error("publication identity disappeared"))?);
    let complete = pending
        .ids
        .iter()
        .filter_map(|(id, asset)| {
            s.catalog
                .upload_intents()
                .get(id)
                .filter(|intent| intent.asset_id == *asset && intent.state == UploadState::Synchronized)
                .map(|_| id.clone())
        })
        .collect::<BTreeSet<_>>();
    let ids = complete.iter().filter(|id| s.catalog.upload_intents().get(*id).is_some_and(|intent| intent.finalization.is_some())).cloned().collect();
    let receipts = finalize::ready(s, &ids)?;
    Ok(Snapshot { state, complete, receipts })
}
pub(super) fn request(item: &Item, events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) -> Result<Snapshot> {
    let (reply, response) = mpsc::sync_channel(1);
    let expected = item.sync.clone().ok_or_else(|| error("enable cloud synchronization before publication"))?;
    events
        .send(Event::Snapshot { connection: item.connection.storage_snapshot(), expected, reply })
        .map_err(|_| error("publication owner closed"))?;
    loop {
        match response.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(error("publication owner closed; staged data retained")),
            Err(mpsc::RecvTimeoutError::Timeout) if cancel.load(Ordering::Relaxed) => {
                return Err(error("publication cancelled; staged data retained"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}
pub(super) fn finish_complete(
    ready: &mut [(&Item, stages::VerifiedUpload)],
    snapshot: &Snapshot,
    events: &mpsc::SyncSender<Event>,
    cancel: &AtomicBool,
) -> Result<Option<Vec<Result<Value>>>> {
    if !ready.iter().all(|(item, _)| snapshot.complete.contains(&item.id)) {
        return Ok(None);
    }
    let Some((first, _)) = ready.first() else { return Ok(Some(Vec::new())) };
    for (item, _) in ready.iter() {
        if !snapshot.receipts.iter().any(|receipt| receipt.id == item.id) {
            let saved = item.load_receipt(cancel, false)?;
            if saved.state != UploadState::Synchronized || saved.append_proof.is_empty() {
                return Ok(None);
            }
        }
    }
    finalize::run(&snapshot.receipts, &first.resolver, events, cancel)?;
    let mut results = Vec::new();
    for (item, _) in ready {
        // A proof consumed by a sibling must already have a complete local receipt.
        // Reading that receipt is never remote evidence or permission to remove edits.
        let saved = item.load_receipt(cancel, false)?;
        if saved.state != UploadState::Synchronized || saved.append_proof.is_empty() || !saved.multipart.is_empty() {
            return Err(error("a synchronized import needs local receipt recovery; staging retained"));
        }
        if !snapshot.receipts.iter().any(|receipt| receipt.id == item.id) {
            let proof = UploadFinalization {
                version: 1,
                protocol: snapshot.state.protocol,
                library: snapshot.state.library.clone(),
                generation: saved.generation,
                proofs: saved.append_proof.clone(),
            };
            finalize::acknowledge(&item.id, proof, events, cancel)?;
        }
        let result = Ok(json!({"import":item.id,"synchronized":true,"receiptOnly":true}));
        item.item_result(events, &result);
        results.push(result);
    }
    Ok(Some(results))
}
