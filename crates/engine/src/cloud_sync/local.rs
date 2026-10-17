//! A frozen causal base plus the ordinary durable catalog, rather than one cloud
//! mutation for every edit while this device is permanently local.
use super::*;

pub(crate) fn prepare_local(catalog: &Catalog, op: Op) -> Result<Op> {
    if authored_state(catalog, &op).is_some() {
        return Err(error("cloud mutations cannot be authored while cloud sync is off"));
    }
    let Some(state) = catalog.sync_state() else { return Ok(op) };
    if catalog.local_sync_baseline().is_some() {
        return Ok(op);
    }
    let baseline = lightcraft_catalog::local_sync::Baseline {
        library: state.library.clone(),
        connection: state.connection.clone(),
        clock: state.clock.clone(),
        documents: project(catalog, catalog, state, None)?,
        imports: catalog.upload_intents().keys().cloned().collect(),
    };
    Ok(Op::Batch { ops: vec![Op::SetLocalSyncBaseline { baseline: Some(Box::new(baseline)) }, op] })
}

/// Freeze all local differences into ordinary mutations before any remote state
/// can be received. Their contexts still name the pre-disconnection causal base.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn rejoin(catalog: &Catalog) -> Result<Op> {
    let Some(baseline) = catalog.local_sync_baseline() else { return Ok(Op::Batch { ops: Vec::new() }) };
    baseline.validate()?;
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("local rejoin baseline has no cloud identity"))?;
    if state.library != baseline.library || state.connection != baseline.connection || state.clock != baseline.clock {
        return Err(error("local rejoin baseline changed unexpectedly; cloud sync remains off and local work is retained"));
    }
    let mut next = catalog.clone();
    let mut ops = ensure_ids(&mut next, &state, None)?;
    let after = project(&next, &next, &state, None)?;
    let mut changes = differences(&baseline.documents, &after, &state);
    retain_develop_versions(&state, &mut changes, &after);
    if enqueue_changes(&mut state, changes)? {
        ops.push(Op::SetSyncState { state: Some(Box::new(state.clone())) });
    }
    ops.extend(referenced_uploads(catalog, &after, Some((&state, &baseline.imports))));
    ops.push(Op::SetLocalSyncBaseline { baseline: None });
    Ok(Op::Batch { ops })
}

/// Preserve obsolete local bytes without scheduling an import that has no shared
/// content mutation. Undoing back to one of those assets explicitly reactivates it.
pub(super) fn referenced_uploads(catalog: &Catalog, after: &Documents, rejoin: Option<(&SyncState, &BTreeSet<String>)>) -> Vec<Op> {
    use lightcraft_catalog::uploads::UploadState;
    let mut referenced: BTreeSet<_> = after.values().filter_map(|fields| fields.get("content")?.get("assetId")?.as_str()).collect();
    if let Some((state, _)) = rejoin {
        referenced.extend(
            state
                .pending
                .values()
                .flat_map(|m| m.changes.iter())
                .filter(|c| c.field == "content")
                .filter_map(|c| c.value.get("assetId").and_then(Value::as_str)),
        );
        referenced.extend(
            state
                .documents
                .values()
                .filter_map(|fields| fields.get("content"))
                .flat_map(|r| r.0.values())
                .filter_map(|value| value.get("assetId").and_then(Value::as_str)),
        );
    }
    catalog
        .upload_intents()
        .iter()
        .filter_map(|(id, intent)| {
            let state = if intent.state == UploadState::Superseded && referenced.contains(intent.asset_id.0.as_str()) {
                UploadState::SavedLocally
            } else if intent.state == UploadState::SavedLocally
                && rejoin.is_some_and(|(_, imports)| !imports.contains(id))
                && !referenced.contains(intent.asset_id.0.as_str())
            {
                UploadState::Superseded
            } else {
                return None;
            };
            let mut intent = intent.clone();
            intent.state = state;
            intent.error = None;
            intent.blocked = None;
            Some(Op::SetUploadIntent { id: id.clone(), intent: Some(Box::new(intent)) })
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn resumes_uploads(catalog: &Catalog, op: &Op) -> bool {
    use lightcraft_catalog::uploads::UploadState;
    let mut ops = vec![op];
    while let Some(op) = ops.pop() {
        match op {
            Op::Batch { ops: batch } => ops.extend(batch),
            Op::SetUploadIntent { id, intent: Some(intent) }
                if intent.state == UploadState::SavedLocally
                    && catalog.upload_intents().get(id).is_some_and(|old| old.state == UploadState::Superseded) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}
