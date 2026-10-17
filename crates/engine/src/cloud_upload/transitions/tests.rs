use super::*;
use lightcraft_catalog::assets::{BlobId, RemoteLocation};
use lightcraft_catalog::{
    Journal, MemStore, Photo, PhotoId,
    replication::{Change, SyncProtocol},
};

fn fixture(protocol: SyncProtocol) -> (Catalog, UploadIntent, Asset) {
    let blob = |hash: &str, key: &str| Blob {
        id: BlobId(hash.repeat(64)),
        size: 12,
        remote: RemoteLocation { connection: "test".into(), key: format!("library/{key}"), version: None },
    };
    let asset = Asset {
        original: blob("a", "original"),
        smart_preview: Some(blob("b", "preview")),
        thumbnail: Some(blob("c", "thumbnail")),
        associated: BTreeMap::new(),
    };
    let intent = UploadIntent {
        asset_id: AssetId("asset".into()),
        photo_id: PhotoId(1),
        state: UploadState::SavedLocally,
        verified: BTreeSet::new(),
        error: None,
        finalization: None,
        blocked: None,
    };
    let mut catalog = Catalog::new();
    for op in [
        Op::SetAsset { id: intent.asset_id.clone(), asset: Some(Box::new(asset.clone())) },
        Op::AddPhoto {
            photo: Box::new(Photo::new(intent.photo_id, Source::Managed { asset_id: intent.asset_id.clone() }, "photo.raw", "RAW", 32, 24, "today")),
        },
        Op::SetCloudIdentity { entity: "photo:1".into(), id: Some("shared-photo".into()) },
        Op::SetUploadIntent { id: "import".into(), intent: Some(Box::new(intent.clone())) },
    ] {
        catalog.apply(op).unwrap();
    }
    let mut state = SyncState::new("library".into(), "writer".into(), "test".into()).unwrap();
    state.protocol = protocol;
    state
        .enqueue(vec![Change {
            entity: "photo:shared-photo".into(),
            field: "content".into(),
            base: None,
            value: json!({"assetId":intent.asset_id,"asset":asset}),
        }])
        .unwrap();
    catalog.apply(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
    (catalog, intent, asset)
}
fn context(intent: &UploadIntent) -> PhaseContext<'_> {
    PhaseContext { ownership: Ownership::Current, attempt: 7, worker: 7, asset: Some(&intent.asset_id), paused: false }
}
fn update(state: UploadState, verified: BTreeSet<String>) -> PhaseUpdate {
    PhaseUpdate { state, verified, error: None, remote_invalid: false }
}
fn changed(decision: IntentDecision) -> UploadIntent {
    match decision {
        IntentDecision::Persist(intent) => *intent,
        IntentDecision::Unchanged => panic!("expected durable transition"),
    }
}

#[test]
fn deleted_or_relinked_photo_completes_retained_phases_after_restart_without_restoring_it() {
    for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
        for removal in 0..3 {
            let (mut catalog, mut intent, asset) = fixture(protocol);
            let store = MemStore::default();
            let (mut journal, _, _) = Journal::open(Box::new(store.clone())).unwrap();
            journal.snapshot(&catalog).unwrap();
            let op = match removal {
                0 => Op::SetDeleted { id: intent.photo_id, deleted: true },
                1 => Op::RemovePhoto { id: intent.photo_id },
                _ => {
                    Op::Relink { id: intent.photo_id, file_name: "local.raw".into(), source: Source::File { path: "local.raw".into() }, format: None }
                }
            };
            catalog.apply(op.clone()).unwrap();
            journal.append(&[op]).unwrap();
            assert_eq!(ContentReadiness::of(&catalog, &intent), ContentReadiness::Retained);
            for state in [UploadState::Uploading, UploadState::ObjectsVerified, UploadState::Published, UploadState::Synchronized] {
                let verified =
                    if state == UploadState::Uploading { BTreeSet::new() } else { asset.blobs().map(|blob| blob.remote.key.clone()).collect() };
                intent = changed(phase(context(&intent), &catalog, Some(&intent), update(state, verified)).unwrap());
                intent.validate(&asset).unwrap();
                assert_eq!(media_ready(intent.state, &asset, &intent.verified), state == UploadState::ObjectsVerified);
                let op = Op::SetUploadIntent { id: "import".into(), intent: Some(Box::new(intent.clone())) };
                catalog.apply(op.clone()).unwrap();
                journal.append(&[op]).unwrap();
                if state == UploadState::ObjectsVerified {
                    let (_, replayed, _) = Journal::open(Box::new(store.clone())).unwrap();
                    catalog = replayed;
                    assert_eq!(catalog.upload_intents().get("import"), Some(&intent));
                    assert_eq!(ContentReadiness::of(&catalog, &intent), ContentReadiness::Retained);
                    // Durable verified keys cannot turn a new/failed transfer attempt into readiness.
                    assert!(!media_ready(UploadState::Uploading, &asset, &intent.verified));
                    let mut incomplete = intent.verified.clone();
                    incomplete.remove(&asset.original.remote.key);
                    assert!(!media_ready(UploadState::ObjectsVerified, &asset, &incomplete));
                }
            }
            let (_, replayed, _) = Journal::open(Box::new(store)).unwrap();
            match removal {
                0 => assert!(replayed.photo(intent.photo_id).unwrap().deleted),
                1 => assert!(replayed.photo(intent.photo_id).is_none()),
                _ => assert!(matches!(replayed.photo(intent.photo_id).unwrap().source, Source::File { .. })),
            }
            catalog.apply(Op::SetSyncState { state: None }).unwrap();
            assert_eq!(ContentReadiness::of(&catalog, &intent), ContentReadiness::Unavailable);
            assert!(phase(context(&intent), &catalog, Some(&intent), update(UploadState::Uploading, BTreeSet::new())).is_err());
        }
    }
}

