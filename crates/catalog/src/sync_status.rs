//! Device-local sync view, indexed when synchronization inputs change (never a remote quota).
use crate::{Catalog, PhotoId, Source, assets::AssetId, uploads::UploadState};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, PoisonError},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncStatus {
    LocalOnly,
    Synced,
    Pending,
    Syncing,
    Issues,
}
impl SyncStatus {
    pub const ALL: [Self; 5] = [Self::Pending, Self::Syncing, Self::Synced, Self::Issues, Self::LocalOnly];
    pub fn label(self) -> &'static str {
        match self {
            Self::LocalOnly => "Local only",
            Self::Synced => "Synced",
            Self::Pending => "Waiting to sync",
            Self::Syncing => "Syncing",
            Self::Issues => "Sync issues",
        }
    }
}
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub cloud_photos: u64,
    pub local_photos: u64,
    pub albums: u64,
    pub original_bytes: u64,
    /// Catalog originals with remote upload evidence, excluding locally staged originals.
    pub cloud_original_bytes: u64,
    pub waiting: u64,
    pub syncing: u64,
    pub synced: u64,
    pub issues: u64,
    pub queued_files: u64,
    pub failed_files: u64,
    pub queued_bytes: u64,
    pub pending_changes: u64,
    pub blocked_import_count: u64,
    pub failed_uploads: u64,
    pub pending_receipts: u64,
    pub failed_receipts: u64,
    pub first_upload_error: Option<String>,
    pub blocked_imports: Vec<BlockedImport>,
    #[serde(skip)]
    pub statuses: BTreeMap<PhotoId, SyncStatus>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedImport {
    pub id: String,
    pub photo_id: PhotoId,
    pub name: String,
    pub reason: String,
    pub verify: bool,
}
#[derive(Debug, Default)]
pub(crate) struct Cache(Mutex<Option<Arc<Overview>>>);
impl Cache {
    pub(crate) fn invalidate(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}
impl Clone for Cache {
    fn clone(&self) -> Self {
        Self::default()
    }
}
impl PartialEq for Cache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Catalog {
    pub fn sync_overview(&self) -> Arc<Overview> {
        let mut cached = self.sync_status_cache.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(view) = cached.as_ref() {
            return view.clone();
        }
        let view = Arc::new(build(self));
        *cached = Some(view.clone());
        view
    }
    pub fn photo_sync_status(&self, id: PhotoId) -> SyncStatus {
        self.sync_overview().statuses.get(&id).copied().unwrap_or(SyncStatus::LocalOnly)
    }
}
fn build(cat: &Catalog) -> Overview {
    let mut out = Overview { albums: cat.albums().filter(|a| !a.folder).count() as u64, ..Default::default() };
    let mut pending = BTreeSet::new();
    let mut conflicts = BTreeSet::new();
    if let Some(s) = cat.sync_state() {
        out.pending_changes = s.pending.len() as u64;
        for mutation in s.pending.values() {
            for change in &mutation.changes {
                pending.insert(change.entity.as_str());
            }
        }
        for (entity, fields) in &s.documents {
            if fields.values().any(|r| r.0.values().next().is_some_and(|first| r.0.values().any(|v| v != first))) {
                conflicts.insert(entity.as_str());
            }
        }
    }
    let mut by_photo = BTreeMap::<PhotoId, SyncStatus>::new();
    let mut by_asset = BTreeMap::<AssetId, SyncStatus>::new();
    let mut files = BTreeSet::new();
    let mut failed_files = BTreeSet::new();
    let mut queue_blobs = BTreeSet::new();
    for (id, intent) in cat.upload_intents() {
        if intent.state == UploadState::Superseded {
            continue;
        }
        if out.first_upload_error.is_none() {
            out.first_upload_error = intent
                .blocked
                .as_ref()
                .map(|block| block.reason.clone())
                .or_else(|| intent.error.clone().filter(|_| intent.state == UploadState::Failed || intent.finalization.is_some()));
        }
        if intent.state == UploadState::Failed || intent.blocked.is_some() {
            out.failed_uploads = out.failed_uploads.saturating_add(1);
        }
        if intent.finalization.is_some() {
            out.pending_receipts = out.pending_receipts.saturating_add(1);
            if intent.error.is_some() {
                out.failed_receipts = out.failed_receipts.saturating_add(1);
            }
        }
        if let Some(block) = &intent.blocked {
            out.blocked_import_count = out.blocked_import_count.saturating_add(1);
            if out.blocked_imports.len() < 32 {
                out.blocked_imports.push(BlockedImport {
                    id: id.clone(),
                    photo_id: intent.photo_id,
                    name: cat.photo(intent.photo_id).map(|photo| photo.file_name.clone()).unwrap_or_else(|| id.clone()),
                    reason: block.reason.clone(),
                    verify: block.verify || intent.state == UploadState::Synchronized,
                });
            }
        }
        let Some(photo) = cat.photo(intent.photo_id).filter(|p| p.in_library() && matches!(p.source, Source::Managed { .. })) else { continue };
        let state = if intent.blocked.is_some() {
            SyncStatus::Issues
        } else {
            match intent.state {
                UploadState::Synchronized => SyncStatus::Synced,
                UploadState::Failed => SyncStatus::Issues,
                UploadState::Uploading => SyncStatus::Syncing,
                _ => SyncStatus::Pending,
            }
        };
        by_photo.entry(photo.id).and_modify(|s| *s = (*s).max(state)).or_insert(state);
        by_asset.entry(intent.asset_id.clone()).and_modify(|s| *s = (*s).max(state)).or_insert(state);
        if intent.state != UploadState::Synchronized || intent.blocked.is_some() {
            files.insert(photo.id);
            if intent.state == UploadState::Failed || intent.blocked.is_some() {
                failed_files.insert(photo.id);
            }
            if let Some(asset) = cat.asset(&intent.asset_id) {
                for blob in asset.blobs().filter(|b| !intent.verified.contains(&b.remote.key)) {
                    if queue_blobs.insert((blob.remote.connection.clone(), blob.remote.key.clone(), blob.remote.version.clone())) {
                        out.queued_bytes = out.queued_bytes.saturating_add(blob.size);
                    }
                }
            }
        }
    }
    out.queued_files = files.len() as u64;
    out.failed_files = failed_files.len() as u64;
    let mut remote_originals = BTreeMap::<AssetId, bool>::new();
    for intent in cat.upload_intents().values() {
        if let Some(asset) = cat.asset(&intent.asset_id) {
            let verified = intent.verified.contains(&asset.original.remote.key);
            remote_originals.entry(intent.asset_id.clone()).and_modify(|v| *v |= verified).or_insert(verified);
        }
    }
    let mut originals = BTreeSet::new();
    let mut cloud_originals = BTreeSet::new();
    for photo in cat.photos().filter(|p| p.in_library()) {
        let status = if let Source::Managed { asset_id } = &photo.source {
            out.cloud_photos = out.cloud_photos.saturating_add(1);
            if let Some(asset) = cat.asset(asset_id)
                && originals.insert((
                    asset.original.remote.connection.clone(),
                    asset.original.remote.key.clone(),
                    asset.original.remote.version.clone(),
                ))
            {
                out.original_bytes = out.original_bytes.saturating_add(asset.original.size);
            }
            if let Some(asset) = cat.asset(asset_id)
                // Records received from the cloud have no device-local upload intent.
                && remote_originals.get(asset_id).copied().unwrap_or(true)
                && cloud_originals.insert((asset.original.remote.connection.clone(), asset.original.remote.key.clone(), asset.original.remote.version.clone()))
            {
                out.cloud_original_bytes = out.cloud_original_bytes.saturating_add(asset.original.size);
            }
            let entity = cat.cloud_identity(&format!("photo:{}", photo.id.0)).map(|id| format!("photo:{id}"));
            let mut state =
                by_photo.get(&photo.id).copied().unwrap_or(SyncStatus::Synced).max(by_asset.get(asset_id).copied().unwrap_or(SyncStatus::Synced));
            if cat.sync_state().is_none() || entity.is_none() || entity.as_ref().is_some_and(|e| pending.contains(e.as_str())) {
                state = state.max(SyncStatus::Pending);
            }
            if entity.as_ref().is_some_and(|e| conflicts.contains(e.as_str())) {
                state = SyncStatus::Issues;
            }
            state
        } else {
            out.local_photos = out.local_photos.saturating_add(1);
            SyncStatus::LocalOnly
        };
        match status {
            SyncStatus::Pending => out.waiting = out.waiting.saturating_add(1),
            SyncStatus::Syncing => out.syncing = out.syncing.saturating_add(1),
            SyncStatus::Issues => out.issues = out.issues.saturating_add(1),
            SyncStatus::Synced => out.synced = out.synced.saturating_add(1),
            SyncStatus::LocalOnly => {}
        }
        out.statuses.insert(photo.id, status);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Filter, Journal, MemStore, Op, Photo, Sort,
        assets::{Asset, Blob, BlobId, RemoteLocation},
        replication::{Change, SyncState},
        uploads::UploadIntent,
    };
    use serde_json::json;
    fn catalog() -> Catalog {
        let mut c = Catalog::new();
        c.apply(Op::SetSyncState { state: Some(Box::new(SyncState::new("library".into(), "device".into(), "personal".into()).unwrap())) }).unwrap();
        for i in 1..=5 {
            let id = AssetId(format!("asset-{i}"));
            let asset = Asset {
                original: Blob {
                    id: BlobId(format!("{i:064x}")),
                    size: 100,
                    remote: RemoteLocation { connection: "personal".into(), key: format!("originals/{i}"), version: None },
                },
                associated: Default::default(),
                smart_preview: None,
                thumbnail: None,
            };
            c.apply(Op::SetAsset { id: id.clone(), asset: Some(Box::new(asset)) }).unwrap();
            c.apply(Op::AddPhoto {
                photo: Box::new(Photo::new(
                    PhotoId(i),
                    if i == 5 { Source::Demo { scene: 0 } } else { Source::Managed { asset_id: id.clone() } },
                    "camera.dng",
                    "DNG",
                    2,
                    2,
                    "",
                )),
            })
            .unwrap();
            c.apply(Op::SetCloudIdentity { entity: format!("photo:{i}"), id: Some(format!("global-{i}")) }).unwrap();
            if i < 4 {
                c.apply(Op::SetUploadIntent {
                    id: format!("upload-{i}"),
                    intent: Some(Box::new(UploadIntent {
                        asset_id: id,
                        photo_id: PhotoId(i),
                        state: [UploadState::SavedLocally, UploadState::Uploading, UploadState::Failed][(i - 1) as usize],
                        error: None,
                        verified: Default::default(),
                        finalization: None,
                        blocked: None,
                    })),
                })
                .unwrap();
            }
        }
        c
    }
    #[test]
    fn sync_status_filters_share_counts_and_invalidate_after_ack_and_late_edits() {
        let mut c = catalog();
        for (status, id) in
            [(SyncStatus::Pending, 1), (SyncStatus::Syncing, 2), (SyncStatus::Issues, 3), (SyncStatus::Synced, 4), (SyncStatus::LocalOnly, 5)]
        {
            assert_eq!(c.query(&Filter { sync_status: Some(status), ..Default::default() }, &Sort::default()), vec![PhotoId(id)]);
        }
        let before = c.sync_overview();
        assert_eq!((before.queued_files, before.failed_files, before.queued_bytes, before.original_bytes), (3, 1, 300, 400));
        assert_eq!(before.cloud_original_bytes, 100);
        assert!(Arc::ptr_eq(&before, &c.sync_overview()));
        c.apply(Op::SetRating { id: PhotoId(1), rating: 2 }).unwrap();
        assert!(Arc::ptr_eq(&before, &c.sync_overview()), "unrelated metadata edits reuse the index until sync intent changes");
        c.apply(Op::Batch { ops: vec![Op::SetRating { id: PhotoId(1), rating: 3 }] }).unwrap();
        assert!(Arc::ptr_eq(&before, &c.sync_overview()));
        let mut intent = c.upload_intents()["upload-1"].clone();
        intent.state = UploadState::Synchronized;
        intent.verified.insert("originals/1".into());
        c.apply(Op::SetUploadIntent { id: "upload-1".into(), intent: Some(Box::new(intent)) }).unwrap();
        assert_eq!(c.photo_sync_status(PhotoId(1)), SyncStatus::Synced);
        assert_eq!(c.sync_overview().cloud_original_bytes, 200);
        let mut state = c.sync_state().unwrap().clone();
        state.enqueue(vec![Change { entity: "photo:global-1".into(), field: "rating".into(), base: None, value: json!(4) }]).unwrap();
        c.apply(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
        assert_eq!(c.photo_sync_status(PhotoId(1)), SyncStatus::Pending);
        assert_eq!(c.sync_overview().queued_files, 2);
        let (mut journal, _, _) = Journal::open(Box::new(MemStore::new())).unwrap();
        journal.snapshot(&c).unwrap();
        // Clones with the same revision must not share a cache after different edits.
        let mut left = c.clone();
        let mut right = c.clone();
        left.apply(Op::SetDeleted { id: PhotoId(2), deleted: true }).unwrap();
        right.apply(Op::SetDeleted { id: PhotoId(3), deleted: true }).unwrap();
        assert_eq!(left.revision, right.revision);
        assert_ne!(left.sync_overview().statuses, right.sync_overview().statuses);
    }
    #[test]
    fn blocked_preflight_is_an_issue_without_revoking_a_synchronized_upload() {
        let mut c = catalog();
        let mut intent = c.upload_intents()["upload-1"].clone();
        intent.state = UploadState::Synchronized;
        intent.verified.insert("originals/1".into());
        // A shared ACK may complete the upload after the original preflight was blocked.
        intent.blocked = Some(crate::uploads::UploadBlock::new("invalid revision manifest; restore it and retry", false));
        c.apply(Op::SetUploadIntent { id: "upload-1".into(), intent: Some(Box::new(intent.clone())) }).unwrap();
        let view = c.sync_overview();
        assert_eq!(c.photo_sync_status(PhotoId(1)), SyncStatus::Issues);
        assert_eq!((view.queued_files, view.failed_files, view.blocked_import_count), (3, 2, 1));
        assert_eq!(view.blocked_imports[0].id, "upload-1");
        assert!(view.blocked_imports[0].verify);
        let files = MemStore::new();
        let (mut journal, _, _) = Journal::open(Box::new(files.clone())).unwrap();
        journal.snapshot(&c).unwrap();
        let (_, mut reopened, _) = Journal::open(Box::new(files)).unwrap();
        assert_eq!(reopened.upload_intents()["upload-1"], intent);
        intent.blocked = None;
        reopened.apply(Op::SetUploadIntent { id: "upload-1".into(), intent: Some(Box::new(intent)) }).unwrap();
        assert_eq!(reopened.photo_sync_status(PhotoId(1)), SyncStatus::Synced);
        assert_eq!(reopened.sync_overview().blocked_import_count, 0);
    }
    #[test]
    fn virtual_copies_share_upload_work_and_original_size_without_counting_deleted_photos() {
        let mut c = catalog();
        let mut copy = c.photo(PhotoId(1)).unwrap().as_ref().clone();
        copy.id = PhotoId(6);
        copy.copy_of = Some(PhotoId(1));
        c.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
        assert_eq!(c.photo_sync_status(PhotoId(6)), SyncStatus::Pending);
        assert_eq!(c.sync_overview().original_bytes, 400);
        assert_eq!(c.sync_overview().cloud_original_bytes, 100);
        assert_eq!(c.sync_overview().queued_files, 3);
        c.apply(Op::SetDeleted { id: PhotoId(3), deleted: true }).unwrap();
        assert_eq!(c.sync_overview().issues, 0);
        assert_eq!(c.sync_overview().queued_files, 2);
        let old: Filter = serde_json::from_value(json!({"rating":2})).unwrap();
        assert!(old.sync_status.is_none());
        assert!(serde_json::from_value::<Filter>(json!({"syncStatus":"guess"})).is_err());
    }
}
