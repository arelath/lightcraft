use super::*;
use lightcraft_catalog::replication::{Mutation, Receipt, Register, Stamp};
use lightcraft_catalog::{Journal, MemStore, Store};
use std::time::Instant;

// Minimal, valid managed metadata. Build bounded authored mutations directly so
// fixture setup itself does not repeatedly clone/validate an ever-growing state.
fn fixture(count: usize) -> Catalog {
    let asset = Asset {
        original: lightcraft_catalog::assets::Blob {
            id: lightcraft_catalog::assets::BlobId("a".repeat(64)),
            size: 12,
            remote: lightcraft_catalog::assets::RemoteLocation { connection: "personal".into(), key: "library/original".into(), version: None },
        },
        smart_preview: None,
        thumbnail: None,
        associated: BTreeMap::new(),
    };
    let mut value = encode(&Catalog::new()).unwrap();
    let mut state = SyncState::new("library".into(), "local".into(), "personal".into()).unwrap();
    let mut photos = serde_json::Map::new();
    let mut identities = serde_json::Map::new();
    for start in (0..count).step_by(100) {
        let sequence = (start / 100 + 1) as u64;
        let mut changes = Vec::new();
        for n in start..(start + 100).min(count) {
            let id = PhotoId(n as u64 + 1);
            let global = format!("p{n:06}");
            let photo = Photo::new(id, Source::Managed { asset_id: AssetId("asset".into()) }, "scale.dng", "DNG", 32, 24, "");
            photos.insert(id.0.to_string(), encode(&photo).unwrap());
            identities.insert(format!("photo:{}", id.0), json!(global));
            for (field, value) in [
                ("content", json!({"assetId":"asset","asset":asset,"format":"DNG","width":32,"height":24})),
                ("file_name", json!("scale.dng")),
                ("rating", json!(0)),
            ] {
                changes.push(Change { entity: format!("photo:{global}"), field: field.into(), base: None, value });
            }
        }
        let context = if sequence == 1 { BTreeMap::new() } else { BTreeMap::from([("fixture".into(), sequence - 1)]) };
        let mutation = Mutation {
            version: 2,
            library: "library".into(),
            revision: Stamp { device: "fixture".into(), sequence },
            context: context.clone(),
            changes,
        };
        mutation.validate().unwrap();
        let stamp = mutation.revision.id();
        state.received.insert(stamp.clone(), Receipt { context, digest: mutation.digest().unwrap() });
        for change in mutation.changes {
            state.documents.entry(change.entity).or_default().insert(change.field, Register(BTreeMap::from([(stamp.clone(), change.value)])));
        }
        state.clock.insert("fixture".into(), sequence);
    }
    state.cursor = Some("a".repeat(64));
    state.generation = 1;
    state.validate().unwrap();
    value["photos"] = Value::Object(photos);
    value["assets"] = json!({"asset":asset});
    value["cloud_identities"] = Value::Object(identities);
    value["next_photo"] = json!(count + 1);
    value["sync_state"] = encode(&state).unwrap();
    Catalog::from_snapshot(&value.to_string()).unwrap()
}
#[test]
fn shared_manifest_rebinds_locally_and_rejects_changed_renditions_atomically() {
    let catalog = fixture(1);
    let mut state = catalog.sync_state().unwrap().clone();
    state.connection = "browser-device".into();
    let (base, content) = state.winning("photo:p000000", "content").unwrap();
    let mut content = content.clone();
    let asset = catalog.asset(&AssetId("asset".into())).unwrap();
    content["asset"] = encode(&asset.rebound("library").unwrap()).unwrap();
    state.enqueue(vec![Change { entity: "photo:p000000".into(), field: "content".into(), base: Some(base), value: content.clone() }]).unwrap();
    let mut rebound = catalog.clone();
    rebound.apply(Op::Batch { ops: install(&catalog, &state).unwrap() }).unwrap();
    assert_eq!(rebound.asset(&AssetId("asset".into())).unwrap(), &asset.rebound("browser-device").unwrap());
    for field in ["smartPreview", "thumbnail", "associated"] {
        let mut changed = state.clone();
        let mut content = content.clone();
        content["asset"][field] = if field == "associated" { json!({"xmp":asset.original}) } else { json!(asset.original) };
        let base = changed.winning("photo:p000000", "content").unwrap().0;
        changed.enqueue(vec![Change { entity: "photo:p000000".into(), field: "content".into(), base: Some(base), value: content }]).unwrap();
        let before = catalog.to_snapshot();
        assert!(install(&catalog, &changed).is_err());
        assert_eq!(catalog.to_snapshot(), before);
    }
}

#[test]
fn metadata_edit_projects_one_photo_and_receipt_only_progress_projects_none() {
    let catalog = fixture(25);
    let mut state = catalog.sync_state().unwrap().clone();
    state
        .enqueue(vec![Change {
            entity: "photo:p000001".into(),
            field: "rating".into(),
            base: state.winning("photo:p000001", "rating").map(|(stamp, _)| stamp),
            value: json!(4),
        }])
        .unwrap();
    PROJECTED_PHOTOS.with(|count| count.set(0));
    let mut next = catalog.clone();
    next.apply(Op::Batch { ops: install(&catalog, &state).unwrap() }).unwrap();
    assert_eq!(PROJECTED_PHOTOS.with(|count| count.get()), 1);
    assert_eq!(next.photo(PhotoId(2)).unwrap().rating, 4);
    assert!(Arc::ptr_eq(next.photo(PhotoId(25)).unwrap(), catalog.photo(PhotoId(25)).unwrap()));
    PROJECTED_PHOTOS.with(|count| count.set(0));
    let mut progress = catalog.sync_state().unwrap().clone();
    progress.cursor = Some("b".repeat(64));
    progress.generation += 1;
    assert!(install(&catalog, &progress).unwrap().is_empty());
    assert_eq!(PROJECTED_PHOTOS.with(|count| count.get()), 0);
}