#[test]
fn superseded_local_revisions_ignore_late_phases_and_never_claim_remote_readiness() {
    let (mut catalog, mut intent, asset) = fixture(SyncProtocol::ConditionalHead);
    intent.state = UploadState::Superseded;
    catalog.apply(Op::SetUploadIntent { id: "import".into(), intent: Some(Box::new(intent.clone())) }).unwrap();
    for state in [UploadState::Uploading, UploadState::ObjectsVerified, UploadState::Synchronized, UploadState::Failed] {
        let update = update(state, asset.blobs().map(|b| b.remote.key.clone()).collect());
        assert!(matches!(phase(context(&intent), &catalog, Some(&intent), update).unwrap(), IntentDecision::Unchanged));
    }
    assert!(!media_ready(intent.state, &asset, &intent.verified));
    assert_eq!(catalog.sync_overview().queued_files, 0);
    assert_eq!(catalog.sync_overview().queued_bytes, 0);
    assert_eq!(catalog.sync_overview().failed_uploads, 0);
}

#[test]
fn stale_failures_and_wrong_owners_preserve_acknowledgement_but_remote_damage_revokes_it() {
    let (catalog, mut intent, asset) = fixture(SyncProtocol::ConditionalHead);
    intent.state = UploadState::Synchronized;
    intent.verified = asset.blobs().map(|blob| blob.remote.key.clone()).collect();
    intent.finalization = Some(UploadFinalization {
        version: 1,
        protocol: SyncProtocol::ConditionalHead,
        library: "library".into(),
        generation: Some(9),
        proofs: BTreeMap::from([("mutation".into(), "d".repeat(64))]),
    });
    let before = intent.clone();
    assert!(matches!(
        phase(context(&intent), &catalog, Some(&intent), update(UploadState::Failed, BTreeSet::new())).unwrap(),
        IntentDecision::Unchanged
    ));
    for ownership in [Ownership::LibraryChanged, Ownership::ConnectionChanged] {
        let mut ctx = context(&intent);
        ctx.ownership = ownership;
        assert!(phase(ctx, &catalog, Some(&intent), update(UploadState::Failed, BTreeSet::new())).is_err());
    }
    let mut ctx = context(&intent);
    ctx.attempt += 1;
    assert!(phase(ctx, &catalog, Some(&intent), update(UploadState::Failed, BTreeSet::new())).is_err());
    let another_asset = AssetId("other".into());
    let mut ctx = context(&intent);
    ctx.asset = Some(&another_asset);
    assert!(phase(ctx, &catalog, Some(&intent), update(UploadState::Failed, BTreeSet::new())).is_err());
    assert!(phase(context(&intent), &catalog, None, update(UploadState::Failed, BTreeSet::new())).is_err());
    assert_eq!(intent, before);
    let mut damage = update(UploadState::Failed, BTreeSet::new());
    damage.remote_invalid = true;
    damage.error = Some("remote bytes corrupt".into());
    let failed = changed(phase(context(&intent), &catalog, Some(&intent), damage).unwrap());
    assert_eq!(failed.state, UploadState::Failed);
    assert!(failed.finalization.is_none());
    assert_eq!(failed.error.as_deref(), Some("remote bytes corrupt"));
    let mut ctx = context(&failed);
    ctx.paused = true;
    let paused = changed(phase(ctx, &catalog, Some(&failed), update(UploadState::Failed, failed.verified.clone())).unwrap());
    assert_eq!(paused.state, UploadState::SavedLocally);
    assert!(paused.error.is_none());
}

