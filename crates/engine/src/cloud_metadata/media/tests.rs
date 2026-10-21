use super::*;
use crate::assets::s3::{AccessKeys, fixture::Fixture};
use crate::cloud_replication::VerifiedMedia;
use lightcraft_catalog::assets::RemoteLocation;
use std::io::Cursor;

const KEY: &str = "library/original";
const PATH: &str = "/photos/library/original";
const BYTES: &[u8] = b"independently verified photo bytes";
fn connection(f: &Fixture) -> S3Connection {
    let c: S3Connection =
        serde_json::from_value(serde_json::json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library"})).unwrap();
    f.state.lock().unwrap().allow_typed_keys = true;
    c.with_keys(AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap())
        .with_metadata_cache(Some(Arc::new(Cache::new(f.dir.join("media-proofs")))))
        .test_endpoint(&f.endpoint)
}
fn blob() -> Blob {
    Blob {
        id: crate::assets::sha256(BYTES),
        size: BYTES.len() as u64,
        remote: RemoteLocation { connection: "personal".into(), key: KEY.into(), version: None },
    }
}
fn seed(f: &Fixture, version: Option<&str>, bytes: &[u8]) {
    let mut state = f.state.lock().unwrap();
    state.objects.insert(PATH.into(), bytes.to_vec());
    state.media_versions.remove(PATH);
    if let Some(version) = version {
        state.media_versions.insert(PATH.into(), version.into());
    }
    state.requests.clear();
    state.traffic.clear();
}
fn verify(c: &S3Connection, b: &Blob) -> Result<bool> {
    VerifiedMedia::default().verify(c, b, &AtomicBool::new(false), &mut |_| {})
}
fn counts(f: &Fixture) -> (usize, usize) {
    let state = f.state.lock().unwrap();
    assert_eq!(state.signature_errors, 0);
    (state.requests.iter().filter(|(m, _)| m == "GET").count(), state.requests.iter().filter(|(m, _)| m == "HEAD").count())
}
#[test]
fn restart_reuses_completed_byte_proof_only_after_exact_current_version_head() {
    let f = Fixture::new();
    let c = connection(&f);
    seed(&f, Some("version-one"), BYTES);
    assert!(verify(&c, &blob()).unwrap());
    assert_eq!(counts(&f), (1, 0));
    // No publication acknowledgement has occurred. All in-memory evidence and
    // cache instances are discarded; only the atomic on-disk byte proof remains.
    drop(c);
    f.state.lock().unwrap().requests.clear();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "cloud_metadata::media::tests::restart_probe", "--nocapture"])
        .env("LIGHTCRAFT_CR82_RECEIPT_TEST", serde_json::json!({"endpoint":f.endpoint,"root":f.dir.join("media-proofs")}).to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert_eq!(counts(&f), (0, 1));
    assert!(f.state.lock().unwrap().requests.iter().all(|(method, _)| method == "HEAD"));
}
#[test]
#[ignore = "isolated recovery probe invoked by the parent with its loopback server and disk cache"]
fn restart_probe() {
    let options: serde_json::Value = serde_json::from_str(&std::env::var("LIGHTCRAFT_CR82_RECEIPT_TEST").unwrap()).unwrap();
    let c: S3Connection =
        serde_json::from_value(serde_json::json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library"})).unwrap();
    let c = c
        .with_keys(AccessKeys::new("EXAMPLE_ACCESS", "EXAMPLE_SECRET").unwrap())
        .with_metadata_cache(Some(Arc::new(Cache::new(PathBuf::from(options["root"].as_str().unwrap())))))
        .test_endpoint(options["endpoint"].as_str().unwrap());
    assert!(verify(&c, &blob()).unwrap());
}
#[test]
fn replacement_and_deletion_cannot_reuse_an_old_version_receipt() {
    let f = Fixture::new();
    let c = connection(&f);
    let b = blob();
    seed(&f, Some("v1"), BYTES);
    assert!(verify(&c, &b).unwrap());
    let corrupt = [0; BYTES.len()];
    seed(&f, Some("v2"), &corrupt);
    assert!(matches!(verify(&c, &b), Err(AssetError::Integrity)));
    assert_eq!(counts(&f), (1, 1));
    seed(&f, Some("v2"), BYTES);
    assert!(verify(&c, &b).unwrap());
    assert_eq!(counts(&f), (1, 1));
    f.state.lock().unwrap().objects.remove(PATH);
    f.state.lock().unwrap().requests.clear();
    assert!(!verify(&c, &b).unwrap());
    assert_eq!(counts(&f), (0, 1));
}
#[test]
fn pinned_receipt_checks_the_exact_historical_version_not_the_latest_object() {
    let f = Fixture::new();
    let c = connection(&f);
    let mut b = blob();
    b.remote.version = Some("v1".into());
    seed(&f, Some("v1"), BYTES);
    assert!(verify(&c, &b).unwrap());
    seed(&f, Some("v2"), &[0; BYTES.len()]);
    f.state.lock().unwrap().historical_versions.insert((PATH.into(), "v1".into()), Some(BYTES.to_vec()));
    assert!(verify(&connection(&f), &b).unwrap());
    assert_eq!(counts(&f), (0, 1));
    assert!(f.state.lock().unwrap().requests[0].1.ends_with("?versionId=v1"));
    f.state.lock().unwrap().historical_versions.clear();
    assert!(!verify(&c, &b).unwrap());
}
#[test]
fn missing_corrupt_or_wrong_binding_receipts_fall_back_to_full_bytes() {
    let f = Fixture::new();
    let c = connection(&f);
    let b = blob();
    let cancel = AtomicBool::new(false);
    seed(&f, Some("v1"), BYTES);
    assert!(verify(&c, &b).unwrap());
    let cache = c.metadata.cache.as_ref().unwrap();
    let id = c.media_receipt_identity(&b).unwrap();
    let valid = cache.load(&id, MAX_RECEIPT, &cancel).unwrap().unwrap();
    for field in ["destination", "key", "hash", "size", "actualVersion", "requestedVersion", "method", "verified", "version"] {
        let mut proof: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        proof[field] = match field {
            "size" | "version" => serde_json::json!(999),
            "verified" => serde_json::json!(false),
            _ => serde_json::json!("wrong"),
        };
        cache.save(&id, &serde_json::to_vec(&proof).unwrap(), &cancel).unwrap();
        f.state.lock().unwrap().requests.clear();
        assert!(verify(&c, &b).unwrap(), "{field}");
        // A different valid actual version requires HEAD then fresh bytes; malformed
        // binding/method data is discarded before any HEAD.
        assert_eq!(counts(&f).0, 1, "{field}");
    }
    cache.save(&id, b"not json", &cancel).unwrap();
    f.state.lock().unwrap().requests.clear();
    assert!(verify(&c, &b).unwrap());
    assert_eq!(counts(&f), (1, 0));
    cache.forget(&id, &cancel).unwrap();
    f.state.lock().unwrap().requests.clear();
    assert!(verify(&c, &b).unwrap());
    assert_eq!(counts(&f), (1, 0));
    let mut other = c.clone();
    other.bucket = "another-bucket".into();
    assert!(other.media_receipt(&b, &cancel).unwrap().is_none());
}
#[test]
fn absent_null_version_and_unknown_providers_never_replace_byte_verification() {
    for version in [None, Some("null"), Some("NULL")] {
        let f = Fixture::new();
        let c = connection(&f);
        seed(&f, version, BYTES);
        assert!(verify(&c, &blob()).unwrap());
        assert!(verify(&connection(&f), &blob()).unwrap());
        assert_eq!(counts(&f), (2, 0));
    }
    let f = Fixture::new();
    let mut c = connection(&f);
    c.endpoint = Some(f.endpoint.clone());
    seed(&f, Some("v1"), BYTES);
    assert!(verify(&c, &blob()).unwrap());
    assert!(verify(&c, &blob()).unwrap());
    assert_eq!(counts(&f), (2, 0));
}
#[test]
fn qualified_b2_versions_reuse_proof_but_missing_version_or_size_still_reads_bytes() {
    let f = Fixture::new();
    let mut c = connection(&f);
    c.endpoint = Some("https://s3.us-east-005.backblazeb2.com".into());
    c.region = "us-east-005".into();
    seed(&f, Some("b2-v1"), BYTES);
    assert!(verify(&c, &blob()).unwrap());
    f.state.lock().unwrap().requests.clear();
    assert!(verify(&c, &blob()).unwrap());
    assert_eq!(counts(&f), (0, 1));
    seed(&f, None, BYTES);
    assert!(verify(&c, &blob()).unwrap());
    assert_eq!(counts(&f), (1, 1));
    let mut longer = BYTES.to_vec();
    longer.push(0);
    seed(&f, Some("b2-v1"), &longer);
    assert!(matches!(verify(&c, &blob()), Err(AssetError::Integrity)));
    assert_eq!(counts(&f), (1, 1));
}
#[test]
fn fresh_integrity_checks_bypass_receipts_and_failed_bytes_never_create_proof() {
    let f = Fixture::new();
    let c = connection(&f);
    let b = blob();
    seed(&f, Some("v1"), BYTES);
    assert!(verify(&c, &b).unwrap());
    seed(&f, Some("v1"), &[0; BYTES.len()]);
    assert!(matches!(verify(&c.fresh_metadata(), &b), Err(AssetError::Integrity)));
    assert_eq!(counts(&f), (1, 0));
    let f = Fixture::new();
    let c = connection(&f);
    seed(&f, Some("v1"), &[0; BYTES.len()]);
    assert!(matches!(verify(&c, &b), Err(AssetError::Integrity)));
    assert!(c.media_receipt(&b, &AtomicBool::new(false)).unwrap().is_none());
    assert!(matches!(VerifiedMedia::default().verify(&c, &b, &AtomicBool::new(true), &mut |_| {}), Err(AssetError::Cancelled)));
}
#[test]
fn upload_headers_are_retained_but_only_completed_readback_creates_proof() {
    let f = Fixture::new();
    let c = connection(&f);
    let b = blob();
    let cancel = AtomicBool::new(false);
    f.state.lock().unwrap().media_versions.insert(PATH.into(), "put-version".into());
    assert_eq!(c.put_blob_observed(&b, &mut Cursor::new(BYTES), &cancel, &mut |_| {}, false).unwrap().as_deref(), Some("put-version"));
    assert!(c.media_receipt(&b, &cancel).unwrap().is_none());
    assert!(verify(&c, &b).unwrap());
    f.state.lock().unwrap().requests.clear();
    assert!(verify(&connection(&f), &b).unwrap());
    assert_eq!(counts(&f), (0, 1));
}
#[test]
fn multipart_success_and_uncertain_completion_require_complete_byte_evidence() {
    for lose in [false, true] {
        let f = Fixture::new();
        let c = connection(&f);
        let b = blob();
        let cancel = AtomicBool::new(false);
        f.state.lock().unwrap().media_versions.insert(PATH.into(), "multipart-version".into());
        let id = c.start_multipart(&b, &cancel).unwrap();
        let etag = c.upload_part(&b, &id, 1, BYTES, &cancel, &mut |_| {}).unwrap();
        f.state.lock().unwrap().lost_complete = lose;
        let result = c.complete_multipart_observed(&b, &id, &[(1, etag)], &cancel, false);
        if lose {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().as_deref(), Some("multipart-version"));
        }
        assert!(c.media_receipt(&b, &cancel).unwrap().is_none());
        assert!(verify(&c, &b).unwrap());
        f.state.lock().unwrap().requests.clear();
        assert!(verify(&connection(&f), &b).unwrap());
        assert_eq!(counts(&f), (0, 1));
    }
    let f = Fixture::new();
    let c = connection(&f);
    let b = blob();
    let cancel = AtomicBool::new(false);
    let id = c.start_multipart(&b, &cancel).unwrap();
    let etag = c.upload_part(&b, &id, 1, BYTES, &cancel, &mut |_| {}).unwrap();
    f.state.lock().unwrap().embedded_error = true;
    assert!(c.complete_multipart_observed(&b, &id, &[(1, etag)], &cancel, false).is_err());
    assert!(!verify(&c, &b).unwrap());
    assert!(c.media_receipt(&b, &cancel).unwrap().is_none());
}
