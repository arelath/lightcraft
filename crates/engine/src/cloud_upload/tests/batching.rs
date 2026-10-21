use super::*;
use crate::assets::s3::fixture::Fixture;
#[test]
fn a_pending_batch_counts_started_photos_without_counting_its_waiting_items_as_active() {
    let f = Fixture::new();
    let (mut s, _) = session_with_connection(
        &f.dir.join("active-count"),
        Some(json!({"id":"test","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint})),
    );
    let token = s.library_token();
    let response = acknowledgement(&mut s, token);
    for (state, expected) in [
        (UploadState::SavedLocally, 0),
        (UploadState::Uploading, 1),
        (UploadState::ObjectsVerified, 1),
        (UploadState::Published, 1),
        (UploadState::Failed, 0),
        (UploadState::Synchronized, 0),
        (UploadState::Superseded, 0),
    ] {
        let mut intent = s.catalog.upload_intents()["upload-ack-test"].clone();
        intent.state = state;
        s.apply_system(Op::SetUploadIntent { id: "upload-ack-test".into(), intent: Some(Box::new(intent)) }).unwrap();
        assert_eq!(s.cloud_uploads.active_files(&s.catalog), expected, "{state:?}");
    }
    drop(response);
}
#[test]
fn publication_snapshot_uses_latest_durable_edits_and_refuses_changed_owners_or_failed_save() {
    let f = Fixture::new();
    let root = f.dir.join("owner-library");
    let (mut s, fail) = session_with_connection(
        &root,
        Some(json!({"id":"test","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"disabled"})),
    );
    let enable = crate::cloud_sync::enable(&s.catalog, "library".into(), "owner".into(), "test".into()).unwrap();
    s.apply_system(enable).unwrap();
    s.selection = crate::Selection::single(PhotoId(1));
    s.persist().unwrap();
    let expected = s.catalog.sync_state().unwrap().clone();
    s.execute("photo.rate", &json!({"ids":[1],"rating":4})).unwrap();
    let latest = s.catalog.sync_state().unwrap().pending.clone();
    assert_ne!(latest, expected.pending);
    let token = s.library_token();
    let response = acknowledgement(&mut s, token);
    let mut pending = s.cloud_uploads.pending.pop().unwrap();
    let connection = s.library.as_mut().unwrap().cloud_connections().unwrap().remove(0);
    let snapshot = publisher::owner(&mut s, &pending, &connection, &expected).unwrap();
    assert_eq!(snapshot.state.pending, latest);
    assert_eq!(
        snapshot.complete,
        BTreeSet::from(["upload-ack-test".into()]),
        "the helper already durably acknowledges this import; later edits remain pending"
    );
    let original = pending.library.clone();
    pending.library = Arc::new(());
    assert!(publisher::owner(&mut s, &pending, &connection, &expected).is_err());
    pending.library = original;
    let revision = pending.connection_revision;
    pending.connection_revision = revision.saturating_add(1);
    assert!(publisher::owner(&mut s, &pending, &connection, &expected).is_err());
    pending.connection_revision = revision;
    let mut other = connection.clone();
    other.prefix = "another".into();
    assert!(publisher::owner(&mut s, &pending, &other, &expected).is_err());
    let mut expected_other = expected.clone();
    expected_other.library = "another".into();
    assert!(publisher::owner(&mut s, &pending, &connection, &expected_other).is_err());
    let mut intent = s.catalog.upload_intents()["upload-ack-test"].clone();
    intent.blocked = Some(UploadBlock::new("synthetic local save failure", false));
    s.apply_system(Op::SetUploadIntent { id: "upload-ack-test".into(), intent: Some(Box::new(intent)) }).unwrap();
    fail.store(true, Ordering::Relaxed);
    assert!(publisher::owner(&mut s, &pending, &connection, &expected).is_err());
    assert_eq!(s.catalog.sync_state().unwrap().pending, latest);
    assert!(f.state.lock().unwrap().requests.is_empty(), "snapshot acquisition does not contact storage");
    fail.store(false, Ordering::Relaxed);
    drop(response);
    drop(pending);
    s.persist().unwrap();
    drop(s);
    let mut reopened = Session::new().with_fs();
    reopened.open_library(&root, false).unwrap();
    assert_eq!(reopened.catalog.sync_state().unwrap().pending, latest);
}

#[test]
fn concurrent_transfer_progress_aggregates_current_streams_without_confusing_charged_bytes() {
    let activity = crate::activity::Activity::default();
    let task = activity.start("upload", "batch", crate::activity::Cancel::Yes);
    let progress = UploadProgress {
        handle: task.handle(),
        bytes: Arc::new(AtomicU64::new(50_000)),
        current: Some(Arc::new(std::sync::Mutex::new([(0, 0); batch::WORKERS]))),
        slot: 0,
    };
    let first = progress.for_worker(0);
    let second = progress.for_worker(1);
    first.progress(3, 10);
    second.progress(7, 20);
    let view = activity.list().remove(0);
    assert_eq!((view.done, view.total), (10, 30));
    first.progress(2, 5);
    let view = activity.list().remove(0);
    assert_eq!((view.done, view.total), (9, 25), "a new stream replaces its previous stream rather than double-counting");
    assert_eq!(progress.bytes.load(Ordering::Relaxed), 50_000, "request-byte accounting remains independent");
}
