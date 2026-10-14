//! Real-binary append acceptance against an S3 endpoint rejecting conditional writes.
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{
    FsStore, Journal, PhotoId,
    replication::{Change, Mutation},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use support::Fixture;
fn ok(f: &Fixture, path: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (out, rows) = f.run(path, commands);
    assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
    rows
}
fn config(f: &Fixture) -> Value {
    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"})
}
fn catalog(p: &Path) -> lightcraft_engine::catalog::Catalog {
    Journal::open(Box::new(FsStore::open(p).unwrap())).unwrap().1
}
#[test]
fn archive_checkpoint_acknowledges_old_pending_and_rejects_a_late_fork_without_losing_work() {
    let f = Fixture::new();
    let (a, b, _) = initialize(&f);
    ok(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":2}))]);
    let saved = ["catalog.snap", "catalog.log"].map(|name| (name, std::fs::read(a.join(name)).unwrap()));
    let original = catalog(&a).sync_state().unwrap().pending.values().next().unwrap().clone();
    ok(&f, &a, &[("cloud.sync", json!({}))]);
    ok(&f, &b, &[("cloud.sync", json!({})), ("photo.rate", json!({"ids":[1],"rating":5})), ("cloud.sync", json!({"checkpoint":true}))]);
    assert!(!catalog(&b).sync_state().unwrap().archives.is_empty());
    for (name, bytes) in &saved {
        std::fs::write(a.join(name), bytes).unwrap();
    }
    // The receipt was compacted before this restored device saved its acknowledgement.
    ok(&f, &a, &[("photo.flag", json!({"ids":[1],"flag":"pick"})), ("cloud.sync", json!({}))]);
    assert!(catalog(&a).sync_state().unwrap().pending.is_empty());
    assert_eq!(catalog(&a).photo(PhotoId(1)).unwrap().rating, 5);
    ok(&f, &a, &[("photo.flag", json!({"ids":[1],"flag":"pick"}))]);
    let before = catalog(&a).sync_state().unwrap().pending.clone();
    let mut fork = original;
    fork.changes[0].value = json!(3);
    let key = format!("/photos/{}", lightcraft_engine::cloud_append::mutation_key("library", &fork).unwrap());
    f.state.lock().unwrap().objects.insert(key, serde_json::to_vec(&fork).unwrap());
    let (out, rows) = f.run(&a, &[("cloud.sync", json!({"audit":true}))]);
    assert!(!out.status.success());
    assert!(rows[0]["error"].to_string().contains("collision"), "{rows:?}");
    assert_eq!(catalog(&a).sync_state().unwrap().pending, before);
}
fn initialize(f: &Fixture) -> (PathBuf, PathBuf, String) {
    {
        let mut s = f.state.lock().unwrap();
        s.append_mode = true;
        s.unsupported_conditional = true;
        s.list_page_size = 1;
    }
    let a = f.dir.join("a");
    let b = f.dir.join("b");
    let rows = ok(f, &a, &[("cloud.configure", config(f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"}))]);
    let library = rows[1]["result"]["library"].as_str().unwrap().to_owned();
    f.import(&a, false);
    ok(f, &a, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
    ok(
        f,
        &b,
        &[
            ("cloud.configure", config(f)),
            ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":"append-log-v1"})),
            ("cloud.sync", json!({})),
        ],
    );
    (a, b, library)
}
#[test]
fn deleted_before_upload_publishes_frozen_import_then_tombstone_without_local_restore() {
    for permanent in [false, true] {
        let f = Fixture::new();
        let (a, b, _) = initialize(&f);
        let (_, source) = f.import(&f.dir.join("seed"), true);
        let rows = ok(&f, &a, &[("cloud.import", json!({"connection":"personal","paths":[source]}))]);
        let id = PhotoId(rows[0]["result"]["ids"][0].as_u64().unwrap());
        let import = catalog(&a).upload_intents().iter().find(|(_, intent)| intent.photo_id == id).unwrap().0.clone();
        let entity = format!("photo:{}", catalog(&a).cloud_identity(&format!("photo:{}", id.0)).unwrap());
        ok(&f, &a, &[(if permanent { "photo.deletePermanently" } else { "photo.delete" }, json!({"ids":[id.0]}))]);
        ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":5})), ("cloud.sync", json!({}))]);
        ok(&f, &a, &[("cloud.sync", json!({}))]);
        assert_eq!(catalog(&a).photo(PhotoId(1)).unwrap().rating, 5);
        assert!(!catalog(&a).sync_state().unwrap().pending.is_empty());
        ok(&f, &a, &[("cloud.upload", json!({"imports":[import]})), ("cloud.sync", json!({}))]);
        let c = catalog(&a);
        assert!(c.sync_state().unwrap().pending.is_empty());
        assert!(c.photo(id).is_none_or(|p| p.deleted));
        assert_eq!(c.sync_state().unwrap().winning(&entity, "deleted").unwrap().1, &json!(true));
        assert!(c.upload_intents().values().all(|i| i.state == lightcraft_engine::catalog::uploads::UploadState::Synchronized));
        ok(&f, &b, &[("cloud.sync", json!({}))]);
        assert_eq!(catalog(&b).sync_state().unwrap().winning(&entity, "deleted").unwrap().1, &json!(true));
    }
}

#[test]
fn append_writers_lost_response_pagination_restart_checkpoints_and_scope() {
    let f = Fixture::new();
    let (a, b, library) = initialize(&f);
    ok(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":4}))]);
    ok(&f, &b, &[("develop.set", json!({"id":1,"control":"light.exposure","value":0.7}))]);
    f.state.lock().unwrap().lost_put = true;
    std::thread::scope(|scope| {
        let one = scope.spawn(|| ok(&f, &a, &[("cloud.sync", json!({"checkpoint":true}))]));
        let two = scope.spawn(|| ok(&f, &b, &[("cloud.sync", json!({"checkpoint":true}))]));
        one.join().unwrap();
        two.join().unwrap();
    });
    ok(&f, &a, &[("cloud.sync", json!({}))]);
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    let ca = catalog(&a);
    let cb = catalog(&b);
    assert_eq!(ca.photo(PhotoId(1)).unwrap().rating, 4);
    assert_eq!(ca.photo(PhotoId(1)).unwrap().develop, cb.photo(PhotoId(1)).unwrap().develop);
    assert!(ca.sync_state().unwrap().pending.is_empty());
    assert!(cb.sync_state().unwrap().pending.is_empty());
    let mut s = f.state.lock().unwrap();
    assert_eq!(s.signature_errors, 0);
    assert!(s.list_calls > 5);
    assert!(!s.requests.iter().any(|(_, k)| k.ends_with("catalog/head.json")));
    assert!(s.objects.keys().any(|k| k.contains("/checkpoints/")));
    s.requests.clear();
    drop(s);
    let c = f.dir.join("c");
    ok(
        &f,
        &c,
        &[
            ("cloud.configure", config(&f)),
            ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":"append-log-v1"})),
            ("cloud.sync", json!({})),
        ],
    );
    assert_eq!(catalog(&c).photo(PhotoId(1)).unwrap().rating, 4);
    let (out, rows) = f.run(
        &a,
        &[(
            "cloud.configure",
            json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"other","endpoint":f.endpoint,"profile":"acceptance"}),
        )],
    );
    assert!(!out.status.success());
    assert!(rows[0]["error"].as_str().unwrap().contains("cannot be retargeted"));
}
#[test]
fn append_deletion_retains_offline_edits_until_explicit_recovery() {
    let f = Fixture::new();
    let (a, b, _) = initialize(&f);
    let global = catalog(&a).cloud_identity("photo:1").unwrap().to_owned();
    assert_eq!(catalog(&b).cloud_identity("photo:1"), Some(global.as_str()));
    ok(&f, &a, &[("photo.deletePermanently", json!({"ids":[1]})), ("cloud.sync", json!({}))]);
    ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":5})), ("cloud.sync", json!({}))]);
    ok(&f, &a, &[("cloud.sync", json!({}))]);
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    assert!(catalog(&a).photo(PhotoId(1)).is_none());
    assert!(catalog(&b).photo(PhotoId(1)).unwrap().deleted);
    assert_eq!(catalog(&b).photo(PhotoId(1)).unwrap().rating, 5);
    ok(&f, &a, &[("cloud.restoreDeleted", json!({"entity":format!("photo:{global}")})), ("cloud.sync", json!({}))]);
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    let ca = catalog(&a);
    let cb = catalog(&b);
    assert_eq!(ca.cloud_identity("photo:1"), Some(global.as_str()));
    assert_eq!(cb.cloud_identity("photo:1"), Some(global.as_str()));
    assert_eq!(ca.photo(PhotoId(1)).unwrap().rating, 5);
    assert!(!ca.photo(PhotoId(1)).unwrap().deleted);
    assert_eq!(ca.photo(PhotoId(1)).unwrap().develop, cb.photo(PhotoId(1)).unwrap().develop);
    assert_eq!(cb.photo(PhotoId(1)).unwrap().rating, 5);
    assert!(!cb.photo(PhotoId(1)).unwrap().deleted);
    assert!(ca.sync_state().unwrap().pending.is_empty());
    assert!(cb.sync_state().unwrap().pending.is_empty());
    let (out, _) = f.run(&a, &[("cloud.restoreDeleted", json!({"entity":format!("photo:{global}")}))]);
    assert!(!out.status.success());
}
#[test]
fn missing_predecessor_failed_page_and_cloned_author_fork_retain_work() {
    let f = Fixture::new();
    let (a, b, library) = initialize(&f);
    ok(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":2})), ("photo.flag", json!({"ids":[1],"flag":"pick"})), ("cloud.sync", json!({}))]);
    let author = catalog(&a).sync_state().unwrap().device.clone();
    let mut s = f.state.lock().unwrap();
    let key = s.objects.keys().find(|k| k.contains(&format!("/ops/{author}/00000000000000000003-"))).unwrap().clone();
    let bytes = s.objects.remove(&key).unwrap();
    drop(s);
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    let state = catalog(&b).sync_state().unwrap().clone();
    assert_eq!(state.append.as_ref().unwrap().frontiers[&author], 2);
    assert!(!state.append.as_ref().unwrap().gaps.is_empty());
    f.state.lock().unwrap().objects.insert(key.clone(), bytes.clone());
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    assert!(catalog(&b).sync_state().unwrap().append.as_ref().unwrap().gaps.is_empty());
    ok(&f, &b, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
    let before = catalog(&b).sync_state().unwrap().pending.clone();
    {
        let mut s = f.state.lock().unwrap();
        s.fail_list_after = Some(s.list_calls + 2);
    }
    let (out, _) = f.run(&b, &[("cloud.sync", json!({}))]);
    assert!(!out.status.success());
    assert_eq!(catalog(&b).sync_state().unwrap().pending, before);
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    let mut fork: Mutation = serde_json::from_slice(&bytes).unwrap();
    fork.changes.push(Change { entity: format!("library:{library}"), field: "label.Green".into(), base: None, value: json!("fork") });
    let fork_key = format!("/photos/{}", lightcraft_engine::cloud_append::mutation_key("library", &fork).unwrap());
    f.state.lock().unwrap().objects.insert(fork_key.clone(), serde_json::to_vec(&fork).unwrap());
    let (out, rows) = f.run(&b, &[("cloud.sync", json!({"checkpoint":true}))]);
    assert!(!out.status.success());
    assert!(rows[0]["error"].as_str().unwrap().contains("collision"));
    let s = f.state.lock().unwrap();
    assert!(s.objects.contains_key(&key) && s.objects.contains_key(&fork_key));
}
#[test]
fn explicit_migration_keeps_pending_ids_and_legacy_objects_blocks_head_continuation() {
    let f = Fixture::new();
    let a = f.dir.join("cas");
    f.import(&a, false);
    ok(
        &f,
        &a,
        &[
            ("cloud.upload", json!({})),
            ("cloud.syncEnable", json!({"connection":"personal"})),
            ("cloud.sync", json!({})),
            ("photo.rate", json!({"ids":[1],"rating":5})),
        ],
    );
    let original = catalog(&a).sync_state().unwrap().clone();
    {
        let mut s = f.state.lock().unwrap();
        s.append_mode = true;
        s.unsupported_conditional = true;
    }
    let (out, _) = f.run(&a, &[("cloud.migrateAppend", json!({}))]);
    assert!(!out.status.success());
    ok(&f, &a, &[("cloud.migrateAppend", json!({"writersStopped":true}))]);
    let migrated = catalog(&a);
    assert_eq!(migrated.sync_state().unwrap().pending, original.pending);
    assert_eq!(migrated.cloud_identity("photo:1"), catalog(&a).cloud_identity("photo:1"));
    ok(&f, &a, &[("cloud.sync", json!({}))]);
    assert_eq!(catalog(&a).photo(PhotoId(1)).unwrap().rating, 5);
    assert!(f.state.lock().unwrap().objects.contains_key("/photos/library/catalog/head.json"));
    let c = f.dir.join("old-client");
    let (out, rows) = f.run(
        &c,
        &[
            ("cloud.configure", config(&f)),
            ("cloud.syncEnable", json!({"connection":"personal","library":original.library})),
            ("cloud.sync", json!({})),
        ],
    );
    assert!(!out.status.success());
    assert!(rows[2]["error"].as_str().unwrap().contains("append manifest"));
}

#[test]
fn one_import_per_job_waits_for_siblings_and_reconciles_all_shared_publications() {
    let f = Fixture::new();
    f.state.lock().unwrap().append_mode = true;
    f.state.lock().unwrap().unsupported_conditional = true;
    let a = f.dir.join("automatic-bulk");
    ok(&f, &a, &[("cloud.configure", config(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"}))]);
    let mut paths = Vec::new();
    for n in 0..2 {
        let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, n * 100, 255]);
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
        let path = f.dir.join(format!("bulk-{n}.png"));
        std::fs::write(&path, bytes).unwrap();
        paths.push(path);
    }
    ok(&f, &a, &[("cloud.import", json!({"connection":"personal","paths":paths}))]);
    let c = catalog(&a);
    let imports = c.upload_intents().keys().cloned().collect::<Vec<_>>();
    assert_eq!(imports.len(), 2);
    ok(&f, &a, &[("cloud.upload", json!({"imports":[imports[0]]}))]);
    let c = catalog(&a);
    assert_eq!(c.upload_intents()[&imports[0]].state, lightcraft_engine::catalog::uploads::UploadState::ObjectsVerified);
    assert!(c.upload_intents()[&imports[0]].error.is_none());
    assert!(!c.sync_state().unwrap().pending.is_empty());
    // Each command reopens the catalog: dependency waiting and proof reconciliation
    // must survive a restart as well as the automatic scheduler's single-item jobs.
    ok(&f, &a, &[("cloud.upload", json!({"imports":[imports[1]]}))]);
    let c = catalog(&a);
    assert!(c.sync_state().unwrap().pending.is_empty());
    assert!(c.upload_intents().values().all(|i| i.state == lightcraft_engine::catalog::uploads::UploadState::Synchronized));
    assert!(c.upload_intents().values().all(|i| i.error.is_none()));
}

#[test]
fn bulk_import_verifies_all_assets_before_publishing_shared_mutation_and_handles_multipart() {
    let f = Fixture::new();
    {
        let mut s = f.state.lock().unwrap();
        s.append_mode = true;
        s.unsupported_conditional = true;
        s.lost_complete = true;
    }
    let a = f.dir.join("bulk");
    ok(&f, &a, &[("cloud.configure", config(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"}))]);
    let (_, small) = f.import(&a, false);
    let mut large = std::fs::read(&small).unwrap();
    large.resize(9 << 20, 0);
    let path = f.dir.join("large.dng");
    std::fs::write(&path, large).unwrap();
    ok(&f, &a, &[("cloud.import", json!({"connection":"personal","paths":[small,path]})), ("cloud.upload", json!({}))]);
    let c = catalog(&a);
    assert_eq!(c.len(), 3);
    assert!(c.upload_intents().values().all(|i| i.state == lightcraft_engine::catalog::uploads::UploadState::Synchronized));
    assert!(c.sync_state().unwrap().pending.is_empty());
    assert!(f.state.lock().unwrap().created > 0);
    assert!(!f.state.lock().unwrap().lost_complete, "committed multipart response was lost");
    assert_cr31_media_cost(&f, &c, false);

    for entry in std::fs::read_dir(a.join("Managed/uploads")).unwrap() {
        let receipt: Value = serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
        assert!(receipt["multipart"].as_object().unwrap().is_empty());
    }
}

#[test]
fn root_rescan_discovers_an_author_inserted_before_the_previous_page_cursor() {
    let f = Fixture::new();
    let (_, b, library) = initialize(&f);
    let m = Mutation {
        version: 1,
        library: library.clone(),
        revision: lightcraft_engine::catalog::replication::Stamp { device: "000-late".into(), sequence: 1 },
        context: Default::default(),
        changes: vec![Change { entity: format!("library:{library}"), field: "label.Green".into(), base: None, value: json!("Late author") }],
    };
    let key = format!("/photos/{}", lightcraft_engine::cloud_append::mutation_key("library", &m).unwrap());
    f.state.lock().unwrap().late_object = Some((key, serde_json::to_vec(&m).unwrap()));
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    assert!(!catalog(&b).sync_state().unwrap().append.as_ref().unwrap().frontiers.contains_key("000-late"));
    ok(&f, &b, &[("cloud.sync", json!({}))]);
    assert_eq!(catalog(&b).sync_state().unwrap().append.as_ref().unwrap().frontiers.get("000-late"), Some(&1));
    assert_eq!(catalog(&b).custom_label_name(lightcraft_engine::catalog::ColorLabel::Green), Some("Late author"));
}

#[test]
fn restored_catalog_rotates_author_and_publishes_historical_pending_without_rewriting() {
    let f = Fixture::new();
    let (a, _, _) = initialize(&f);
    ok(&f, &a, &[("photo.rate", json!({"ids":[1],"rating":5}))]);
    let before = catalog(&a).sync_state().unwrap().clone();
    let restored = f.dir.join("restored");
    std::fs::create_dir_all(&restored).unwrap();
    for name in ["catalog.snap", "catalog.log"] {
        let source = a.join(name);
        if source.is_file() {
            std::fs::copy(source, restored.join(name)).unwrap();
        }
    }
    ok(&f, &restored, &[("cloud.configure", config(&f))]);
    let state = catalog(&restored).sync_state().unwrap().clone();
    assert_ne!(state.device, before.device);
    assert_eq!(state.pending, before.pending);
    assert!(state.previous_authors.contains(&before.device));
    ok(&f, &restored, &[("photo.flag", json!({"ids":[1],"flag":"pick"})), ("cloud.sync", json!({}))]);
    ok(&f, &a, &[("cloud.sync", json!({}))]);
    let original = catalog(&a);
    assert_eq!(original.photo(PhotoId(1)).unwrap().rating, 5);
    assert_eq!(original.photo(PhotoId(1)).unwrap().flag, lightcraft_engine::catalog::Flag::Pick);
    assert!(original.sync_state().unwrap().pending.is_empty());
}

#[test]
fn explicit_migration_rebinds_failed_legacy_upload_receipts_and_keeps_staged_originals() {
    let f = Fixture::new();
    let a = f.dir.join("legacy-failed");
    ok(&f, &a, &[("cloud.configure", config(&f)), ("cloud.syncEnable", json!({"connection":"personal"}))]);
    f.import(&a, false);
    f.state.lock().unwrap().unsupported_conditional = true;
    let (out, _) = f.run(&a, &[("cloud.upload", json!({}))]);
    assert!(!out.status.success());
    let pending = catalog(&a).sync_state().unwrap().pending.clone();
    f.state.lock().unwrap().append_mode = true;
    ok(&f, &a, &[("cloud.migrateAppend", json!({"writersStopped":true}))]);
    assert_eq!(catalog(&a).sync_state().unwrap().pending, pending);
    ok(&f, &a, &[("cloud.upload", json!({}))]);
    let c = catalog(&a);
    assert!(c.sync_state().unwrap().pending.is_empty());
    assert!(c.upload_intents().values().all(|i| i.state == lightcraft_engine::catalog::uploads::UploadState::Synchronized));
}

fn raw_bytes() -> Vec<u8> {
    use lightcraft_raw::*;
    let (w, h) = (32, 24);
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16((0..w * h).map(|i| 256 + ((i % w) * 300 + (i / w) * 200) as u16).collect()),
        cfa: Some(Cfa::bayer("RGGB").unwrap()),
        bits: 14,
        black: BlackLevel::uniform(256.0),
        white: vec![16000.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color: ColorData {
            illuminant: [17, 21],
            color_matrix: [
                Some(lightcraft_color::Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])),
                Some(lightcraft_color::Mat3([[0.7, 0.3, -0.1], [-0.35, 1.3, 0.1], [0.05, -0.2, 1.0]])),
            ],
            as_shot_neutral: Some([0.5, 1.0, 0.7]),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        opcodes: Default::default(),
        metadata: Default::default(),
    };
    write_dng(&raw, &DngWriteOptions::default()).unwrap()
}

#[test]
fn append_dng_xmp_and_external_revisions_keep_photo_identity_edits_and_verified_dependencies() {
    let f = Fixture::new();
    {
        let mut s = f.state.lock().unwrap();
        s.append_mode = true;
        s.unsupported_conditional = true;
    }
    let a = f.dir.join("revisions");
    let source = f.dir.join("generated.dng");
    std::fs::write(&source, raw_bytes()).unwrap();
    ok(
        &f,
        &a,
        &[
            ("cloud.configure", config(&f)),
            ("cloud.syncEnable", json!({"connection":"personal","protocol":"append-log-v1"})),
            ("cloud.import", json!({"connection":"personal","paths":[source]})),
            ("cloud.upload", json!({})),
            ("photo.rate", json!({"ids":[1],"rating":4})),
            ("develop.set", json!({"id":1,"control":"light.exposure","value":0.75})),
            ("cloud.sync", json!({})),
        ],
    );
    let before = catalog(&a);
    let identity = before.cloud_identity("photo:1").unwrap().to_owned();
    let original = before.photo(PhotoId(1)).unwrap().source.clone();
    std::fs::remove_file(&source).unwrap();
    ok(
        &f,
        &a,
        &[
            ("photo.convertToDng", json!({"ids":[1],"wait":true})),
            ("cloud.upload", json!({})),
            ("photo.saveMetadataToFile", json!({"ids":[1]})),
            ("cloud.upload", json!({})),
        ],
    );
    let c = catalog(&a);
    let p = c.photo(PhotoId(1)).unwrap();
    assert_ne!(p.source, original);
    assert_eq!(p.rating, 4);
    assert_eq!(p.develop.light.exposure, 0.75);
    assert_eq!(c.cloud_identity("photo:1"), Some(identity.as_str()));
    assert!(c.sync_state().unwrap().pending.is_empty());
    assert!(c.upload_intents().values().all(|i| i.state == lightcraft_engine::catalog::uploads::UploadState::Synchronized));
    let rows = ok(&f, &a, &[("photo.editExternal", json!({"ids":[1],"format":"tiff","bitDepth":16}))]);
    let id = rows[0]["result"]["id"].as_u64().unwrap();
    let working = PathBuf::from(rows[0]["result"]["path"].as_str().unwrap());
    let image = lightcraft_raster::Rgba8::from_fn(13, 9, |_, _| [60, 120, 180, 255]);
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
    std::fs::write(&working, bytes).unwrap();
    ok(&f, &a, &[("photo.reload", json!({"ids":[id]})), ("cloud.upload", json!({})), ("cloud.sync", json!({"checkpoint":true}))]);
    let c = catalog(&a);
    let p = c.photo(PhotoId(id)).unwrap();
    assert_eq!((p.width, p.height), (13, 9));
    assert!(c.sync_state().unwrap().pending.is_empty());
    assert!(f.state.lock().unwrap().objects.keys().any(|k| k.ends_with(".xmp")));
}

/// Normal and lost-completion publication use fresh evidence, not another media download.
fn assert_cr31_media_cost(f: &Fixture, c: &lightcraft_engine::catalog::Catalog, existing: bool) {
    let objects: std::collections::BTreeMap<_, _> = c
        .upload_intents()
        .values()
        .flat_map(|intent| c.asset(&intent.asset_id).unwrap().blobs())
        .map(|blob| (format!("/photos/{}", blob.remote.key), blob.size))
        .collect();
    let state = f.state.lock().unwrap();
    assert_eq!(state.signature_errors, 0);
    for (key, size) in objects {
        let media: Vec<_> = state.traffic.iter().filter(|request| request.target.split('?').next() == Some(key.as_str())).collect();
        let gets: Vec<_> = media.iter().filter(|request| request.method == "GET" && !request.target.contains("uploadId=")).collect();
        assert_eq!(
            gets.iter().filter(|request| request.status == 200).count(),
            1,
            "one full verification, including recovery probes: {key}: {gets:?}"
        );
        assert_eq!(
            gets.iter().map(|request| request.response_bytes).sum::<u64>(),
            size,
            "no original/preview bytes repeated during publication: {key}"
        );
        assert_eq!(gets.len(), if existing { 1 } else { 2 }, "only initial availability and one byte check: {key}");
        let puts: Vec<_> = media.iter().filter(|request| request.method == "PUT").collect();
        assert_eq!(puts.iter().map(|request| request.request_bytes).sum::<u64>(), if existing { 0 } else { size }, "send each payload once: {key}");
        assert_eq!(
            puts.len(),
            if existing {
                0
            } else if size > 8 << 20 {
                2
            } else {
                1
            },
            "no hidden upload retry: {key}"
        );
    }
}

#[test]
fn cr31_bulk_stage_costs_are_bounded_in_both_protocols_and_restart_verifies_again() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let library = f.dir.join("stages");
        ok(&f, &library, &[("cloud.configure", config(&f)), ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol}))]);
        let mut paths = Vec::new();
        for n in 0..3 {
            let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, n * 50, 255]);
            let mut bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
            if n == 2 {
                bytes.resize(9 << 20, 0);
            }
            let path = f.dir.join(format!("stage-{n}.png"));
            std::fs::write(&path, bytes).unwrap();
            paths.push(path);
        }
        ok(&f, &library, &[("cloud.import", json!({"connection":"personal","paths":paths})), ("cloud.upload", json!({}))]);
        let c = catalog(&library);
        assert!(
            c.upload_intents()
                .values()
                .all(|intent| intent.state == lightcraft_engine::catalog::uploads::UploadState::Synchronized && intent.finalization.is_none())
        );
        assert_cr31_media_cost(&f, &c, false);
        // Empty publication/ordinary scans have no media payload requests.
        let before = f.state.lock().unwrap().traffic.len();
        ok(&f, &library, &[("cloud.sync", json!({})), ("cloud.upload", json!({}))]);
        let state = f.state.lock().unwrap();
        let media_keys: std::collections::BTreeSet<_> =
            c.assets().values().flat_map(|asset| asset.blobs()).map(|blob| format!("/photos/{}", blob.remote.key)).collect();
        assert!(
            state.traffic[before..].iter().all(|request| !media_keys.contains(request.target.split('?').next().unwrap())),
            "metadata-only work must not fetch originals"
        );
        drop(state);
        // Explicit verification after reopen must freshly check bytes once, never trust disk keys.
        f.state.lock().unwrap().traffic.clear();
        ok(&f, &library, &[("cloud.upload", json!({"verify":true}))]);
        assert_cr31_media_cost(&f, &catalog(&library), true);
    }
}

#[test]
fn cr31_separate_sibling_workers_do_not_download_each_others_bulk_media_again() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let library = f.dir.join("sibling-stages");
        let mut paths = Vec::new();
        for n in 0..3 {
            let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, n * 50, 255]);
            let mut bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
            if n == 2 {
                bytes.resize(9 << 20, 0)
            }
            let path = f.dir.join(format!("sibling-{n}.png"));
            std::fs::write(&path, bytes).unwrap();
            paths.push(path);
        }
        ok(
            &f,
            &library,
            &[
                ("cloud.configure", config(&f)),
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
                ("cloud.import", json!({"connection":"personal","paths":paths})),
            ],
        );
        let c = catalog(&library);
        let mut ids: Vec<_> = c.upload_intents().iter().map(|(id, intent)| (c.asset(&intent.asset_id).unwrap().original.size, id.clone())).collect();
        ids.sort();
        ok(
            &f,
            &library,
            &[
                ("cloud.upload", json!({"imports":[ids[0].1],"wait":false})),
                ("cloud.upload", json!({"imports":[ids[1].1],"wait":false})),
                ("cloud.upload", json!({"imports":[ids[2].1],"wait":true})),
                ("cloud.sync", json!({})),
            ],
        );
        let c = catalog(&library);
        assert!(c.upload_intents().values().all(|intent| intent.state == lightcraft_engine::catalog::uploads::UploadState::Synchronized));
        assert_cr31_media_cost(&f, &c, false);
    }
}