#[test]
fn dependency_waiting_cannot_claim_publication_and_newer_receipt_proofs_remain_queued() {
    for protocol in [SyncProtocol::ConditionalHead, SyncProtocol::AppendLog] {
        let (catalog, mut intent, asset) = fixture(protocol);
        let state = catalog.sync_state().unwrap();
        let expected = BTreeMap::from([("mutation".into(), "d".repeat(64))]);
        assert!(matches!(publication_readiness(state, Some(9), BTreeMap::new(), true).unwrap(), PublicationReadiness::Waiting));
        assert!(matches!(publication_readiness(state, Some(9), expected.clone(), false).unwrap(), PublicationReadiness::Waiting));
        let PublicationReadiness::Covered(proof) = publication_readiness(state, Some(9), expected, true).unwrap() else {
            panic!("exact proof must cover the upload")
        };
        proof.validate().unwrap();
        assert_eq!(proof.generation, if protocol.is_head() { Some(9) } else { None });
        if protocol.is_head() {
            assert!(publication_readiness(state, None, proof.proofs.clone(), true).is_err());
        }
        intent.state = UploadState::Synchronized;
        intent.verified = asset.blobs().map(|blob| blob.remote.key.clone()).collect();
        intent.finalization = Some(proof.clone());
        if !protocol.is_head() {
            let mut contradictory = proof.clone();
            contradictory.generation = Some(9);
            assert!(finalized(Ownership::Current, Some(&intent), Some(&intent.asset_id), &contradictory).is_err());
        }
        let mut older = proof.clone();
        older.proofs.clear();
        assert!(finalized(Ownership::Current, Some(&intent), Some(&intent.asset_id), &older).is_err());
        for owner in [Ownership::LibraryChanged, Ownership::ConnectionChanged] {
            assert!(finalized(owner, Some(&intent), Some(&intent.asset_id), &proof).is_err());
        }
        let mut later = proof.clone();
        later.proofs.insert("later".into(), "e".repeat(64));
        if protocol.is_head() {
            later.generation = Some(10);
        }
        let finalized = changed(finalized(Ownership::Current, Some(&intent), Some(&intent.asset_id), &later).unwrap());
        assert_eq!(finalized.state, UploadState::Synchronized);
        assert!(finalized.finalization.is_none());
    }
}

#[test]
fn publication_batches_require_one_destination_library_protocol_and_causal_clock() {
    let connection: S3Connection =
        serde_json::from_value(json!({"id":"test","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9"}))
            .unwrap();
    let state = SyncState::new("library".into(), "writer".into(), "test".into()).unwrap();
    let mut rotated = connection.clone();
    rotated.profile = Some("rotated-credentials".into());
    assert!(
        publication_binding(
            [PublicationBinding { connection: &connection, state: Some(&state) }, PublicationBinding { connection: &rotated, state: Some(&state) }]
                .into_iter()
        )
        .is_ok()
    );
    let mut changed_connection = connection.clone();
    changed_connection.bucket = "other".into();
    let other_library = SyncState::new("other".into(), "writer".into(), "test".into()).unwrap();
    let mut other_protocol = state.clone();
    other_protocol.protocol = SyncProtocol::AppendLog;
    let mut later = state.clone();
    later.enqueue(vec![Change { entity: "library:library".into(), field: "label.Green".into(), base: None, value: json!("Tag") }]).unwrap();
    for (candidate_connection, candidate_state) in [
        (&changed_connection, Some(&state)),
        (&connection, Some(&other_library)),
        (&connection, Some(&other_protocol)),
        (&connection, Some(&later)),
        (&connection, None),
    ] {
        assert!(
            publication_binding(
                [
                    PublicationBinding { connection: &rotated, state: Some(&state) },
                    PublicationBinding { connection: candidate_connection, state: candidate_state }
                ]
                .into_iter()
            )
            .is_err()
        );
    }
}
