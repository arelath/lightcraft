//! Cloud mode covers the whole added library, including later local imports.
//! Durable adoption requests stage originals; the ordinary upload queue verifies
//! remote bytes before PUT. Browsing a folder never enrolls its Local records.
use super::*;
use std::collections::{BTreeSet, VecDeque};

#[derive(Default)]
pub(super) struct Queue {
    library: Option<Arc<()>>,
    checked: Option<u64>,
    pending: VecDeque<PhotoId>,
    attempted: BTreeSet<PhotoId>,
}
fn eligible(photo: &Photo) -> bool {
    !photo.deleted && !photo.local && matches!(photo.source, Source::File { .. } | Source::Demo { .. })
}
pub(crate) fn resume(s: &mut Session) {
    if s.cloud_paused
        || s.cloud_transition.local_only()
        || s.cloud_transition.blocking()
        || s.cloud_replication_jobs.migrating()
        || s.cloud_replication_jobs.preparing_migration()
    {
        return;
    }
    let Some(state) = s.catalog.sync_state() else { return };
    let connection = state.connection.clone();
    if s.library.as_ref().is_none_or(|library| !library.on_disk)
        || s.cloud_imports.preparing() > 0
        || !s.cloud_imports.pending.is_empty()
        || s.cloud_uploads.reclaiming()
        || s.cache_move.is_some()
        || s.interaction.is_some()
        || s.unsaved().is_some()
    {
        return;
    }
    let token = s.library_token();
    let queue = &mut s.cloud_imports.automatic;
    if queue.library.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &token)) {
        *queue = Queue { library: Some(token), ..Default::default() };
    }
    // A partial batch keeps its original request identity, including already
    // adopted siblings. Retrying a smaller new request would strand its issues.
    let retry = s.cloud_imports.failures.iter().find_map(|issue| {
        let params = &issue["retry"];
        let ids = params["adoptionIds"].as_array()?;
        (issue["command"] == "cloud.adopt"
            && params["automatic"] == true
            && params["connection"] == connection
            && ids
                .iter()
                .filter_map(Value::as_u64)
                .map(PhotoId)
                .any(|id| !queue.attempted.contains(&id) && s.catalog.photo(id).is_some_and(|photo| eligible(photo))))
        .then(|| params.clone())
    });
    if let Some(params) = retry {
        queue.attempted.extend(params["adoptionIds"].as_array().into_iter().flatten().filter_map(Value::as_u64).map(PhotoId));
        if let Err(reason) = s.execute("cloud.adopt", &params) {
            log::warn!("automatic cloud batch retry: {reason}");
        }
        return;
    }
    if queue.pending.is_empty() && queue.checked != Some(s.catalog.revision) {
        queue.pending.extend(s.catalog.photos().filter(|photo| eligible(photo) && !queue.attempted.contains(&photo.id)).take(1000).map(|p| p.id));
        // More than one bounded page is allowed. No fixed limit on library size.
        queue.checked = (queue.pending.len() < 1000).then_some(s.catalog.revision);
    }
    let mut ids = Vec::new();
    while let Some(id) = queue.pending.pop_front() {
        if s.catalog.photo(id).is_none_or(|photo| !eligible(photo)) || !queue.attempted.insert(id) {
            continue;
        }
        ids.push(id.0);
    }
    // Enroll the bounded page at once so preparation reports the whole batch.
    // Adoption retains per-candidate failures and deduplicates virtual copies;
    // its single worker still prepares originals sequentially.
    if !ids.is_empty()
        && let Err(reason) = s.execute("cloud.adopt", &json!({"connection":connection,"ids":ids,"wait":false,"automatic":true}))
    {
        s.cloud_imports.failures.push(json!({"id":"library-auto","name":"Library cloud synchronization","reason":reason.to_string(),"command":"cloud.adopt","retry":{"connection":connection,"ids":ids,"wait":false,"automatic":true}}));
    }
}

pub(crate) fn retry(s: &mut Session) {
    s.cloud_imports.automatic = Queue::default();
}
