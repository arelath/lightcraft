use super::*;
use crate::assets::s3::{AccessKeys, fixture::Fixture};
use crate::cloud_metadata::Cache;
use std::time::Instant;

fn routed(item: &mut Item, f: &Fixture) {
    item.connection = item
        .connection
        .clone()
        .with_keys(AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap())
        .with_metadata_cache(Some(Arc::new(Cache::new(f.dir.join("proofs")))))
        .test_endpoint(&f.endpoint);
}
#[test]
fn an_interrupted_staged_multipart_upload_resumes_its_selected_algorithm_without_acknowledging_edits() {
    let f = Fixture::new();
    {
        let mut state = f.state.lock().unwrap();
        state.allow_typed_keys = true;
        state.aws_checksums = true;
        state.hold_part = true;
    }
    let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 100, 255]);
    let mut bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
    bytes.resize(9 << 20, 0);
    let source = f.dir.join("original.png");
    std::fs::write(&source, bytes).unwrap();
    let library = f.dir.join(format!("checksum-library-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    assert!(!library.exists());
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    s.execute("cloud.configure", &json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","profile":"disabled"})).unwrap();
    s.execute("cloud.syncEnable", &json!({"connection":"personal"})).unwrap();
    s.execute("cloud.import", &json!({"connection":"personal","paths":[source]})).unwrap();
    s.persist().unwrap();
    let pending = s.catalog.sync_state().unwrap().pending.clone();
    assert!(!pending.is_empty());
    let (mut selected, skipped) = items(&mut s, &json!({}), false).unwrap();
    assert!(skipped.is_empty());
    assert_eq!(selected.len(), 1);
    let mut item = selected.remove(0);
    routed(&mut item, &f);
    let id = item.id.clone();
    let blob = item.imported.asset.original.clone();
    f.state.lock().unwrap().media_versions.insert(format!("/photos/{}", blob.remote.key), "staged-version".into());
    let guard = s.activity.start("upload", "Checksum interruption test", crate::activity::Cancel::Yes);
    let progress = UploadProgress { handle: guard.handle(), bytes: Arc::new(AtomicU64::new(0)), current: None, slot: 0 };
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || {
        let mut receipt = item.load_receipt(&worker_cancel, false).unwrap();
        let result = item.upload_blob(&mut receipt, &blob, &worker_cancel, &progress, &mut VerifiedMedia::default());
        (item, receipt, result)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f.state.lock().unwrap().requests.iter().any(|(method, path)| method == "PUT" && path.contains("partNumber=1")) {
        assert!(Instant::now() < deadline, "first selected-algorithm part did not reach the fixture");
        std::thread::sleep(Duration::from_millis(5));
    }
    cancel.store(true, Ordering::Relaxed);
    let (item, _, result) = worker.join().unwrap();
    assert!(result.is_err());
    f.state.lock().unwrap().hold_part = false;
    let receipt_path = item.root.join("uploads").join(format!("{id}.json"));
    let saved = std::fs::read(&receipt_path).unwrap();
    let receipt = read_receipt(&receipt_path).unwrap().unwrap();
    let original = item.imported.asset.original.clone();
    let multipart = receipt.multipart.get(&original.remote.key).unwrap();
    assert_eq!(multipart.checksum, MultipartChecksum::Sha256Confirmed);
    assert!(multipart.parts.is_empty(), "lost response is not durable part evidence");
    let mut invalid = receipt.clone();
    invalid.storage.as_mut().unwrap().endpoint = Some("http://127.0.0.1:9".into());
    assert!(invalid.write(&item.root).is_err());
    assert_eq!(std::fs::read(&receipt_path).unwrap(), saved);
    let mut invalid = receipt.clone();
    invalid
        .multipart
        .get_mut(&original.remote.key)
        .unwrap()
        .parts
        .insert(1, Part { etag: "opaque".into(), hash: sha256(b"abc"), size: PART_BYTES as u64, provider_checksum: Some("forged".into()) });
    assert!(invalid.write(&item.root).is_err());
    assert_eq!(std::fs::read(&receipt_path).unwrap(), saved);
    drop(item);
    drop(guard);
    s.close_library().unwrap();
    drop(s);
    let mut restored = Session::new().with_fs();
    restored.open_library(&library, false).unwrap();
    assert_eq!(restored.catalog.sync_state().unwrap().pending, pending);
    let (mut selected, skipped) = items(&mut restored, &json!({"imports":[id]}), false).unwrap();
    assert!(skipped.is_empty());
    assert_eq!(selected.len(), 1);
    let mut item = selected.remove(0);
    routed(&mut item, &f);
    let mut receipt = item.load_receipt(&AtomicBool::new(false), false).unwrap();
    let guard = restored.activity.start("upload", "Checksum resume test", crate::activity::Cancel::Yes);
    let progress = UploadProgress { handle: guard.handle(), bytes: Arc::new(AtomicU64::new(0)), current: None, slot: 0 };
    let mut media = VerifiedMedia::default();
    item.upload_blob(&mut receipt, &original, &AtomicBool::new(false), &progress, &mut media).unwrap();
    assert!(item.remote_verified(&original, &AtomicBool::new(false), &progress, &mut media).unwrap());
    assert!(read_receipt(&receipt_path).unwrap().unwrap().multipart.is_empty());
    assert_eq!(restored.catalog.sync_state().unwrap().pending, pending);
    assert!(restored.catalog.upload_intents()[&id].finalization.is_none());
    let state = f.state.lock().unwrap();
    assert_eq!(state.created, 1);
    assert_eq!(state.signature_errors, 0);
    assert!(state.requests.iter().all(|(method, _)| method != "GET" && method != "DELETE"));
    assert_eq!(state.requests.iter().filter(|(method, path)| method == "PUT" && path.contains("partNumber=1")).count(), 2);
    drop(state);
    drop(item);
    drop(guard);
    restored.close_library().unwrap();
    drop(restored);
}
#[test]
fn old_multipart_receipts_keep_full_readback_and_unknown_algorithms_are_preserved_as_errors() {
    let old = json!({"id":"upload-one","parts":{"1":{"etag":"opaque","hash":sha256(b"abc"),"size":3}}});
    let parsed: Multipart = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(parsed.checksum, MultipartChecksum::None);
    assert!(parsed.parts[&1].provider_checksum.is_none());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), old);
    let mut future = old;
    future["checksum"] = json!("unsupported-future-algorithm");
    assert!(serde_json::from_value::<Multipart>(future).is_err());
}