#[test]
fn later_photo_arrival_refreshes_members_cover_copy_smart_selection_and_stacks() {
    let mut catalog = fixture(2);
    let mut state = catalog.sync_state().unwrap().clone();
    let changes = [
        ("photo:p000001", "copy_of", json!("later")),
        ("album:regular", "name", json!("Regular")),
        ("album:regular", "member:later", json!(true)),
        ("album:regular", "cover", json!("later")),
        ("album:smart", "name", json!("Selection")),
        ("album:smart", "smart", json!({"only":["later"],"onlyRestricted":true})),
        ("stack:pair", "photos", json!(["p000000", "later"])),
    ]
    .into_iter()
    .map(|(entity, field, value)| Change { entity: entity.into(), field: field.into(), base: None, value })
    .collect();
    state.enqueue(changes).unwrap();
    catalog.apply(Op::Batch { ops: install(&catalog, &state).unwrap() }).unwrap();
    catalog.apply(Op::SetSyncState { state: Some(Box::new(state.clone())) }).unwrap();
    assert!(catalog.photo(PhotoId(2)).unwrap().copy_of.is_none());
    assert_eq!(catalog.stacks().count(), 0);
    let content = state.winning("photo:p000000", "content").unwrap().1.clone();
    state.enqueue(vec![Change { entity: "photo:later".into(), field: "content".into(), base: None, value: content }]).unwrap();
    catalog.apply(Op::Batch { ops: install(&catalog, &state).unwrap() }).unwrap();
    let target = PhotoId(local(&catalog, "photo", "later").unwrap());
    assert_eq!(catalog.photo(PhotoId(2)).unwrap().copy_of, Some(target));
    let album = catalog.album(AlbumId(local(&catalog, "album", "regular").unwrap())).unwrap();
    assert_eq!(album.photos, vec![target]);
    assert_eq!(album.cover, Some(target));
    let smart = catalog.album(AlbumId(local(&catalog, "album", "smart").unwrap())).unwrap().smart.as_ref().unwrap();
    assert_eq!(smart.only, vec![target]);
    assert_eq!(catalog.stacks().next().unwrap().photos, vec![PhotoId(1), target]);
}

#[test]
#[ignore = "explicit CR9 10k/50k owner-thread and journal scale measurement"]
fn scale_unchanged_and_single_photo_owner_application() {
    for count in [10_000, 50_000] {
        let catalog = fixture(count);
        let state = catalog.sync_state().unwrap();
        let remote = state.portable();
        let outcome = crate::cloud_outcome::Outcome::unchanged(
            remote.clone(),
            crate::cloud_outcome::HeadPosition::new(state.cursor.clone().unwrap(), state.generation).unwrap(),
        );
        let mut timings = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            assert!(super::super::unchanged_outcome(&catalog, &outcome));
            timings.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        timings.sort_by(f64::total_cmp);
        let state_bytes = encode(state).unwrap().to_string().len();
        let mut peer = state.clone();
        peer.device = "peer".into();
        peer.enqueue(vec![Change {
            entity: "photo:p000001".into(),
            field: "rating".into(),
            base: peer.winning("photo:p000001", "rating").map(|(stamp, _)| stamp),
            value: json!(4),
        }])
        .unwrap();
        let remote = peer.portable();
        let store = MemStore::new();
        let (mut journal, _, _) = Journal::open(Box::new(store.clone())).unwrap();
        journal.snapshot(&catalog).unwrap();
        let mut session = crate::Session::new();
        session
            .open_library_in(
                crate::library::LibraryStores {
                    dir: "scale".into(),
                    catalog: Box::new(store.clone()),
                    files: Box::new(MemStore::new()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        let bytes_before = store.clone().read(lightcraft_catalog::journal::LOG).unwrap().unwrap_or_default().len();
        PROJECTED_PHOTOS.with(|count| count.set(0));
        let start = Instant::now();
        let op = incoming_checkpoint(&session.catalog, &remote, &BTreeSet::new(), "b".repeat(64), 2).unwrap();
        let prepare_ms = start.elapsed().as_secs_f64() * 1000.0;
        let old_full_record_bytes = serde_json::to_vec(&op).unwrap().len();
        let start = Instant::now();
        session.apply_received(op).unwrap();
        session.persist().unwrap();
        let changed_ms = prepare_ms + start.elapsed().as_secs_f64() * 1000.0;
        let projected = PROJECTED_PHOTOS.with(|count| count.get());
        let log = store.clone().read(lightcraft_catalog::journal::LOG).unwrap().unwrap_or_default();
        let growth = log.len() - bytes_before;
        assert_eq!(projected, 1);
        assert!(growth < 20_000, "small edit wrote {growth} bytes at {count} photos");
        assert!(old_full_record_bytes > state_bytes / 2);
        let (_, restored, report) = Journal::open(Box::new(store)).unwrap();
        assert_eq!(report.replayed, 1);
        assert_eq!(restored.to_snapshot(), session.catalog.to_snapshot());
        println!(
            "CR9 photos={count} state_bytes={state_bytes} unchanged_owner_median_ms={:.3} unchanged_owner_max_ms={:.3} unchanged_journal_growth=0 single_edit_owner_ms={changed_ms:.3} projected_photos={projected} delta_journal_bytes={growth} previous_full_record_bytes={old_full_record_bytes}",
            timings[3], timings[6]
        );
    }
}
