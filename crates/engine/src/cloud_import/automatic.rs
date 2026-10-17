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
    if s.cloud_paused || s.cloud_replication_jobs.migrating() || s.cloud_replication_jobs.preparing_migration() {
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
    if queue.pending.is_empty() && queue.checked != Some(s.catalog.revision) {
        queue.pending.extend(s.catalog.photos().filter(|photo| eligible(photo) && !queue.attempted.contains(&photo.id)).take(1000).map(|p| p.id));
        // More than one bounded page is allowed. No fixed limit on library size.
        queue.checked = (queue.pending.len() < 1000).then_some(s.catalog.revision);
    }
    while let Some(id) = queue.pending.pop_front() {
        if s.catalog.photo(id).is_none_or(|photo| !eligible(photo)) || !queue.attempted.insert(id) {
            continue;
        }
        // One stable per-photo request survives partial completion and restart.
        // Virtual copies sharing this original are relinked by adoption itself.
        if let Err(reason) = s.execute("cloud.adopt", &json!({"connection":connection,"ids":[id.0],"wait":false,"automatic":true})) {
            s.cloud_imports.failures.push(json!({"id":format!("library-auto-{}",id.0),"name":"Library cloud synchronization","reason":reason.to_string(),"command":"cloud.adopt","retry":{"connection":connection,"ids":[id.0],"wait":false}}));
        }
        return;
    }
}

pub(crate) fn retry(s: &mut Session) {
    s.cloud_imports.automatic = Queue::default();
}
